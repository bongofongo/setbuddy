//! End-to-end tests over the same wiring `main.rs` builds: a real SQLite store,
//! a real mpv on a shared socket, and the `Player` facade on top.
//!
//! ```text
//! cargo test -p setwave-cli --features integration -- --test-threads=1
//! ```
#![cfg(feature = "integration")]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use setwave_core::player::Player;
use setwave_core::selection::EngineRegistry;
use setwave_core::store::Store;
use setwave_engine::{PlaybackEngine, SharedEngine};
use setwave_mpv::MpvEngine;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../setwave-mpv/tests/assets")
        .join(name)
        .canonicalize()
        .expect("test asset must exist")
}

struct Harness {
    player: Player,
    socket: PathBuf,
}

impl Harness {
    fn new(tag: &str) -> Self {
        let socket = std::env::temp_dir().join(format!("sw-e2e-{}-{tag}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let store = Arc::new(Store::in_memory().unwrap());
        Self::with_store(store, socket)
    }

    fn with_store(store: Arc<Store>, socket: PathBuf) -> Self {
        let engine: SharedEngine = Arc::new(MpvEngine::shared(&socket).unwrap());
        let player = Player::new(store, EngineRegistry::new(vec![engine])).unwrap();
        Harness { player, socket }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.player.quit();
        // `quit` shuts the engine down, but if the test panicked before the
        // player was fully wired there could still be an mpv on this socket.
        if MpvEngine::is_running_at(&self.socket) {
            if let Ok(engine) = MpvEngine::shared(&self.socket) {
                let _ = engine.snapshot();
                engine.shutdown();
            }
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

fn wait_position(
    player: &Player,
    what: &str,
    timeout: Duration,
    pred: impl Fn(f64) -> bool,
) -> f64 {
    let deadline = Instant::now() + timeout;
    loop {
        let pos = player.status().unwrap().position_secs.unwrap_or(f64::NAN);
        if pos.is_finite() && pred(pos) {
            return pos;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; position was {pos:?}"
        );
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// Stops whatever is listening on a shared socket, however the test exits — a
/// shared engine outlives its owner by design, so a panic would leak an mpv.
struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        if MpvEngine::is_running_at(&self.0) {
            if let Ok(engine) = MpvEngine::shared(&self.0) {
                let _ = engine.snapshot();
                engine.shutdown();
            }
        }
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Engine level: does a shared-socket engine honour an explicit start offset?
#[test]
fn shared_engine_honours_start_offset() {
    let socket = std::env::temp_dir().join(format!("sw-e2e-{}-engine.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    let _cleanup = SocketGuard(socket.clone());
    let engine = MpvEngine::shared(&socket).unwrap();

    engine
        .load(asset("tiny.webm").to_string_lossy().into_owned(), Some(3.0))
        .unwrap();

    let deadline = Instant::now() + Duration::from_millis(800);
    let mut pos = f64::NAN;
    while Instant::now() < deadline {
        if let Some(p) = engine.snapshot().position_secs {
            pos = p;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    engine.shutdown();
    let _ = std::fs::remove_file(&socket);
    assert!(
        (2.5..5.5).contains(&pos),
        "shared engine should start near 3s, got {pos:.3}s"
    );
}

/// Player level: a stored position must reach the engine on the next play.
///
/// Uses the 200s fixture deliberately — the resume guards ignore anything in the
/// first 30s or last 60s, so a short file can never exercise this rule.
#[test]
fn player_resumes_from_a_stored_position() {
    let h = Harness::new("resume");
    let path = asset("long.webm");
    let track = h.player.play_path(&path).unwrap();
    assert_eq!(
        track.duration_secs.map(|d| d.round()),
        Some(200.0),
        "fixture duration should have been probed"
    );

    // Seed a position the way a previous session would have left one.
    h.player.store().set_resume(track.id, 90.0).unwrap();
    h.player.stop().unwrap();

    h.player.play_track_id(track.id).unwrap();
    let pos = wait_position(&h.player, "resumed position", Duration::from_secs(5), |p| {
        p > 0.0
    });
    assert!(
        (89.0..95.0).contains(&pos),
        "player should have resumed near 90s, got {pos:.3}s"
    );
}

/// The whole loop the CLI performs: play, let it run, stop (which persists the
/// position), then play again and land where you left off.
#[test]
fn position_survives_a_stop_and_replay() {
    let h = Harness::new("cycle");
    let path = asset("long.webm");
    let track = h.player.play_path(&path).unwrap();

    h.player.seek_absolute(120.0).unwrap();
    wait_position(&h.player, "seek to land", Duration::from_secs(5), |p| {
        p > 119.0
    });
    h.player.tick().unwrap();

    let stored = h.player.store().resume_for(track.id).unwrap();
    assert!(
        stored.map(|s| (119.0..126.0).contains(&s)).unwrap_or(false),
        "expected a stored position near 120s, got {stored:?}"
    );

    h.player.stop().unwrap();
    h.player.play_track_id(track.id).unwrap();
    let pos = wait_position(&h.player, "resumed position", Duration::from_secs(5), |p| {
        p > 0.0
    });
    assert!(
        (119.0..126.0).contains(&pos),
        "replay should resume near 120s, got {pos:.3}s"
    );
}

/// Restarting must beat the saved position even when the track being restarted
/// is the one already playing — the case where a naive "clear then play" fails,
/// because persisting the outgoing position writes the row straight back.
#[test]
fn restarting_the_currently_playing_track_ignores_its_saved_position() {
    let h = Harness::new("restart");
    let path = asset("long.webm");
    let track = h.player.play_path(&path).unwrap();

    h.player.seek_absolute(120.0).unwrap();
    wait_position(&h.player, "seek to land", Duration::from_secs(5), |p| {
        p > 119.0
    });
    h.player.tick().unwrap();
    assert!(h.player.store().resume_for(track.id).unwrap().is_some());

    h.player.restart_track_id(track.id).unwrap();
    let pos = wait_position(
        &h.player,
        "restarted playback",
        Duration::from_secs(5),
        |p| p.is_finite(),
    );
    assert!(
        pos < 10.0,
        "restart should have begun from the top, got {pos:.3}s"
    );
    assert_eq!(
        h.player.store().resume_for(track.id).unwrap(),
        None,
        "restarting discards the saved position"
    );
}
