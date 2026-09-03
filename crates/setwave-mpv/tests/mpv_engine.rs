//! Integration tests against a real mpv process.
//!
//! Gated behind the `integration` feature so `cargo test` still passes on a
//! machine without mpv:
//!
//! ```text
//! cargo test -p setwave-mpv --features integration -- --test-threads=1
//! ```
//!
//! These assert the behaviour the whole product rests on — that the pop-out
//! toggle does not interrupt audio — against real VP9/Opus media rather than a
//! mock that would happily agree with us.
#![cfg(feature = "integration")]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use setwave_engine::{EngineSnapshot, PlaybackEngine};
use setwave_mpv::MpvEngine;

fn asset(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/assets")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

fn engine() -> MpvEngine {
    MpvEngine::new().expect("mpv must be installed to run integration tests")
}

/// Poll until `pred` holds, returning the snapshot that satisfied it.
fn wait_for(
    engine: &MpvEngine,
    what: &str,
    timeout: Duration,
    pred: impl Fn(&EngineSnapshot) -> bool,
) -> EngineSnapshot {
    let deadline = Instant::now() + timeout;
    loop {
        let snap = engine.snapshot();
        if pred(&snap) {
            return snap;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; last snapshot: {snap:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn position(engine: &MpvEngine) -> f64 {
    engine.snapshot().position_secs.unwrap_or(f64::NAN)
}

/// Read a position that has stopped moving.
///
/// After a pause, mpv may still have one `time-pos` update in flight. Sampling
/// immediately can catch the value from before that update and then see it
/// land, which looks exactly like a paused player advancing. Settling first
/// tests the property we actually care about rather than the timing.
fn settled_position(engine: &MpvEngine, timeout: Duration) -> f64 {
    let deadline = Instant::now() + timeout;
    let mut previous = position(engine);
    loop {
        std::thread::sleep(Duration::from_millis(120));
        let current = position(engine);
        if (current - previous).abs() < 0.001 || Instant::now() > deadline {
            return current;
        }
        previous = current;
    }
}

#[test]
fn plays_video_file_audio_only_until_popped_out() {
    let e = engine();
    e.load(asset("tiny.webm"), None).unwrap();

    let snap = wait_for(&e, "playback to start", Duration::from_secs(10), |s| {
        s.position_secs.unwrap_or(0.0) > 0.0
    });
    assert!(snap.has_video, "the file does have a video track");
    assert!(
        !snap.video_visible,
        "engine is audio-first: no window until the user pops it out"
    );
    assert!(snap.duration_secs.unwrap_or(0.0) > 5.0);

    e.set_video_visible(true).unwrap();
    wait_for(&e, "video output to appear", Duration::from_secs(10), |s| {
        s.video_visible
    });

    e.set_video_visible(false).unwrap();
    wait_for(
        &e,
        "video output to go away",
        Duration::from_secs(10),
        |s| !s.video_visible,
    );
}

/// The headline claim from the M0 spike, pinned as a test: toggling video does
/// not restart playback, so position only ever moves forward across a pop-out.
#[test]
fn pop_out_toggle_never_rewinds_playback() {
    let e = engine();
    e.load(asset("tiny.webm"), None).unwrap();
    wait_for(&e, "playback to start", Duration::from_secs(10), |s| {
        s.position_secs.unwrap_or(0.0) > 0.2
    });

    let mut previous = position(&e);
    for round in 0..3 {
        for visible in [true, false] {
            e.set_video_visible(visible).unwrap();
            std::thread::sleep(Duration::from_millis(350));
            let now = position(&e);
            assert!(
                now + 0.05 >= previous,
                "round {round}: setting video visible={visible} rewound playback \
                 from {previous:.3}s to {now:.3}s"
            );
            previous = now;
        }
    }
    assert!(
        previous > 0.5,
        "playback should have kept running throughout the toggles, got {previous:.3}s"
    );
}

/// `load` must not return until mpv has actually opened the file at the
/// requested offset.
///
/// The window here is deliberately tight. mpv reads its `start` option when the
/// file opens, not when `loadfile` is acknowledged, so clearing that option too
/// eagerly drops the offset — and only for files slow enough to load, which is
/// precisely the long sets resume exists for. A generous poll would let that bug
/// pass by watching playback wander past 3s on its own.
#[test]
fn load_starts_at_requested_position() {
    let e = engine();
    e.load(asset("tiny.webm"), Some(3.0)).unwrap();
    let snap = wait_for(&e, "resumed position", Duration::from_millis(600), |s| {
        s.position_secs.is_some()
    });
    let pos = snap.position_secs.unwrap();
    assert!(
        (2.5..5.5).contains(&pos),
        "load should have returned already positioned near 3s, got {pos:.3}s"
    );
}

/// `start` is a per-file option; a resumed track must not drag its offset into
/// whatever plays next.
#[test]
fn start_offset_does_not_leak_into_the_next_load() {
    let e = engine();
    e.load(asset("tiny.webm"), Some(3.0)).unwrap();
    wait_for(&e, "resumed position", Duration::from_secs(10), |s| {
        s.position_secs.unwrap_or(0.0) > 2.5
    });

    e.load(asset("tiny.mp3"), None).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    let pos = position(&e);
    assert!(
        pos < 2.0,
        "second track should start from the beginning, got {pos:.3}s"
    );
}

#[test]
fn pause_holds_position_and_resume_continues() {
    let e = engine();
    e.load(asset("tiny.webm"), None).unwrap();
    wait_for(&e, "playback to start", Duration::from_secs(10), |s| {
        s.position_secs.unwrap_or(0.0) > 0.2
    });

    e.set_paused(true).unwrap();
    let held = settled_position(&e, Duration::from_secs(3));
    // A wide window with a loose tolerance: over 1.5s a playing file advances
    // ~1.5s, so 0.35s separates "paused" from "playing" by a wide margin
    // without depending on how quickly mpv acts on the command under load.
    std::thread::sleep(Duration::from_millis(1500));
    let still = position(&e);
    assert!(
        (still - held).abs() < 0.35,
        "paused playback advanced from {held:.3}s to {still:.3}s"
    );
    assert!(e.snapshot().paused);

    e.set_paused(false).unwrap();
    std::thread::sleep(Duration::from_millis(600));
    assert!(
        position(&e) > still,
        "playback did not resume after unpausing"
    );
}

#[test]
fn seek_moves_to_absolute_position() {
    let e = engine();
    e.load(asset("tiny.webm"), None).unwrap();
    wait_for(&e, "playback to start", Duration::from_secs(10), |s| {
        s.position_secs.unwrap_or(0.0) > 0.1
    });

    e.seek_absolute(4.0).unwrap();
    let snap = wait_for(&e, "seek to land", Duration::from_secs(5), |s| {
        s.position_secs.unwrap_or(0.0) > 3.5
    });
    assert!(snap.position_secs.unwrap() >= 3.5);
}

/// The stated regression floor: ordinary audio files must be flawless, not an
/// afterthought of the video path.
#[test]
fn plays_plain_mp3_and_wav() {
    for (file, label) in [("tiny.mp3", "mp3"), ("tiny.wav", "wav")] {
        let e = engine();
        e.load(asset(file), None).unwrap();
        let snap = wait_for(&e, label, Duration::from_secs(10), |s| {
            s.position_secs.unwrap_or(0.0) > 0.2
        });
        assert!(
            !snap.has_video,
            "{label} must not be treated as having video to pop out"
        );
        assert!(
            snap.duration_secs.unwrap_or(0.0) > 1.0,
            "{label} duration was not detected"
        );
        assert!(!snap.idle, "{label} should be loaded and playing");
    }
}

#[test]
fn stop_returns_engine_to_idle() {
    let e = engine();
    e.load(asset("tiny.mp3"), None).unwrap();
    wait_for(&e, "playback to start", Duration::from_secs(10), |s| {
        s.position_secs.unwrap_or(0.0) > 0.1
    });

    e.stop().unwrap();
    let snap = wait_for(&e, "idle after stop", Duration::from_secs(5), |s| s.idle);
    assert!(snap.path.is_none());
}

/// Quitting must never leave an mpv holding the audio device.
#[test]
fn shutdown_reaps_the_mpv_process() {
    let before = count_mpv_processes();
    {
        let e = engine();
        e.load(asset("tiny.mp3"), None).unwrap();
        wait_for(&e, "playback to start", Duration::from_secs(10), |s| {
            s.position_secs.unwrap_or(0.0) > 0.1
        });
        assert!(
            count_mpv_processes() > before,
            "engine should have spawned an mpv"
        );
        e.shutdown();
        e.shutdown(); // idempotent
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while count_mpv_processes() > before && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        count_mpv_processes(),
        before,
        "shutdown left an orphaned mpv process behind"
    );
}

/// Dropping the engine without an explicit shutdown must also clean up.
#[test]
fn drop_reaps_the_mpv_process() {
    let before = count_mpv_processes();
    {
        let e = engine();
        e.load(asset("tiny.mp3"), None).unwrap();
        wait_for(&e, "playback to start", Duration::from_secs(10), |s| {
            s.position_secs.unwrap_or(0.0) > 0.1
        });
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while count_mpv_processes() > before && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        count_mpv_processes(),
        before,
        "dropping the engine left an orphaned mpv process behind"
    );
}

/// Stops whatever is listening on a shared socket, however the test exits.
struct SharedSocketGuard(std::path::PathBuf);

impl Drop for SharedSocketGuard {
    fn drop(&mut self) {
        if MpvEngine::is_running_at(&self.0) {
            if let Ok(engine) = MpvEngine::shared(&self.0) {
                // Touch it so the engine adopts the running process, then stop it.
                let _ = engine.snapshot();
                engine.shutdown();
            }
        }
        let _ = std::fs::remove_file(&self.0);
    }
}

fn count_mpv_processes() -> usize {
    std::process::Command::new("pgrep")
        .args(["-x", "mpv"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count()
        })
        .unwrap_or(0)
}

/// The CLI is a series of short-lived processes. A shared engine must therefore
/// leave mpv playing when it is dropped, and a later engine must adopt it with
/// its state intact — that is what makes `setwave play` then `setwave pause`
/// work without a daemon.
#[test]
fn shared_engine_outlives_its_process_and_is_adopted() {
    // `Instant::now().elapsed()` is ~0, which would make this name collide
    // between runs; the wall clock actually varies.
    let socket = std::env::temp_dir().join(format!(
        "setwave-test-shared-{}-{}.sock",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_file(&socket);
    let before = count_mpv_processes();

    // A shared engine deliberately outlives its owner, so a panic partway
    // through this test would leak a running mpv onto the developer's machine.
    let _cleanup = SharedSocketGuard(socket.clone());

    // First "invocation": start playing, then go away.
    {
        let e = MpvEngine::shared(&socket).unwrap();
        e.load(asset("tiny.webm"), None).unwrap();
        wait_for(&e, "playback to start", Duration::from_secs(10), |s| {
            s.position_secs.unwrap_or(0.0) > 0.3
        });
    }
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        count_mpv_processes() > before,
        "dropping a shared engine must leave mpv playing"
    );
    assert!(MpvEngine::is_running_at(&socket));

    // Second "invocation": adopt it and observe the same file, still advancing.
    let adopted = MpvEngine::shared(&socket).unwrap();
    let snap = wait_for(&adopted, "adopted state", Duration::from_secs(10), |s| {
        s.path.is_some()
    });
    assert!(
        snap.path.as_deref().unwrap().ends_with("tiny.webm"),
        "adopted engine should report the file already playing, got {:?}",
        snap.path
    );
    assert!(
        snap.has_video,
        "adopted engine should know the file has video"
    );

    let first = snap.position_secs.unwrap_or(0.0);
    assert!(first > 0.3, "adopted playback should be underway");
    adopted.set_paused(true).unwrap();
    assert!(
        adopted.snapshot().paused,
        "adopted engine can control playback"
    );

    adopted.shutdown();
    let deadline = Instant::now() + Duration::from_secs(5);
    while count_mpv_processes() > before && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        count_mpv_processes(),
        before,
        "explicit shutdown must stop a shared mpv"
    );
    let _ = std::fs::remove_file(&socket);
}

/// A seek must be visible in the very next snapshot, with no polling.
///
/// mpv replies to the seek command before emitting the corresponding `time-pos`
/// property change. A UI that reads position straight after a seek would
/// otherwise get the old value and visibly snap backwards before catching up —
/// exactly what a released scrubber thumb does.
#[test]
fn seek_is_visible_in_the_next_snapshot() {
    let e = engine();
    e.load(asset("long.webm"), None).unwrap();
    wait_for(&e, "playback to start", Duration::from_secs(10), |s| {
        s.position_secs.unwrap_or(0.0) > 0.2
    });

    e.seek_absolute(120.0).unwrap();
    // Deliberately no sleep and no polling.
    let pos = e.snapshot().position_secs.unwrap_or(0.0);
    assert!(
        (119.0..127.0).contains(&pos),
        "snapshot right after a seek should already report ~120s, got {pos:.3}s"
    );
}

/// Seeking back from the end must clear the finished flag, or the queue would
/// advance on the next tick as though the track had just ended.
#[test]
fn seeking_back_from_the_end_clears_eof() {
    let e = engine();
    e.load(asset("tiny.webm"), None).unwrap();
    wait_for(&e, "end of file", Duration::from_secs(20), |s| s.eof);

    e.seek_absolute(1.0).unwrap();
    assert!(
        !e.snapshot().eof,
        "eof should be cleared by seeking backwards"
    );
}

#[test]
fn placement_shows_an_empty_window_and_takes_it_down_again() {
    let e = engine();
    let pid = e
        .begin_window_placement()
        .expect("placement should start")
        .expect("mpv owns a window, so it reports a pid");
    assert!(pid > 0);
    // Nothing is loaded, yet a video output exists: that is the empty window.
    let snap = wait_for(&e, "the placement window", Duration::from_secs(5), |s| {
        s.video_visible
    });
    assert!(snap.idle, "placement must not load anything");

    e.end_window_placement().expect("placement should end");
    wait_for(&e, "the window to go", Duration::from_secs(5), |s| {
        !s.video_visible
    });
    e.shutdown();
}
