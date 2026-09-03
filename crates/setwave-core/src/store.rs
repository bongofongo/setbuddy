//! Durable state: the library index, resume positions, watched folders, and the
//! queue.
//!
//! The queue lives here rather than only in memory because the CLI is a series
//! of short-lived processes — `setwave queue add`, then `setwave next` a minute
//! later — and both must see the same queue.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::error::{CoreError, Result};
use crate::probe::Probed;
use crate::track::Track;

const SCHEMA_VERSION: i32 = 1;

pub struct Store {
    conn: Mutex<Connection>,
}

/// A file as the scanner found it on disk, before any metadata is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedFile {
    pub path: String,
    pub size_bytes: i64,
    pub mtime: i64,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        Self::from_connection(conn)
    }

    /// An ephemeral store, for tests.
    pub fn in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        // WAL keeps a scan from blocking the menu bar's reads.
        let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=NORMAL;")?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.conn();
        let version: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version >= SCHEMA_VERSION {
            return Ok(());
        }
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS tracks (
                id             INTEGER PRIMARY KEY,
                path           TEXT    NOT NULL UNIQUE,
                size_bytes     INTEGER NOT NULL,
                mtime          INTEGER NOT NULL,
                title          TEXT,
                artist         TEXT,
                album          TEXT,
                duration_secs  REAL,
                has_video      INTEGER NOT NULL DEFAULT 0,
                added_at       INTEGER NOT NULL,
                last_played_at INTEGER
            );
            CREATE INDEX IF NOT EXISTS tracks_last_played ON tracks(last_played_at DESC);

            -- Separate from tracks so clearing a resume never rewrites metadata.
            CREATE TABLE IF NOT EXISTS resume (
                track_id      INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
                position_secs REAL    NOT NULL,
                updated_at    INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS folders (
                id       INTEGER PRIMARY KEY,
                path     TEXT    NOT NULL UNIQUE,
                added_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS queue_items (
                position INTEGER PRIMARY KEY,
                track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            "#,
        )?;
        conn.execute_batch(&format!("PRAGMA user_version={SCHEMA_VERSION}"))?;
        Ok(())
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("store connection poisoned")
    }

    // ---- tracks ----------------------------------------------------------

    /// Insert or update a file, returning its track id.
    ///
    /// Metadata columns are only overwritten when the caller actually learned
    /// something, so a failed probe never erases tags read on an earlier scan.
    pub fn upsert_track(&self, file: &ScannedFile, probed: &Probed) -> Result<i64> {
        let conn = self.conn();
        let now = now_unix();
        conn.execute(
            r#"
            INSERT INTO tracks (path, size_bytes, mtime, title, artist, album,
                                duration_secs, has_video, added_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(path) DO UPDATE SET
                size_bytes    = excluded.size_bytes,
                mtime         = excluded.mtime,
                title         = COALESCE(excluded.title, tracks.title),
                artist        = COALESCE(excluded.artist, tracks.artist),
                album         = COALESCE(excluded.album, tracks.album),
                duration_secs = COALESCE(excluded.duration_secs, tracks.duration_secs),
                has_video     = excluded.has_video
            "#,
            params![
                file.path,
                file.size_bytes,
                file.mtime,
                probed.title,
                probed.artist,
                probed.album,
                probed.duration_secs,
                probed.has_video as i32,
                now,
            ],
        )?;
        Ok(conn.query_row(
            "SELECT id FROM tracks WHERE path = ?1",
            params![file.path],
            |r| r.get(0),
        )?)
    }

    /// Learn a duration we only discovered at playback time.
    pub fn set_duration(&self, track_id: i64, duration_secs: f64) -> Result<()> {
        if !duration_secs.is_finite() || duration_secs <= 0.0 {
            return Ok(());
        }
        self.conn().execute(
            "UPDATE tracks SET duration_secs = ?2 WHERE id = ?1",
            params![track_id, duration_secs],
        )?;
        Ok(())
    }

    pub fn track_by_id(&self, id: i64) -> Result<Option<Track>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {TRACK_COLUMNS} FROM tracks WHERE id = ?1"),
                params![id],
                row_to_track,
            )
            .optional()?)
    }

    pub fn track_by_path(&self, path: &str) -> Result<Option<Track>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {TRACK_COLUMNS} FROM tracks WHERE path = ?1"),
                params![path],
                row_to_track,
            )
            .optional()?)
    }

    /// Existing id for a file whose size and mtime are unchanged.
    ///
    /// This is what keeps a rescan cheap: a folder of unchanged 3 GB sets costs
    /// one `stat` and one indexed lookup each, and no probing at all.
    pub fn unchanged_track_id(&self, file: &ScannedFile) -> Result<Option<i64>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id FROM tracks WHERE path = ?1 AND size_bytes = ?2 AND mtime = ?3",
                params![file.path, file.size_bytes, file.mtime],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Track>> {
        let conn = self.conn();
        let like = format!("%{}%", query.trim().replace('%', "\\%"));
        let mut stmt = conn.prepare(&format!(
            r#"SELECT {TRACK_COLUMNS} FROM tracks
               WHERE path LIKE ?1 ESCAPE '\' OR title LIKE ?1 ESCAPE '\'
                  OR artist LIKE ?1 ESCAPE '\' OR album LIKE ?1 ESCAPE '\'
               ORDER BY last_played_at DESC NULLS LAST, id DESC
               LIMIT ?2"#
        ))?;
        let rows = stmt.query_map(params![like, limit as i64], row_to_track)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn recents(&self, limit: usize) -> Result<Vec<Track>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            r#"SELECT {TRACK_COLUMNS} FROM tracks
               WHERE last_played_at IS NOT NULL
               ORDER BY last_played_at DESC LIMIT ?1"#
        ))?;
        let rows = stmt.query_map(params![limit as i64], row_to_track)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Every indexed track inside `folder`, in path order.
    ///
    /// Path order is the closest thing a folder of files has to an intended
    /// sequence — a set ripped as `01 …`, `02 …` stages in the order its author
    /// numbered it, and the user reorders from there.
    pub fn tracks_under(&self, folder: &str) -> Result<Vec<Track>> {
        let conn = self.conn();
        let trimmed = folder.trim_end_matches('/');
        // Both LIKE wildcards are escaped: a folder called `100%_sets` must not
        // match everything on disk.
        let prefix = format!(
            "{}/%",
            trimmed
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let mut stmt = conn.prepare(&format!(
            r#"SELECT {TRACK_COLUMNS} FROM tracks
               WHERE path LIKE ?1 ESCAPE '\' ORDER BY path"#
        ))?;
        let rows = stmt.query_map(params![prefix], row_to_track)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn all_tracks(&self, limit: usize) -> Result<Vec<Track>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {TRACK_COLUMNS} FROM tracks ORDER BY path LIMIT ?1"
        ))?;
        let rows = stmt.query_map(params![limit as i64], row_to_track)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn track_count(&self) -> Result<i64> {
        Ok(self
            .conn()
            .query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))?)
    }

    pub fn mark_played(&self, track_id: i64) -> Result<()> {
        self.conn().execute(
            "UPDATE tracks SET last_played_at = ?2 WHERE id = ?1",
            params![track_id, now_unix()],
        )?;
        Ok(())
    }

    /// Drop tracks whose files are gone, returning how many were removed.
    pub fn forget_missing(&self) -> Result<usize> {
        let paths: Vec<(i64, String)> = {
            let conn = self.conn();
            let mut stmt = conn.prepare("SELECT id, path FROM tracks")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let missing: Vec<i64> = paths
            .into_iter()
            .filter(|(_, p)| !Path::new(p).exists())
            .map(|(id, _)| id)
            .collect();
        let conn = self.conn();
        for id in &missing {
            conn.execute("DELETE FROM tracks WHERE id = ?1", params![id])?;
        }
        Ok(missing.len())
    }

    // ---- resume ----------------------------------------------------------

    pub fn set_resume(&self, track_id: i64, position_secs: f64) -> Result<()> {
        self.conn().execute(
            r#"INSERT INTO resume (track_id, position_secs, updated_at) VALUES (?1, ?2, ?3)
               ON CONFLICT(track_id) DO UPDATE SET
                   position_secs = excluded.position_secs,
                   updated_at    = excluded.updated_at"#,
            params![track_id, position_secs, now_unix()],
        )?;
        Ok(())
    }

    pub fn resume_for(&self, track_id: i64) -> Result<Option<f64>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT position_secs FROM resume WHERE track_id = ?1",
                params![track_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn clear_resume(&self, track_id: i64) -> Result<()> {
        self.conn()
            .execute("DELETE FROM resume WHERE track_id = ?1", params![track_id])?;
        Ok(())
    }

    // ---- folders ---------------------------------------------------------

    pub fn add_folder(&self, path: &str) -> Result<()> {
        self.conn().execute(
            "INSERT OR IGNORE INTO folders (path, added_at) VALUES (?1, ?2)",
            params![path, now_unix()],
        )?;
        Ok(())
    }

    pub fn remove_folder(&self, path: &str) -> Result<bool> {
        Ok(self
            .conn()
            .execute("DELETE FROM folders WHERE path = ?1", params![path])?
            > 0)
    }

    pub fn folders(&self) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT path FROM folders ORDER BY path")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    // ---- queue -----------------------------------------------------------

    pub fn save_queue(&self, items: &[i64], current: Option<usize>) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM queue_items", [])?;
        {
            let mut stmt =
                tx.prepare("INSERT INTO queue_items (position, track_id) VALUES (?1, ?2)")?;
            for (i, track_id) in items.iter().enumerate() {
                stmt.execute(params![i as i64, track_id])?;
            }
        }
        tx.execute(
            "INSERT INTO settings (key, value) VALUES ('queue_index', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![current.map(|c| c.to_string()).unwrap_or_default()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn load_queue(&self) -> Result<(Vec<i64>, Option<usize>)> {
        let conn = self.conn();
        let items: Vec<i64> = {
            let mut stmt = conn.prepare("SELECT track_id FROM queue_items ORDER BY position")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let current = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'queue_index'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|i| *i < items.len());
        Ok((items, current))
    }

    // ---- settings --------------------------------------------------------

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}

const TRACK_COLUMNS: &str = "id, path, size_bytes, mtime, title, artist, album, \
                             duration_secs, has_video, added_at, last_played_at";

fn row_to_track(row: &Row<'_>) -> rusqlite::Result<Track> {
    Ok(Track {
        id: row.get(0)?,
        path: row.get(1)?,
        size_bytes: row.get(2)?,
        mtime: row.get(3)?,
        title: row.get(4)?,
        artist: row.get(5)?,
        album: row.get(6)?,
        duration_secs: row.get(7)?,
        has_video: row.get::<_, i32>(8)? != 0,
        added_at: row.get(9)?,
        last_played_at: row.get(10)?,
    })
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl From<rusqlite::Error> for CoreError {
    fn from(e: rusqlite::Error) -> Self {
        CoreError::Storage {
            message: e.to_string(),
        }
    }
}
