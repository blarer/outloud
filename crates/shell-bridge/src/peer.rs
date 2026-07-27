//! Peer credential checks for the unix socket.
//!
//! The socket accepts a buffer and hands back a replacement that a shell
//! will splice into its command line. Whoever can speak on it can therefore
//! stage text one keypress away from execution. Filesystem permissions on
//! the socket (0700 parent dir, 0600 socket) are the first gate; this module
//! is the second, because permissions stop protecting you the moment the
//! path is wrong (world-writable $TMPDIR ancestors, inherited fds, or a
//! future refactor that moves the socket). Belt and suspenders on a
//! command-execution surface.
//!
//! macOS: `getsockopt(SOL_LOCAL, LOCAL_PEERCRED)` fills `xucred`.
//! Linux: `getsockopt(SOL_SOCKET, SO_PEERCRED)` fills `ucred`.
//! Both report the credentials the peer held at `connect()` time, which the
//! peer cannot forge (the kernel records them; no message content involved).

use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;

/// Effective uid of the connected peer, straight from the kernel.
pub fn peer_uid(stream: &UnixStream) -> std::io::Result<u32> {
    let fd = stream.as_raw_fd();

    #[cfg(target_os = "macos")]
    {
        // SAFETY: xucred is a plain-old-data struct; the kernel writes at
        // most `len` bytes into it, and we verify len and version afterward.
        let mut cred: libc::xucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::xucred>() as libc::socklen_t;
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_LOCAL,
                libc::LOCAL_PEERCRED,
                &mut cred as *mut _ as *mut libc::c_void,
                &mut len,
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // XUCRED_VERSION mismatch would mean the struct layout changed under
        // us; refusing is safer than reading a misaligned uid.
        if cred.cr_version != libc::XUCRED_VERSION {
            return Err(std::io::Error::other("unexpected xucred version"));
        }
        Ok(cred.cr_uid)
    }

    #[cfg(target_os = "linux")]
    {
        // SAFETY: same shape as above with Linux's ucred.
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut cred as *mut _ as *mut libc::c_void,
                &mut len,
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(cred.uid)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = fd;
        // No peer credential API we know how to use: refuse rather than
        // silently accept, because this gate exists for a reason.
        Err(std::io::Error::other(
            "peer credential check unsupported on this platform",
        ))
    }
}

/// Accept only the daemon's own uid. Root is deliberately NOT allowed: root
/// does not need our socket to run commands as us via legitimate means, so
/// a root connection here is at best confused tooling and at worst a
/// container/namespace uid-mapping surprise. Rejecting it costs nothing.
pub fn peer_is_self(stream: &UnixStream) -> std::io::Result<bool> {
    // SAFETY: geteuid cannot fail.
    let me = unsafe { libc::geteuid() };
    Ok(peer_uid(stream)? == me)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn own_connection_reports_own_uid() {
        let dir = std::env::temp_dir().join(format!("sb-peer-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.sock");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let client = UnixStream::connect(&path).unwrap();
        let (server_side, _) = listener.accept().unwrap();

        let me = unsafe { libc::geteuid() };
        assert_eq!(peer_uid(&server_side).unwrap(), me);
        assert_eq!(peer_uid(&client).unwrap(), me);
        assert!(peer_is_self(&server_side).unwrap());

        drop((client, server_side, listener));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }
}
