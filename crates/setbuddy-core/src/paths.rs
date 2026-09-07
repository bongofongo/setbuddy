//! Where Setwave keeps its state.

use std::path::PathBuf;

/// Root for the database and the shared mpv socket.
///
/// `SETWAVE_STATE_DIR` overrides it, which keeps tests off the real library.
pub fn state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SETWAVE_STATE_DIR") {
        return PathBuf::from(dir);
    }
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("Setwave")
}

pub fn database_path() -> PathBuf {
    state_dir().join("setwave.db")
}

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
/// sandboxed `SETWAVE_STATE_DIR`, a long home directory — it falls back to a
/// short path derived from a hash of the state directory. The hash keeps the
/// name stable across invocations, which is what makes adoption work, and keeps
/// two different state directories from colliding on one socket.
pub fn engine_socket_path() -> PathBuf {
    socket_path_for(&state_dir(), &std::env::temp_dir())
}

fn socket_path_for(state_dir: &std::path::Path, temp_dir: &std::path::Path) -> PathBuf {
    let preferred = state_dir.join("mpv.sock");
    if fits(&preferred) {
        return preferred;
    }
    let name = format!("setwave-{}.sock", short_hash(state_dir));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn short_state_directories_keep_the_socket_beside_the_database() {
        let path = socket_path_for(Path::new("/Users/x/Library/Setwave"), Path::new("/tmp"));
        assert_eq!(path, PathBuf::from("/Users/x/Library/Setwave/mpv.sock"));
    }

    #[test]
    fn deep_state_directories_fall_back_to_a_short_path() {
        let deep = Path::new(
            "/private/tmp/claude-501/-Users-someone-projects-setwave/             1641e1f3-c0f7-4dab-b6f1-8a40d1ccf300/scratchpad/state",
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
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}
