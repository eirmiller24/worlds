//! `.worlds/server.json`: how clients find the server (`docs/api.md`,
//! "Discovery").

use std::fs;
use std::io::{self, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The contents of `.worlds/server.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    /// The WebSocket port on 127.0.0.1.
    pub port: u16,
    /// 64 hex characters; `session.hello` needs it.
    pub token: String,
    /// The process serving it.
    pub pid: u32,
    /// The absolute, canonical project directory.
    pub project: String,
}

impl ServerInfo {
    /// `<dir>/.worlds/server.json`.
    pub fn path(dir: &Path) -> PathBuf {
        dir.join(".worlds").join("server.json")
    }

    /// Read the file; `Ok(None)` when there is none.
    pub fn read(dir: &Path) -> io::Result<Option<ServerInfo>> {
        match fs::read_to_string(Self::path(dir)) {
            Ok(text) => serde_json::from_str(&text)
                .map(Some)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Live: the pid is a running process and the port accepts a
    /// connection. Otherwise the file is stale.
    pub fn is_live(&self) -> bool {
        pid_alive(self.pid)
            && TcpStream::connect_timeout(
                &SocketAddr::from(([127, 0, 0, 1], self.port)),
                Duration::from_millis(300),
            )
            .is_ok()
    }

    /// Write the file atomically with mode 0600: a temp file in `.worlds/`,
    /// then a rename over the old one.
    pub(crate) fn write(&self, dir: &Path) -> io::Result<()> {
        let wdir = dir.join(".worlds");
        fs::create_dir_all(&wdir)?;
        let tmp = wdir.join(format!(".server.json.{}.tmp", self.pid));
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)? + "\n";
        let res = (|| {
            let mut f = open_private(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
            fs::rename(&tmp, Self::path(dir))
        })();
        if res.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        res
    }

    /// Remove the file if it still holds this server's pid and token.
    pub(crate) fn remove_if_ours(&self, dir: &Path) {
        if let Ok(Some(on_disk)) = Self::read(dir)
            && on_disk.pid == self.pid
            && on_disk.token == self.token
        {
            let _ = fs::remove_file(Self::path(dir));
        }
    }
}

/// Take the project's server lock, `<dir>/.worlds/server.lock`: an
/// exclusive advisory lock held for as long as the returned file is open
/// (the OS releases it when the process dies). `Ok(None)` when another open
/// file holds it, i.e. another server is starting or running. Where the
/// filesystem can't lock, `Ok(Some(None))`: go on without one.
pub(crate) fn lock_project(dir: &Path) -> io::Result<Option<Option<fs::File>>> {
    let wdir = dir.join(".worlds");
    fs::create_dir_all(&wdir)?;
    let f = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(wdir.join("server.lock"))?;
    match f.try_lock() {
        Ok(()) => Ok(Some(Some(f))),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(e)) => {
            tracing::warn!("can't lock {}: {e}; going on without it", wdir.display());
            Ok(Some(None))
        }
    }
}

#[cfg(unix)]
fn open_private(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_private(path: &Path) -> io::Result<fs::File> {
    fs::File::create(path)
}

/// Whether a process with this pid exists (Linux: `/proc/<pid>`; elsewhere
/// assumed alive, and the port check decides).
fn pid_alive(pid: u32) -> bool {
    let proc = Path::new("/proc");
    if proc.is_dir() {
        proc.join(pid.to_string()).exists()
    } else {
        true
    }
}

/// A new token: 32 random bytes as 64 hex characters.
pub(crate) fn new_token() -> String {
    let bytes: [u8; 32] = rand::random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Compare a presented token with ours in time that doesn't depend on
/// where they differ.
pub(crate) fn token_eq(presented: &str, ours: &str) -> bool {
    let (a, b) = (presented.as_bytes(), ours.as_bytes());
    let mut diff = (a.len() ^ b.len()) as u8 | u8::from(a.len() != b.len());
    for (i, ours) in b.iter().enumerate() {
        diff |= a.get(i).copied().unwrap_or(0) ^ ours;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        let t = new_token();
        assert_eq!(t.len(), 64);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(token_eq(&t, &t));
        assert!(!token_eq(&t[..63], &t));
        assert!(!token_eq("", &t));
        let mut other = t.clone().into_bytes();
        other[10] = if other[10] == b'a' { b'b' } else { b'a' };
        assert!(!token_eq(std::str::from_utf8(&other).unwrap(), &t));
    }

    #[cfg(unix)]
    #[test]
    fn written_private_and_removed_only_when_ours() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let info = ServerInfo {
            port: 1,
            token: new_token(),
            pid: std::process::id(),
            project: dir.path().to_string_lossy().into_owned(),
        };
        info.write(dir.path()).unwrap();
        let mode = fs::metadata(ServerInfo::path(dir.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(ServerInfo::read(dir.path()).unwrap(), Some(info.clone()));
        let other = ServerInfo {
            token: new_token(),
            ..info.clone()
        };
        other.remove_if_ours(dir.path());
        assert!(ServerInfo::path(dir.path()).exists());
        info.remove_if_ours(dir.path());
        assert!(!ServerInfo::path(dir.path()).exists());
        // Only the final file remains in .worlds/ (no temp files).
        assert_eq!(fs::read_dir(dir.path().join(".worlds")).unwrap().count(), 0);
    }
}
