//! `setwave` — the cross-platform command line player.
//!
//! Each invocation is short-lived. Continuity comes from two places: the SQLite
//! store holds the library, queue and resume positions, and the engine adopts
//! the mpv already running on Setwave's well-known socket. So `setwave play`
//! followed a minute later by `setwave pause` addresses one continuous playback
//! session, with no daemon in between.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use clap::{Args, Parser, Subcommand};
use setwave_core::library::{scan_all, scan_folder};
use setwave_core::paths;
use setwave_core::player::Player;
use setwave_core::queue::RepeatMode;
use setwave_core::selection::{EnginePolicy, EngineRegistry};
use setwave_core::store::Store;
use setwave_core::track::format_duration;
use setwave_core::{PlayerStatus, Track};
use setwave_engine::{EngineError, SharedEngine};
use setwave_mpv::MpvEngine;

#[derive(Parser)]
#[command(
    name = "setwave",
    about = "Play downloaded sets and music from the command line",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Play a file, or the best library match for a search term
    Play(PlayArgs),
    /// Pause playback
    Pause,
    /// Resume playback
    Resume,
    /// Toggle between playing and paused
    Toggle,
    /// Skip to the next track in the queue
    Next,
    /// Go back to the previous track
    Prev,
    /// Stop playback and unload the current track
    Stop,
    /// Seek, e.g. `+30`, `-1:00`, `1:12:44`
    Seek { position: String },
    /// Show or hide the video window
    Video {
        /// on, off, or toggle
        #[arg(default_value = "toggle")]
        state: String,
    },
    /// Set the volume, 0-100
    Volume { percent: f64 },
    /// Set the playback rate, 1.0 being normal
    Speed { rate: f64 },
    /// Repeat mode: off, all, or one
    Repeat { mode: String },
    /// Shuffle: on or off
    Shuffle { state: String },
    /// Show what is playing
    Status,
    /// Manage the play queue
    #[command(subcommand)]
    Queue(QueueCommand),
    /// Manage watched folders and the index
    #[command(subcommand)]
    Library(LibraryCommand),
    /// Show or choose the playback engine
    #[command(subcommand)]
    Engine(EngineCommand),
    /// Stop playback and shut the engine down
    Quit,
}

#[derive(Args)]
struct PlayArgs {
    /// A file path, or words to search the library for
    target: Vec<String>,
    /// Start from the beginning, ignoring any saved position
    #[arg(long)]
    restart: bool,
}

#[derive(Subcommand)]
enum QueueCommand {
    /// Append files, or library matches, to the queue
    Add { targets: Vec<String> },
    /// List the queue
    List,
    /// Empty the queue
    Clear,
}

#[derive(Subcommand)]
enum LibraryCommand {
    /// Watch a folder and index it
    Add { folder: PathBuf },
    /// Stop watching a folder
    Remove { folder: PathBuf },
    /// List watched folders
    List,
    /// Rescan every watched folder
    Scan,
    /// Search the index
    Search {
        query: Vec<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Show recently played tracks
    Recent {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
}

#[derive(Subcommand)]
enum EngineCommand {
    /// List available engines
    List,
    /// Choose an engine by id, or `auto`
    Use { id: String },
}

fn main() {
    if let Err(error) = run() {
        // An uninstalled engine is a setup problem, not a crash: say what to do.
        if let Some(EngineError::EngineMissing { display_name, hint }) =
            error.downcast_ref::<EngineError>()
        {
            eprintln!("setwave: {display_name} is not installed.\n  {hint}");
            std::process::exit(2);
        }
        eprintln!("setwave: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let player = build_player()?;

    match cli.command {
        Command::Play(args) => {
            let track = play_target(&player, &args)?;
            println!("▶ {}", track.display_label());
        }
        Command::Pause => {
            player.set_paused(true)?;
            println!("⏸ paused");
        }
        Command::Resume => {
            player.set_paused(false)?;
            println!("▶ playing");
        }
        Command::Toggle => {
            let paused = player.toggle_paused()?;
            println!("{}", if paused { "⏸ paused" } else { "▶ playing" });
        }
        Command::Next => match player.next()? {
            Some(track) => println!("▶ {}", track.display_label()),
            None => println!("end of queue"),
        },
        Command::Prev => match player.previous()? {
            Some(track) => println!("▶ {}", track.display_label()),
            None => println!("start of queue"),
        },
        Command::Stop => {
            player.stop()?;
            println!("stopped");
        }
        Command::Seek { position } => {
            let target = match parse_seek(&position)? {
                Seek::Absolute(secs) => {
                    player.seek_absolute(secs)?;
                    secs
                }
                Seek::Relative(delta) => player.seek_relative(delta)?,
            };
            println!("⇥ {}", format_duration(target));
        }
        Command::Video { state } => {
            let visible = match state.trim().to_ascii_lowercase().as_str() {
                "on" | "show" => {
                    player.set_video_visible(true)?;
                    true
                }
                "off" | "hide" => {
                    player.set_video_visible(false)?;
                    false
                }
                "toggle" => player.toggle_video()?,
                other => return Err(anyhow!("unknown video state \"{other}\" (on, off, toggle)")),
            };
            println!("video {}", if visible { "shown" } else { "hidden" });
        }
        Command::Volume { percent } => {
            player.set_volume(percent)?;
            println!("volume {percent:.0}%");
        }
        Command::Speed { rate } => {
            player.set_speed(rate)?;
            println!("speed {rate:.2}x");
        }
        Command::Repeat { mode } => {
            let mode = RepeatMode::parse(&mode)
                .ok_or_else(|| anyhow!("unknown repeat mode (off, all, one)"))?;
            player.set_repeat(mode)?;
            println!("repeat {}", mode.as_str());
        }
        Command::Shuffle { state } => {
            let on = parse_on_off(&state)?;
            player.set_shuffle(on)?;
            println!("shuffle {}", if on { "on" } else { "off" });
        }
        Command::Status => {
            // Record a position even when nothing else is driving the loop.
            let _ = player.tick();
            print_status(&player.status()?);
        }
        Command::Queue(cmd) => queue_command(&player, cmd)?,
        Command::Library(cmd) => library_command(&player, cmd)?,
        Command::Engine(cmd) => engine_command(&player, cmd)?,
        Command::Quit => {
            player.quit()?;
            println!("stopped and shut down");
        }
    }
    Ok(())
}

/// Wire the store and the engine registry together.
///
/// The mpv engine is bound to the well-known socket so this invocation adopts
/// whatever is already playing. Registration order here *is* preference order:
/// when an AVFoundation engine lands, it goes in front of mpv and the policy
/// picks it for the containers it can handle.
fn build_player() -> Result<Player> {
    paths::ensure_state_dir().context("could not create Setwave's state directory")?;
    let store = Arc::new(Store::open(&paths::database_path())?);
    let engine: SharedEngine = Arc::new(MpvEngine::shared(paths::engine_socket_path())?);
    Ok(Player::new(store, EngineRegistry::new(vec![engine]))?)
}

/// Resolve a play target: an existing path, otherwise a library search.
fn resolve_target(player: &Player, words: &[String]) -> Result<Track> {
    if words.is_empty() {
        return Err(anyhow!("nothing to play"));
    }
    let joined = words.join(" ");
    let path = Path::new(&joined);
    if path.exists() {
        return Ok(player.ensure_indexed(path)?);
    }
    Ok(player.find_track(&joined)?)
}

fn play_target(player: &Player, args: &PlayArgs) -> Result<Track> {
    let track = resolve_target(player, &args.target)?;
    if args.restart {
        player.restart_track_id(track.id)?;
    } else {
        player.play_track_id(track.id)?;
    }
    Ok(track)
}

fn queue_command(player: &Player, cmd: QueueCommand) -> Result<()> {
    match cmd {
        QueueCommand::Add { targets } => {
            if targets.is_empty() {
                return Err(anyhow!("nothing to add"));
            }
            let mut added = 0;
            for target in &targets {
                let track = resolve_target(player, std::slice::from_ref(target))?;
                player.queue_add([track.id])?;
                println!("+ {}", track.display_label());
                added += 1;
            }
            println!("{added} added");
        }
        QueueCommand::List => {
            let tracks = player.queue_tracks()?;
            if tracks.is_empty() {
                println!("queue is empty");
                return Ok(());
            }
            let current = player.status()?.queue_index;
            for (i, track) in tracks.iter().enumerate() {
                let marker = if Some(i) == current { "▶" } else { " " };
                println!("{marker} {:>3}. {}", i + 1, track.display_label());
            }
        }
        QueueCommand::Clear => {
            player.queue_clear()?;
            println!("queue cleared");
        }
    }
    Ok(())
}

fn library_command(player: &Player, cmd: LibraryCommand) -> Result<()> {
    let store = player.store();
    match cmd {
        LibraryCommand::Add { folder } => {
            let canonical = folder
                .canonicalize()
                .with_context(|| format!("no such folder: {}", folder.display()))?;
            let path = canonical.to_string_lossy().into_owned();
            store.add_folder(&path)?;
            let report = scan_folder(store, &canonical)?;
            println!(
                "watching {path}\n  {} found, {} added, {} updated, {} unchanged",
                report.seen, report.added, report.updated, report.unchanged
            );
        }
        LibraryCommand::Remove { folder } => {
            // Canonicalise when possible, but still allow removing a folder
            // that has since been deleted or unmounted.
            let path = folder
                .canonicalize()
                .unwrap_or(folder)
                .to_string_lossy()
                .into_owned();
            if store.remove_folder(&path)? {
                println!("no longer watching {path}");
            } else {
                println!("{path} was not being watched");
            }
        }
        LibraryCommand::List => {
            let folders = store.folders()?;
            if folders.is_empty() {
                println!("no watched folders — add one with `setwave library add <dir>`");
            }
            for folder in folders {
                println!("{folder}");
            }
        }
        LibraryCommand::Scan => {
            let report = scan_all(store)?;
            println!(
                "{} found, {} added, {} updated, {} unchanged, {} removed",
                report.seen, report.added, report.updated, report.unchanged, report.removed
            );
            println!("{} tracks in the library", store.track_count()?);
        }
        LibraryCommand::Search { query, limit } => {
            let tracks = store.search(&query.join(" "), limit)?;
            if tracks.is_empty() {
                println!("nothing matched");
            }
            for track in tracks {
                print_track_line(&track, store.resume_for(track.id)?);
            }
        }
        LibraryCommand::Recent { limit } => {
            let tracks = store.recents(limit)?;
            if tracks.is_empty() {
                println!("nothing played yet");
            }
            for track in tracks {
                print_track_line(&track, store.resume_for(track.id)?);
            }
        }
    }
    Ok(())
}

fn engine_command(player: &Player, cmd: EngineCommand) -> Result<()> {
    match cmd {
        EngineCommand::List => {
            for id in player.engine_ids() {
                println!("{id}");
            }
        }
        EngineCommand::Use { id } => {
            player.set_engine_policy(EnginePolicy::parse(&id))?;
            println!("engine set to {id}");
        }
    }
    Ok(())
}

fn print_track_line(track: &Track, resume_secs: Option<f64>) {
    let duration = track
        .duration_secs
        .map(format_duration)
        .unwrap_or_else(|| "--:--".into());
    let mark = if track.has_video { "▣" } else { "♪" };
    match resume_secs {
        Some(secs) => println!(
            "{mark} {:<8} {}  [resume {}]",
            duration,
            track.display_label(),
            format_duration(secs)
        ),
        None => println!("{mark} {:<8} {}", duration, track.display_label()),
    }
}

fn print_status(status: &PlayerStatus) {
    let Some(track) = status.track.as_ref() else {
        println!("nothing playing");
        return;
    };

    let state = if status.idle {
        "◼"
    } else if status.paused {
        "⏸"
    } else {
        "▶"
    };
    let position = status
        .position_secs
        .map(format_duration)
        .unwrap_or_else(|| "--:--".into());
    let duration = status
        .duration_secs
        .map(format_duration)
        .unwrap_or_else(|| "--:--".into());

    println!("{state} {}", track.display_label());
    println!("  {position} / {duration}{}", progress_bar(status));
    if status.has_video {
        println!(
            "  video: {}",
            if status.video_visible {
                "popped out"
            } else {
                "hidden"
            }
        );
    }
    if status.queue_len > 0 {
        let position_in_queue = status
            .queue_index
            .map(|i| format!("{} of {}", i + 1, status.queue_len))
            .unwrap_or_else(|| format!("{} tracks", status.queue_len));
        let mut flags = vec![format!("queue {position_in_queue}")];
        if status.repeat != RepeatMode::Off {
            flags.push(format!("repeat {}", status.repeat.as_str()));
        }
        if !status.loop_positions.is_empty() {
            flags.push(format!("loop {} rows", status.loop_positions.len()));
        }
        if status.shuffle {
            flags.push("shuffle".into());
        }
        println!("  {}", flags.join(" · "));
    }
}

fn progress_bar(status: &PlayerStatus) -> String {
    let (Some(position), Some(duration)) = (status.position_secs, status.duration_secs) else {
        return String::new();
    };
    if !(duration > 0.0) {
        return String::new();
    }
    const WIDTH: usize = 24;
    let filled = ((position / duration) * WIDTH as f64)
        .round()
        .clamp(0.0, WIDTH as f64) as usize;
    format!("  {}{}", "━".repeat(filled), "─".repeat(WIDTH - filled))
}

enum Seek {
    Absolute(f64),
    Relative(f64),
}

/// Parse `+30`, `-1:00`, `90`, `12:30` or `1:12:44`.
fn parse_seek(input: &str) -> Result<Seek> {
    let trimmed = input.trim();
    let (sign, rest) = match trimmed.strip_prefix('+') {
        Some(rest) => (1.0, rest),
        None => match trimmed.strip_prefix('-') {
            Some(rest) => (-1.0, rest),
            None => (0.0, trimmed),
        },
    };
    let seconds = parse_timecode(rest)
        .ok_or_else(|| anyhow!("could not read \"{input}\" as a time or offset"))?;
    Ok(if sign == 0.0 {
        Seek::Absolute(seconds)
    } else {
        Seek::Relative(sign * seconds)
    })
}

/// `SS`, `MM:SS`, or `HH:MM:SS` in seconds.
fn parse_timecode(input: &str) -> Option<f64> {
    if input.is_empty() {
        return None;
    }
    let mut total = 0.0;
    for part in input.split(':') {
        let value: f64 = part.trim().parse().ok()?;
        if value < 0.0 {
            return None;
        }
        total = total * 60.0 + value;
    }
    total.is_finite().then_some(total)
}

fn parse_on_off(input: &str) -> Result<bool> {
    match input.trim().to_ascii_lowercase().as_str() {
        "on" | "yes" | "true" | "1" => Ok(true),
        "off" | "no" | "false" | "0" => Ok(false),
        other => Err(anyhow!("expected on or off, got \"{other}\"")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_seconds_and_timecodes() {
        assert_eq!(parse_timecode("90"), Some(90.0));
        assert_eq!(parse_timecode("1:30"), Some(90.0));
        assert_eq!(parse_timecode("1:12:44"), Some(4364.0));
        assert_eq!(parse_timecode(""), None);
        assert_eq!(parse_timecode("abc"), None);
    }

    #[test]
    fn distinguishes_absolute_from_relative_seeks() {
        assert!(matches!(parse_seek("1:12:44").unwrap(), Seek::Absolute(s) if s == 4364.0));
        assert!(matches!(parse_seek("+30").unwrap(), Seek::Relative(s) if s == 30.0));
        assert!(matches!(parse_seek("-1:00").unwrap(), Seek::Relative(s) if s == -60.0));
        assert!(parse_seek("nonsense").is_err());
    }

    #[test]
    fn reads_on_and_off() {
        assert!(parse_on_off("on").unwrap());
        assert!(!parse_on_off("OFF").unwrap());
        assert!(parse_on_off("maybe").is_err());
    }
}
