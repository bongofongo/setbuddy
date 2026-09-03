//! Player behaviour, driven by the deterministic `NullEngine`.
//!
//! No mpv, no audio device, and no sleeping: time moves only when the test says
//! so, which makes the resume and queue-advance rules testable as rules rather
//! than as timing luck.

use std::path::PathBuf;
use std::sync::Arc;

use setwave_core::player::Player;
use setwave_core::queue::RepeatMode;
use setwave_core::selection::EngineRegistry;
use setwave_core::store::Store;
use setwave_engine::null::NullEngine;

struct Fixture {
    player: Player,
    engine: Arc<NullEngine>,
    store: Arc<Store>,
    dir: PathBuf,
}

/// A scratch directory of empty media files. They are never decoded — the
/// engine is a stub — but they must exist for indexing to canonicalise them.
fn fixture(files: &[&str], duration: Option<f64>) -> Fixture {
    let dir = std::env::temp_dir().join(format!(
        "setwave-core-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    for name in files {
        std::fs::write(dir.join(name), b"").unwrap();
    }

    let store = Arc::new(Store::in_memory().unwrap());
    let engine = Arc::new(NullEngine::new().with_duration(duration));
    let registry = EngineRegistry::new(vec![engine.clone()]);
    let player = Player::new(store.clone(), registry).unwrap();
    Fixture {
        player,
        engine,
        store,
        dir,
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn playing_a_file_indexes_it_and_starts_from_the_beginning() {
    let f = fixture(&["set.webm"], Some(7200.0));
    let track = f.player.play_path(&f.dir.join("set.webm")).unwrap();

    assert_eq!(track.display_title(), "set");
    assert!(track.has_video, "a .webm is treated as carrying video");
    assert_eq!(f.store.track_count().unwrap(), 1);

    let status = f.player.status().unwrap();
    assert_eq!(status.queue_len, 1);
    assert_eq!(status.queue_index, Some(0));
    assert!(!status.idle);
    assert_eq!(status.position_secs, Some(0.0));
}

#[test]
fn position_is_written_while_playing_and_resumed_on_the_next_play() {
    let f = fixture(&["set.webm"], Some(7200.0));
    let path = f.dir.join("set.webm");
    let track = f.player.play_path(&path).unwrap();

    // Play an hour and a quarter into the set.
    f.engine.tick(4364.0);
    f.player.tick().unwrap();

    let stored = f.store.resume_for(track.id).unwrap();
    assert_eq!(stored, Some(4364.0), "position should have been persisted");

    // Play it again: the engine must be asked to start at the stored offset.
    f.player.play_path(&path).unwrap();
    let loads: Vec<String> = f
        .engine
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("load("))
        .collect();
    assert_eq!(loads.len(), 2);
    assert!(
        loads[1].contains("Some(4364.0)"),
        "second load should resume, got {}",
        loads[1]
    );
    assert_eq!(f.player.status().unwrap().position_secs, Some(4364.0));
}

#[test]
fn a_position_inside_the_guard_bands_is_never_stored() {
    let f = fixture(&["set.webm"], Some(7200.0));
    let track = f.player.play_path(&f.dir.join("set.webm")).unwrap();

    f.engine.tick(20.0); // inside the opening 30s
    f.player.tick().unwrap();
    assert_eq!(
        f.store.resume_for(track.id).unwrap(),
        None,
        "the first 30 seconds are not worth resuming"
    );
}

#[test]
fn finishing_a_track_clears_its_resume() {
    let f = fixture(&["set.webm"], Some(7200.0));
    let track = f.player.play_path(&f.dir.join("set.webm")).unwrap();

    f.engine.tick(3600.0);
    f.player.tick().unwrap();
    assert!(f.store.resume_for(track.id).unwrap().is_some());

    // Run to the end.
    f.engine.tick(4000.0);
    f.player.tick().unwrap();
    assert_eq!(
        f.store.resume_for(track.id).unwrap(),
        None,
        "a finished track should start from the top next time"
    );
}

#[test]
fn end_of_file_advances_the_queue() {
    let f = fixture(&["a.mp3", "b.mp3"], Some(100.0));
    let a = f.player.ensure_indexed(&f.dir.join("a.mp3")).unwrap();
    let b = f.player.ensure_indexed(&f.dir.join("b.mp3")).unwrap();
    f.player.set_queue(vec![a.id, b.id], Some(0)).unwrap();
    f.player.play_track_id(a.id).unwrap();

    f.engine.tick(150.0); // past the end of a
    f.player.tick().unwrap();

    let status = f.player.status().unwrap();
    assert_eq!(
        status.track.as_ref().map(|t| t.id),
        Some(b.id),
        "playback should have moved on to the next queued track"
    );
    assert_eq!(status.queue_index, Some(1));
}

#[test]
fn end_of_the_last_track_stops_rather_than_looping() {
    let f = fixture(&["a.mp3"], Some(100.0));
    let a = f.player.ensure_indexed(&f.dir.join("a.mp3")).unwrap();
    f.player.set_queue(vec![a.id], Some(0)).unwrap();
    f.player.play_track_id(a.id).unwrap();

    f.engine.tick(150.0);
    f.player.tick().unwrap();

    assert!(f.player.status().unwrap().idle, "queue exhausted, so idle");
    assert!(f.engine.calls().iter().any(|c| c == "stop()"));
}

#[test]
fn repeat_one_replays_the_same_track_at_end_of_file() {
    let f = fixture(&["a.mp3", "b.mp3"], Some(100.0));
    let a = f.player.ensure_indexed(&f.dir.join("a.mp3")).unwrap();
    let b = f.player.ensure_indexed(&f.dir.join("b.mp3")).unwrap();
    f.player.set_queue(vec![a.id, b.id], Some(0)).unwrap();
    f.player.set_repeat(RepeatMode::One).unwrap();
    f.player.play_track_id(a.id).unwrap();

    f.engine.tick(150.0);
    f.player.tick().unwrap();

    assert_eq!(f.player.status().unwrap().track.map(|t| t.id), Some(a.id));
    assert_ne!(f.player.status().unwrap().track.map(|t| t.id), Some(b.id));
}

#[test]
fn next_and_previous_walk_the_queue() {
    let f = fixture(&["a.mp3", "b.mp3", "c.mp3"], Some(600.0));
    let ids: Vec<i64> = ["a.mp3", "b.mp3", "c.mp3"]
        .iter()
        .map(|n| f.player.ensure_indexed(&f.dir.join(n)).unwrap().id)
        .collect();
    f.player.set_queue(ids.clone(), Some(0)).unwrap();
    f.player.play_track_id(ids[0]).unwrap();

    assert_eq!(f.player.next().unwrap().map(|t| t.id), Some(ids[1]));
    assert_eq!(f.player.next().unwrap().map(|t| t.id), Some(ids[2]));
    assert_eq!(f.player.next().unwrap(), None, "end of queue");
    assert_eq!(f.player.previous().unwrap().map(|t| t.id), Some(ids[1]));
}

#[test]
fn queue_and_settings_survive_a_new_player_over_the_same_store() {
    let f = fixture(&["a.mp3", "b.mp3"], Some(600.0));
    let a = f.player.ensure_indexed(&f.dir.join("a.mp3")).unwrap();
    let b = f.player.ensure_indexed(&f.dir.join("b.mp3")).unwrap();
    f.player.set_queue(vec![a.id, b.id], Some(1)).unwrap();
    f.player.set_repeat(RepeatMode::All).unwrap();
    f.player.set_shuffle(true).unwrap();

    // A second CLI invocation over the same database.
    let engine: Arc<NullEngine> = Arc::new(NullEngine::new());
    let reborn = Player::new(f.store.clone(), EngineRegistry::new(vec![engine])).unwrap();
    let status = reborn.status().unwrap();
    assert_eq!(status.queue_len, 2);
    assert_eq!(status.queue_index, Some(1));
    assert_eq!(status.repeat, RepeatMode::All);
    assert!(status.shuffle);
}

#[test]
fn staging_a_folder_queues_everything_in_path_order_without_playing() {
    let f = fixture(&["02-second.webm", "01-first.webm"], Some(60.0));
    let staged = f.player.stage_path(&f.dir).unwrap();

    let names: Vec<&str> = staged
        .iter()
        .map(|t| t.path.rsplit('/').next().unwrap())
        .collect();
    assert_eq!(names, ["01-first.webm", "02-second.webm"], "path order");

    let status = f.player.status().unwrap();
    assert_eq!(status.queue_len, 2);
    assert_eq!(status.queue_index, None, "staging must not select a track");
    assert!(status.track.is_none(), "staging must not start playback");
}

#[test]
fn staging_lands_above_the_playing_track_and_survives_a_restart() {
    let f = fixture(&["playing.webm", "staged.webm"], Some(60.0));
    let playing = f.player.play_path(&f.dir.join("playing.webm")).unwrap();
    let staged = f.player.stage_path(&f.dir.join("staged.webm")).unwrap();
    assert_eq!(staged.len(), 1);

    let status = f.player.status().unwrap();
    assert_eq!(status.queue_len, 2);
    assert_eq!(status.queue_index, Some(1), "pushed down by the stage");
    assert_eq!(
        status.track.as_ref().map(|t| t.id),
        Some(playing.id),
        "the stage must not change what is playing"
    );

    // Reordering is persisted, so the arrangement is not lost on relaunch.
    assert!(f.player.queue_move(1, 0).unwrap());
    let (items, index) = f.store.load_queue().unwrap();
    assert_eq!(items, vec![playing.id, staged[0].id]);
    assert_eq!(index, Some(0), "the current index followed its track");
}

#[test]
fn a_loop_confines_playback_and_survives_a_new_player() {
    let f = fixture(&["a.webm", "b.webm", "c.webm", "d.webm"], Some(60.0));
    let ids: Vec<i64> = ["a.webm", "b.webm", "c.webm", "d.webm"]
        .iter()
        .map(|n| f.player.ensure_indexed(&f.dir.join(n)).unwrap().id)
        .collect();
    f.player.set_queue(ids.clone(), Some(0)).unwrap();
    f.player.set_repeat(RepeatMode::All).unwrap();

    // Loop rows 1 and 3; repeat is displaced by it.
    f.player.set_loop(Some(vec![1, 3])).unwrap();
    let status = f.player.status().unwrap();
    assert_eq!(status.loop_positions, vec![1, 3]);
    assert_eq!(status.repeat, RepeatMode::Off);

    assert_eq!(
        f.player.next().unwrap().map(|t| t.id),
        Some(ids[1]),
        "enters the loop"
    );
    assert_eq!(f.player.next().unwrap().map(|t| t.id), Some(ids[3]));
    assert_eq!(
        f.player.next().unwrap().map(|t| t.id),
        Some(ids[1]),
        "and wraps inside it"
    );

    // Scrambling the two looped slots keeps the loop on those slots.
    f.player.queue_scramble(Some(&[1, 3])).unwrap();
    assert_eq!(f.player.status().unwrap().loop_positions, vec![1, 3]);

    let again = Player::new(f.store.clone(), EngineRegistry::new(vec![f.engine.clone()])).unwrap();
    let status = again.status().unwrap();
    assert_eq!(status.loop_positions, vec![1, 3], "the loop is restored");
    assert_eq!(
        status.repeat,
        RepeatMode::Off,
        "without resurrecting repeat"
    );

    again.set_repeat(RepeatMode::One).unwrap();
    assert!(
        again.status().unwrap().loop_positions.is_empty(),
        "repeat lifts the loop"
    );
}

#[test]
fn seek_relative_is_clamped_to_the_file() {
    let f = fixture(&["set.webm"], Some(100.0));
    f.player.play_path(&f.dir.join("set.webm")).unwrap();

    assert_eq!(
        f.player.seek_relative(-30.0).unwrap(),
        0.0,
        "clamped at zero"
    );
    assert_eq!(
        f.player.seek_relative(5000.0).unwrap(),
        100.0,
        "clamped at the end"
    );
}

#[test]
fn video_pop_out_is_refused_for_audio_only_tracks() {
    let f = fixture(&["a.mp3", "set.webm"], Some(600.0));

    f.player.play_path(&f.dir.join("a.mp3")).unwrap();
    assert!(
        f.player.toggle_video().is_err(),
        "an mp3 has nothing to pop out"
    );

    f.player.play_path(&f.dir.join("set.webm")).unwrap();
    assert!(f.player.toggle_video().unwrap(), "video should pop out");
    assert!(!f.player.toggle_video().unwrap(), "and go away again");
}

#[test]
fn duration_discovered_at_playback_time_is_written_back() {
    // Empty files cannot be probed, so the index has no duration to start with.
    let f = fixture(&["set.webm"], Some(5400.0));
    let track = f.player.play_path(&f.dir.join("set.webm")).unwrap();
    assert_eq!(
        track.duration_secs, None,
        "nothing to probe in an empty file"
    );

    f.player.tick().unwrap();
    assert_eq!(
        f.store
            .track_by_id(track.id)
            .unwrap()
            .unwrap()
            .duration_secs,
        Some(5400.0),
        "the engine's duration should be learned on first play"
    );
}

#[test]
fn unsupported_files_are_rejected_before_reaching_an_engine() {
    let f = fixture(&["notes.txt"], Some(600.0));
    assert!(f.player.play_path(&f.dir.join("notes.txt")).is_err());
    assert!(
        f.engine.calls().is_empty(),
        "engine should never have been asked to open it"
    );
}

#[test]
fn quitting_shuts_the_engine_down() {
    let f = fixture(&["a.mp3"], Some(600.0));
    f.player.play_path(&f.dir.join("a.mp3")).unwrap();
    f.player.quit().unwrap();
    assert!(f.engine.was_shutdown());
}
