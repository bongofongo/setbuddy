//! The facade the CLI and the menu bar app both drive.
//!
//! Owns the queue, writes resume positions, and routes every playback call to
//! whichever engine the registry selected. It knows nothing about mpv.

use std::path::Path;
use std::sync::{Arc, Mutex};

use setbuddy_engine::{EngineSnapshot, SharedEngine, VideoWindowLayout};

use crate::error::{CoreError, Result};
use crate::probe::probe;
use crate::queue::{Queue, RepeatMode};
use crate::resume;
use crate::selection::{EnginePolicy, EngineRegistry};
use crate::store::{ScannedFile, Store};
use crate::track::{is_media_file, Track};

/// How often a position is written while playing. Frequent enough that a crash
/// costs seconds, rare enough not to wake the disk every tick.
pub const RESUME_WRITE_INTERVAL_SECS: f64 = 15.0;

const SETTING_REPEAT: &str = "repeat";
const SETTING_SHUFFLE: &str = "shuffle";
const SETTING_ENGINE_POLICY: &str = "engine_policy";
/// Comma-separated queue positions; empty when playback walks the whole queue.
const SETTING_LOOP: &str = "queue_loop";
/// `VideoWindowLayout` in its settings form: `40%`, `fullscreen`, `1280`, or
/// `1280+100+50`.
const SETTING_VIDEO_WINDOW: &str = "video_window_size";
const SETTING_VIDEO_SURFACE: &str = "video_surface";

#[derive(Debug, Clone, PartialEq)]
pub struct PlayerStatus {
    pub track: Option<Track>,
    pub position_secs: Option<f64>,
    pub duration_secs: Option<f64>,
    pub paused: bool,
    pub idle: bool,
    pub has_video: bool,
    pub video_visible: bool,
    pub queue_len: usize,
    pub queue_index: Option<usize>,
    pub repeat: RepeatMode,
    pub shuffle: bool,
    /// Queue positions playback is confined to; empty when it is not.
    pub loop_positions: Vec<usize>,
    pub engine_id: Option<String>,
}

struct State {
    queue: Queue,
    current: Option<Track>,
    active: Option<SharedEngine>,
    /// Position last written to the resume table.
    last_written: f64,
}

pub struct Player {
    store: Arc<Store>,
    registry: Mutex<EngineRegistry>,
    state: Mutex<State>,
}

impl Player {
    /// Restore queue, repeat/shuffle and engine policy from the store, then
    /// adopt whatever is already playing.
    pub fn new(store: Arc<Store>, registry: EngineRegistry) -> Result<Self> {
        let mut queue = Queue::new();
        let (items, index) = store.load_queue()?;
        queue.replace(items, index);
        if let Some(mode) = store
            .setting(SETTING_REPEAT)?
            .and_then(|m| RepeatMode::parse(&m))
        {
            queue.set_repeat(mode);
        }
        queue.set_shuffle(store.setting(SETTING_SHUFFLE)?.as_deref() == Some("on"));
        // After repeat, so a saved loop wins the exclusivity the way it did
        // when it was set.
        if let Some(saved) = store.setting(SETTING_LOOP)? {
            let positions: Vec<usize> = saved
                .split(',')
                .filter_map(|p| p.trim().parse().ok())
                .collect();
            if !positions.is_empty() {
                queue.set_loop(Some(positions));
            }
        }

        let policy = store
            .setting(SETTING_ENGINE_POLICY)?
            .map(|p| EnginePolicy::parse(&p))
            .unwrap_or_default();

        let player = Self {
            store,
            registry: Mutex::new(registry.with_policy(policy)),
            state: Mutex::new(State {
                queue,
                current: None,
                active: None,
                last_written: f64::NAN,
            }),
        };
        // Engines start at their own default; hand them the saved layout
        // before anything can pop a window out.
        if let Some(layout) = player.video_window_layout()? {
            player.apply_video_window_layout(layout)?;
        }
        player.adopt_running_engine()?;
        Ok(player)
    }

    /// Find an engine that is already playing something.
    ///
    /// The CLI runs as separate short-lived processes, so "what is playing" is
    /// not in memory — it has to be read back from the engine. Snapshotting
    /// never starts an engine, so this stays free when nothing is playing.
    fn adopt_running_engine(&self) -> Result<()> {
        let engines: Vec<SharedEngine> = {
            let registry = self.registry.lock().expect("registry poisoned");
            registry.all().to_vec()
        };
        for engine in engines {
            let snap = engine.snapshot();
            if let Some(path) = snap.path.clone() {
                let track = self.store.track_by_path(&path)?;
                let mut state = self.state.lock().expect("player state poisoned");
                state.active = Some(engine);
                if let Some(track) = track {
                    state.queue.select_track(track.id);
                    state.current = Some(track);
                }
                return Ok(());
            }
        }
        Ok(())
    }

    // ---- library ---------------------------------------------------------

    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// Index a file if it is not already known, returning its track.
    pub fn ensure_indexed(&self, path: &Path) -> Result<Track> {
        let canonical = path
            .canonicalize()
            .map_err(|e| CoreError::Io(e))?
            .to_string_lossy()
            .into_owned();
        if !is_media_file(Path::new(&canonical)) {
            return Err(CoreError::NotMedia { path: canonical });
        }
        if let Some(existing) = self.store.track_by_path(&canonical)? {
            return Ok(existing);
        }
        let meta = std::fs::metadata(&canonical)?;
        let file = ScannedFile {
            path: canonical.clone(),
            size_bytes: meta.len() as i64,
            mtime: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        };
        let id = self
            .store
            .upsert_track(&file, &probe(Path::new(&canonical)))?;
        self.store
            .track_by_id(id)?
            .ok_or_else(|| CoreError::Internal {
                message: "track vanished immediately after indexing".into(),
            })
    }

    /// First library match for a free-text query.
    pub fn find_track(&self, query: &str) -> Result<Track> {
        self.store
            .search(query, 1)?
            .into_iter()
            .next()
            .ok_or_else(|| CoreError::NoMatch {
                query: query.to_string(),
            })
    }

    // ---- playback --------------------------------------------------------

    /// Play a file, replacing the queue with it.
    pub fn play_path(&self, path: &Path) -> Result<Track> {
        let track = self.ensure_indexed(path)?;
        self.set_queue(vec![track.id], Some(0))?;
        self.play_track(&track, false)?;
        Ok(track)
    }

    pub fn play_track_id(&self, track_id: i64) -> Result<Track> {
        self.start_track_id(track_id, false)
    }

    /// Play from the beginning, discarding any saved position.
    ///
    /// The discard happens *after* the outgoing position is persisted. Clearing
    /// it beforehand does not work when the track being restarted is the one
    /// already playing: persisting would simply write the position straight back.
    pub fn restart_track_id(&self, track_id: i64) -> Result<Track> {
        self.start_track_id(track_id, true)
    }

    fn start_track_id(&self, track_id: i64, restart: bool) -> Result<Track> {
        let track = self
            .store
            .track_by_id(track_id)?
            .ok_or_else(|| CoreError::NoMatch {
                query: track_id.to_string(),
            })?;
        {
            let mut state = self.state.lock().expect("player state poisoned");
            if !state.queue.select_track(track_id) {
                state.queue.extend([track_id]);
                state.queue.select_track(track_id);
            }
        }
        self.persist_queue()?;
        self.play_track(&track, restart)?;
        Ok(track)
    }

    /// Load a track on the right engine, resuming if there is a position worth
    /// resuming from.
    fn play_track(&self, track: &Track, restart: bool) -> Result<()> {
        // Whatever was playing gets its position written before we move on —
        // including when it is this same track, which is why a restart must
        // clear the saved position only after this point.
        self.persist_resume()?;
        if restart {
            self.store.clear_resume(track.id)?;
        }

        let engine = {
            let registry = self.registry.lock().expect("registry poisoned");
            registry.select_for(&track.path)?
        };

        // Switching engines must not leave the old one holding the audio device.
        {
            let previous = {
                let state = self.state.lock().expect("player state poisoned");
                state.active.clone()
            };
            if let Some(previous) = previous {
                if previous.capabilities().id != engine.capabilities().id {
                    let _ = previous.stop();
                }
            }
        }

        let start_at = resume::start_at(self.store.resume_for(track.id)?, track.duration_secs);
        engine.load(track.path.clone(), start_at)?;
        self.store.mark_played(track.id)?;

        let mut state = self.state.lock().expect("player state poisoned");
        state.active = Some(engine);
        state.current = Some(track.clone());
        state.last_written = start_at.unwrap_or(0.0);
        Ok(())
    }

    pub fn set_paused(&self, paused: bool) -> Result<()> {
        let engine = self.active_engine()?;
        engine.set_paused(paused)?;
        // Pausing is a natural save point.
        self.persist_resume()?;
        Ok(())
    }

    pub fn toggle_paused(&self) -> Result<bool> {
        let engine = self.active_engine()?;
        let now_paused = !engine.snapshot().paused;
        engine.set_paused(now_paused)?;
        self.persist_resume()?;
        Ok(now_paused)
    }

    pub fn next(&self) -> Result<Option<Track>> {
        self.persist_resume()?;
        let next_id = {
            let mut state = self.state.lock().expect("player state poisoned");
            state.queue.next()
        };
        self.persist_queue()?;
        self.play_or_stop(next_id)
    }

    pub fn previous(&self) -> Result<Option<Track>> {
        self.persist_resume()?;
        let previous_id = {
            let mut state = self.state.lock().expect("player state poisoned");
            state.queue.previous()
        };
        self.persist_queue()?;
        self.play_or_stop(previous_id)
    }

    fn play_or_stop(&self, track_id: Option<i64>) -> Result<Option<Track>> {
        match track_id {
            Some(id) => {
                let track = self.store.track_by_id(id)?.ok_or(CoreError::Internal {
                    message: format!("queued track {id} is no longer in the library"),
                })?;
                self.play_track(&track, false)?;
                Ok(Some(track))
            }
            None => {
                self.stop()?;
                Ok(None)
            }
        }
    }

    pub fn stop(&self) -> Result<()> {
        self.persist_resume()?;
        if let Ok(engine) = self.active_engine() {
            engine.stop()?;
        }
        let mut state = self.state.lock().expect("player state poisoned");
        state.current = None;
        state.last_written = f64::NAN;
        Ok(())
    }

    pub fn seek_absolute(&self, seconds: f64) -> Result<()> {
        self.active_engine()?.seek_absolute(seconds.max(0.0))?;
        Ok(())
    }

    /// Seek by a delta, clamped to the file.
    pub fn seek_relative(&self, delta_secs: f64) -> Result<f64> {
        let engine = self.active_engine()?;
        let snap = engine.snapshot();
        let current = snap.position_secs.unwrap_or(0.0);
        let target = (current + delta_secs)
            .max(0.0)
            .min(snap.duration_secs.unwrap_or(f64::MAX));
        engine.seek_absolute(target)?;
        Ok(target)
    }

    /// The pop-out. Audio is unaffected either way.
    pub fn set_video_visible(&self, visible: bool) -> Result<()> {
        self.active_engine()?.set_video_visible(visible)?;
        Ok(())
    }

    pub fn toggle_video(&self) -> Result<bool> {
        let engine = self.active_engine()?;
        let snap = engine.snapshot();
        if !snap.has_video {
            return Err(CoreError::Internal {
                message: "the current track has no video to pop out".into(),
            });
        }
        let visible = !snap.video_visible;
        engine.set_video_visible(visible)?;
        Ok(visible)
    }

    pub fn set_video_ontop(&self, ontop: bool) -> Result<()> {
        self.active_engine()?.set_video_ontop(ontop)?;
        Ok(())
    }

    /// The saved pop-out layout, if one has been chosen.
    pub fn video_window_layout(&self) -> Result<Option<VideoWindowLayout>> {
        Ok(self
            .store
            .setting(SETTING_VIDEO_WINDOW)?
            .and_then(|spec| VideoWindowLayout::parse(&spec)))
    }

    /// Choose where and how large the pop-out is. Persisted, and pushed to
    /// every engine rather than only the active one: the choice is about the
    /// window, whichever backend ends up drawing it.
    pub fn set_video_window_layout(&self, layout: VideoWindowLayout) -> Result<()> {
        self.store
            .set_setting(SETTING_VIDEO_WINDOW, &layout.as_str())?;
        self.apply_video_window_layout(layout)
    }

    /// Where the picture goes when video is switched on, as the app saved it.
    ///
    /// Core stores it and does nothing else with it: only an engine running
    /// inside the app's own process can hand it a surface to draw, and knowing
    /// which engine that is belongs to the app, not here. `None` until chosen.
    pub fn video_surface(&self) -> Result<Option<String>> {
        Ok(self.store.setting(SETTING_VIDEO_SURFACE)?)
    }

    pub fn set_video_surface(&self, surface: &str) -> Result<()> {
        self.store.set_setting(SETTING_VIDEO_SURFACE, surface)?;
        Ok(())
    }

    /// Put up an empty video window for the user to place by hand. Returns the
    /// id of the process owning it, so the app can read its bounds back, or
    /// `None` when no engine can show one.
    pub fn begin_window_placement(&self) -> Result<Option<u32>> {
        let engines: Vec<SharedEngine> = {
            let registry = self.registry.lock().expect("registry poisoned");
            registry.all().to_vec()
        };
        for engine in engines {
            if let Some(pid) = engine.begin_window_placement()? {
                return Ok(Some(pid));
            }
        }
        Ok(None)
    }

    pub fn end_window_placement(&self) -> Result<()> {
        let engines: Vec<SharedEngine> = {
            let registry = self.registry.lock().expect("registry poisoned");
            registry.all().to_vec()
        };
        for engine in engines {
            engine.end_window_placement()?;
        }
        Ok(())
    }

    fn apply_video_window_layout(&self, layout: VideoWindowLayout) -> Result<()> {
        let engines: Vec<SharedEngine> = {
            let registry = self.registry.lock().expect("registry poisoned");
            registry.all().to_vec()
        };
        for engine in engines {
            engine.set_video_window_layout(layout)?;
        }
        Ok(())
    }

    pub fn set_volume(&self, percent: f64) -> Result<()> {
        self.active_engine()?.set_volume(percent)?;
        Ok(())
    }

    pub fn set_speed(&self, rate: f64) -> Result<()> {
        self.active_engine()?.set_speed(rate)?;
        Ok(())
    }

    // ---- queue -----------------------------------------------------------

    pub fn set_queue(&self, track_ids: Vec<i64>, start: Option<usize>) -> Result<()> {
        {
            let mut state = self.state.lock().expect("player state poisoned");
            state.queue.replace(track_ids, start);
        }
        self.persist_queue()
    }

    pub fn queue_add(&self, track_ids: impl IntoIterator<Item = i64>) -> Result<()> {
        {
            let mut state = self.state.lock().expect("player state poisoned");
            state.queue.extend(track_ids);
        }
        self.persist_queue()
    }

    /// Put a file, or everything under a folder, at the top of the queue.
    ///
    /// Staging deliberately does not start playback. The stage is what makes
    /// reordering and shuffling meaningful: the user adds, arranges, and only
    /// then plays.
    ///
    /// A folder is scanned on the way in, so files that were never in a watched
    /// folder are indexed by the act of staging them. Blocking for exactly that
    /// reason — call it off a UI thread.
    pub fn stage_path(&self, path: &Path) -> Result<Vec<Track>> {
        let tracks = if path.is_dir() {
            let root = path.canonicalize()?;
            crate::library::scan_folder(&self.store, &root)?;
            let found = self.store.tracks_under(&root.to_string_lossy())?;
            if found.is_empty() {
                return Err(CoreError::NoMatch {
                    query: root.to_string_lossy().into_owned(),
                });
            }
            found
        } else {
            vec![self.ensure_indexed(path)?]
        };

        {
            let mut state = self.state.lock().expect("player state poisoned");
            state.queue.insert_front(tracks.iter().map(|t| t.id));
        }
        self.persist_queue()?;
        Ok(tracks)
    }

    /// Reorder the queue, returning whether anything moved.
    pub fn queue_move(&self, from: usize, to: usize) -> Result<bool> {
        let moved = {
            let mut state = self.state.lock().expect("player state poisoned");
            state.queue.move_item(from, to)
        };
        if moved {
            self.persist_queue()?;
        }
        Ok(moved)
    }

    /// Scramble the tracks at `positions` among themselves, or the whole queue
    /// with `None`. See [`Queue::scramble`].
    pub fn queue_scramble(&self, positions: Option<&[usize]>) -> Result<()> {
        {
            let mut state = self.state.lock().expect("player state poisoned");
            state.queue.scramble(positions);
        }
        self.persist_queue()
    }

    /// Confine playback to `positions`, or lift it with `None`. Replaces any
    /// repeat mode; see [`Queue::set_loop`].
    pub fn set_loop(&self, positions: Option<Vec<usize>>) -> Result<()> {
        {
            let mut state = self.state.lock().expect("player state poisoned");
            state.queue.set_loop(positions);
        }
        // Repeat was reset by the queue, so its stored value must follow.
        self.persist_playback_rules()
    }

    pub fn queue_clear(&self) -> Result<()> {
        {
            let mut state = self.state.lock().expect("player state poisoned");
            state.queue.clear();
        }
        self.persist_queue()
    }

    /// Rescan every watched folder, then drop anything the scan forgot from
    /// the queue.
    ///
    /// The two belong together. A scan deletes the rows of files that are gone,
    /// and SQLite cascades that into `queue_items` — but the queue this player
    /// holds in memory still lists them, and the next save would try to write a
    /// row pointing at a track that no longer exists. Callers get one call that
    /// leaves the library and the queue agreeing with each other.
    ///
    /// Walks the disk and probes new files, so keep it off a UI thread.
    pub fn rescan_library(&self) -> Result<crate::library::ScanReport> {
        let report = crate::library::scan_all(&self.store)?;
        if report.removed > 0 {
            self.prune_missing_tracks()?;
        }
        Ok(report)
    }

    /// Drop queued rows whose track is no longer in the library, returning
    /// whether any went. What is playing keeps playing if it survived.
    pub fn prune_missing_tracks(&self) -> Result<bool> {
        let ids = {
            let state = self.state.lock().expect("player state poisoned");
            state.queue.items().to_vec()
        };
        if ids.is_empty() {
            return Ok(false);
        }
        let alive = self.store.existing_track_ids(&ids)?;
        // Compared as sets: a queue may hold the same track twice, so the row
        // count and the id count are not the same number.
        if ids.iter().all(|id| alive.contains(id)) {
            return Ok(false);
        }
        let changed = {
            let mut state = self.state.lock().expect("player state poisoned");
            state.queue.retain(|id| alive.contains(&id))
        };
        if changed {
            self.persist_queue()?;
        }
        Ok(changed)
    }

    pub fn queue_tracks(&self) -> Result<Vec<Track>> {
        let ids = {
            let state = self.state.lock().expect("player state poisoned");
            state.queue.items().to_vec()
        };
        let mut tracks = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(track) = self.store.track_by_id(id)? {
                tracks.push(track);
            }
        }
        Ok(tracks)
    }

    pub fn set_repeat(&self, mode: RepeatMode) -> Result<()> {
        self.state
            .lock()
            .expect("player state poisoned")
            .queue
            .set_repeat(mode);
        // The queue cleared any loop, so both settings are written together.
        self.persist_playback_rules()
    }

    /// Repeat and loop, which the queue keeps exclusive, written as a pair so
    /// the store never shows one without the other's reset.
    fn persist_playback_rules(&self) -> Result<()> {
        let (repeat, loop_positions) = {
            let state = self.state.lock().expect("player state poisoned");
            (
                state.queue.repeat(),
                state
                    .queue
                    .loop_set()
                    .map(<[usize]>::to_vec)
                    .unwrap_or_default(),
            )
        };
        self.store.set_setting(SETTING_REPEAT, repeat.as_str())?;
        let joined = loop_positions
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(",");
        self.store.set_setting(SETTING_LOOP, &joined)
    }

    pub fn set_shuffle(&self, on: bool) -> Result<()> {
        self.state
            .lock()
            .expect("player state poisoned")
            .queue
            .set_shuffle(on);
        self.store
            .set_setting(SETTING_SHUFFLE, if on { "on" } else { "off" })
    }

    /// The policy in force, restored from the store at construction. What the
    /// settings UI has to show to be telling the truth after a restart.
    pub fn engine_policy(&self) -> EnginePolicy {
        self.registry
            .lock()
            .expect("registry poisoned")
            .policy()
            .clone()
    }

    /// What each registered engine can do, in preference order. The settings
    /// UI names engines by `display_name`, never by id.
    pub fn engine_capabilities(&self) -> Vec<setbuddy_engine::EngineCapabilities> {
        self.registry
            .lock()
            .expect("registry poisoned")
            .all()
            .iter()
            .map(|e| e.capabilities())
            .collect()
    }

    /// Force an engine, or go back to picking by capability.
    ///
    /// An id no engine answers to is refused rather than stored: accepting it
    /// would persist a policy under which *every* file reports as unplayable,
    /// and the failure would surface later, at play time, far from the choice
    /// that caused it.
    pub fn set_engine_policy(&self, policy: EnginePolicy) -> Result<()> {
        {
            let registry = self.registry.lock().expect("registry poisoned");
            if let EnginePolicy::Force(id) = &policy {
                if registry.by_id(id).is_none() {
                    return Err(CoreError::Internal {
                        message: format!(
                            "no engine with id \"{id}\" is available (have: {})",
                            registry.ids().join(", ")
                        ),
                    });
                }
            }
        }
        self.store
            .set_setting(SETTING_ENGINE_POLICY, &policy.as_str())?;
        self.registry
            .lock()
            .expect("registry poisoned")
            .set_policy(policy);
        Ok(())
    }

    pub fn engine_ids(&self) -> Vec<String> {
        self.registry.lock().expect("registry poisoned").ids()
    }

    fn persist_queue(&self) -> Result<()> {
        let (items, index) = {
            let state = self.state.lock().expect("player state poisoned");
            (state.queue.items().to_vec(), state.queue.current_index())
        };
        self.store.save_queue(&items, index)?;
        // Staging, moving and clearing all shift or drop the loop with them.
        self.persist_playback_rules()
    }

    // ---- status and upkeep -----------------------------------------------

    fn active_engine(&self) -> Result<SharedEngine> {
        if let Some(engine) = self
            .state
            .lock()
            .expect("player state poisoned")
            .active
            .clone()
        {
            return Ok(engine);
        }
        Err(CoreError::NothingPlaying)
    }

    pub fn status(&self) -> Result<PlayerStatus> {
        let (engine, mut track, queue_len, queue_index, repeat, shuffle, loop_positions) = {
            let state = self.state.lock().expect("player state poisoned");
            (
                state.active.clone(),
                state.current.clone(),
                state.queue.len(),
                state.queue.current_index(),
                state.queue.repeat(),
                state.queue.shuffle_enabled(),
                state
                    .queue
                    .loop_set()
                    .map(<[usize]>::to_vec)
                    .unwrap_or_default(),
            )
        };

        let snap = engine.as_ref().map(|e| e.snapshot()).unwrap_or_default();
        // An engine adopted mid-session may be playing something we have not
        // looked up yet.
        if track.is_none() {
            if let Some(path) = snap.path.as_ref() {
                track = self.store.track_by_path(path)?;
            }
        }

        Ok(PlayerStatus {
            duration_secs: snap
                .duration_secs
                .or(track.as_ref().and_then(|t| t.duration_secs)),
            has_video: snap.has_video || track.as_ref().map(|t| t.has_video).unwrap_or(false),
            track,
            position_secs: snap.position_secs,
            paused: snap.paused,
            idle: snap.idle || engine.is_none(),
            video_visible: snap.video_visible,
            queue_len,
            queue_index,
            repeat,
            shuffle,
            loop_positions,
            engine_id: engine.map(|e| e.capabilities().id),
        })
    }

    /// Periodic upkeep: persist position, learn durations, advance at EOF.
    ///
    /// The app calls this on a timer; the CLI calls it once per invocation so a
    /// position is recorded even when nothing else drives the loop.
    pub fn tick(&self) -> Result<()> {
        let Ok(engine) = self.active_engine() else {
            return Ok(());
        };
        let snap = engine.snapshot();

        // A duration we could not probe is learned the first time it plays.
        if let (Some(track), Some(duration)) = (self.current_track(), snap.duration_secs) {
            if track.duration_secs.is_none() {
                self.store.set_duration(track.id, duration)?;
                let mut state = self.state.lock().expect("player state poisoned");
                if let Some(current) = state.current.as_mut() {
                    current.duration_secs = Some(duration);
                }
            }
        }

        if snap.eof {
            return self.on_end_of_file();
        }
        self.maybe_write_resume(&snap)?;
        Ok(())
    }

    pub fn current_track(&self) -> Option<Track> {
        self.state
            .lock()
            .expect("player state poisoned")
            .current
            .clone()
    }

    fn on_end_of_file(&self) -> Result<()> {
        // A finished track has no position worth keeping.
        if let Some(track) = self.current_track() {
            self.store.clear_resume(track.id)?;
        }
        let next = {
            let mut state = self.state.lock().expect("player state poisoned");
            state.last_written = f64::NAN;
            state.queue.advance_on_eof()
        };
        self.persist_queue()?;
        match next {
            Some(id) => {
                let track = self.store.track_by_id(id)?;
                match track {
                    Some(track) => self.play_track(&track, false),
                    None => self.stop(),
                }
            }
            None => self.stop(),
        }
    }

    fn maybe_write_resume(&self, snap: &EngineSnapshot) -> Result<()> {
        let Some(position) = snap.position_secs else {
            return Ok(());
        };
        let Some(track) = self.current_track() else {
            return Ok(());
        };
        let duration = snap.duration_secs.or(track.duration_secs);

        let last = self
            .state
            .lock()
            .expect("player state poisoned")
            .last_written;
        let moved_enough =
            !last.is_finite() || (position - last).abs() >= RESUME_WRITE_INTERVAL_SECS;
        if !moved_enough {
            return Ok(());
        }

        if resume::should_store(position, duration) {
            self.store.set_resume(track.id, position)?;
        } else {
            // Scrubbing back to the start should discard a stale resume rather
            // than leave the old one to fire on the next play.
            self.store.clear_resume(track.id)?;
        }
        self.state
            .lock()
            .expect("player state poisoned")
            .last_written = position;
        Ok(())
    }

    /// Write the current position immediately.
    pub fn persist_resume(&self) -> Result<()> {
        let Ok(engine) = self.active_engine() else {
            return Ok(());
        };
        let snap = engine.snapshot();
        let Some(position) = snap.position_secs else {
            return Ok(());
        };
        let Some(track) = self.current_track() else {
            return Ok(());
        };
        let duration = snap.duration_secs.or(track.duration_secs);
        if resume::should_store(position, duration) {
            self.store.set_resume(track.id, position)?;
            self.state
                .lock()
                .expect("player state poisoned")
                .last_written = position;
        }
        Ok(())
    }

    /// Save state and stop every engine. Playback does not survive this.
    pub fn quit(&self) -> Result<()> {
        self.persist_resume()?;
        self.persist_queue()?;
        let registry = self.registry.lock().expect("registry poisoned");
        for engine in registry.all() {
            engine.shutdown();
        }
        Ok(())
    }
}
