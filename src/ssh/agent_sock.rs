//! SSH agent socket discovery for GUI-spawned processes.
//!
//! Cursor often starts MCP without [`SSH_AUTH_SOCK`]. Passphrase-protected
//! private keys then fail file auth while interactive OpenSSH still works via
//! the launchd agent.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Ensures `SSH_AUTH_SOCK` points at a usable agent socket.
///
/// Discovers the socket via existing env, macOS `launchctl`, or common `/tmp`
/// paths, then sets the process env so libssh2 agent auth can use it.
///
/// # Returns
///
/// Path to the usable socket when found.
pub fn ensure_ssh_auth_sock() -> Option<PathBuf> {
    if let Some(sock) = env_sock_if_usable() {
        return Some(sock);
    }

    #[cfg(target_os = "macos")]
    if let Some(sock) = launchctl_ssh_auth_sock() {
        set_ssh_auth_sock(&sock);
        tracing::info!(path = %sock.display(), "Discovered SSH_AUTH_SOCK via launchctl");
        return Some(sock);
    }

    if let Some(sock) = scan_tmp_agent_socks() {
        set_ssh_auth_sock(&sock);
        tracing::info!(path = %sock.display(), "Discovered SSH_AUTH_SOCK via /tmp scan");
        return Some(sock);
    }

    tracing::debug!("No usable SSH_AUTH_SOCK found");
    None
}

fn env_sock_if_usable() -> Option<PathBuf> {
    let sock = std::env::var_os("SSH_AUTH_SOCK")?;
    let path = PathBuf::from(sock);
    if socket_usable(&path) {
        Some(path)
    } else {
        tracing::debug!(path = %path.display(), "SSH_AUTH_SOCK set but not usable");
        None
    }
}

#[cfg(target_os = "macos")]
fn launchctl_ssh_auth_sock() -> Option<PathBuf> {
    let output = Command::new("launchctl")
        .args(["getenv", "SSH_AUTH_SOCK"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sock = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sock.is_empty() {
        return None;
    }
    let path = PathBuf::from(sock);
    socket_usable(&path).then_some(path)
}

fn scan_tmp_agent_socks() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    for root in ["/tmp", "/private/tmp", "/var/run"] {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("ssh-")
                && let Ok(inner) = std::fs::read_dir(entry.path())
            {
                for file in inner.flatten() {
                    let fname = file.file_name();
                    let fname = fname.to_string_lossy();
                    if fname.starts_with("agent.") {
                        candidates.push(file.path());
                    }
                }
            }
            if name.starts_with("com.apple.launchd.") {
                let listeners = entry.path().join("Listeners");
                if socket_usable(&listeners) {
                    candidates.push(listeners);
                }
            }
        }
    }

    candidates.into_iter().find(|p| socket_usable(p))
}

/// Indicates whether `path` exists as a candidate agent socket.
fn socket_usable(path: &Path) -> bool {
    path.exists()
}

fn set_ssh_auth_sock(path: &Path) {
    // Required so libssh2 agent auth sees the socket.
    // SAFETY: called before concurrent auth work in this process; MCP setup is single-threaded.
    unsafe {
        std::env::set_var("SSH_AUTH_SOCK", path);
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn missing_socket_path_is_unusable() {
        assert!(!socket_usable(Path::new("/no/such/ssh/agent.sock")));
    }
}
