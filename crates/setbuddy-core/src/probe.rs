//! Reading metadata out of a file.
//!
//! Two sources, in order of cost. `lofty` is pure Rust and handles tagged audio
//! without leaving the process. Video containers — the `.webm` and `.mkv` a set
//! arrives in — it cannot read, so those fall to `ffprobe` when it is present.
//!
//! Neither is required. A file that cannot be probed is still indexed and still
//! plays; it just shows its filename and learns its duration on first play.
//! That matters because `ffprobe` arrives as a dependency of Homebrew's mpv
//! rather than anything Setbuddy installs, so it may simply not be there.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::track::is_video_extension;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Probed {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_secs: Option<f64>,
    pub has_video: bool,
}

impl Probed {
    /// True when nothing useful was learned, so callers can retry later.
    pub fn is_bare(&self) -> bool {
        self.duration_secs.is_none() && self.title.is_none()
    }
}

/// Best-effort metadata. Never fails: an unreadable file yields an empty result
/// with `has_video` guessed from the extension.
pub fn probe(path: &Path) -> Probed {
    let mut probed = Probed {
        has_video: is_video_extension(path),
        ..Probed::default()
    };

    if !is_video_extension(path) {
        if let Some(tags) = probe_with_lofty(path) {
            probed.title = tags.title;
            probed.artist = tags.artist;
            probed.album = tags.album;
            probed.duration_secs = tags.duration_secs;
            // An audio file with cover art is not something to pop out.
            probed.has_video = false;
        }
    }

    if probed.duration_secs.is_none() {
        if let Some(ff) = probe_with_ffprobe(path) {
            probed.duration_secs = ff.duration_secs.or(probed.duration_secs);
            probed.title = probed.title.or(ff.title);
            probed.artist = probed.artist.or(ff.artist);
            probed.album = probed.album.or(ff.album);
            probed.has_video = ff.has_video;
        }
    }
    probed
}

fn non_empty(s: impl AsRef<str>) -> Option<String> {
    let trimmed = s.as_ref().trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn probe_with_lofty(path: &Path) -> Option<Probed> {
    use lofty::file::{AudioFile, TaggedFileExt};
    use lofty::probe::Probe;
    use lofty::tag::Accessor;

    let tagged = Probe::open(path).ok()?.read().ok()?;
    let duration = tagged.properties().duration();
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());

    Some(Probed {
        title: tag.and_then(|t| t.title()).and_then(non_empty),
        artist: tag.and_then(|t| t.artist()).and_then(non_empty),
        album: tag.and_then(|t| t.album()).and_then(non_empty),
        duration_secs: (duration > Duration::ZERO).then(|| duration.as_secs_f64()),
        has_video: false,
    })
}

/// Whether `ffprobe` is on PATH. Cached would be nicer, but a scan calls this
/// once per unprobed file and the cost is a `stat`.
pub fn ffprobe_available() -> bool {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|dir| dir.join("ffprobe").is_file()))
        .unwrap_or(false)
}

fn probe_with_ffprobe(path: &Path) -> Option<Probed> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration:format_tags=title,artist,album:stream=codec_type,disposition",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let format = json.get("format");
    let tags = format.and_then(|f| f.get("tags"));
    let tag = |key: &str| {
        tags.and_then(|t| t.get(key))
            .and_then(|v| v.as_str())
            .and_then(non_empty)
    };

    let has_video = json
        .get("streams")
        .and_then(|s| s.as_array())
        .map(|streams| {
            streams.iter().any(|s| {
                s.get("codec_type").and_then(|v| v.as_str()) == Some("video")
                    // Embedded cover art is a video stream with the attached_pic
                    // disposition; it is artwork, not something to pop out.
                    && s.get("disposition")
                        .and_then(|d| d.get("attached_pic"))
                        .and_then(|v| v.as_i64())
                        != Some(1)
            })
        })
        .unwrap_or(false);

    Some(Probed {
        title: tag("title"),
        artist: tag("artist"),
        album: tag("album"),
        duration_secs: format
            .and_then(|f| f.get("duration"))
            .and_then(|v| v.as_str())
            .and_then(|d| d.parse::<f64>().ok())
            .filter(|d| d.is_finite() && *d > 0.0),
        has_video,
    })
}

/// Everything `ffprobe` will say about a file, flattened to label/value pairs
/// for display: the container and its tags, then each stream.
///
/// Deliberately not stored — it is asked for when someone opens the details
/// panel, and only then. Empty when `ffprobe` is missing or the file is not
/// readable; the caller still has the indexed fields.
pub fn details(path: &Path) -> Vec<(String, String)> {
    let Ok(output) = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_format",
            "-show_streams",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return Vec::new();
    };

    let text = |v: &serde_json::Value| -> Option<String> {
        match v {
            serde_json::Value::String(s) => non_empty(s),
            serde_json::Value::Number(n) => Some(n.to_string()),
            serde_json::Value::Bool(b) => Some(b.to_string()),
            _ => None,
        }
    };
    let mut out = Vec::new();
    let mut push = |label: &str, value: Option<String>| {
        if let Some(value) = value {
            out.push((label.to_string(), value));
        }
    };

    if let Some(format) = json.get("format") {
        push("Container", format.get("format_long_name").and_then(text));
        push(
            "Bit rate",
            format
                .get("bit_rate")
                .and_then(text)
                .map(|b| format_bitrate(&b)),
        );
        if let Some(tags) = format.get("tags").and_then(|t| t.as_object()) {
            let mut keys: Vec<&String> = tags.keys().collect();
            keys.sort_unstable_by_key(|k| k.to_lowercase());
            for key in keys {
                // Tag names arrive as written by whatever wrote the file:
                // `TITLE`, `title`, `Title`. One shape for all of them.
                push(&titlecase(key), tags.get(key).and_then(text));
            }
        }
    }

    if let Some(streams) = json.get("streams").and_then(|s| s.as_array()) {
        // Numbered within their kind — "Video 1", "Audio 1" — since that is
        // how anyone thinks of them; the file's own stream order is not.
        let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for stream in streams {
            let kind = stream
                .get("codec_type")
                .and_then(text)
                .unwrap_or_else(|| "stream".into());
            let nth = seen
                .entry(kind.clone())
                .and_modify(|n| *n += 1)
                .or_insert(1);
            let label = format!("{} {}", titlecase(&kind), nth);
            let mut parts: Vec<String> = Vec::new();
            if let Some(codec) = stream.get("codec_name").and_then(text) {
                parts.push(codec);
            }
            if let (Some(w), Some(h)) = (
                stream.get("width").and_then(text),
                stream.get("height").and_then(text),
            ) {
                parts.push(format!("{w}×{h}"));
            }
            if let Some(fps) = stream
                .get("avg_frame_rate")
                .and_then(text)
                .and_then(|r| frame_rate(&r))
            {
                parts.push(format!("{fps} fps"));
            }
            if let Some(rate) = stream.get("sample_rate").and_then(text) {
                parts.push(format!("{rate} Hz"));
            }
            if let Some(layout) = stream.get("channel_layout").and_then(text).or_else(|| {
                stream
                    .get("channels")
                    .and_then(text)
                    .map(|c| format!("{c} ch"))
            }) {
                parts.push(layout);
            }
            if let Some(bits) = stream.get("bit_rate").and_then(text) {
                parts.push(format_bitrate(&bits));
            }
            push(&label, non_empty(parts.join(" · ")));
        }
    }
    out
}

fn titlecase(key: &str) -> String {
    let mut chars = key
        .replace('_', " ")
        .to_lowercase()
        .chars()
        .collect::<Vec<_>>();
    if let Some(first) = chars.first_mut() {
        *first = first.to_ascii_uppercase();
    }
    chars.into_iter().collect()
}

fn format_bitrate(bits_per_sec: &str) -> String {
    match bits_per_sec.parse::<f64>() {
        Ok(b) if b >= 1_000_000.0 => format!("{:.1} Mb/s", b / 1_000_000.0),
        Ok(b) if b >= 1_000.0 => format!("{:.0} kb/s", b / 1_000.0),
        Ok(b) => format!("{b:.0} b/s"),
        Err(_) => bits_per_sec.to_string(),
    }
}

/// ffprobe writes frame rates as a ratio, `30000/1001`. Shown as a number.
fn frame_rate(ratio: &str) -> Option<String> {
    let (num, den) = ratio.split_once('/')?;
    let (num, den): (f64, f64) = (num.parse().ok()?, den.parse().ok()?);
    if den == 0.0 || num == 0.0 {
        return None;
    }
    let fps = num / den;
    Some(if (fps - fps.round()).abs() < 0.01 {
        format!("{}", fps.round() as i64)
    } else {
        format!("{fps:.2}")
    })
}
