//! SSH agent socket discovery for GUI-spawned processes.
//!
//! Cursor often starts MCP without [`SSH_AUTH_SOCK`]. Passphrase-protected
//! private keys then fail file auth while interactive OpenSSH still works via
//! the launchd agent.

use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;

/// Ensures `SSH_AUTH_SOCK` points at a usable agent socket.
///
/// Discovers the socket via existing env, macOS `launchctl`, or common `/tmp`
/// paths, then sets the process env so libssh2 agent auth can use it.
///
/// # Returns
///
/// Path to the usable socket.
///
/// # Errors
///
/// Returns the reason for each failed discovery step.
pub fn ensure_ssh_auth_sock() -> Result<PathBuf, String> {
    let mut reasons: Vec<String> = Vec::new();

    match std::env::var_os("SSH_AUTH_SOCK") {
        Some(sock) => {
            let path = PathBuf::from(sock);
            if socket_usable(&path) {
                return Ok(path);
            }
            reasons.push(format!("env SSH_AUTH_SOCK={} is stale", path.display()));
        }
        None => reasons.push("env SSH_AUTH_SOCK is unset".to_string()),
    }

    #[cfg(target_os = "macos")]
    match launchctl_ssh_auth_sock() {
        Ok(sock) => {
            set_ssh_auth_sock(&sock);
            tracing::info!(path = %sock.display(), "Discovered SSH_AUTH_SOCK via launchctl");
            return Ok(sock);
        }
        Err(reason) => reasons.push(format!("launchctl: {reason}")),
    }

    if let Some(sock) = scan_tmp_agent_socks() {
        set_ssh_auth_sock(&sock);
        tracing::info!(path = %sock.display(), "Discovered SSH_AUTH_SOCK via /tmp scan");
        return Ok(sock);
    }
    reasons.push("no agent socket under /tmp, /private/tmp, /var/run".to_string());

    Err(reasons.join("; "))
}

#[cfg(target_os = "macos")]
fn launchctl_ssh_auth_sock() -> Result<PathBuf, String> {
    let output = Command::new("launchctl")
        .args(["getenv", "SSH_AUTH_SOCK"])
        .output()
        .map_err(|e| format!("cannot run launchctl ({e})"))?;
    let sock = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() || sock.is_empty() {
        return Err("SSH_AUTH_SOCK not set in launchd".to_string());
    }
    let path = PathBuf::from(sock);
    if socket_usable(&path) {
        Ok(path)
    } else {
        Err(format!("socket {} is not reachable", path.display()))
    }
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

/// Indicates whether `path` is an existing Unix socket.
fn socket_usable(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::metadata(path).is_ok_and(|m| m.file_type().is_socket())
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
