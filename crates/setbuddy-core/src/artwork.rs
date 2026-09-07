//! Cover art and video thumbnails.
//!
//! Two sources, because the two kinds of file carry pictures differently. A
//! tagged audio file usually has cover art embedded, which `lofty` reads without
//! leaving the process. A downloaded set has nothing embedded at all, so the
//! thumbnail is a frame grabbed out of the video with `ffmpeg`.
//!
//! Both are optional. A file with no art and no `ffmpeg` simply has no
//! thumbnail; the UI falls back to an icon. Results are cached on disk keyed by
//! the file's identity, so the cost is paid once per track — including the
//! "there is no art here" answer, which is recorded too rather than being
//! rediscovered by running `ffmpeg` on every menu open.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::paths;
use crate::track::{is_video_extension, Track};

/// Long edge of a generated thumbnail. Sized for listening mode, where the
/// image is the whole panel on a Retina display, rather than for the 52pt row
/// it also feeds. Small sources are never scaled up — this is a ceiling.
const THUMBNAIL_WIDTH: u32 = 1280;

/// JPEG quality handed to ffmpeg, on its 2 (best) to 31 scale. The default sits
/// near the middle and shows it once a frame is drawn 380pt wide.
const THUMBNAIL_QUALITY: &str = "3";

pub fn cache_dir() -> PathBuf {
    paths::state_dir().join("artwork")
}

/// Cached artwork for `track`, generating it on first request.
///
/// Blocking: extracting a frame from a two-hour set takes a moment, so callers
/// on a UI thread should do this off it.
pub fn artwork_for(track: &Track) -> Option<PathBuf> {
    let dir = cache_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return None;
    }

    // Keyed by size and mtime as well as id: replace the file and the old
    // thumbnail stops being used.
    // The width is part of the key so raising it retires the smaller images
    // already on disk instead of serving them forever.
    let stem = format!(
        "{}-{}-{}-w{THUMBNAIL_WIDTH}",
        track.id, track.mtime, track.size_bytes
    );
    let image = dir.join(format!("{stem}.jpg"));
    if image.is_file() {
        return Some(image);
    }
    // A previous attempt found nothing. Don't run ffmpeg again for this file.
    let miss = dir.join(format!("{stem}.none"));
    if miss.is_file() {
        return None;
    }

    let source = Path::new(&track.path);
    if !source.is_file() {
        return None;
    }

    let extracted = if is_video_extension(source) {
        grab_video_frame(source, track.duration_secs, &image)
    } else {
        embedded_cover(source, &image).or_else(|| {
            // Some audio files carry art ffmpeg can see but lofty cannot read,
            // so fall back to the same frame-grab path — an attached picture is
            // a video stream as far as ffmpeg is concerned.
            grab_video_frame(source, None, &image)
        })
    };

    match extracted {
        Some(path) => Some(path),
        None => {
            let _ = std::fs::write(&miss, b"");
            None
        }
    }
}

/// Remove every cached image. Used when the user clears the library.
pub fn clear_cache() -> std::io::Result<()> {
    let dir = cache_dir();
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

fn embedded_cover(source: &Path, destination: &Path) -> Option<PathBuf> {
    use lofty::file::TaggedFileExt;
    use lofty::probe::Probe;

    let tagged = Probe::open(source).ok()?.read().ok()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    let picture = tag.pictures().first()?;
    if picture.data().is_empty() {
        return None;
    }

    // Written through ffmpeg rather than dumped raw so the cache holds one
    // format at one size, whatever the tag happened to contain.
    let raw = destination.with_extension("raw");
    std::fs::write(&raw, picture.data()).ok()?;
    let converted = transcode(&raw, destination, None);
    let _ = std::fs::remove_file(&raw);
    converted
}

/// Pick a frame that represents the file.
///
/// A tenth of the way in avoids the black frames and title cards that open most
/// sets, and is capped so seeking stays quick on a long one.
fn frame_timestamp(duration_secs: Option<f64>) -> f64 {
    match duration_secs {
        Some(duration) if duration > 120.0 => (duration * 0.1).min(600.0),
        Some(duration) if duration > 20.0 => duration * 0.25,
        _ => 1.0,
    }
}

fn grab_video_frame(source: &Path, duration: Option<f64>, destination: &Path) -> Option<PathBuf> {
    transcode(source, destination, Some(frame_timestamp(duration)))
}

fn transcode(source: &Path, destination: &Path, seek_to: Option<f64>) -> Option<PathBuf> {
    let mut command = Command::new("ffmpeg");
    command.args(["-v", "error", "-nostdin", "-y"]);
    if let Some(seconds) = seek_to {
        // Before -i, so ffmpeg seeks rather than decoding up to the timestamp.
        command.args(["-ss", &format!("{seconds:.3}")]);
    }
    command
        .arg("-i")
        .arg(source)
        .args([
            "-frames:v",
            "1",
            "-q:v",
            THUMBNAIL_QUALITY,
            "-vf",
            // `min` keeps a cover smaller than the ceiling at its own size:
            // upscaling here would cost bytes without adding detail. -2 keeps
            // the height even, which the JPEG encoder requires.
            &format!("scale=w='min({THUMBNAIL_WIDTH},iw)':h=-2"),
        ])
        .arg(destination);

    let output = command.output().ok()?;
    // ffmpeg reports success even when a seek past the end produced no frame,
    // so trust the file rather than the exit status.
    if output.status.success() && destination.is_file() {
        if std::fs::metadata(destination).map(|m| m.len()).unwrap_or(0) > 0 {
            return Some(destination.to_path_buf());
        }
    }
    let _ = std::fs::remove_file(destination);

    // A seek past the end yields nothing; one retry from the start is worth it
    // for a file whose duration we guessed wrong.
    if seek_to.is_some_and(|s| s > 0.0) {
        return transcode(source, destination, Some(0.0));
    }
    None
}
