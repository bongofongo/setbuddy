//! Swift bindings for Setbuddy.
//!
//! All UniFFI concerns live here and nowhere else. `setbuddy-core` and
//! `setbuddy-engine` stay free of bindings machinery, so the FFI surface can be
//! reshaped without touching the domain — and, more to the point, so the
//! engine contract stays a plain Rust trait that any backend can satisfy.
//!
//! ## The v2 escape hatch
//!
//! [`PlaybackEngine`] here mirrors [`setbuddy_engine::PlaybackEngine`] but is
//! exported `with_foreign`, meaning **Swift can implement it**. A Swift
//! AVFoundation engine passed to [`Setbuddy::with_engines`] is wrapped by
//! `ForeignEngine` and registered ahead of mpv, at which point engine selection
//! routes `.mp3` and `.mp4` to it and `.webm` to mpv, with no change anywhere
//! else. That direction is exercised now, in `tests/swift`, rather than being
//! discovered to be impossible in v2.

uniffi::setup_scaffolding!("setbuddy");

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use setbuddy_core::paths;
use setbuddy_core::player::Player;
use setbuddy_core::queue::RepeatMode as CoreRepeatMode;
use setbuddy_core::selection::{EnginePolicy, EngineRegistry};
use setbuddy_core::store::Store;
use setbuddy_core::track::Track as CoreTrack;
use setbuddy_engine::SharedEngine;
use setbuddy_mpv::MpvEngine;

// ---------------------------------------------------------------------------
// Types crossing the boundary
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, uniffi::Record)]
pub struct Track {
    pub id: i64,
    pub path: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_secs: Option<f64>,
    pub has_video: bool,
    pub last_played_at: Option<i64>,
    pub size_bytes: i64,
    /// Unix seconds. The file's modification time and when it was indexed.
    pub mtime: i64,
    pub added_at: i64,
    /// Pre-rendered "Artist — Title", falling back to the filename. Computed
    /// here so every surface displays a track the same way.
    pub display_label: String,
    /// Saved position, when there is one worth resuming from.
    pub resume_secs: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum RepeatMode {
    Off,
    All,
    One,
}

impl From<CoreRepeatMode> for RepeatMode {
    fn from(mode: CoreRepeatMode) -> Self {
        match mode {
            CoreRepeatMode::Off => RepeatMode::Off,
            CoreRepeatMode::All => RepeatMode::All,
            CoreRepeatMode::One => RepeatMode::One,
        }
    }
}

impl From<RepeatMode> for CoreRepeatMode {
    fn from(mode: RepeatMode) -> Self {
        match mode {
            RepeatMode::Off => CoreRepeatMode::Off,
            RepeatMode::All => CoreRepeatMode::All,
            RepeatMode::One => CoreRepeatMode::One,
        }
    }
}

/// Everything the menu bar needs to draw itself, in one read.
#[derive(Debug, Clone, uniffi::Record)]
pub struct PlayerSnapshot {
    pub track: Option<Track>,
    pub position_secs: Option<f64>,
    pub duration_secs: Option<f64>,
    pub paused: bool,
    pub idle: bool,
    pub has_video: bool,
    pub video_visible: bool,
    pub queue_len: u32,
    pub queue_index: Option<u32>,
    pub repeat: RepeatMode,
    pub shuffle: bool,
    /// Queue positions playback is confined to; empty when it is not.
    pub loop_positions: Vec<u32>,
    pub engine_id: Option<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct EngineCapabilities {
    pub id: String,
    pub display_name: String,
    pub containers: Vec<String>,
    pub video: bool,
    pub ontop_window: bool,
    pub native_pip: bool,
}

#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct EngineSnapshot {
    pub position_secs: Option<f64>,
    pub duration_secs: Option<f64>,
    pub paused: bool,
    pub idle: bool,
    pub eof: bool,
    pub has_video: bool,
    pub video_visible: bool,
    pub path: Option<String>,
}

/// One line of a track's full metadata, as `ffprobe` reports it.
#[derive(Debug, Clone, uniffi::Record)]
pub struct MetadataEntry {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ScanReport {
    pub seen: u32,
    pub added: u32,
    pub updated: u32,
    pub unchanged: u32,
    pub removed: u32,
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum SetbuddyError {
    /// The backend is not installed. `hint` is user-facing copy for the
    /// onboarding sheet — for mpv, "Install it with `brew install mpv`".
    #[error("{display_name} is not installed. {hint}")]
    EngineMissing { display_name: String, hint: String },

    #[error("nothing is playing")]
    NothingPlaying,

    #[error("{path} is not a media file Setbuddy recognises")]
    UnsupportedFile { path: String },

    #[error("nothing matches \"{query}\"")]
    NoMatch { query: String },

    #[error("{message}")]
    Playback { message: String },

    #[error("{message}")]
    Storage { message: String },
}

impl From<setbuddy_engine::EngineError> for SetbuddyError {
    fn from(error: setbuddy_engine::EngineError) -> Self {
        use setbuddy_engine::EngineError as E;
        match error {
            E::EngineMissing { display_name, hint } => {
                SetbuddyError::EngineMissing { display_name, hint }
            }
            E::Unsupported { path } => SetbuddyError::UnsupportedFile { path },
            other => SetbuddyError::Playback {
                message: other.to_string(),
            },
        }
    }
}

impl From<setbuddy_core::CoreError> for SetbuddyError {
    fn from(error: setbuddy_core::CoreError) -> Self {
        use setbuddy_core::CoreError as E;
        match error {
            E::Engine(engine) => engine.into(),
            E::NothingPlaying => SetbuddyError::NothingPlaying,
            E::NotMedia { path } => SetbuddyError::UnsupportedFile { path },
            E::NoMatch { query } => SetbuddyError::NoMatch { query },
            E::Storage { message } => SetbuddyError::Storage { message },
            other => SetbuddyError::Playback {
                message: other.to_string(),
            },
        }
    }
}

type Result<T> = std::result::Result<T, SetbuddyError>;

// ---------------------------------------------------------------------------
// Foreign engines
// ---------------------------------------------------------------------------

/// The engine contract, implementable from Swift.
///
/// Mirrors [`setbuddy_engine::PlaybackEngine`]. An implementation written in
/// Swift — AVFoundation in v2 — is adapted back to that trait by
/// `ForeignEngine`, so core cannot tell the difference between it and mpv.
#[uniffi::export(with_foreign)]
pub trait PlaybackEngine: Send + Sync {
    fn capabilities(&self) -> EngineCapabilities;
    fn load(&self, path: String, start_at: Option<f64>) -> Result<()>;
    fn set_paused(&self, paused: bool) -> Result<()>;
    fn seek_absolute(&self, seconds: f64) -> Result<()>;
    fn set_volume(&self, percent: f64) -> Result<()>;
    fn set_speed(&self, rate: f64) -> Result<()>;
    /// Show or hide the video surface without interrupting audio.
    fn set_video_visible(&self, visible: bool) -> Result<()>;
    fn set_video_ontop(&self, ontop: bool) -> Result<()>;
    /// Where and how large the video window is when it next appears, in the
    /// settings form: `"40%"`, `"fill"`, `"fullscreen"`, `"1280"`,
    /// `"1280+100+50"`, `"1280+100+50/1"`.
    ///
    /// A string rather than the Rust enum, because that same string is what
    /// crosses the boundary in `Setbuddy::set_video_window_layout` and what the
    /// store holds; one grammar, parsed at each edge, beats two.
    fn set_video_window_layout(&self, spec: String) -> Result<()>;
    fn stop(&self) -> Result<()>;
    fn snapshot(&self) -> EngineSnapshot;
    fn shutdown(&self);
}

/// Adapts a foreign (Swift) engine to the plain Rust engine contract.
struct ForeignEngine(Arc<dyn PlaybackEngine>);

/// Foreign errors carry no structure back across the boundary beyond their
/// variant, so preserve the one the UI acts on and flatten the rest.
fn to_engine_error(error: SetbuddyError) -> setbuddy_engine::EngineError {
    match error {
        SetbuddyError::EngineMissing { display_name, hint } => {
            setbuddy_engine::EngineError::EngineMissing { display_name, hint }
        }
        SetbuddyError::UnsupportedFile { path } => {
            setbuddy_engine::EngineError::Unsupported { path }
        }
        other => setbuddy_engine::EngineError::Internal {
            message: other.to_string(),
        },
    }
}

impl setbuddy_engine::PlaybackEngine for ForeignEngine {
    fn capabilities(&self) -> setbuddy_engine::EngineCapabilities {
        let caps = self.0.capabilities();
        setbuddy_engine::EngineCapabilities {
            id: caps.id,
            display_name: caps.display_name,
            containers: caps.containers,
            video: caps.video,
            ontop_window: caps.ontop_window,
            native_pip: caps.native_pip,
        }
    }

    fn load(
        &self,
        path: String,
        start_at: Option<f64>,
    ) -> std::result::Result<(), setbuddy_engine::EngineError> {
        self.0.load(path, start_at).map_err(to_engine_error)
    }

    fn set_paused(&self, paused: bool) -> std::result::Result<(), setbuddy_engine::EngineError> {
        self.0.set_paused(paused).map_err(to_engine_error)
    }

    fn seek_absolute(&self, seconds: f64) -> std::result::Result<(), setbuddy_engine::EngineError> {
        self.0.seek_absolute(seconds).map_err(to_engine_error)
    }

    fn set_volume(&self, percent: f64) -> std::result::Result<(), setbuddy_engine::EngineError> {
        self.0.set_volume(percent).map_err(to_engine_error)
    }

    fn set_speed(&self, rate: f64) -> std::result::Result<(), setbuddy_engine::EngineError> {
        self.0.set_speed(rate).map_err(to_engine_error)
    }

    fn set_video_visible(
        &self,
        visible: bool,
    ) -> std::result::Result<(), setbuddy_engine::EngineError> {
        self.0.set_video_visible(visible).map_err(to_engine_error)
    }

    fn set_video_ontop(
        &self,
        ontop: bool,
    ) -> std::result::Result<(), setbuddy_engine::EngineError> {
        self.0.set_video_ontop(ontop).map_err(to_engine_error)
    }

    fn set_video_window_layout(
        &self,
        layout: setbuddy_engine::VideoWindowLayout,
    ) -> std::result::Result<(), setbuddy_engine::EngineError> {
        self.0
            .set_video_window_layout(layout.as_str())
            .map_err(to_engine_error)
    }

    // `begin_window_placement` is deliberately left at its default `None`: the
    // host reads a placed window's bounds back *by owning pid*, and a foreign
    // engine's window belongs to the app's own process, where the placement
    // overlay's full-screen dimmers would be picked up instead. Placement runs
    // on an engine with its own process; the layout it produces is applied to
    // every engine, so the foreign one still lands where the user put it.

    fn stop(&self) -> std::result::Result<(), setbuddy_engine::EngineError> {
        self.0.stop().map_err(to_engine_error)
    }

    fn snapshot(&self) -> setbuddy_engine::EngineSnapshot {
        let snap = self.0.snapshot();
        setbuddy_engine::EngineSnapshot {
            position_secs: snap.position_secs,
            duration_secs: snap.duration_secs,
            paused: snap.paused,
            idle: snap.idle,
            eof: snap.eof,
            has_video: snap.has_video,
            video_visible: snap.video_visible,
            path: snap.path,
        }
    }

    fn shutdown(&self) {
        self.0.shutdown()
    }
}

/// Notified as playback advances, so the menu bar does not have to poll.
#[uniffi::export(with_foreign)]
pub trait PlayerObserver: Send + Sync {
    fn on_snapshot(&self, snapshot: PlayerSnapshot);
}

// ---------------------------------------------------------------------------
// The handle the app holds
// ---------------------------------------------------------------------------

struct Ticker {
    running: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

#[derive(uniffi::Object)]
pub struct Setbuddy {
    player: Arc<Player>,
    store: Arc<Store>,
    observers: Arc<Mutex<Vec<Arc<dyn PlayerObserver>>>>,
    ticker: Mutex<Option<Ticker>>,
}

#[uniffi::export]
impl Setbuddy {
    /// Build with mpv as the only engine.
    #[uniffi::constructor]
    pub fn new() -> Result<Arc<Self>> {
        Self::build(Vec::new())
    }

    /// Build with foreign engines registered *ahead of* mpv.
    ///
    /// Registration order is preference order, so a Swift AVFoundation engine
    /// takes the containers it can handle and mpv keeps the rest.
    #[uniffi::constructor]
    pub fn with_engines(engines: Vec<Arc<dyn PlaybackEngine>>) -> Result<Arc<Self>> {
        Self::build(engines)
    }

    /// Registered engine ids, in preference order.
    pub fn engine_ids(&self) -> Vec<String> {
        self.player.engine_ids()
    }

    // ---- playback ----

    pub fn play_path(&self, path: String) -> Result<Track> {
        let track = self.player.play_path(std::path::Path::new(&path))?;
        Ok(self.to_ffi_track(track))
    }

    pub fn play_track(&self, track_id: i64) -> Result<Track> {
        let track = self.player.play_track_id(track_id)?;
        Ok(self.to_ffi_track(track))
    }

    /// Play from the start, discarding any saved position.
    pub fn restart_track(&self, track_id: i64) -> Result<Track> {
        let track = self.player.restart_track_id(track_id)?;
        Ok(self.to_ffi_track(track))
    }

    pub fn set_paused(&self, paused: bool) -> Result<()> {
        Ok(self.player.set_paused(paused)?)
    }

    pub fn toggle_paused(&self) -> Result<bool> {
        Ok(self.player.toggle_paused()?)
    }

    pub fn next(&self) -> Result<Option<Track>> {
        Ok(self.player.next()?.map(|t| self.to_ffi_track(t)))
    }

    pub fn previous(&self) -> Result<Option<Track>> {
        Ok(self.player.previous()?.map(|t| self.to_ffi_track(t)))
    }

    pub fn stop(&self) -> Result<()> {
        Ok(self.player.stop()?)
    }

    pub fn seek_absolute(&self, seconds: f64) -> Result<()> {
        Ok(self.player.seek_absolute(seconds)?)
    }

    pub fn seek_relative(&self, delta_secs: f64) -> Result<f64> {
        Ok(self.player.seek_relative(delta_secs)?)
    }

    pub fn set_volume(&self, percent: f64) -> Result<()> {
        Ok(self.player.set_volume(percent)?)
    }

    pub fn set_speed(&self, rate: f64) -> Result<()> {
        Ok(self.player.set_speed(rate)?)
    }

    // ---- the pop-out ----

    pub fn set_video_visible(&self, visible: bool) -> Result<()> {
        Ok(self.player.set_video_visible(visible)?)
    }

    /// Toggle the floating video window. Errors when the track has no video.
    pub fn toggle_video(&self) -> Result<bool> {
        Ok(self.player.toggle_video()?)
    }

    pub fn set_video_ontop(&self, ontop: bool) -> Result<()> {
        Ok(self.player.set_video_ontop(ontop)?)
    }

    /// Where and how large the pop-out is when it next appears: `"40%"` of
    /// the screen's width, `"fill"` (edge to edge but windowed),
    /// `"fullscreen"`, a pixel width like `"1280"`, or a width, top-left
    /// corner and screen like `"1280+100+50/0"`. Persisted.
    pub fn set_video_window_layout(&self, spec: String) -> Result<()> {
        let layout = setbuddy_engine::VideoWindowLayout::parse(&spec).ok_or_else(|| {
            SetbuddyError::Playback {
                message: format!(
                    "\"{spec}\" is not a window layout; use a percentage like 40%, \"fill\", \
                     \"fullscreen\", a width like 1280, or width+x+y/screen like 1280+100+50/0"
                ),
            }
        })?;
        Ok(self.player.set_video_window_layout(layout)?)
    }

    /// Show an empty video window for the user to drag and resize. Returns the
    /// id of the process owning it, for reading its bounds back, or `None`
    /// when no engine can show one. Pair with `end_window_placement`.
    pub fn begin_window_placement(&self) -> Result<Option<u32>> {
        Ok(self.player.begin_window_placement()?)
    }

    pub fn end_window_placement(&self) -> Result<()> {
        Ok(self.player.end_window_placement()?)
    }

    /// Where the picture goes when video is switched on: `"window"`, the
    /// engine's own floating window, or `"panel"`, a surface the app draws
    /// inside its own UI. Defaults to `"window"`.
    ///
    /// Whether `"panel"` can be honoured is not core's to answer — it depends
    /// on the engine playing the file being one the app holds in its own
    /// process. The app asks its engine; this only remembers the preference.
    pub fn video_surface(&self) -> Result<String> {
        Ok(self
            .player
            .video_surface()?
            .unwrap_or_else(|| "window".into()))
    }

    pub fn set_video_surface(&self, surface: String) -> Result<()> {
        let surface = surface.trim().to_ascii_lowercase();
        if surface != "window" && surface != "panel" {
            return Err(SetbuddyError::Playback {
                message: format!(
                    "\"{surface}\" is not a video surface; use \"window\" or \"panel\""
                ),
            });
        }
        Ok(self.player.set_video_surface(&surface)?)
    }

    /// The saved pop-out layout in the same form, or the engines' default.
    pub fn video_window_layout(&self) -> Result<String> {
        Ok(self
            .player
            .video_window_layout()?
            .unwrap_or_default()
            .as_str())
    }

    // ---- queue ----

    pub fn queue_tracks(&self) -> Result<Vec<Track>> {
        Ok(self
            .player
            .queue_tracks()?
            .into_iter()
            .map(|t| self.to_ffi_track(t))
            .collect())
    }

    pub fn queue_add_path(&self, path: String) -> Result<Track> {
        let track = self.player.ensure_indexed(std::path::Path::new(&path))?;
        self.player.queue_add([track.id])?;
        Ok(self.to_ffi_track(track))
    }

    pub fn queue_add_track(&self, track_id: i64) -> Result<()> {
        Ok(self.player.queue_add([track_id])?)
    }

    /// Stage a file, or every media file under a folder, at the top of the
    /// queue without starting playback.
    ///
    /// Blocking for a folder — it is scanned and probed on the way in — so call
    /// it off the main thread.
    pub fn stage_path(&self, path: String) -> Result<Vec<Track>> {
        Ok(self
            .player
            .stage_path(std::path::Path::new(&path))?
            .into_iter()
            .map(|t| self.to_ffi_track(t))
            .collect())
    }

    /// Move a queued track. Indices are into `queue_tracks`.
    pub fn queue_move(&self, from: u32, to: u32) -> Result<bool> {
        Ok(self.player.queue_move(from as usize, to as usize)?)
    }

    /// Scramble the tracks at `positions` among themselves; the whole queue
    /// when `positions` is empty. A one-shot reorder, not a mode.
    pub fn queue_scramble(&self, positions: Vec<u32>) -> Result<()> {
        let positions: Vec<usize> = positions.into_iter().map(|p| p as usize).collect();
        Ok(self.player.queue_scramble(if positions.is_empty() {
            None
        } else {
            Some(&positions)
        })?)
    }

    /// Loop playback over `positions` until cleared with an empty list.
    /// Replaces any repeat mode.
    pub fn set_loop(&self, positions: Vec<u32>) -> Result<()> {
        let positions: Vec<usize> = positions.into_iter().map(|p| p as usize).collect();
        Ok(self.player.set_loop(if positions.is_empty() {
            None
        } else {
            Some(positions)
        })?)
    }

    pub fn queue_clear(&self) -> Result<()> {
        Ok(self.player.queue_clear()?)
    }

    pub fn set_repeat(&self, mode: RepeatMode) -> Result<()> {
        Ok(self.player.set_repeat(mode.into())?)
    }

    pub fn set_shuffle(&self, on: bool) -> Result<()> {
        Ok(self.player.set_shuffle(on)?)
    }

    // ---- library ----

    pub fn search(&self, query: String, limit: u32) -> Result<Vec<Track>> {
        Ok(self
            .store
            .search(&query, limit as usize)?
            .into_iter()
            .map(|t| self.to_ffi_track(t))
            .collect())
    }

    pub fn recents(&self, limit: u32) -> Result<Vec<Track>> {
        Ok(self
            .store
            .recents(limit as usize)?
            .into_iter()
            .map(|t| self.to_ffi_track(t))
            .collect())
    }

    pub fn all_tracks(&self, limit: u32) -> Result<Vec<Track>> {
        Ok(self
            .store
            .all_tracks(limit as usize)?
            .into_iter()
            .map(|t| self.to_ffi_track(t))
            .collect())
    }

    /// Path to a cached thumbnail for a track, generating it on first request.
    ///
    /// Returns a path rather than bytes: the image is already on disk, and
    /// copying it across the FFI boundary on every menu open would be waste.
    /// Blocking — extracting a frame from a long set takes a moment, so call it
    /// off the main thread.
    pub fn artwork_path(&self, track_id: i64) -> Result<Option<String>> {
        let Some(track) = self.store.track_by_id(track_id)? else {
            return Ok(None);
        };
        Ok(setbuddy_core::artwork::artwork_for(&track)
            .map(|path| path.to_string_lossy().into_owned()))
    }

    /// Everything `ffprobe` knows about a track — container, tags, streams —
    /// beyond the indexed fields. Shells out, so call it off the main thread.
    /// Empty when `ffprobe` is not installed.
    pub fn track_details(&self, track_id: i64) -> Result<Vec<MetadataEntry>> {
        let Some(track) = self.store.track_by_id(track_id)? else {
            return Ok(Vec::new());
        };
        Ok(
            setbuddy_core::probe::details(std::path::Path::new(&track.path))
                .into_iter()
                .map(|(label, value)| MetadataEntry { label, value })
                .collect(),
        )
    }

    pub fn watched_folders(&self) -> Result<Vec<String>> {
        Ok(self.store.folders()?)
    }

    pub fn add_folder(&self, path: String) -> Result<()> {
        Ok(self.store.add_folder(&path)?)
    }

    pub fn remove_folder(&self, path: String) -> Result<bool> {
        Ok(self.store.remove_folder(&path)?)
    }

    /// Rescan every watched folder. Long-running; call off the main thread.
    pub fn scan(&self) -> Result<ScanReport> {
        let report = self.player.rescan_library()?;
        Ok(ScanReport {
            seen: report.seen as u32,
            added: report.added as u32,
            updated: report.updated as u32,
            unchanged: report.unchanged as u32,
            removed: report.removed as u32,
        })
    }

    /// The engine policy in force: `"auto"` or an engine id. Read back so the
    /// settings UI shows what is actually set rather than a guess.
    pub fn engine_policy(&self) -> String {
        self.player.engine_policy().as_str()
    }

    /// What each registered engine can do, in preference order.
    pub fn engines(&self) -> Vec<EngineCapabilities> {
        self.player
            .engine_capabilities()
            .into_iter()
            .map(|caps| EngineCapabilities {
                id: caps.id,
                display_name: caps.display_name,
                containers: caps.containers,
                video: caps.video,
                ontop_window: caps.ontop_window,
                native_pip: caps.native_pip,
            })
            .collect()
    }

    pub fn set_engine_policy(&self, policy: String) -> Result<()> {
        Ok(self
            .player
            .set_engine_policy(EnginePolicy::parse(&policy))?)
    }

    // ---- state ----

    pub fn snapshot(&self) -> Result<PlayerSnapshot> {
        self.build_snapshot()
    }

    /// Periodic upkeep: persist position, learn durations, advance at EOF.
    pub fn tick(&self) -> Result<()> {
        Ok(self.player.tick()?)
    }

    pub fn add_observer(&self, observer: Arc<dyn PlayerObserver>) {
        self.observers
            .lock()
            .expect("observers poisoned")
            .push(observer);
    }

    /// Start a background thread that ticks and notifies observers.
    ///
    /// Replaces any ticker already running, so calling it twice is safe.
    pub fn start_ticker(&self, interval_ms: u64) {
        self.stop_ticker();
        let running = Arc::new(AtomicBool::new(true));
        let flag = running.clone();
        let player = self.player.clone();
        let store = self.store.clone();
        let observers = self.observers.clone();
        let interval = std::time::Duration::from_millis(interval_ms.max(50));

        let handle = std::thread::Builder::new()
            .name("setbuddy-ticker".into())
            .spawn(move || {
                while flag.load(Ordering::Relaxed) {
                    let _ = player.tick();
                    if let Ok(snapshot) = snapshot_of(&player, &store) {
                        let listeners = observers.lock().map(|o| o.clone()).unwrap_or_default();
                        for observer in listeners {
                            observer.on_snapshot(snapshot.clone());
                        }
                    }
                    std::thread::sleep(interval);
                }
            })
            .ok();

        *self.ticker.lock().expect("ticker poisoned") = Some(Ticker { running, handle });
    }

    pub fn stop_ticker(&self) {
        if let Some(ticker) = self.ticker.lock().expect("ticker poisoned").take() {
            ticker.running.store(false, Ordering::Relaxed);
            if let Some(handle) = ticker.handle {
                let _ = handle.join();
            }
        }
    }

    /// Save state and stop the engines. Playback does not survive this.
    pub fn quit(&self) -> Result<()> {
        self.stop_ticker();
        Ok(self.player.quit()?)
    }
}

impl Setbuddy {
    fn build(foreign: Vec<Arc<dyn PlaybackEngine>>) -> Result<Arc<Self>> {
        // Before any engine is looked for and before the ticker thread exists:
        // launched from Finder, this process has only the system directories on
        // PATH and would find none of the tools it shells out to.
        paths::ensure_tool_path();
        paths::ensure_state_dir().map_err(|e| SetbuddyError::Storage {
            message: e.to_string(),
        })?;
        let store = Arc::new(Store::open(&paths::database_path())?);

        // Foreign engines first: registration order is preference order.
        let mut engines: Vec<SharedEngine> = foreign
            .into_iter()
            .map(|e| Arc::new(ForeignEngine(e)) as SharedEngine)
            .collect();
        // mpv is the catch-all, not a requirement. A front end that brought
        // its own engine keeps working on a machine with no mpv installed —
        // it just cannot open the containers only mpv reads, which selection
        // already reports as unsupported. With no engine at all there is
        // nothing to build, so the missing-engine error stands.
        match MpvEngine::shared(paths::engine_socket_path()) {
            Ok(mpv) => engines.push(Arc::new(mpv)),
            Err(missing) if !engines.is_empty() => {
                log_engine_unavailable(&missing);
            }
            Err(missing) => return Err(missing.into()),
        }

        let player = Arc::new(Player::new(store.clone(), EngineRegistry::new(engines))?);
        Ok(Arc::new(Setbuddy {
            player,
            store,
            observers: Arc::new(Mutex::new(Vec::new())),
            ticker: Mutex::new(None),
        }))
    }

    fn to_ffi_track(&self, track: CoreTrack) -> Track {
        track_to_ffi(&self.store, track)
    }

    fn build_snapshot(&self) -> Result<PlayerSnapshot> {
        snapshot_of(&self.player, &self.store)
    }
}

impl Drop for Setbuddy {
    fn drop(&mut self) {
        // Never leave the ticker thread running with a dangling player.
        self.stop_ticker();
    }
}

fn track_to_ffi(store: &Store, track: CoreTrack) -> Track {
    let display_label = track.display_label();
    // A resume the rules would refuse is not shown, so the UI never offers to
    // resume somewhere playback would not actually start.
    let resume_secs = store
        .resume_for(track.id)
        .ok()
        .flatten()
        .and_then(|stored| setbuddy_core::resume::start_at(Some(stored), track.duration_secs));
    Track {
        id: track.id,
        path: track.path,
        title: track.title,
        artist: track.artist,
        album: track.album,
        duration_secs: track.duration_secs,
        has_video: track.has_video,
        last_played_at: track.last_played_at,
        size_bytes: track.size_bytes,
        mtime: track.mtime,
        added_at: track.added_at,
        display_label,
        resume_secs,
    }
}

fn snapshot_of(player: &Player, store: &Store) -> Result<PlayerSnapshot> {
    let status = player.status()?;
    Ok(PlayerSnapshot {
        track: status.track.map(|t| track_to_ffi(store, t)),
        position_secs: status.position_secs,
        duration_secs: status.duration_secs,
        paused: status.paused,
        idle: status.idle,
        has_video: status.has_video,
        video_visible: status.video_visible,
        queue_len: status.queue_len as u32,
        queue_index: status.queue_index.map(|i| i as u32),
        repeat: status.repeat.into(),
        shuffle: status.shuffle,
        loop_positions: status.loop_positions.iter().map(|p| *p as u32).collect(),
        engine_id: status.engine_id,
    })
}

/// An engine that could not be registered. Not an error for the caller: the
/// others carry on without it.
fn log_engine_unavailable(error: &setbuddy_engine::EngineError) {
    eprintln!("setbuddy: playback engine unavailable, continuing without it: {error}");
}

/// Whether mpv is on `PATH`.
///
/// Call before [`Setbuddy::new`] to show the onboarding sheet without
/// constructing anything.
#[uniffi::export]
pub fn mpv_available() -> bool {
    // Reached before the constructor — the onboarding sheet is what it decides —
    // so it fixes PATH itself rather than relying on having been built first.
    paths::ensure_tool_path();
    MpvEngine::is_available()
}

/// Seconds as `H:MM:SS` or `M:SS`, so Swift renders times identically to the CLI.
#[uniffi::export]
pub fn format_duration(seconds: f64) -> String {
    setbuddy_core::format_duration(seconds)
}
