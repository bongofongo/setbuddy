//! Artwork extraction against real media.
//!
//! These need `ffmpeg` on PATH, which arrives with Homebrew's mpv. When it is
//! absent the module is supposed to degrade to "no thumbnail" rather than fail,
//! so the tests assert that instead of skipping silently.

use std::path::{Path, PathBuf};

use setbuddy_core::artwork::{artwork_for, cache_dir};
use setbuddy_core::track::Track;

fn ffmpeg_available() -> bool {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|dir| dir.join("ffmpeg").is_file()))
        .unwrap_or(false)
}

fn asset(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../setbuddy-mpv/tests/assets")
        .join(name)
        .canonicalize()
        .expect("fixture must exist")
}

/// One state directory for the whole test binary, so the cache is isolated.
fn use_temp_state_dir() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let dir =
            std::env::temp_dir().join(format!("setbuddy-artwork-tests-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("SETBUDDY_STATE_DIR", &dir);
    });
}

fn track(id: i64, name: &str, duration: Option<f64>) -> Track {
    let path = asset(name);
    let meta = std::fs::metadata(&path).unwrap();
    Track {
        id,
        path: path.to_string_lossy().into_owned(),
        size_bytes: meta.len() as i64,
        mtime: 1_700_000_000,
        title: None,
        artist: None,
        album: None,
        duration_secs: duration,
        has_video: name.ends_with(".webm"),
        added_at: 0,
        last_played_at: None,
    }
}

#[test]
fn grabs_a_frame_from_a_video_file() {
    use_temp_state_dir();
    let track = track(1, "long.webm", Some(200.0));
    let art = artwork_for(&track);

    if !ffmpeg_available() {
        assert!(art.is_none(), "without ffmpeg there is simply no thumbnail");
        return;
    }

    let art = art.expect("a video file should yield a thumbnail");
    assert!(art.is_file());
    let size = std::fs::metadata(&art).unwrap().len();
    assert!(size > 1000, "thumbnail looks empty at {size} bytes");
    assert!(art.starts_with(cache_dir()), "should live in the cache");
}

#[test]
fn extracts_embedded_cover_art_from_audio() {
    use_temp_state_dir();
    if !ffmpeg_available() {
        return;
    }
    let track = track(2, "tiny_art.mp3", Some(6.0));
    let art = artwork_for(&track).expect("the fixture has an attached picture");
    assert!(std::fs::metadata(&art).unwrap().len() > 500);
}

#[test]
fn audio_without_art_is_remembered_as_having_none() {
    use_temp_state_dir();
    let track = track(3, "tiny.wav", Some(3.0));
    assert!(artwork_for(&track).is_none(), "a bare wav has no art");

    // The negative answer is cached, so a second look does no work. The rest of
    // the file name is the cache's business — matching on the prefix keeps this
    // from breaking every time the thumbnail size changes.
    let prefix = format!("{}-{}-{}", track.id, track.mtime, track.size_bytes);
    let marker = std::fs::read_dir(cache_dir())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .any(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.starts_with(&prefix) && name.ends_with(".none")
        });
    assert!(marker, "expected a negative-cache marker");
    assert!(artwork_for(&track).is_none());
}

#[test]
fn artwork_is_generated_once_and_reused() {
    use_temp_state_dir();
    if !ffmpeg_available() {
        return;
    }
    let track = track(4, "long.webm", Some(200.0));
    let first = artwork_for(&track).unwrap();
    let stamp = std::fs::metadata(&first).unwrap().modified().unwrap();

    std::thread::sleep(std::time::Duration::from_millis(1100));
    let second = artwork_for(&track).unwrap();

    assert_eq!(first, second);
    assert_eq!(
        stamp,
        std::fs::metadata(&second).unwrap().modified().unwrap(),
        "a cached thumbnail should be reused, not regenerated"
    );
}

#[test]
fn a_changed_file_gets_a_new_thumbnail() {
    use_temp_state_dir();
    if !ffmpeg_available() {
        return;
    }
    let mut track = track(5, "long.webm", Some(200.0));
    let first = artwork_for(&track).unwrap();

    // Same track id, different file identity — as if it were re-downloaded.
    track.mtime += 1;
    let second = artwork_for(&track).unwrap();
    assert_ne!(first, second, "cache key must include the file's identity");
}

#[test]
fn a_missing_file_yields_no_artwork() {
    use_temp_state_dir();
    let mut track = track(6, "tiny.wav", Some(3.0));
    track.path = "/nowhere/gone.wav".into();
    assert!(artwork_for(&track).is_none());
}
