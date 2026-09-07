//! The durable layer: what SQLite is asked, and what it must never be asked.
//!
//! These are the rules the whole app leans on without restating them — that a
//! search for `100%` is a search for `100%`, that saving a queue cannot fail
//! because a file was deleted underneath it, and that forgetting a track takes
//! its resume position with it.

use setbuddy_core::probe::Probed;
use setbuddy_core::store::{ScannedFile, Store};

fn store() -> Store {
    Store::in_memory().unwrap()
}

/// A scratch directory of real, empty files. `forget_missing` asks the disk, so
/// a test about it needs paths that actually exist.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "setbuddy-store-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    /// Create `name` on disk and index it.
    fn real(&self, store: &Store, name: &str) -> i64 {
        let path = self.0.join(name);
        std::fs::write(&path, b"").unwrap();
        add(store, &path.to_string_lossy())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn file(path: &str) -> ScannedFile {
    ScannedFile {
        path: path.into(),
        size_bytes: 1,
        mtime: 1,
    }
}

fn add(store: &Store, path: &str) -> i64 {
    store.upsert_track(&file(path), &Probed::default()).unwrap()
}

fn tagged(store: &Store, path: &str, title: &str, artist: &str) -> i64 {
    store
        .upsert_track(
            &file(path),
            &Probed {
                title: Some(title.into()),
                artist: Some(artist.into()),
                ..Probed::default()
            },
        )
        .unwrap()
}

// ---- search ---------------------------------------------------------------

#[test]
fn search_matches_path_and_every_tag() {
    let s = store();
    tagged(&s, "/sets/one.webm", "Live at Dekmantel", "Palms Trax");
    add(&s, "/music/other.mp3");

    for query in ["Dekmantel", "palms", "sets/one"] {
        let hits = s.search(query, 10).unwrap();
        assert_eq!(hits.len(), 1, "\"{query}\" should find exactly one track");
        assert_eq!(hits[0].path, "/sets/one.webm");
    }
    assert!(s.search("nothing here", 10).unwrap().is_empty());
}

/// `LIKE` wildcards in a query are the user's literal characters. A search box
/// that quietly turns `_` into "any character" returns tracks nobody asked for.
#[test]
fn search_treats_like_wildcards_as_ordinary_characters() {
    let s = store();
    let underscore = add(&s, "/sets/Boiler_Room.webm");
    add(&s, "/sets/BoilerXRoom.webm");
    let percent = add(&s, "/sets/100% Silk.webm");
    add(&s, "/sets/decoy.webm");

    let hits = s.search("Boiler_Room", 10).unwrap();
    assert_eq!(
        hits.iter().map(|t| t.id).collect::<Vec<_>>(),
        vec![underscore],
        "an underscore must not match any character"
    );

    let hits = s.search("100%", 10).unwrap();
    assert_eq!(
        hits.iter().map(|t| t.id).collect::<Vec<_>>(),
        vec![percent],
        "a percent must not match the whole library"
    );
}

/// A backslash is the escape character for the pattern, so an unescaped one in
/// a query leaves a dangling escape — which SQLite reads as something else
/// entirely, and which no user typing a Windows-ish path intended.
#[test]
fn search_survives_a_backslash_in_the_query() {
    let s = store();
    let literal = add(&s, "/sets/back\\slash.webm");
    add(&s, "/sets/plain.webm");

    let hits = s.search("back\\slash", 10).unwrap();
    assert_eq!(hits.iter().map(|t| t.id).collect::<Vec<_>>(), vec![literal]);
    // A trailing escape is the pathological case: it must not error or match
    // everything.
    assert!(s.search("\\", 10).unwrap().len() <= 1);
}

#[test]
fn search_honours_its_limit_and_prefers_recently_played() {
    let s = store();
    let old = add(&s, "/sets/a-set.webm");
    let recent = add(&s, "/sets/b-set.webm");
    add(&s, "/sets/c-set.webm");
    s.mark_played(recent).unwrap();

    let hits = s.search("set", 2).unwrap();
    assert_eq!(hits.len(), 2, "the limit is respected");
    assert_eq!(hits[0].id, recent, "played tracks come first");
    assert!(hits.iter().all(|t| t.id != old) || hits[1].id == old);
}

/// A folder called `100%_sets` must not stage the entire library.
#[test]
fn tracks_under_escapes_wildcards_in_the_folder_name() {
    let s = store();
    let inside = add(&s, "/lib/100%_sets/one.webm");
    add(&s, "/lib/1005Xsets/two.webm");
    add(&s, "/lib/elsewhere/three.webm");

    let found = s.tracks_under("/lib/100%_sets").unwrap();
    assert_eq!(found.iter().map(|t| t.id).collect::<Vec<_>>(), vec![inside]);
}

#[test]
fn tracks_under_ignores_a_trailing_slash_and_returns_path_order() {
    let s = store();
    add(&s, "/lib/sets/02-b.webm");
    add(&s, "/lib/sets/01-a.webm");
    add(&s, "/lib/setsX/other.webm");

    for folder in ["/lib/sets", "/lib/sets/"] {
        let names: Vec<String> = s
            .tracks_under(folder)
            .unwrap()
            .into_iter()
            .map(|t| t.path)
            .collect();
        assert_eq!(
            names,
            ["/lib/sets/01-a.webm", "/lib/sets/02-b.webm"],
            "{folder} must list its own files, in path order"
        );
    }
}

// ---- indexing -------------------------------------------------------------

/// Re-probing a file that has changed must not erase tags an earlier, better
/// probe found — an ffprobe that has since been uninstalled reports nothing.
#[test]
fn a_failed_reprobe_never_erases_what_an_earlier_one_learned() {
    let s = store();
    let id = s
        .upsert_track(
            &file("/sets/one.webm"),
            &Probed {
                title: Some("Live Set".into()),
                duration_secs: Some(7200.0),
                has_video: true,
                ..Probed::default()
            },
        )
        .unwrap();

    let changed = ScannedFile {
        path: "/sets/one.webm".into(),
        size_bytes: 999,
        mtime: 999,
    };
    let same_id = s
        .upsert_track(
            &changed,
            &Probed {
                has_video: true,
                ..Probed::default()
            },
        )
        .unwrap();

    assert_eq!(same_id, id, "the same file keeps its id");
    let track = s.track_by_id(id).unwrap().unwrap();
    assert_eq!(track.title.as_deref(), Some("Live Set"));
    assert_eq!(track.duration_secs, Some(7200.0));
    assert_eq!(track.size_bytes, 999, "the file's identity is updated");
    assert_eq!(track.mtime, 999);
}

#[test]
fn unchanged_files_are_recognised_by_size_and_mtime() {
    let s = store();
    let id = add(&s, "/sets/one.webm");
    assert_eq!(
        s.unchanged_track_id(&file("/sets/one.webm")).unwrap(),
        Some(id)
    );

    let touched = ScannedFile {
        mtime: 2,
        ..file("/sets/one.webm")
    };
    assert_eq!(
        s.unchanged_track_id(&touched).unwrap(),
        None,
        "a changed mtime means re-probe"
    );
    assert_eq!(
        s.unchanged_track_id(&file("/sets/absent.webm")).unwrap(),
        None
    );
}

#[test]
fn a_duration_learned_at_playback_time_is_kept_but_nonsense_is_not() {
    let s = store();
    let id = add(&s, "/sets/one.webm");
    s.set_duration(id, 4364.0).unwrap();
    assert_eq!(
        s.track_by_id(id).unwrap().unwrap().duration_secs,
        Some(4364.0)
    );

    for bad in [0.0, -5.0, f64::NAN, f64::INFINITY] {
        s.set_duration(id, bad).unwrap();
        assert_eq!(
            s.track_by_id(id).unwrap().unwrap().duration_secs,
            Some(4364.0),
            "{bad} must not overwrite a real duration"
        );
    }
}

// ---- resume ---------------------------------------------------------------

#[test]
fn resume_positions_round_trip_and_are_cleared_with_their_track() {
    let s = store();
    let id = add(&s, "/sets/one.webm");
    assert_eq!(s.resume_for(id).unwrap(), None);

    s.set_resume(id, 90.0).unwrap();
    s.set_resume(id, 120.0).unwrap();
    assert_eq!(s.resume_for(id).unwrap(), Some(120.0), "the latest wins");

    s.clear_resume(id).unwrap();
    assert_eq!(s.resume_for(id).unwrap(), None);
}

#[test]
fn forgetting_a_missing_file_takes_its_resume_position_with_it() {
    let s = store();
    let gone = add(&s, "/definitely/not/here.webm");
    s.set_resume(gone, 500.0).unwrap();

    assert_eq!(s.forget_missing().unwrap(), 1);
    assert!(s.track_by_id(gone).unwrap().is_none());
    assert_eq!(
        s.resume_for(gone).unwrap(),
        None,
        "an orphaned resume row would fire on whatever reused the id"
    );
}

// ---- the queue ------------------------------------------------------------

#[test]
fn a_saved_queue_loads_back_with_the_same_rows_and_index() {
    let s = store();
    let ids: Vec<i64> = ["a", "b", "c"]
        .iter()
        .map(|n| add(&s, &format!("/sets/{n}.webm")))
        .collect();

    s.save_queue(&ids, Some(1)).unwrap();
    assert_eq!(s.load_queue().unwrap(), (ids.clone(), Some(1)));

    s.save_queue(&ids, None).unwrap();
    assert_eq!(s.load_queue().unwrap(), (ids.clone(), None));

    s.save_queue(&[], None).unwrap();
    assert_eq!(s.load_queue().unwrap(), (Vec::new(), None));
}

/// The regression this exists for: a scan forgets a deleted file, SQLite
/// cascades the deletion into `queue_items`, and the caller still holds the
/// old id. Writing it back would violate the foreign key and fail the save —
/// which would make every later queue operation error until a restart.
#[test]
fn saving_a_queue_that_names_a_forgotten_track_still_succeeds() {
    let s = store();
    let scratch = Scratch::new();
    let kept = scratch.real(&s, "kept.webm");
    let gone = add(&s, "/definitely/not/here.webm");
    let also_kept = scratch.real(&s, "also-kept.webm");

    s.save_queue(&[kept, gone, also_kept], Some(2)).unwrap();
    assert_eq!(s.forget_missing().unwrap(), 1);

    // The caller's copy of the queue still lists the forgotten track.
    s.save_queue(&[kept, gone, also_kept], Some(2))
        .expect("a deleted file must not break saving the queue");

    let (items, index) = s.load_queue().unwrap();
    assert_eq!(items, vec![kept, also_kept], "the ghost row is not written");
    assert_eq!(
        index,
        Some(1),
        "and the current row follows its track to its new position"
    );
}

#[test]
fn a_current_row_that_was_forgotten_selects_nothing() {
    let s = store();
    let scratch = Scratch::new();
    let kept = scratch.real(&s, "kept.webm");
    let gone = add(&s, "/definitely/not/here.webm");
    s.forget_missing().unwrap();

    s.save_queue(&[kept, gone], Some(1)).unwrap();
    assert_eq!(s.load_queue().unwrap(), (vec![kept], None));
}

#[test]
fn existing_track_ids_reports_only_what_is_still_indexed() {
    let s = store();
    let scratch = Scratch::new();
    let kept = scratch.real(&s, "kept.webm");
    let gone = add(&s, "/definitely/not/here.webm");
    s.forget_missing().unwrap();

    let alive = s.existing_track_ids(&[kept, gone, 9999]).unwrap();
    assert!(alive.contains(&kept));
    assert!(!alive.contains(&gone));
    assert!(!alive.contains(&9999));
    assert!(s.existing_track_ids(&[]).unwrap().is_empty());
}

/// `queue_index` is stored as text in the settings table, so it has to survive
/// whatever is in there — including a value left by an older build.
#[test]
fn a_queue_index_past_the_end_is_ignored_rather_than_trusted() {
    let s = store();
    let id = add(&s, "/sets/one.webm");
    s.save_queue(&[id], Some(0)).unwrap();
    s.set_setting("queue_index", "17").unwrap();
    assert_eq!(s.load_queue().unwrap(), (vec![id], None));

    s.set_setting("queue_index", "not a number").unwrap();
    assert_eq!(s.load_queue().unwrap(), (vec![id], None));
}

// ---- folders and settings -------------------------------------------------

#[test]
fn watching_a_folder_twice_watches_it_once() {
    let s = store();
    s.add_folder("/lib/sets").unwrap();
    s.add_folder("/lib/sets").unwrap();
    s.add_folder("/lib/music").unwrap();
    assert_eq!(s.folders().unwrap(), ["/lib/music", "/lib/sets"]);

    assert!(s.remove_folder("/lib/sets").unwrap());
    assert!(
        !s.remove_folder("/lib/sets").unwrap(),
        "removing what is not watched reports so rather than failing"
    );
    assert_eq!(s.folders().unwrap(), ["/lib/music"]);
}

#[test]
fn settings_are_last_write_wins() {
    let s = store();
    assert_eq!(s.setting("repeat").unwrap(), None);
    s.set_setting("repeat", "all").unwrap();
    s.set_setting("repeat", "one").unwrap();
    assert_eq!(s.setting("repeat").unwrap().as_deref(), Some("one"));
    s.set_setting("repeat", "").unwrap();
    assert_eq!(
        s.setting("repeat").unwrap().as_deref(),
        Some(""),
        "an empty value is a value, not an absence"
    );
}

#[test]
fn recents_lists_only_what_has_been_played_newest_first() {
    let s = store();
    let a = add(&s, "/sets/a.webm");
    let b = add(&s, "/sets/b.webm");
    add(&s, "/sets/never.webm");

    s.mark_played(a).unwrap();
    s.mark_played(b).unwrap();

    let recents = s.recents(10).unwrap();
    assert_eq!(recents.len(), 2, "unplayed tracks are not recent");
    assert!(recents.iter().all(|t| t.last_played_at.is_some()));
    assert!(s.recents(0).unwrap().is_empty());
}
