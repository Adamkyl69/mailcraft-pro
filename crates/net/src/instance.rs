//! The single-instance socket's name, and reading it: shared by MailCraft (which listens there)
//! and the `mailcraft-cli mcp` bridge (which connects), so both work out the same path.
//!
//! The socket is in a folder only the user can reach (`mailcraft_store::scratch_dir`); its name
//! has the data folder's hash and the Windows session or the display. On Windows the socket
//! comes from `uds_windows` (std has no Unix sockets there).

use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(unix)]
pub use std::os::unix::net::UnixStream;
#[cfg(windows)]
pub use uds_windows::UnixStream;

/// Longest key file read.
const MAX_KEY: u64 = 256;

/// How long either side waits for the other.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// The running MailCraft's key, from beside its socket.
pub fn read_key(socket: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut key = String::new();
    std::fs::File::open(socket.with_extension("key"))?.take(MAX_KEY).read_to_string(&mut key)?;
    Ok(key.trim().to_string())
}

/// The socket for `data` in this user's private scratch folder.
pub fn socket_path(data: &Path) -> Option<PathBuf> {
    let dir = mailcraft_store::scratch_dir("instance")?;
    Some(dir.join(socket_name(data, &screen())))
}

/// The Windows session, or the display on Linux and the BSDs (`WAYLAND_DISPLAY`, else
/// `DISPLAY`): a launch reaches only a window on its own screen.
fn screen() -> String {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    if cfg!(windows) {
        windows_session(session_id(), var("SESSIONNAME").as_deref())
    } else if cfg!(target_os = "macos") {
        String::new()
    } else {
        var("WAYLAND_DISPLAY").or_else(|| var("DISPLAY")).unwrap_or_default()
    }
}

/// The Windows session this process runs in, from its access token: the console, each Remote
/// Desktop sign-in and session 0 (services) each have their own. `None` if Windows won't say.
#[cfg(windows)]
fn session_id() -> Option<u32> {
    use winsafe::{HACCESSTOKEN, TokenInfo, co::TOKEN_INFORMATION_CLASS};
    match HACCESSTOKEN::GetCurrentProcessToken().GetTokenInformation(TOKEN_INFORMATION_CLASS::SessionId) {
        Ok(TokenInfo::SessionId(id)) => Some(*id),
        _ => None,
    }
}

#[cfg(not(windows))]
fn session_id() -> Option<u32> {
    None
}

/// The Windows session as the socket names it: its id (`session`). A launch from Task Scheduler,
/// a service or a remote shell has no `SESSIONNAME` but still runs in a session, so it finds that
/// session's window instead of opening a second MailCraft on the same data; and a window in
/// another session (the same user at the console and over Remote Desktop) is never reached. Only
/// when the id can't be read does `SESSIONNAME` (`Console`, `RDP-Tcp#3`…) name it (as PdfCraft's
/// single instance).
fn windows_session(session: Option<u32>, session_name: Option<&str>) -> String {
    match session {
        Some(id) => format!("s{id}"),
        None => session_name.unwrap_or_default().to_string(),
    }
}

/// The socket's file name: the data folder's hash (its path, case folded on Windows and macOS,
/// whose file systems mostly are) and the screen, letters, digits, `-` and `_` only.
pub fn socket_name(data: &Path, screen: &str) -> String {
    let data = std::path::absolute(data).unwrap_or_else(|_| data.to_path_buf());
    let mut key = data.to_string_lossy().trim_end_matches(['/', '\\']).to_string();
    if cfg!(any(windows, target_os = "macos")) {
        key = key.to_lowercase();
    }
    let screen: String = screen.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')).collect();
    let screen: String = screen.get(screen.len().saturating_sub(24)..).unwrap_or_default().to_string();
    let hash = mailcraft_store::fnv64(&key);
    if screen.is_empty() { format!("{hash:016x}.sock") } else { format!("{hash:016x}-{screen}.sock") }
}

/// How long one read on the socket waits before it's made again, until [`TIMEOUT`]. Windows can
/// miss waking a read for a close that comes just as the read starts (about one in four hundred),
/// as a launch that asks again closes a connection it sent nothing on; data always wakes it. The
/// read then waited out all of [`TIMEOUT`], and the ask behind it timed out and opened a second
/// window. A read made again sees the close at once.
pub const READ_SLICE: Duration = Duration::from_millis(50);

/// Reads `stream` until `TIMEOUT` from when it's made, [`READ_SLICE`] at a time.
pub struct Patient<'a> {
    pub stream: &'a UnixStream,
    pub until: std::time::Instant,
}

impl<'a> Patient<'a> {
    pub fn new(stream: &'a UnixStream) -> Self {
        Patient { stream, until: std::time::Instant::now() + TIMEOUT }
    }

    /// What can be read without waiting: never a wait with no timeout set.
    fn read_now(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.stream.set_nonblocking(true)?;
        let read = std::io::Read::read(&mut self.stream, buf);
        self.stream.set_nonblocking(false)?;
        read
    }
}

impl std::io::Read for Patient<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::io::ErrorKind::{Interrupted, TimedOut, WouldBlock};
        loop {
            let left = self.until.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return Err(TimedOut.into());
            }
            // A timeout of zero is refused: at least a millisecond.
            if let Err(refused) = self.stream.set_read_timeout(Some(left.min(READ_SLICE).max(Duration::from_millis(1)))) {
                // macOS refuses a timeout on a socket whose other end has closed ("invalid
                // argument"): what's there is read without waiting, the close as the end it is.
                return self.read_now(buf).map_err(|_| refused);
            }
            match std::io::Read::read(&mut self.stream, buf) {
                Err(e) if matches!(e.kind(), TimedOut | WouldBlock | Interrupted) => {}
                read => return read,
            }
        }
    }
}

#[cfg(test)]
mod tests;
