//! Scanning watched folders, and what a scan owes the queue.
//!
//! The interesting case is not the happy scan — it is the one that finds a file
//! gone. Deleting a track cascades into the stored queue, so the copy the
//! player holds in memory has to move too, or the next save writes a row
//! pointing at nothing.

use std::path::PathBuf;
use std::sync::Arc;

use setbuddy_core::library::scan_folder;
use setbuddy_core::player::Player;
use setbuddy_core::selection::EngineRegistry;
use setbuddy_core::store::Store;
use setbuddy_engine::null::NullEngine;

struct Fixture {
    player: Player,
    store: Arc<Store>,
    dir: PathBuf,
}

fn fixture(files: &[&str]) -> Fixture {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "setbuddy-library-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    // Canonical from the start: the temp directory is behind a symlink on
    // macOS, and the index stores canonical paths.
    let dir = dir.canonicalize().unwrap();
    for name in files {
        std::fs::write(dir.join(name), b"").unwrap();
    }

    let store = Arc::new(Store::in_memory().unwrap());
    let engine = Arc::new(NullEngine::new().with_duration(Some(600.0)));
    let player = Player::new(store.clone(), EngineRegistry::new(vec![engine])).unwrap();
    Fixture { player, store, dir }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn a_scan_indexes_media_and_ignores_everything_else() {
    let f = fixture(&["set.webm", "track.mp3", "notes.txt", "cover.jpg"]);
    let report = scan_folder(&f.store, &f.dir).unwrap();

    assert_eq!(report.seen, 2, "only media files are seen");
    assert_eq!(report.added, 2);
    assert_eq!(report.updated, 0);
    assert_eq!(report.unchanged, 0);
    assert_eq!(f.store.track_count().unwrap(), 2);
}

#[test]
fn a_second_scan_costs_nothing_and_notices_a_changed_file() {
    let f = fixture(&["set.webm", "track.mp3"]);
    scan_folder(&f.store, &f.dir).unwrap();

    let again = scan_folder(&f.store, &f.dir).unwrap();
    assert_eq!(again.unchanged, 2, "nothing was re-probed");
    assert_eq!(again.added, 0);

    // Rewrite one file: a different size is a different file's worth of content.
    std::fs::write(f.dir.join("set.webm"), b"now with bytes in it").unwrap();
    let third = scan_folder(&f.store, &f.dir).unwrap();
    assert_eq!(third.updated, 1);
    assert_eq!(third.unchanged, 1);
    assert_eq!(
        f.store.track_count().unwrap(),
        2,
        "still the same two files"
    );
}

#[test]
fn nested_folders_are_walked_and_a_missing_root_is_not_fatal() {
    let f = fixture(&["top.webm"]);
    std::fs::create_dir_all(f.dir.join("deep/deeper")).unwrap();
    std::fs::write(f.dir.join("deep/deeper/buried.mp3"), b"").unwrap();

    assert_eq!(scan_folder(&f.store, &f.dir).unwrap().seen, 2);
    assert_eq!(
        scan_folder(&f.store, &f.dir.join("not-here")).unwrap().seen,
        0,
        "a folder that has been unmounted must not fail the scan"
    );
}

#[test]
fn rescanning_forgets_files_that_are_gone() {
    let f = fixture(&["keep.webm", "delete-me.webm"]);
    f.store.add_folder(&f.dir.to_string_lossy()).unwrap();
    f.player.rescan_library().unwrap();
    assert_eq!(f.store.track_count().unwrap(), 2);

    std::fs::remove_file(f.dir.join("delete-me.webm")).unwrap();
    let report = f.player.rescan_library().unwrap();

    assert_eq!(report.removed, 1);
    assert_eq!(report.unchanged, 1);
    assert_eq!(f.store.track_count().unwrap(), 1);
}

/// The regression: a rescan that forgets a queued file used to leave its id in
/// the player's queue, and the very next queue operation failed on the foreign
/// key. Everything downstream of it — staging, reordering, even `next` —
/// stayed broken until the app was restarted.
#[test]
fn a_rescan_drops_deleted_files_from_the_queue_and_the_queue_still_works() {
    let f = fixture(&["a.webm", "gone.webm", "c.webm"]);
    f.store.add_folder(&f.dir.to_string_lossy()).unwrap();
    f.player.rescan_library().unwrap();

    let staged = f.player.stage_path(&f.dir).unwrap();
    assert_eq!(staged.len(), 3);
    let surviving: Vec<i64> = staged
        .iter()
        .filter(|t| !t.path.ends_with("gone.webm"))
        .map(|t| t.id)
        .collect();

    std::fs::remove_file(f.dir.join("gone.webm")).unwrap();
    assert_eq!(f.player.rescan_library().unwrap().removed, 1);

    let queue: Vec<i64> = f
        .player
        .queue_tracks()
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(queue, surviving, "the deleted file left the queue");
    assert_eq!(f.player.status().unwrap().queue_len, 2);

    // The operations that used to fail on the dangling foreign key.
    f.player.queue_move(0, 1).unwrap();
    f.player.stage_path(&f.dir.join("a.webm")).unwrap();
    assert_eq!(f.store.load_queue().unwrap().0.len(), 3);
}

/// What is playing keeps playing when something *else* is forgotten.
#[test]
fn a_rescan_does_not_disturb_the_track_that_is_playing() {
    let f = fixture(&["playing.webm", "gone.webm"]);
    f.store.add_folder(&f.dir.to_string_lossy()).unwrap();
    f.player.rescan_library().unwrap();
    f.player.stage_path(&f.dir).unwrap();

    let playing = f.player.play_path(&f.dir.join("playing.webm")).unwrap();
    // `play_path` replaces the queue, so put both rows back with the playing
    // one second — the position that has to survive the removal.
    let gone = f
        .store
        .track_by_path(&f.dir.join("gone.webm").to_string_lossy())
        .unwrap()
        .unwrap();
    f.player
        .set_queue(vec![gone.id, playing.id], Some(1))
        .unwrap();
    assert_eq!(f.player.status().unwrap().queue_index, Some(1));

    std::fs::remove_file(f.dir.join("gone.webm")).unwrap();
    f.player.rescan_library().unwrap();

    let status = f.player.status().unwrap();
    assert_eq!(status.track.map(|t| t.id), Some(playing.id));
    assert_eq!(status.queue_len, 1);
    assert_eq!(
        status.queue_index,
        Some(0),
        "the playing row moved up with the deletion, and is still selected"
    );
}

#[test]
fn pruning_is_a_no_op_when_every_queued_file_is_still_there() {
    let f = fixture(&["a.webm", "b.webm"]);
    f.store.add_folder(&f.dir.to_string_lossy()).unwrap();
    f.player.rescan_library().unwrap();
    f.player.stage_path(&f.dir).unwrap();

    assert!(!f.player.prune_missing_tracks().unwrap());
    assert_eq!(f.player.status().unwrap().queue_len, 2);
}

#[test]
fn a_rescan_with_no_watched_folders_reports_nothing_rather_than_emptying_the_library() {
    let f = fixture(&["a.webm"]);
    let track = f.player.ensure_indexed(&f.dir.join("a.webm")).unwrap();

    let report = f.player.rescan_library().unwrap();
    assert_eq!(report.seen, 0, "no folders are watched");
    assert_eq!(report.removed, 0);
    assert!(
        f.store.track_by_id(track.id).unwrap().is_some(),
        "a file opened directly is still indexed, watched folder or not"
    );
}
