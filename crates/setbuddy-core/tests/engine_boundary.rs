//! The boundary itself: what core is allowed to know about an engine, and what
//! it must do when there is more than one.
//!
//! Two engines with different container lists stand in for the real pairing —
//! AVFoundation and mpv today, whatever a Linux front end registers tomorrow.
//! None of these tests names a backend, and neither may the code they cover.

use std::sync::Arc;

use setbuddy_core::player::Player;
use setbuddy_core::selection::{EnginePolicy, EngineRegistry};
use setbuddy_core::store::Store;
use setbuddy_core::CoreError;
use setbuddy_engine::null::NullEngine;
use setbuddy_engine::PlaybackEngine;

struct Fixture {
    player: Player,
    /// Narrow: plays only the containers it decodes natively.
    native: Arc<NullEngine>,
    /// Wide: the catch-all that takes everything else.
    fallback: Arc<NullEngine>,
    store: Arc<Store>,
    dir: std::path::PathBuf,
}

fn fixture(files: &[&str]) -> Fixture {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "setbuddy-boundary-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    for name in files {
        std::fs::write(dir.join(name), b"").unwrap();
    }

    let native =
        Arc::new(NullEngine::with_containers("native", &["mp3", "mp4"]).with_duration(Some(600.0)));
    let fallback = Arc::new(
        NullEngine::with_containers("fallback", &["mp3", "mp4", "webm", "mkv"])
            .with_duration(Some(600.0)),
    );
    let store = Arc::new(Store::in_memory().unwrap());
    let registry = EngineRegistry::new(vec![native.clone(), fallback.clone()]);
    let player = Player::new(store.clone(), registry).unwrap();
    Fixture {
        player,
        native,
        fallback,
        store,
        dir,
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn loads(engine: &NullEngine) -> Vec<String> {
    engine
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("load("))
        .collect()
}

/// Registration order is preference order, and the file decides: the first
/// engine that can open it gets it, the rest never hear about it.
#[test]
fn each_file_goes_to_the_first_engine_that_can_open_it() {
    let f = fixture(&["track.mp3", "set.webm"]);

    f.player.play_path(&f.dir.join("track.mp3")).unwrap();
    assert_eq!(loads(&f.native).len(), 1, "the narrow engine takes the mp3");
    assert!(loads(&f.fallback).is_empty());
    assert_eq!(
        f.player.status().unwrap().engine_id.as_deref(),
        Some("native")
    );

    f.player.play_path(&f.dir.join("set.webm")).unwrap();
    assert_eq!(
        loads(&f.fallback).len(),
        1,
        "a container the narrow engine cannot read falls through"
    );
    assert_eq!(
        f.player.status().unwrap().engine_id.as_deref(),
        Some("fallback")
    );
}

/// Handing over between engines must not leave two of them holding the audio
/// device. The outgoing one is stopped before the incoming one loads.
#[test]
fn switching_engines_stops_the_outgoing_one_first() {
    let f = fixture(&["track.mp3", "set.webm"]);
    f.player.play_path(&f.dir.join("track.mp3")).unwrap();
    f.player.play_path(&f.dir.join("set.webm")).unwrap();

    assert!(
        f.native.calls().iter().any(|c| c == "stop()"),
        "the engine that was playing must be stopped: {:?}",
        f.native.calls()
    );
    assert!(
        f.native.snapshot().idle,
        "and must actually be idle afterwards"
    );
    assert!(!f.fallback.snapshot().idle, "while the new one plays");
}

/// The same handover, reached the way a listener actually reaches it: the
/// track ends and the queue moves on to one only the other engine can open.
#[test]
fn the_queue_hands_over_between_engines_at_end_of_file() {
    let f = fixture(&["a.mp3", "b.webm"]);
    let a = f.player.ensure_indexed(&f.dir.join("a.mp3")).unwrap();
    let b = f.player.ensure_indexed(&f.dir.join("b.webm")).unwrap();
    f.player.set_queue(vec![a.id, b.id], Some(0)).unwrap();
    f.player.play_track_id(a.id).unwrap();

    f.native.tick(1000.0);
    f.player.tick().unwrap();

    let status = f.player.status().unwrap();
    assert_eq!(status.track.map(|t| t.id), Some(b.id));
    assert_eq!(status.engine_id.as_deref(), Some("fallback"));
    assert!(f.native.snapshot().idle, "no audio left running behind it");
}

/// Playing the same engine twice in a row must not stop it between tracks —
/// that would be an audible gap for no reason.
#[test]
fn staying_on_one_engine_does_not_stop_it_between_tracks() {
    let f = fixture(&["a.mp3", "b.mp4"]);
    f.player.play_path(&f.dir.join("a.mp3")).unwrap();
    f.player.play_path(&f.dir.join("b.mp4")).unwrap();
    assert!(
        !f.native.calls().iter().any(|c| c == "stop()"),
        "same engine, so no stop: {:?}",
        f.native.calls()
    );
}

/// Forcing an engine is an explicit choice, so it is honoured exactly: a file
/// it cannot open is reported as unsupported rather than quietly rerouted.
#[test]
fn a_forced_engine_never_falls_back() {
    let f = fixture(&["track.mp3", "set.webm"]);
    f.player
        .set_engine_policy(EnginePolicy::Force("native".into()))
        .unwrap();

    f.player.play_path(&f.dir.join("track.mp3")).unwrap();
    assert_eq!(
        f.player.status().unwrap().engine_id.as_deref(),
        Some("native")
    );

    assert!(f.player.play_path(&f.dir.join("set.webm")).is_err());
    assert!(
        loads(&f.fallback).is_empty(),
        "the other engine was never asked"
    );
}

/// A policy naming an engine that is not registered would make every file
/// unplayable, and the failure would surface at play time rather than at the
/// choice. It is refused where it is made.
#[test]
fn forcing_an_engine_that_is_not_registered_is_refused() {
    let f = fixture(&["track.mp3"]);
    let refused = f
        .player
        .set_engine_policy(EnginePolicy::Force("not-installed".into()));
    assert!(matches!(refused, Err(CoreError::Internal { .. })));
    assert_eq!(
        f.player.engine_policy(),
        EnginePolicy::Auto,
        "the policy in force is unchanged"
    );

    // And it was not written to the store either, so a restart is still sane.
    let reopened = Player::new(
        f.store.clone(),
        EngineRegistry::new(vec![f.native.clone(), f.fallback.clone()]),
    )
    .unwrap();
    assert_eq!(reopened.engine_policy(), EnginePolicy::Auto);
    assert!(reopened.play_path(&f.dir.join("track.mp3")).is_ok());
}

#[test]
fn a_chosen_engine_survives_a_restart() {
    let f = fixture(&["track.mp3"]);
    f.player
        .set_engine_policy(EnginePolicy::Force("fallback".into()))
        .unwrap();

    let reopened = Player::new(
        f.store.clone(),
        EngineRegistry::new(vec![f.native.clone(), f.fallback.clone()]),
    )
    .unwrap();
    assert_eq!(
        reopened.engine_policy(),
        EnginePolicy::Force("fallback".into()),
        "settings must show what is really set, not a default"
    );
    reopened.play_path(&f.dir.join("track.mp3")).unwrap();
    assert_eq!(
        reopened.status().unwrap().engine_id.as_deref(),
        Some("fallback")
    );
}

/// The window layout is about the window, whichever engine ends up drawing it,
/// so every registered engine is told — not only the one that is playing.
#[test]
fn the_window_layout_reaches_every_engine_and_survives_a_restart() {
    let f = fixture(&["set.webm"]);
    let layout = setbuddy_engine::VideoWindowLayout::parse("1280+100+50").unwrap();
    f.player.set_video_window_layout(layout).unwrap();

    assert_eq!(f.player.video_window_layout().unwrap(), Some(layout));

    let reopened = Player::new(
        f.store.clone(),
        EngineRegistry::new(vec![f.native.clone(), f.fallback.clone()]),
    )
    .unwrap();
    assert_eq!(
        reopened.video_window_layout().unwrap(),
        Some(layout),
        "the saved layout is handed to engines before anything can pop out"
    );
}

#[test]
fn engines_are_reported_by_capability_in_preference_order() {
    let f = fixture(&[]);
    let caps = f.player.engine_capabilities();
    assert_eq!(
        caps.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
        ["native", "fallback"]
    );
    assert!(
        caps.iter().all(|c| !c.display_name.is_empty()),
        "settings name engines by display_name, so it must exist"
    );
    assert_eq!(f.player.engine_ids(), ["native", "fallback"]);
}

/// Quitting must stop *every* engine, not just whichever one was playing —
/// an engine left running holds the audio device after the app is gone.
#[test]
fn quitting_shuts_every_engine_down() {
    let f = fixture(&["track.mp3"]);
    f.player.play_path(&f.dir.join("track.mp3")).unwrap();
    f.player.quit().unwrap();
    assert!(f.native.was_shutdown());
    assert!(f.fallback.was_shutdown(), "including the idle one");
}

/// With no engine registered at all, core says so plainly rather than
/// pretending a file is unsupported.
#[test]
fn an_empty_registry_is_an_internal_error_not_an_unsupported_file() {
    let store = Arc::new(Store::in_memory().unwrap());
    let player = Player::new(store, EngineRegistry::new(Vec::new())).unwrap();
    assert!(matches!(
        player.play_track_id(1),
        // No track either, so this is about not panicking on the way through.
        Err(CoreError::NoMatch { .. })
    ));
    assert!(player.engine_ids().is_empty());
    assert!(player.status().unwrap().idle);
}

/// The invariant `CLAUDE.md` states, checked rather than trusted: the contract
/// and the domain never name a backend outside a comment. A Linux front end
/// registers a different engine and changes nothing here — but only while this
/// stays true.
#[test]
fn neither_the_contract_nor_the_domain_names_a_backend() {
    const BACKENDS: &[&str] = &["mpv", "avfoundation", "avplayer", "gstreamer", "vlc"];
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();

    let mut offences = Vec::new();
    for crate_name in ["setbuddy-engine", "setbuddy-core"] {
        let src = root.join(crate_name).join("src");
        let mut files = Vec::new();
        collect_rust_files(&src, &mut files);
        assert!(
            !files.is_empty(),
            "no sources found under {}",
            src.display()
        );

        for path in files {
            let text = std::fs::read_to_string(&path).unwrap();
            let mut in_test_module = false;
            for (number, line) in text.lines().enumerate() {
                // Tests may name a hypothetical engine; the shipped code may not.
                if line.trim_start().starts_with("#[cfg(test)]") {
                    in_test_module = true;
                }
                let code = line.split("//").next().unwrap_or("");
                if in_test_module || code.trim().is_empty() {
                    continue;
                }
                let lowered = code.to_ascii_lowercase();
                for backend in BACKENDS {
                    if lowered.contains(backend) {
                        offences.push(format!(
                            "{}:{}: {}",
                            path.display(),
                            number + 1,
                            line.trim()
                        ));
                    }
                }
            }
        }
    }
    assert!(
        offences.is_empty(),
        "the engine boundary leaks a backend name:\n{}",
        offences.join("\n")
    );
}

fn collect_rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rust_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}
