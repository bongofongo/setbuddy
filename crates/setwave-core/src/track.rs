//! What the library knows about one file.

/// Extensions the scanner will index.
///
/// Core keeps its own list rather than asking an engine: the library is a
/// durable record of what you own, and must not change shape because a
/// different playback engine was selected today.
pub const MEDIA_EXTENSIONS: &[&str] = &[
    // Sets and video
    "webm", "mkv", "mp4", "m4v", "mov", "avi", "flv", "ts", //
    // Music
    "mp3", "m4a", "wav", "flac", "opus", "ogg", "oga", "aac", "alac", "aiff", "aif", "wma", "wv",
    "ape",
];

/// Containers that usually carry video. Used as a prior before probing, so an
/// unprobeable file still lands in a sensible place.
pub const VIDEO_EXTENSIONS: &[&str] = &["webm", "mkv", "mp4", "m4v", "mov", "avi", "flv", "ts"];

pub fn is_media_file(path: &std::path::Path) -> bool {
    extension_of(path)
        .map(|ext| MEDIA_EXTENSIONS.contains(&ext.as_str()))
        .unwrap_or(false)
}

pub fn is_video_extension(path: &std::path::Path) -> bool {
    extension_of(path)
        .map(|ext| VIDEO_EXTENSIONS.contains(&ext.as_str()))
        .unwrap_or(false)
}

pub fn extension_of(path: &std::path::Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
}

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub id: i64,
    pub path: String,
    /// Size and modified time form the file's identity. Cheap to compare, so a
    /// rescan of a folder of 3 GB sets does not re-probe anything unchanged.
    pub size_bytes: i64,
    pub mtime: i64,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_secs: Option<f64>,
    pub has_video: bool,
    pub added_at: i64,
    pub last_played_at: Option<i64>,
}

impl Track {
    /// Best available name. Falls back to the filename, which for a downloaded
    /// set is usually the most informative thing there is.
    pub fn display_title(&self) -> String {
        if let Some(title) = self.title.as_ref().filter(|t| !t.trim().is_empty()) {
            return title.clone();
        }
        std::path::Path::new(&self.path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&self.path)
            .to_string()
    }

    /// "Artist — Title" when both are known, otherwise just the title.
    pub fn display_label(&self) -> String {
        match self.artist.as_ref().filter(|a| !a.trim().is_empty()) {
            Some(artist) => format!("{artist} — {}", self.display_title()),
            None => self.display_title(),
        }
    }
}

/// Seconds rendered as `H:MM:SS` or `M:SS` — a two-hour set needs the hours.
pub fn format_duration(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "--:--".into();
    }
    let total = secs.round() as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn track() -> Track {
        Track {
            id: 1,
            path: "/sets/Palms Trax | Boiler Room Berlin.webm".into(),
            size_bytes: 0,
            mtime: 0,
            title: None,
            artist: None,
            album: None,
            duration_secs: None,
            has_video: true,
            added_at: 0,
            last_played_at: None,
        }
    }

    #[test]
    fn falls_back_to_the_filename() {
        assert_eq!(track().display_title(), "Palms Trax | Boiler Room Berlin");
    }

    #[test]
    fn blank_tags_do_not_win_over_the_filename() {
        let mut t = track();
        t.title = Some("   ".into());
        assert_eq!(t.display_title(), "Palms Trax | Boiler Room Berlin");
    }

    #[test]
    fn labels_include_the_artist_when_known() {
        let mut t = track();
        t.title = Some("Live Set".into());
        t.artist = Some("Palms Trax".into());
        assert_eq!(t.display_label(), "Palms Trax — Live Set");
    }

    #[test]
    fn durations_show_hours_for_long_sets() {
        assert_eq!(format_duration(72.0), "1:12");
        assert_eq!(format_duration(4364.0), "1:12:44");
        assert_eq!(format_duration(f64::NAN), "--:--");
    }

    #[test]
    fn recognises_media_by_extension_case_insensitively() {
        assert!(is_media_file(Path::new("/a/set.WebM")));
        assert!(is_media_file(Path::new("/a/track.mp3")));
        assert!(!is_media_file(Path::new("/a/notes.txt")));
        assert!(is_video_extension(Path::new("/a/set.mkv")));
        assert!(!is_video_extension(Path::new("/a/track.flac")));
    }
}
