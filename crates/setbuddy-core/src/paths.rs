//! Where Setbuddy keeps its state.

use std::path::{Path, PathBuf};

/// The database, and what it and the state directory were called before the
/// app was renamed. The old names exist only so an existing library survives
/// that rename; nothing else may depend on them.
const DATABASE_FILE: &str = "setbuddy.db";
const PREVIOUS_APP_NAME: &str = "Setwave";
const PREVIOUS_DATABASE_FILE: &str = "setwave.db";

/// Root for the database and the shared engine socket.
///
/// `SETBUDDY_STATE_DIR` overrides it, which keeps tests off the real library.
pub fn state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SETBUDDY_STATE_DIR") {
        return PathBuf::from(dir);
    }
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("Setbuddy")
}

pub fn database_path() -> PathBuf {
    state_dir().join(DATABASE_FILE)
}

/// The socket successive processes meet on. Deliberately not named after any
/// backend: core must not know which engine ends up listening there.
const ENGINE_SOCKET_FILE: &str = "engine.sock";

/// Longest socket path we will hand to the kernel.
///
/// `sockaddr_un.sun_path` is 104 bytes on macOS and 108 on Linux, including the
/// terminator; binding a longer path fails with "path must be shorter than
/// SUN_LEN". 100 leaves headroom on both.
const MAX_SOCKET_PATH_BYTES: usize = 100;

/// The well-known engine socket. Successive CLI invocations meet here, which is
/// what lets them drive one continuous playback session without a daemon.
///
/// Normally this sits beside the database. When the state directory is deep
/// enough that the path would not fit in `sun_path` — a nested checkout, a
/// sandboxed `SETBUDDY_STATE_DIR`, a long home directory — it falls back to a
/// short path derived from a hash of the state directory. The hash keeps the
/// name stable across invocations, which is what makes adoption work, and keeps
/// two different state directories from colliding on one socket.
pub fn engine_socket_path() -> PathBuf {
    socket_path_for(&state_dir(), &std::env::temp_dir())
}

fn socket_path_for(state_dir: &std::path::Path, temp_dir: &std::path::Path) -> PathBuf {
    let preferred = state_dir.join(ENGINE_SOCKET_FILE);
    if fits(&preferred) {
        return preferred;
    }
    let name = format!("setbuddy-{}.sock", short_hash(state_dir));
    let in_temp = temp_dir.join(&name);
    if fits(&in_temp) {
        return in_temp;
    }
    // Last resort: the shortest directory that is writable everywhere we run.
    PathBuf::from("/tmp").join(name)
}

fn fits(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().len() <= MAX_SOCKET_PATH_BYTES
}

/// FNV-1a. Not cryptographic — it only has to be stable and short.
fn short_hash(path: &std::path::Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.as_os_str().as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Directories a GUI launch drops from `PATH`.
///
/// A process started from Finder — or from `open` — inherits launchd's `PATH`,
/// which is `/usr/bin:/bin:/usr/sbin:/sbin`. The shell's is never consulted, so
/// the helper tools Setbuddy shells out to (`ffprobe`, `ffmpeg`, the external
/// player) are invisible to the bundled app even though they are installed, and
/// every one of them reports itself missing. These are the prefixes the two
/// package managers anyone on macOS actually uses install into.
#[cfg(target_os = "macos")]
const TOOL_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"];
/// Elsewhere the session hands a launched app the same `PATH` a shell has, so
/// there is nothing to put back. Left empty rather than guessed at.
#[cfg(not(target_os = "macos"))]
const TOOL_DIRS: &[&str] = &[];

/// Put those directories back on `PATH` for this process and its children.
///
/// Call once from each entry point, before anything looks for a tool and before
/// any thread is spawned: this mutates process-global state that every later
/// `Command` inherits, and reading the environment from another thread while it
/// is written is undefined. `Once` makes a second call free rather than unsafe.
///
/// Existing entries keep their order and their precedence, so a run from a shell
/// behaves exactly as it did before — this only ever appends.
pub fn ensure_tool_path() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let current = std::env::var_os("PATH").unwrap_or_default();
        std::env::set_var("PATH", augmented_path(&current, TOOL_DIRS));
    });
}

/// `PATH` with each of `extra` appended unless it is already present.
///
/// Empty components are dropped on the way through: on Unix an empty entry means
/// the working directory, and an unset `PATH` would otherwise produce one.
fn augmented_path(current: &std::ffi::OsStr, extra: &[&str]) -> std::ffi::OsString {
    let mut dirs: Vec<PathBuf> = std::env::split_paths(current)
        .filter(|d| !d.as_os_str().is_empty())
        .collect();
    for candidate in extra.iter().map(PathBuf::from) {
        if !dirs.contains(&candidate) {
            dirs.push(candidate);
        }
    }
    // Only fails on a component containing the separator, which none of ours
    // does; leaving PATH untouched is the right answer if it ever did.
    std::env::join_paths(dirs).unwrap_or_else(|_| current.to_os_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::path::Path;

    /// The bug this exists for: the app launched from Finder sees only the
    /// system directories, so every helper tool it shells out to looks absent.
    #[test]
    fn a_launchd_path_regains_the_package_manager_directories() {
        let gui = OsStr::new("/usr/bin:/bin:/usr/sbin:/sbin");
        let augmented = augmented_path(gui, &["/opt/homebrew/bin", "/usr/local/bin"]);
        let dirs: Vec<PathBuf> = std::env::split_paths(&augmented).collect();
        assert!(dirs.contains(&PathBuf::from("/opt/homebrew/bin")));
        assert!(dirs.contains(&PathBuf::from("/usr/local/bin")));
    }

    /// A developer's own PATH must still decide which binary wins, so what was
    /// there stays in front of what we add.
    #[test]
    fn the_existing_path_keeps_its_order_and_its_precedence() {
        let shell = OsStr::new("/my/tools:/usr/bin");
        let augmented = augmented_path(shell, &["/opt/homebrew/bin"]);
        let dirs: Vec<PathBuf> = std::env::split_paths(&augmented).collect();
        assert_eq!(
            dirs,
            [
                PathBuf::from("/my/tools"),
                PathBuf::from("/usr/bin"),
                PathBuf::from("/opt/homebrew/bin"),
            ]
        );
    }

    #[test]
    fn a_directory_already_on_path_is_not_added_again() {
        let already = OsStr::new("/opt/homebrew/bin:/usr/bin");
        let augmented = augmented_path(already, &["/opt/homebrew/bin", "/usr/local/bin"]);
        let dirs: Vec<PathBuf> = std::env::split_paths(&augmented).collect();
        assert_eq!(
            dirs.iter()
                .filter(|d| *d == &PathBuf::from("/opt/homebrew/bin"))
                .count(),
            1
        );
        assert_eq!(dirs.len(), 3);
    }

    /// An empty component means the working directory, which is not somewhere
    /// we want to be looking for an executable.
    #[test]
    fn an_unset_path_does_not_gain_the_working_directory() {
        let augmented = augmented_path(OsStr::new(""), &["/opt/homebrew/bin"]);
        let dirs: Vec<PathBuf> = std::env::split_paths(&augmented).collect();
        assert_eq!(dirs, [PathBuf::from("/opt/homebrew/bin")]);
    }

    /// The rename must not cost the user their library.
    #[test]
    fn a_library_under_the_previous_name_is_adopted_whole() {
        let root = std::env::temp_dir().join(format!("setbuddy-adopt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let previous = root.join(PREVIOUS_APP_NAME);
        let current = root.join("Setbuddy");
        std::fs::create_dir_all(previous.join("artwork")).unwrap();
        for suffix in ["", "-wal", "-shm"] {
            std::fs::write(
                previous.join(format!("{PREVIOUS_DATABASE_FILE}{suffix}")),
                suffix.as_bytes(),
            )
            .unwrap();
        }
        std::fs::write(previous.join("artwork/cover.jpg"), b"art").unwrap();

        adopt(&previous, &current).unwrap();

        assert!(!previous.exists(), "the old directory is moved, not copied");
        assert!(
            current.join("artwork/cover.jpg").is_file(),
            "artwork came too"
        );
        for suffix in ["", "-wal", "-shm"] {
            let moved = current.join(format!("{DATABASE_FILE}{suffix}"));
            assert_eq!(
                std::fs::read(&moved).unwrap(),
                suffix.as_bytes(),
                "{} must arrive under the new name",
                moved.display()
            );
        }

        // Second run: there is a library in place, so nothing is touched.
        std::fs::create_dir_all(&previous).unwrap();
        std::fs::write(previous.join("stray.db"), b"").unwrap();
        adopt(&previous, &current).unwrap();
        assert!(
            previous.join("stray.db").is_file(),
            "an existing library wins"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn short_state_directories_keep_the_socket_beside_the_database() {
        let path = socket_path_for(Path::new("/Users/x/Library/Setbuddy"), Path::new("/tmp"));
        assert_eq!(
            path,
            PathBuf::from("/Users/x/Library/Setbuddy").join(ENGINE_SOCKET_FILE)
        );
    }

    #[test]
    fn deep_state_directories_fall_back_to_a_short_path() {
        let deep = Path::new(
            "/private/tmp/claude-501/-Users-someone-projects-setbuddy/             1641e1f3-c0f7-4dab-b6f1-8a40d1ccf300/scratchpad/state",
        );
        let path = socket_path_for(deep, Path::new("/tmp"));
        assert!(
            fits(&path),
            "fallback must fit in sun_path: {}",
            path.display()
        );
        assert!(path.starts_with("/tmp"));
    }

    #[test]
    fn the_fallback_is_stable_and_collision_free() {
        let a = Path::new("/very/long/path/".repeat(8).as_str()).to_path_buf();
        let b = Path::new(&format!("{}/other", a.display())).to_path_buf();
        let temp = Path::new("/tmp");
        assert_eq!(
            socket_path_for(&a, temp),
            socket_path_for(&a, temp),
            "the same state dir must always resolve to the same socket"
        );
        assert_ne!(
            socket_path_for(&a, temp),
            socket_path_for(&b, temp),
            "different state dirs must not share a socket"
        );
    }
}

pub fn ensure_state_dir() -> std::io::Result<PathBuf> {
    let dir = state_dir();
    // A library indexed under the old name is the user's work — folders,
    // play counts, resume positions, cached artwork — not ours to drop on the
    // floor because the app was renamed. Best-effort: if it cannot be moved,
    // starting with an empty library beats refusing to start.
    if std::env::var_os("SETBUDDY_STATE_DIR").is_none() && !dir.exists() {
        if let Some(previous) = dirs::data_local_dir().map(|d| d.join(PREVIOUS_APP_NAME)) {
            let _ = adopt(&previous, &dir);
        }
    }
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Move the state directory left by the previous name into place, renaming the
/// database inside it.
fn adopt(previous: &Path, current: &Path) -> std::io::Result<()> {
    if !previous.is_dir() || current.exists() {
        return Ok(());
    }
    std::fs::rename(previous, current)?;
    // SQLite names its write-ahead log and shared-memory file after the
    // database, so all three move together or the log is orphaned — and an
    // orphaned log is playback history that was never folded in.
    for suffix in ["", "-wal", "-shm"] {
        let from = current.join(format!("{PREVIOUS_DATABASE_FILE}{suffix}"));
        if from.exists() {
            std::fs::rename(from, current.join(format!("{DATABASE_FILE}{suffix}")))?;
        }
    }
    Ok(())
}
