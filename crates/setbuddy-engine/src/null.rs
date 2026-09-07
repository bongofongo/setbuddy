//! A deterministic in-memory engine.
//!
//! Two jobs. It lets `setwave-core` be tested — queue advance, resume writes,
//! engine selection — with no mpv, no audio device, and no sleeping: time only
//! moves when a test calls [`NullEngine::tick`]. And it is the Rust twin of the
//! throwaway Swift `NullEngine` used in M2 to prove the foreign-trait direction
//! compiles, so both sides of the boundary are exercised against the same shape.

use std::sync::Mutex;

use crate::{EngineCapabilities, EngineError, EngineSnapshot, PlaybackEngine};

#[derive(Debug, Default)]
struct State {
    path: Option<String>,
    position: f64,
    duration: Option<f64>,
    paused: bool,
    video_visible: bool,
    has_video: bool,
    ontop: bool,
    volume: f64,
    speed: f64,
    shutdown: bool,
    /// Every call recorded, so tests can assert on interaction, not just state.
    calls: Vec<String>,
}

/// An engine that plays nothing, precisely.
#[derive(Debug)]
pub struct NullEngine {
    caps: EngineCapabilities,
    /// Duration reported for any file loaded, mimicking a probe.
    default_duration: Option<f64>,
    state: Mutex<State>,
}

impl NullEngine {
    /// Claims the same containers the mpv engine does, so selection tests are
    /// meaningful without depending on the mpv crate.
    pub fn new() -> Self {
        Self::with_containers(
            "null",
            &[
                "webm", "mkv", "mp4", "m4a", "mov", "mp3", "wav", "flac", "opus", "ogg", "aac",
            ],
        )
    }

    /// A restricted engine, for testing capability-based selection.
    pub fn with_containers(id: &str, containers: &[&str]) -> Self {
        Self {
            caps: EngineCapabilities {
                id: id.to_string(),
                display_name: format!("Null ({id})"),
                containers: containers.iter().map(|c| c.to_string()).collect(),
                video: true,
                ontop_window: true,
                native_pip: false,
            },
            default_duration: Some(180.0),
            state: Mutex::new(State {
                volume: 100.0,
                speed: 1.0,
                ..State::default()
            }),
        }
    }

    pub fn with_duration(mut self, duration: Option<f64>) -> Self {
        self.default_duration = duration;
        self
    }

    /// Advance playback by `secs`, honouring pause and clamping at the end of
    /// the file. This is the only way time moves.
    pub fn tick(&self, secs: f64) {
        let mut s = self.state.lock().expect("null engine state poisoned");
        if s.paused || s.path.is_none() {
            return;
        }
        let speed = s.speed;
        s.position += secs * speed;
        if let Some(d) = s.duration {
            if s.position >= d {
                s.position = d;
            }
        }
    }

    /// Names of engine methods called so far, in order.
    pub fn calls(&self) -> Vec<String> {
        self.state
            .lock()
            .expect("null engine state poisoned")
            .calls
            .clone()
    }

    pub fn was_shutdown(&self) -> bool {
        self.state
            .lock()
            .expect("null engine state poisoned")
            .shutdown
    }

    fn record(&self, call: impl Into<String>) {
        self.state
            .lock()
            .expect("null engine state poisoned")
            .calls
            .push(call.into());
    }
}

impl Default for NullEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaybackEngine for NullEngine {
    fn capabilities(&self) -> EngineCapabilities {
        self.caps.clone()
    }

    fn load(&self, path: String, start_at: Option<f64>) -> Result<(), EngineError> {
        self.record(format!("load({path}, {start_at:?})"));
        let has_video = self
            .caps
            .video
            .then(|| {
                matches!(
                    path.rsplit_once('.')
                        .map(|(_, e)| e.to_ascii_lowercase())
                        .as_deref(),
                    Some("webm" | "mkv" | "mp4" | "mov")
                )
            })
            .unwrap_or(false);
        let mut s = self.state.lock().expect("null engine state poisoned");
        s.path = Some(path);
        s.position = start_at.unwrap_or(0.0);
        s.duration = self.default_duration;
        s.paused = false;
        s.has_video = has_video;
        // Audio-first, matching every real engine: loading a file with video
        // does not present a window until the pop-out is asked for.
        s.video_visible = false;
        Ok(())
    }

    fn set_paused(&self, paused: bool) -> Result<(), EngineError> {
        self.record(format!("set_paused({paused})"));
        self.state
            .lock()
            .expect("null engine state poisoned")
            .paused = paused;
        Ok(())
    }

    fn seek_absolute(&self, seconds: f64) -> Result<(), EngineError> {
        self.record(format!("seek_absolute({seconds})"));
        let mut s = self.state.lock().expect("null engine state poisoned");
        let max = s.duration.unwrap_or(f64::MAX);
        s.position = seconds.clamp(0.0, max);
        Ok(())
    }

    fn set_volume(&self, percent: f64) -> Result<(), EngineError> {
        self.record(format!("set_volume({percent})"));
        self.state
            .lock()
            .expect("null engine state poisoned")
            .volume = percent;
        Ok(())
    }

    fn set_speed(&self, rate: f64) -> Result<(), EngineError> {
        self.record(format!("set_speed({rate})"));
        self.state.lock().expect("null engine state poisoned").speed = rate;
        Ok(())
    }

    fn set_video_visible(&self, visible: bool) -> Result<(), EngineError> {
        self.record(format!("set_video_visible({visible})"));
        self.state
            .lock()
            .expect("null engine state poisoned")
            .video_visible = visible;
        Ok(())
    }

    fn set_video_ontop(&self, ontop: bool) -> Result<(), EngineError> {
        self.record(format!("set_video_ontop({ontop})"));
        self.state.lock().expect("null engine state poisoned").ontop = ontop;
        Ok(())
    }

    fn stop(&self) -> Result<(), EngineError> {
        self.record("stop()");
        let mut s = self.state.lock().expect("null engine state poisoned");
        s.path = None;
        s.position = 0.0;
        s.duration = None;
        s.has_video = false;
        s.video_visible = false;
        Ok(())
    }

    fn snapshot(&self) -> EngineSnapshot {
        let s = self.state.lock().expect("null engine state poisoned");
        EngineSnapshot {
            position_secs: s.path.as_ref().map(|_| s.position),
            duration_secs: s.duration,
            paused: s.paused,
            idle: s.path.is_none(),
            eof: match (s.duration, s.path.as_ref()) {
                (Some(d), Some(_)) => s.position >= d,
                _ => false,
            },
            has_video: s.has_video,
            video_visible: s.video_visible,
            path: s.path.clone(),
        }
    }

    fn shutdown(&self) {
        self.record("shutdown()");
        self.state
            .lock()
            .expect("null engine state poisoned")
            .shutdown = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_matching_ignores_case_and_dot() {
        let caps = NullEngine::new().capabilities();
        assert!(caps.handles_extension("webm"));
        assert!(caps.handles_extension(".WEBM"));
        assert!(caps.handles_path("/sets/Palms Trax.WebM"));
        assert!(!caps.handles_path("/sets/notes.txt"));
        assert!(!caps.handles_path("/sets/no-extension"));
    }

    #[test]
    fn tick_advances_only_while_playing_and_clamps_at_eof() {
        let e = NullEngine::new().with_duration(Some(10.0));
        e.load("/sets/a.webm".into(), None).unwrap();
        e.tick(4.0);
        assert_eq!(e.snapshot().position_secs, Some(4.0));

        e.set_paused(true).unwrap();
        e.tick(4.0);
        assert_eq!(
            e.snapshot().position_secs,
            Some(4.0),
            "paused must not advance"
        );

        e.set_paused(false).unwrap();
        e.tick(100.0);
        let snap = e.snapshot();
        assert_eq!(snap.position_secs, Some(10.0), "clamped to duration");
        assert!(snap.eof);
    }

    #[test]
    fn load_honours_start_at_and_detects_video() {
        let e = NullEngine::new();
        e.load("/sets/a.webm".into(), Some(72.5)).unwrap();
        let snap = e.snapshot();
        assert_eq!(snap.position_secs, Some(72.5));
        assert!(snap.has_video);

        assert!(
            !e.snapshot().video_visible,
            "video is not presented until it is popped out"
        );

        e.load("/music/b.mp3".into(), None).unwrap();
        assert!(
            !e.snapshot().has_video,
            "audio-only file has no video track"
        );
    }

    #[test]
    fn stop_returns_to_idle() {
        let e = NullEngine::new();
        e.load("/sets/a.webm".into(), None).unwrap();
        e.stop().unwrap();
        let snap = e.snapshot();
        assert!(snap.idle);
        assert!(!snap.eof, "idle is not eof");
        assert_eq!(snap.position_secs, None);
    }
}
