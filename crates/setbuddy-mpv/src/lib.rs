//! The mpv playback engine.
//!
//! This is the only crate in the workspace that knows mpv exists. It satisfies
//! [`PlaybackEngine`] and is otherwise invisible: `setwave-core`, the CLI, and the
//! menu bar app address it purely through that trait.
//!
//! Behaviour here is grounded in the M0 spike (`docs/mpv-notes.md`), which
//! verified on real VP9/Opus media that toggling the `vid` property destroys and
//! recreates the video window with **zero** discontinuity in `audio-pts`. That is
//! what makes [`MpvEngine::set_video_visible`] a true pop-out rather than a
//! stop-and-restart.

mod ipc;

use std::io::ErrorKind;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use setwave_engine::{
    EngineCapabilities, EngineError, EngineSnapshot, PlaybackEngine, VideoWindowLayout,
};

use crate::ipc::{Ipc, IpcError, OBSERVED};

const DISPLAY_NAME: &str = "mpv";
const INSTALL_HINT: &str = "Install it with `brew install mpv`, then try again.";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const LOAD_TIMEOUT: Duration = Duration::from_secs(20);
const SOCKET_WAIT: Duration = Duration::from_secs(10);

/// Containers Setwave indexes and hands to mpv. mpv itself plays far more; this
/// list is the honest intersection with what the library scanner recognises.
const CONTAINERS: &[&str] = &[
    "webm", "mkv", "mp4", "m4v", "mov", "avi", "flv", "ts", "m4a", "mp3", "wav", "flac", "opus",
    "ogg", "oga", "aac", "alac", "aiff", "aif", "wma", "wv", "ape",
];

/// A layout in mpv's own terms. `geometry` is an exact size and place;
/// `autofit` a box to fit within; either is empty when it does not apply,
/// and mpv treats an empty value as unset. All of it is in physical pixels
/// — mpv does not think in points — with `+x+y` measured from the top-left
/// of the screen's usable area.
struct WindowOptions {
    geometry: String,
    autofit: String,
    fullscreen: bool,
    /// An index, or `default` to let mpv choose.
    screen: String,
}

fn window_options(layout: VideoWindowLayout) -> WindowOptions {
    let mut options = WindowOptions {
        geometry: String::new(),
        autofit: String::new(),
        fullscreen: false,
        screen: "default".into(),
    };
    // Presets sit in the middle of the screen. mpv reads a percentage
    // position as "this far along, aligned so 100% is the far edge", so
    // `+50%+50%` is a true centre whatever the window's size turns out to be.
    match layout {
        VideoWindowLayout::ScreenFraction(fraction) => {
            options.geometry = format!("{}%+50%+50%", (fraction * 100.0).round().clamp(1.0, 100.0));
        }
        // A box the size of the screen, not a width: `geometry=100%` would
        // set the width alone and push a tall video off the bottom. The
        // geometry here is position only; autofit does the sizing.
        VideoWindowLayout::Fill => {
            options.autofit = "100%x100%".into();
            options.geometry = "+50%+50%".into();
        }
        VideoWindowLayout::Fullscreen => options.fullscreen = true,
        VideoWindowLayout::Custom {
            width,
            position,
            screen,
        } => {
            options.geometry = match position {
                None => width.to_string(),
                Some((x, y)) => format!("{width}+{x}+{y}"),
            };
            if let Some(index) = screen {
                options.screen = index.to_string();
            }
        }
    }
    options
}

/// The spawn arguments for a layout. Empty values are left out rather than
/// passed as `--geometry=`, which mpv would read as an argument error.
fn layout_args(layout: VideoWindowLayout) -> Vec<String> {
    let options = window_options(layout);
    let mut args = vec![
        format!("--screen={}", options.screen),
        format!(
            "--fullscreen={}",
            if options.fullscreen { "yes" } else { "no" }
        ),
    ];
    if !options.geometry.is_empty() {
        args.push(format!("--geometry={}", options.geometry));
    }
    if !options.autofit.is_empty() {
        args.push(format!("--autofit={}", options.autofit));
    }
    args
}

/// Arguments the M0 spike validated. Every one of these is load-bearing; see
/// `docs/mpv-notes.md` for what breaks without it.
fn spawn_args(socket: &Path, layout: VideoWindowLayout) -> Vec<String> {
    [
        // Never inherit the user's own mpv setup. Their config can carry
        // `save-position-on-quit` (which fights ResumeStore for control of
        // position), a personal `input-ipc-server` path, and Lua scripts that
        // would execute inside Setwave's player. Discovered the hard way in M0.
        "--no-config",
        "--load-scripts=no",
        // Stay alive with nothing loaded so the process outlives a track change.
        "--idle=yes",
        "--no-terminal",
        // No window until video is explicitly switched on — the app is
        // audio-first and pops video out on demand.
        "--force-window=no",
        // Hold the last frame at EOF instead of quitting, so `end-file` is
        // observable and the queue decides what happens next.
        "--keep-open=yes",
        // Leave the media keys to the app's MPRemoteCommandCenter, or both
        // handlers fire and every press toggles twice.
        "--input-media-keys=no",
        // Relinquish the macOS Now Playing widget so Setwave owns it.
        "--media-controls=no",
        // Register as a UIElement: no Dock icon, no Cmd-Tab entry. The video
        // window still displays normally under this policy.
        "--macos-app-activation-policy=accessory",
        "--macos-menu-shortcuts=no",
        // Chrome-free floating video.
        "--title-bar=no",
        "--border=no",
        "--ontop=yes",
        "--ontop-level=system",
        // Fullscreen in place rather than in a Space of its own: the window is
        // a floating set, not an app the user switched to, and this keeps it
        // on the screen they are looking at and above everything on it.
        "--native-fs=no",
    ]
    .iter()
    .map(|s| s.to_string())
    // Laid out at spawn as well as by property, so the very first pop-out of
    // a fresh process is already where and how large it should be.
    .chain(layout_args(layout))
    .chain(std::iter::once(format!(
        "--input-ipc-server={}",
        socket.display()
    )))
    .collect()
}

/// Locate mpv on `PATH` without pulling in a dependency for it.
fn find_mpv() -> Result<PathBuf, EngineError> {
    let path = std::env::var_os("PATH").ok_or_else(|| EngineError::EngineMissing {
        display_name: DISPLAY_NAME.into(),
        hint: INSTALL_HINT.into(),
    })?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("mpv"))
        .find(|candidate| {
            std::fs::metadata(candidate)
                .map(|m| m.is_file() || m.file_type().is_symlink())
                .unwrap_or(false)
        })
        .ok_or_else(|| EngineError::EngineMissing {
            display_name: DISPLAY_NAME.into(),
            hint: INSTALL_HINT.into(),
        })
}

/// State we re-apply after a respawn, so a crashed mpv is invisible to the user.
#[derive(Debug, Clone)]
struct Desired {
    video_visible: bool,
    ontop: bool,
    layout: VideoWindowLayout,
    volume: f64,
    speed: f64,
    paused: bool,
    path: Option<String>,
    position: f64,
    has_video: bool,
}

impl Default for Desired {
    fn default() -> Self {
        Self {
            // Audio-first: a set opened from the menu bar plays without a window
            // until the user pops it out.
            video_visible: false,
            ontop: true,
            layout: VideoWindowLayout::default(),
            volume: 100.0,
            speed: 1.0,
            paused: false,
            path: None,
            position: 0.0,
            has_video: false,
        }
    }
}

struct Session {
    /// `None` when we attached to an mpv started by an earlier process — we can
    /// still drive it over IPC, we just cannot `wait` on it.
    child: Mutex<Option<Child>>,
    ipc: Arc<Ipc>,
    socket: PathBuf,
    /// True when we adopted an mpv started by an earlier process. Such a session
    /// already has state, so it must be read rather than overwritten.
    attached: bool,
}

impl Session {
    fn alive(&self) -> bool {
        if !self.ipc.is_connected() {
            return false;
        }
        match self.child.lock().expect("mpv child lock poisoned").as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            // Attached: a live socket is the only liveness signal available.
            None => true,
        }
    }

    fn request(
        &self,
        operation: &str,
        args: Vec<Value>,
        timeout: Duration,
    ) -> Result<Value, EngineError> {
        let reply = self.ipc.request(args, timeout).map_err(|e| match e {
            IpcError::Timeout => EngineError::Timeout {
                operation: operation.to_string(),
                timeout_ms: timeout.as_millis() as u64,
            },
            IpcError::Disconnected(message) => EngineError::Disconnected {
                display_name: DISPLAY_NAME.into(),
                message,
            },
            IpcError::Encode(message) => EngineError::Internal { message },
        })?;
        if !reply.ok() {
            return Err(EngineError::Rejected {
                operation: operation.to_string(),
                message: reply.error,
            });
        }
        Ok(reply.data)
    }

    fn set_property(&self, name: &str, value: Value) -> Result<(), EngineError> {
        self.request(
            &format!("set {name}"),
            vec![json!("set_property"), json!(name), value.clone()],
            DEFAULT_TIMEOUT,
        )?;
        self.ipc.cache_prop(name, value);
        Ok(())
    }

    /// Stop the process and clean up its socket. Idempotent.
    fn terminate(&self) {
        // Ask politely first so mpv can release the audio device cleanly.
        let _ = self
            .ipc
            .request(vec![json!("quit")], Duration::from_millis(500));
        let deadline = Instant::now() + Duration::from_millis(1500);
        match self.child.lock().expect("mpv child lock poisoned").as_mut() {
            Some(child) => loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(25))
                    }
                    // Past the grace period, or we cannot tell: never leave an
                    // orphaned mpv holding the audio device.
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
            },
            // Adopted: there is no child to reap, so wait for mpv to close the
            // socket instead. Without this, `setwave quit` would return while
            // mpv was still audibly playing.
            None => {
                while self.ipc.is_connected() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// Where the engine's IPC socket lives, and what that implies for ownership.
#[derive(Debug, Clone)]
enum SocketMode {
    /// A private socket for this engine alone; the process dies with it.
    Ephemeral,
    /// A well-known socket shared across processes. The CLI runs as a series of
    /// short-lived invocations — `setwave play`, then `setwave pause` a minute
    /// later — so the mpv process, not the CLI, is what holds playback state.
    /// An engine in this mode attaches to a running mpv if one is listening and
    /// leaves it running when dropped.
    Shared(PathBuf),
}

pub struct MpvEngine {
    exe: PathBuf,
    mode: SocketMode,
    /// Whether dropping the engine should stop mpv. False for shared sockets,
    /// where playback is meant to outlive the process that started it.
    kill_on_drop: bool,
    session: Mutex<Option<Arc<Session>>>,
    desired: Mutex<Desired>,
    /// False once `shutdown` runs, so a stray call cannot resurrect the process.
    live: Mutex<bool>,
}

impl MpvEngine {
    /// Locate mpv but do not start it. Returns [`EngineError::EngineMissing`]
    /// with an actionable hint, which the app turns into its onboarding sheet.
    pub fn new() -> Result<Self, EngineError> {
        Ok(Self {
            exe: find_mpv()?,
            mode: SocketMode::Ephemeral,
            kill_on_drop: true,
            session: Mutex::new(None),
            desired: Mutex::new(Desired::default()),
            live: Mutex::new(true),
        })
    }

    /// An engine bound to a well-known socket, shared between processes.
    ///
    /// If an mpv is already listening there it is adopted, inheriting whatever
    /// is playing; otherwise one is started detached from the calling terminal.
    /// Dropping this engine leaves mpv running — only [`PlaybackEngine::shutdown`]
    /// stops it. This is what lets successive CLI invocations act as a remote
    /// control over one continuous playback session, with no daemon.
    pub fn shared(socket: impl Into<PathBuf>) -> Result<Self, EngineError> {
        Ok(Self {
            exe: find_mpv()?,
            mode: SocketMode::Shared(socket.into()),
            kill_on_drop: false,
            session: Mutex::new(None),
            desired: Mutex::new(Desired::default()),
            live: Mutex::new(true),
        })
    }

    /// Whether a shared engine would adopt a running mpv rather than start one.
    pub fn is_running_at(socket: impl AsRef<Path>) -> bool {
        std::os::unix::net::UnixStream::connect(socket.as_ref()).is_ok()
    }

    /// Whether mpv is installed, for a pre-flight check that avoids constructing
    /// an engine just to discover it is absent.
    pub fn is_available() -> bool {
        find_mpv().is_ok()
    }

    fn unique_socket() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("setwave-mpv-{}-{}.sock", std::process::id(), n))
    }

    /// Get a live session, starting or restarting mpv if needed.
    ///
    /// A respawn re-applies volume, speed, video visibility and reloads the last
    /// file at its last position, so an mpv crash costs the user a gap in audio
    /// rather than their place in a two-hour set.
    fn session(&self) -> Result<Arc<Session>, EngineError> {
        if !*self.live.lock().expect("mpv live lock poisoned") {
            return Err(EngineError::Internal {
                message: "engine has been shut down".into(),
            });
        }

        let mut died_because: Option<String> = None;
        let mut slot = self.session.lock().expect("mpv session lock poisoned");
        if let Some(existing) = slot.as_ref() {
            if existing.alive() {
                return Ok(Arc::clone(existing));
            }
            // Dead: reap it before replacing, keeping the reason so that if the
            // respawn also fails the user hears why the first one died rather
            // than a bare "could not start mpv".
            died_because = existing.ipc.disconnect_reason();
            existing.terminate();
            slot.take();
        }

        let session = Arc::new(self.spawn().map_err(|e| match (e, died_because) {
            (
                EngineError::Spawn {
                    display_name,
                    message,
                },
                Some(reason),
            ) => EngineError::Spawn {
                display_name,
                message: format!("{message} (previous instance ended: {reason})"),
            },
            (e, _) => e,
        })?);

        if session.attached {
            // Adopted a running mpv: read its state instead of stamping our
            // defaults over whatever the user is already listening to.
            self.sync_from(&session);
            *slot = Some(Arc::clone(&session));
            return Ok(session);
        }

        let desired = self
            .desired
            .lock()
            .expect("mpv desired lock poisoned")
            .clone();
        self.apply_desired(&session, &desired)?;
        *slot = Some(Arc::clone(&session));
        drop(slot);

        // Restore what was playing. Done outside the session lock because
        // `load_into` takes it again via the normal path.
        if let Some(path) = desired.path.clone() {
            let resume_at = (desired.position > 0.5).then_some(desired.position);
            self.load_into(&session, path, resume_at)?;
            if desired.paused {
                session.set_property("pause", json!(true))?;
            }
        }
        Ok(session)
    }

    fn spawn(&self) -> Result<Session, EngineError> {
        let socket = match &self.mode {
            SocketMode::Ephemeral => Self::unique_socket(),
            SocketMode::Shared(path) => path.clone(),
        };

        // Adopt a running instance before starting a new one.
        if matches!(self.mode, SocketMode::Shared(_)) {
            if let Ok(stream) = std::os::unix::net::UnixStream::connect(&socket) {
                let ipc = Ipc::connect(stream).map_err(|e| EngineError::Spawn {
                    display_name: DISPLAY_NAME.into(),
                    message: format!("could not start IPC reader: {e}"),
                })?;
                let session = Session {
                    child: Mutex::new(None),
                    ipc,
                    socket,
                    attached: true,
                };
                self.observe_properties(&session)?;
                return Ok(session);
            }
            // Nothing listening: a leftover socket file from a dead mpv would
            // stop the new one from binding.
            let _ = std::fs::remove_file(&socket);
            if let Some(parent) = socket.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
        } else {
            let _ = std::fs::remove_file(&socket);
        }

        let layout = self
            .desired
            .lock()
            .expect("mpv desired lock poisoned")
            .layout;
        let mut command = Command::new(&self.exe);
        command
            .args(spawn_args(&socket, layout))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if matches!(self.mode, SocketMode::Shared(_)) {
            // Detach from the caller's process group so closing the terminal
            // that ran `setwave play` does not take playback down with it.
            command.process_group(0);
        }
        let child = command.spawn().map_err(|e| match e.kind() {
            ErrorKind::NotFound => EngineError::EngineMissing {
                display_name: DISPLAY_NAME.into(),
                hint: INSTALL_HINT.into(),
            },
            _ => EngineError::Spawn {
                display_name: DISPLAY_NAME.into(),
                message: e.to_string(),
            },
        })?;
        let mut child = child;

        // mpv creates the socket a moment after exec. Poll rather than sleeping
        // a fixed pessimistic amount, and give up if the process dies meanwhile.
        let deadline = Instant::now() + SOCKET_WAIT;
        let stream = loop {
            if let Ok(Some(status)) = child.try_wait() {
                let _ = std::fs::remove_file(&socket);
                return Err(EngineError::Spawn {
                    display_name: DISPLAY_NAME.into(),
                    message: format!("mpv exited immediately with {status}"),
                });
            }
            match std::os::unix::net::UnixStream::connect(&socket) {
                Ok(s) => break s,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = std::fs::remove_file(&socket);
                    return Err(EngineError::Spawn {
                        display_name: DISPLAY_NAME.into(),
                        message: format!(
                            "mpv never accepted a connection on {}: {e}",
                            socket.display()
                        ),
                    });
                }
            }
        };

        let ipc = Ipc::connect(stream).map_err(|e| EngineError::Spawn {
            display_name: DISPLAY_NAME.into(),
            message: format!("could not start IPC reader: {e}"),
        })?;

        let session = Session {
            child: Mutex::new(Some(child)),
            ipc,
            socket,
            attached: false,
        };
        self.observe_properties(&session)?;
        Ok(session)
    }

    fn observe_properties(&self, session: &Session) -> Result<(), EngineError> {
        for (i, prop) in OBSERVED.iter().enumerate() {
            session.request(
                "observe_property",
                vec![json!("observe_property"), json!(i as u64 + 1), json!(prop)],
                DEFAULT_TIMEOUT,
            )?;
        }
        Ok(())
    }

    /// Read live state out of an adopted mpv into `desired`, so snapshots and a
    /// later respawn reflect what is actually playing.
    fn sync_from(&self, session: &Session) {
        let get = |name: &str| {
            session
                .request(
                    "get_property",
                    vec![json!("get_property"), json!(name)],
                    DEFAULT_TIMEOUT,
                )
                .ok()
        };

        let path = get("path").and_then(|v| v.as_str().map(str::to_owned));
        let has_video = get("track-list")
            .and_then(|tracks| {
                tracks.as_array().map(|list| {
                    list.iter().any(|t| {
                        t.get("type").and_then(Value::as_str) == Some("video")
                            && t.get("albumart").and_then(Value::as_bool) != Some(true)
                    })
                })
            })
            .unwrap_or(false);

        let mut desired = self.desired.lock().expect("mpv desired lock poisoned");
        desired.position = get("time-pos").and_then(|v| v.as_f64()).unwrap_or(0.0);
        desired.paused = get("pause").and_then(|v| v.as_bool()).unwrap_or(false);
        desired.volume = get("volume")
            .and_then(|v| v.as_f64())
            .unwrap_or(desired.volume);
        desired.speed = get("speed")
            .and_then(|v| v.as_f64())
            .unwrap_or(desired.speed);
        desired.ontop = get("ontop")
            .and_then(|v| v.as_bool())
            .unwrap_or(desired.ontop);
        // `current-vo` is null exactly when no video output exists.
        desired.video_visible = get("current-vo").map(|v| !v.is_null()).unwrap_or(false);
        desired.has_video = has_video;
        desired.path = path;
    }

    /// Push a layout to a live session. `geometry` is read when a window is
    /// created, so it lands on the next pop-out; `fullscreen` is live, so a
    /// showing window follows it immediately.
    fn apply_layout(session: &Session, layout: VideoWindowLayout) -> Result<(), EngineError> {
        let options = window_options(layout);
        session.set_property("screen", json!(options.screen))?;
        session.set_property("geometry", json!(options.geometry))?;
        session.set_property("autofit", json!(options.autofit))?;
        session.set_property("fullscreen", json!(options.fullscreen))
    }

    fn apply_desired(&self, session: &Session, desired: &Desired) -> Result<(), EngineError> {
        session.set_property("volume", json!(desired.volume))?;
        session.set_property("speed", json!(desired.speed))?;
        session.set_property("ontop", json!(desired.ontop))?;
        Self::apply_layout(session, desired.layout)?;
        session.set_property("vid", video_selection(desired.video_visible))?;
        Ok(())
    }

    /// The actual load, given a session. Split out so respawn-and-restore can
    /// reuse it without recursing through `session()`.
    fn load_into(
        &self,
        session: &Session,
        path: String,
        start_at: Option<f64>,
    ) -> Result<(), EngineError> {
        // Select video *before* loading. Setting it afterwards would briefly
        // create and destroy a window on every audio-only track.
        let video_visible = self
            .desired
            .lock()
            .expect("mpv desired lock poisoned")
            .video_visible;
        session.set_property("vid", video_selection(video_visible))?;

        // `start` is a per-file option; set it, load, then clear it so it cannot
        // leak into the next track.
        match start_at {
            Some(secs) if secs > 0.0 => {
                session.set_property("start", json!(format!("{secs:.3}")))?
            }
            _ => session.set_property("start", json!("none"))?,
        }

        let generation = session.ipc.file_loaded_generation();
        let queued = session.request(
            "loadfile",
            vec![json!("loadfile"), json!(path.clone()), json!("replace")],
            DEFAULT_TIMEOUT,
        );
        if queued.is_err() {
            let _ = session.set_property("start", json!("none"));
            queued?;
        }

        // `loadfile` returns as soon as the load is *queued*; mpv reads `start`
        // later, when the file actually opens. Clearing it before then loses the
        // resume offset — and does so only for files slow enough to load, which
        // is to say exactly the two-hour sets this feature exists for.
        let loaded = session.ipc.wait_file_loaded(generation, LOAD_TIMEOUT);
        let _ = session.set_property("start", json!("none"));
        if !loaded {
            return Err(EngineError::Timeout {
                operation: format!("load {path}"),
                timeout_ms: LOAD_TIMEOUT.as_millis() as u64,
            });
        }

        // Ask the track list whether this file has video at all. `vid` alone
        // cannot answer it: when video is switched off, `vid` is `false` for an
        // audio-only file and a video file alike.
        let has_video = session
            .request(
                "get_property track-list",
                vec![json!("get_property"), json!("track-list")],
                DEFAULT_TIMEOUT,
            )
            .map(|tracks| {
                tracks
                    .as_array()
                    .map(|list| {
                        list.iter().any(|t| {
                            t.get("type").and_then(Value::as_str) == Some("video")
                                // Cover art in an mp3 is a video track to mpv,
                                // but it is not something to pop out.
                                && t.get("albumart").and_then(Value::as_bool) != Some(true)
                        })
                    })
                    .unwrap_or(false)
            })
            .unwrap_or(false);

        let mut desired = self.desired.lock().expect("mpv desired lock poisoned");
        desired.path = Some(path);
        desired.position = start_at.unwrap_or(0.0);
        desired.has_video = has_video;
        desired.paused = false;
        Ok(())
    }

    /// Remember the live position so a respawn can restore it.
    fn record_position(&self, session: &Session) {
        if let Some(pos) = session.ipc.prop("time-pos").as_f64() {
            self.desired
                .lock()
                .expect("mpv desired lock poisoned")
                .position = pos;
        }
    }
}

/// mpv's `vid` takes a track id or `no`; `auto` restores default selection.
fn video_selection(visible: bool) -> Value {
    json!(if visible { "auto" } else { "no" })
}

impl PlaybackEngine for MpvEngine {
    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities {
            id: "mpv".into(),
            display_name: DISPLAY_NAME.into(),
            containers: CONTAINERS.iter().map(|c| c.to_string()).collect(),
            video: true,
            ontop_window: true,
            // mpv owns its own window; system PiP is an AVFoundation affordance.
            native_pip: false,
        }
    }

    fn load(&self, path: String, start_at: Option<f64>) -> Result<(), EngineError> {
        let session = self.session()?;
        self.load_into(&session, path, start_at)
    }

    fn set_paused(&self, paused: bool) -> Result<(), EngineError> {
        let session = self.session()?;
        session.set_property("pause", json!(paused))?;
        self.record_position(&session);
        self.desired
            .lock()
            .expect("mpv desired lock poisoned")
            .paused = paused;
        Ok(())
    }

    fn seek_absolute(&self, seconds: f64) -> Result<(), EngineError> {
        let target = seconds.max(0.0);
        let session = self.session()?;
        session.request(
            "seek",
            vec![json!("seek"), json!(target), json!("absolute")],
            DEFAULT_TIMEOUT,
        )?;

        // mpv acknowledges the seek before it emits the matching `time-pos`
        // update, so without this a snapshot taken immediately afterwards still
        // reports the *old* position. A scrubber reading that would spring back
        // to where the drag started before jumping forward a tick later.
        // The real value overwrites this as soon as it arrives.
        session.ipc.cache_prop("time-pos", json!(target));
        // Seeking away from the end of a file means it is no longer finished;
        // a stale `eof-reached` here would make the queue advance unbidden.
        session.ipc.cache_prop("eof-reached", json!(false));

        self.desired
            .lock()
            .expect("mpv desired lock poisoned")
            .position = target;
        Ok(())
    }

    fn set_volume(&self, percent: f64) -> Result<(), EngineError> {
        let clamped = percent.clamp(0.0, 100.0);
        let session = self.session()?;
        session.set_property("volume", json!(clamped))?;
        self.desired
            .lock()
            .expect("mpv desired lock poisoned")
            .volume = clamped;
        Ok(())
    }

    fn set_speed(&self, rate: f64) -> Result<(), EngineError> {
        let clamped = rate.clamp(0.01, 100.0);
        let session = self.session()?;
        session.set_property("speed", json!(clamped))?;
        self.desired
            .lock()
            .expect("mpv desired lock poisoned")
            .speed = clamped;
        Ok(())
    }

    fn set_video_visible(&self, visible: bool) -> Result<(), EngineError> {
        // The pop-out. Verified in M0 to leave `audio-pts` perfectly continuous:
        // mpv emits `video-reconfig` but no `playback-restart`, so audio is never
        // interrupted and position is preserved without any seek on our part.
        let session = self.session()?;
        session.set_property("vid", video_selection(visible))?;
        self.desired
            .lock()
            .expect("mpv desired lock poisoned")
            .video_visible = visible;
        Ok(())
    }

    fn set_video_ontop(&self, ontop: bool) -> Result<(), EngineError> {
        let session = self.session()?;
        session.set_property("ontop", json!(ontop))?;
        self.desired
            .lock()
            .expect("mpv desired lock poisoned")
            .ontop = ontop;
        Ok(())
    }

    fn set_video_window_layout(&self, layout: VideoWindowLayout) -> Result<(), EngineError> {
        self.desired
            .lock()
            .expect("mpv desired lock poisoned")
            .layout = layout;
        // A setting change must not start mpv. If a session is up, the layout
        // is set on it — geometry for the next window creation, fullscreen at
        // once; if not, the spawn arguments carry it when one starts.
        let live = {
            let slot = self.session.lock().expect("mpv session lock poisoned");
            slot.as_ref().map(Arc::clone)
        };
        if let Some(session) = live {
            if session.alive() {
                Self::apply_layout(&session, layout)?;
            }
        }
        Ok(())
    }

    fn begin_window_placement(&self) -> Result<Option<u32>, EngineError> {
        // The one place a setting is allowed to start mpv: there is no window
        // to place without a process to own it.
        let session = self.session()?;
        let layout = self
            .desired
            .lock()
            .expect("mpv desired lock poisoned")
            .layout;
        // Windowed even for a fullscreen layout — a fullscreen window cannot
        // be dragged, and its geometry is what placement is for.
        let options = window_options(layout);
        session.set_property("screen", json!(options.screen))?;
        session.set_property("geometry", json!(options.geometry))?;
        session.set_property("autofit", json!(options.autofit))?;
        session.set_property("fullscreen", json!(false))?;
        // `force-window` opens the window with nothing to draw in it.
        session.set_property("force-window", json!(true))?;

        // mpv reports its own pid; an adopted session has no child to ask.
        let pid = session
            .request(
                "get pid",
                vec![json!("get_property"), json!("pid")],
                DEFAULT_TIMEOUT,
            )
            .ok()
            .and_then(|v| v.as_u64())
            .map(|p| p as u32)
            .or_else(|| {
                session
                    .child
                    .lock()
                    .expect("mpv child lock poisoned")
                    .as_ref()
                    .map(Child::id)
            });
        Ok(pid)
    }

    fn end_window_placement(&self) -> Result<(), EngineError> {
        let live = {
            let slot = self.session.lock().expect("mpv session lock poisoned");
            slot.as_ref().map(Arc::clone)
        };
        let Some(session) = live else { return Ok(()) };
        if !session.alive() {
            return Ok(());
        }
        session.set_property("force-window", json!(false))?;
        // Back to the layout as set, fullscreen included.
        let layout = self
            .desired
            .lock()
            .expect("mpv desired lock poisoned")
            .layout;
        Self::apply_layout(&session, layout)
    }

    fn stop(&self) -> Result<(), EngineError> {
        let session = self.session()?;
        session.request("stop", vec![json!("stop")], DEFAULT_TIMEOUT)?;
        let mut desired = self.desired.lock().expect("mpv desired lock poisoned");
        desired.path = None;
        desired.position = 0.0;
        desired.has_video = false;
        Ok(())
    }

    fn snapshot(&self) -> EngineSnapshot {
        let mut session = {
            let slot = self.session.lock().expect("mpv session lock poisoned");
            slot.as_ref().map(Arc::clone)
        };

        // Polling must never *start* mpv — an idle app stays idle. Adopting one
        // that is already running is a different matter: without this, a fresh
        // `setwave status` would report "nothing playing" while a set is audibly
        // playing from an earlier invocation.
        if session.is_none() {
            if let SocketMode::Shared(path) = &self.mode {
                let live = *self.live.lock().expect("mpv live lock poisoned");
                if live && Self::is_running_at(path) {
                    session = self.session().ok();
                }
            }
        }

        let desired = self
            .desired
            .lock()
            .expect("mpv desired lock poisoned")
            .clone();

        // Never start mpv just to answer a poll — an idle app must stay idle.
        let Some(session) = session else {
            return EngineSnapshot {
                paused: desired.paused,
                idle: true,
                path: desired.path.clone(),
                has_video: desired.has_video,
                ..EngineSnapshot::default()
            };
        };

        let ipc = &session.ipc;
        let loaded = desired.path.is_some();
        EngineSnapshot {
            position_secs: ipc.prop("time-pos").as_f64().or({
                // Before the first frame, the last known position is a better
                // answer than nothing — it keeps the scrubber from jumping to 0.
                loaded.then_some(desired.position)
            }),
            duration_secs: ipc.prop("duration").as_f64(),
            paused: ipc.prop("pause").as_bool().unwrap_or(desired.paused),
            idle: ipc.prop("idle-active").as_bool().unwrap_or(!loaded),
            eof: ipc.prop("eof-reached").as_bool().unwrap_or(false),
            has_video: desired.has_video,
            video_visible: !ipc.prop("current-vo").is_null(),
            path: desired.path.clone(),
        }
    }

    fn shutdown(&self) {
        {
            let mut live = self.live.lock().expect("mpv live lock poisoned");
            if !*live {
                return;
            }
            *live = false;
        }
        if let Some(session) = self
            .session
            .lock()
            .expect("mpv session lock poisoned")
            .take()
        {
            session.terminate();
        }
    }
}

impl Drop for MpvEngine {
    fn drop(&mut self) {
        if self.kill_on_drop {
            self.shutdown();
        }
        // A shared engine deliberately leaves mpv playing: the next CLI
        // invocation adopts it. `shutdown` is the only thing that stops it.
    }
}
