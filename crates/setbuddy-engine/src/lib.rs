//! The playback engine contract.
//!
//! This crate defines *what a player can do* and nothing about *how*. No engine
//! implementation lives here, and no code in this crate may reference mpv,
//! AVFoundation, or any other backend. `setwave-core` depends only on this
//! contract, which is what allows the v1 mpv engine to be replaced — or joined —
//! without touching the library index, queue, resume logic, CLI, or UI.
//!
//! Every type crossing this boundary is deliberately restricted to shapes UniFFI
//! can represent (`String`, `f64`, `bool`, `Option`, `Vec`, plain records, enums
//! with named fields). That is not incidental: `setwave-ffi` mirrors this trait
//! as a UniFFI *foreign trait* and adapts implementations written in Swift back
//! to it, which is how an AVFoundation engine will satisfy this same contract in
//! v2. Keeping the signatures FFI-clean is free; retrofitting them is not.

use std::sync::Arc;

/// What an engine can play and what window behaviour it offers.
///
/// Used by engine selection in `setwave-core` — the policy asks capabilities,
/// never the engine's identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineCapabilities {
    /// Stable machine identifier, e.g. `"mpv"`, `"avfoundation"`.
    pub id: String,
    /// Human-facing name for settings UI.
    pub display_name: String,
    /// Lowercase file extensions this engine can open, without the dot.
    pub containers: Vec<String>,
    /// Whether the engine can present video at all.
    pub video: bool,
    /// Whether the engine can float its video window above other windows.
    pub ontop_window: bool,
    /// Whether the engine offers system Picture-in-Picture.
    pub native_pip: bool,
}

impl EngineCapabilities {
    /// Case- and dot-insensitive extension test.
    pub fn handles_extension(&self, ext: &str) -> bool {
        let needle = ext.trim_start_matches('.').to_ascii_lowercase();
        self.containers
            .iter()
            .any(|c| c.to_ascii_lowercase() == needle)
    }

    /// Whether this engine can open the given path, judged by extension.
    pub fn handles_path(&self, path: &str) -> bool {
        match path.rsplit_once('.') {
            Some((_, ext)) if !ext.is_empty() => self.handles_extension(ext),
            _ => false,
        }
    }
}

/// A point-in-time read of engine state. Cheap enough to poll for a scrubber.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EngineSnapshot {
    /// Playback position. `None` before the first frame is decoded.
    pub position_secs: Option<f64>,
    /// Total duration, `None` if not yet known or not applicable.
    pub duration_secs: Option<f64>,
    pub paused: bool,
    /// No file loaded.
    pub idle: bool,
    /// Current file has played to completion.
    pub eof: bool,
    /// Loaded file carries a video track.
    pub has_video: bool,
    /// Video surface is currently presented.
    pub video_visible: bool,
    /// Absolute path of the loaded file, if any.
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    /// The backend is not installed. `hint` is a user-facing remedy the UI turns
    /// into an onboarding sheet — for mpv, `brew install mpv`.
    #[error("{display_name} is not installed. {hint}")]
    EngineMissing { display_name: String, hint: String },

    #[error("failed to start {display_name}: {message}")]
    Spawn {
        display_name: String,
        message: String,
    },

    /// The engine process died or the transport broke.
    #[error("lost connection to {display_name}: {message}")]
    Disconnected {
        display_name: String,
        message: String,
    },

    #[error("engine rejected {operation}: {message}")]
    Rejected { operation: String, message: String },

    #[error("engine did not respond to {operation} within {timeout_ms}ms")]
    Timeout { operation: String, timeout_ms: u64 },

    #[error("no engine can play {path}")]
    Unsupported { path: String },

    #[error("{message}")]
    Internal { message: String },
}

/// Where and how large the video window is when it appears.
///
/// Height is never part of it: the window follows the video's aspect, so a
/// width is the whole size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VideoWindowLayout {
    /// A fraction of the screen's width, in (0, 1], placed by the system.
    ScreenFraction(f64),
    /// As large as fits the screen — edge to edge in width or height,
    /// whichever the video's aspect reaches first — but still a window.
    Fill,
    /// The whole screen.
    Fullscreen,
    /// A width in physical pixels, optionally where its top-left corner goes
    /// — physical pixels from the top-left of the screen's usable area — and
    /// optionally which screen, by index.
    Custom {
        width: u32,
        position: Option<(u32, u32)>,
        screen: Option<u32>,
    },
}

impl VideoWindowLayout {
    /// Parse the settings form: `"fill"`, `"fullscreen"`, `"40%"`, `"1280"`,
    /// `"1280+100+50"` (width, then x and y), or `"1280+100+50/1"` (on the
    /// second screen).
    pub fn parse(spec: &str) -> Option<Self> {
        let spec = spec.trim();
        if spec.eq_ignore_ascii_case("fill") {
            return Some(Self::Fill);
        }
        let (spec, screen) = match spec.split_once('/') {
            Some((head, screen)) => (head.trim(), Some(screen.trim().parse::<u32>().ok()?)),
            None => (spec, None),
        };
        if spec.eq_ignore_ascii_case("fullscreen") {
            return Some(Self::Fullscreen);
        }
        if let Some(percent) = spec.strip_suffix('%') {
            let percent: f64 = percent.trim().parse().ok()?;
            return (percent > 0.0 && percent <= 100.0)
                .then_some(Self::ScreenFraction(percent / 100.0));
        }
        let mut parts = spec.split('+');
        let width: u32 = parts.next()?.trim().parse().ok()?;
        if width == 0 {
            return None;
        }
        let position = match (parts.next(), parts.next()) {
            (None, _) => None,
            (Some(x), Some(y)) => Some((x.trim().parse().ok()?, y.trim().parse().ok()?)),
            (Some(_), None) => return None,
        };
        if parts.next().is_some() {
            return None;
        }
        Some(Self::Custom {
            width,
            position,
            screen,
        })
    }

    /// The settings form, round-tripping through [`Self::parse`].
    pub fn as_str(&self) -> String {
        match self {
            Self::ScreenFraction(fraction) => format!("{}%", (fraction * 100.0).round()),
            Self::Fill => "fill".into(),
            Self::Fullscreen => "fullscreen".into(),
            Self::Custom {
                width,
                position,
                screen,
            } => {
                let mut spec = width.to_string();
                if let Some((x, y)) = position {
                    spec.push_str(&format!("+{x}+{y}"));
                }
                if let Some(screen) = screen {
                    spec.push_str(&format!("/{screen}"));
                }
                spec
            }
        }
    }
}

impl Default for VideoWindowLayout {
    /// Big enough to watch, small enough to leave the desktop usable.
    fn default() -> Self {
        Self::ScreenFraction(0.4)
    }
}

#[cfg(test)]
mod layout_tests {
    use super::VideoWindowLayout as L;

    #[test]
    fn settings_form_round_trips() {
        for spec in [
            "40%",
            "fill",
            "fullscreen",
            "1280",
            "1280+100+50",
            "1280+100+50/1",
            "1280/2",
        ] {
            assert_eq!(L::parse(spec).unwrap().as_str(), spec);
        }
        assert_eq!(L::parse("1280/x"), None, "a screen is an index");
        assert_eq!(L::parse("FullScreen"), Some(L::Fullscreen));
        assert_eq!(
            L::parse(" 25 % "),
            Some(L::ScreenFraction(0.25)),
            "whitespace is forgiven"
        );
        assert_eq!(L::parse("0%"), None);
        assert_eq!(L::parse("120%"), None);
        assert_eq!(L::parse("0"), None);
        assert_eq!(L::parse("1280+100"), None, "a position needs both axes");
        assert_eq!(L::parse("1280+1+2+3"), None);
        assert_eq!(L::parse("wide"), None);
    }
}

/// The contract every playback backend satisfies.
///
/// Implementations must be safe to call from multiple threads and must not block
/// for longer than their own timeout — the menu bar UI calls these directly.
pub trait PlaybackEngine: Send + Sync {
    fn capabilities(&self) -> EngineCapabilities;

    /// Load `path`, optionally starting at `start_at` seconds. Replaces whatever
    /// is currently loaded. Begins playing unless the engine is paused.
    fn load(&self, path: String, start_at: Option<f64>) -> Result<(), EngineError>;

    fn set_paused(&self, paused: bool) -> Result<(), EngineError>;
    fn seek_absolute(&self, seconds: f64) -> Result<(), EngineError>;

    /// Volume as a percentage, 0.0–100.0.
    fn set_volume(&self, percent: f64) -> Result<(), EngineError>;

    /// Playback rate, 1.0 being normal speed.
    fn set_speed(&self, rate: f64) -> Result<(), EngineError>;

    /// Show or hide the video surface without interrupting audio.
    ///
    /// This is the pop-out toggle. The mpv engine implements it via the `vid`
    /// property, which the M0 spike verified preserves audio continuity exactly
    /// (see `docs/mpv-notes.md`). An AVFoundation engine would show/hide its
    /// `NSWindow`. Engines whose `capabilities().video` is false may no-op.
    fn set_video_visible(&self, visible: bool) -> Result<(), EngineError>;

    /// Float the video window above other windows.
    fn set_video_ontop(&self, ontop: bool) -> Result<(), EngineError>;

    /// Where and how large the video window is when it next appears. Size
    /// and position take effect on the next pop-out, not on a window already
    /// showing; fullscreen may be applied to a live window.
    ///
    /// Defaulted rather than required: an engine with no window of its own —
    /// or a foreign one written before this existed — has nothing to do here.
    fn set_video_window_layout(&self, layout: VideoWindowLayout) -> Result<(), EngineError> {
        let _ = layout;
        Ok(())
    }

    /// Show an empty video window for the user to place by hand, returning
    /// the id of the process that owns it so the host can read the window's
    /// bounds back. `None` from an engine that has no window to show.
    ///
    /// The window comes up at the current layout, windowed even if that
    /// layout is fullscreen, and stays until [`Self::end_window_placement`].
    fn begin_window_placement(&self) -> Result<Option<u32>, EngineError> {
        Ok(None)
    }

    /// Take the placement window down and go back to the layout as set.
    fn end_window_placement(&self) -> Result<(), EngineError> {
        Ok(())
    }

    /// Unload the current file and return to idle.
    fn stop(&self) -> Result<(), EngineError>;

    /// Current state. Must never block on the engine process; return the last
    /// known values if a refresh is in flight.
    fn snapshot(&self) -> EngineSnapshot;

    /// Release the backend. Idempotent; implementations must not leave orphaned
    /// processes behind.
    fn shutdown(&self);
}

/// Engines are shared across the UI thread, the CLI, and the resume ticker.
pub type SharedEngine = Arc<dyn PlaybackEngine>;

pub mod null;
