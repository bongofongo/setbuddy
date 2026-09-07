//! `probe::details` against real files: the shape the details panel relies on.

use std::path::{Path, PathBuf};

use setbuddy_core::probe::{details, ffprobe_available};

fn asset(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../setbuddy-mpv/tests/assets")
        .join(name)
        .canonicalize()
        .expect("fixture must exist")
}

fn value_of<'a>(rows: &'a [(String, String)], label: &str) -> Option<&'a str> {
    rows.iter()
        .find(|(l, _)| l == label)
        .map(|(_, v)| v.as_str())
}

#[test]
fn a_video_file_lists_its_container_and_both_streams() {
    if !ffprobe_available() {
        return;
    }
    let rows = details(&asset("long.webm"));
    assert!(value_of(&rows, "Container").is_some(), "{rows:?}");
    let video = value_of(&rows, "Video 1").expect("a video stream row");
    assert!(
        video.contains("vp9") || video.contains("vp8"),
        "codec named: {video}"
    );
    assert!(video.contains('×'), "resolution shown: {video}");
    assert!(video.contains("fps"), "frame rate shown: {video}");
    let audio = value_of(&rows, "Audio 1").expect("an audio stream row");
    assert!(audio.contains("Hz"), "sample rate shown: {audio}");
}

#[test]
fn tags_are_flattened_with_one_casing() {
    if !ffprobe_available() {
        return;
    }
    let rows = details(&asset("tiny_art.mp3"));
    assert_eq!(value_of(&rows, "Title"), Some("Tone With Art"), "{rows:?}");
    assert_eq!(value_of(&rows, "Artist"), Some("Test Artist"));
    assert!(
        rows.iter()
            .all(|(l, _)| !l.contains('_') && !l.chars().all(char::is_uppercase)),
        "labels are readable: {rows:?}"
    );
}

#[test]
fn an_unreadable_file_yields_nothing_rather_than_failing() {
    assert!(details(Path::new("/definitely/not/here.webm")).is_empty());
}
