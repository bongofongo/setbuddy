//! Scanning watched folders into the index.

use std::path::Path;

use walkdir::WalkDir;

use crate::error::Result;
use crate::probe::probe;
use crate::store::{ScannedFile, Store};
use crate::track::is_media_file;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanReport {
    /// Media files seen on disk.
    pub seen: usize,
    /// Files new to the index.
    pub added: usize,
    /// Files whose size or mtime changed, so they were re-probed.
    pub updated: usize,
    /// Files unchanged since the last scan, skipped without probing.
    pub unchanged: usize,
    /// Index entries dropped because the file is gone.
    pub removed: usize,
}

impl ScanReport {
    fn merge(&mut self, other: ScanReport) {
        self.seen += other.seen;
        self.added += other.added;
        self.updated += other.updated;
        self.unchanged += other.unchanged;
        self.removed += other.removed;
    }
}

/// Index every media file under `root`.
///
/// Unchanged files cost a `stat` and an indexed lookup — no probing — so
/// rescanning a folder of two-hour sets is fast enough to do on every launch.
pub fn scan_folder(store: &Store, root: &Path) -> Result<ScanReport> {
    let mut report = ScanReport::default();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        // A missing or unreadable subdirectory should not abort the whole scan.
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() || !is_media_file(entry.path()) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let file = ScannedFile {
            path: entry.path().to_string_lossy().into_owned(),
            size_bytes: meta.len() as i64,
            mtime: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        };
        report.seen += 1;

        if store.unchanged_track_id(&file)?.is_some() {
            report.unchanged += 1;
            continue;
        }
        let existed = store.track_by_path(&file.path)?.is_some();
        store.upsert_track(&file, &probe(entry.path()))?;
        if existed {
            report.updated += 1;
        } else {
            report.added += 1;
        }
    }
    Ok(report)
}

/// Rescan every watched folder and forget files that have disappeared.
pub fn scan_all(store: &Store) -> Result<ScanReport> {
    let mut report = ScanReport::default();
    for folder in store.folders()? {
        report.merge(scan_folder(store, Path::new(&folder))?);
    }
    report.removed = store.forget_missing()?;
    Ok(report)
}
