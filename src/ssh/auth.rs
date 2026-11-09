//! SSH public-key authentication that mirrors OpenSSH behaviour.
//!
//! Order: agent identities, then identity files. An OpenSSH user certificate
//! (`<key>-cert.pub`) is tried before the bare key. Every failed step is
//! recorded, so the final error explains what happened.

use std::path::{Path, PathBuf};

use anyhow::Result;
use async_ssh2_lite::{AsyncSession, TokioTcpStream};
use base64::Engine;

use super::agent_sock::ensure_ssh_auth_sock;
use super::config::SshHostConfig;

const DEFAULT_KEYS: [&str; 3] = ["id_ed25519", "id_rsa", "id_ecdsa"];

/// Runs key authentication. Returns `Ok(())` when the session is authenticated.
///
/// # Errors
///
/// Returns an error that lists each attempted method and its failure reason.
pub async fn authenticate_with_keys(
    session: &AsyncSession<TokioTcpStream>,
    user: &str,
    config: &SshHostConfig,
    step_timeout: impl Fn() -> std::time::Duration,
) -> Result<()> {
    let mut notes: Vec<String> = Vec::new();
    let key_files = candidate_key_files(config, &mut notes);

    if try_agent(session, user, config, &key_files, &mut notes, &step_timeout).await {
        return Ok(());
    }
    if try_key_files(session, user, &key_files, &mut notes, &step_timeout).await {
        return Ok(());
    }

    anyhow::bail!(
        "SSH key authentication failed for {user}. Attempts:\n{}\n{}",
        notes
            .iter()
            .map(|n| format!("  - {n}"))
            .collect::<Vec<_>>()
            .join("\n"),
        hint(&notes)
    )
}

fn hint(notes: &[String]) -> &'static str {
    if notes.iter().any(|n| n.contains("passphrase")) {
        "Hint: load the passphrase key with `ssh-add`, then retry."
    } else if notes.iter().any(|n| n.starts_with("agent: no socket")) {
        "Hint: start ssh-agent or set SSH_AUTH_SOCK in the MCP server env."
    } else {
        "Hint: check that the key or certificate is accepted by the server (`ssh -v`)."
    }
}

/// Returns the identity files in OpenSSH order.
fn candidate_key_files(config: &SshHostConfig, notes: &mut Vec<String>) -> Vec<PathBuf> {
    if let Some(file) = &config.identity_file {
        if !file.exists() {
            notes.push(format!("identity file {} not found", file.display()));
            return Vec::new();
        }
        return vec![file.clone()];
    }
    if config.identities_only {
        notes.push("IdentitiesOnly is set but no IdentityFile was given".to_string());
        return Vec::new();
    }
    let home = std::env::var("HOME").unwrap_or_default();
    DEFAULT_KEYS
        .iter()
        .map(|name| Path::new(&home).join(".ssh").join(name))
        .filter(|path| path.exists())
        .collect()
}

fn cert_path(key: &Path) -> PathBuf {
    let mut name = key.as_os_str().to_owned();
    name.push("-cert.pub");
    PathBuf::from(name)
}

fn pub_path(key: &Path) -> PathBuf {
    let mut name = key.as_os_str().to_owned();
    name.push(".pub");
    PathBuf::from(name)
}

/// Decodes the key blob from an OpenSSH `.pub` file (`type base64 comment`).
fn read_pub_blob(path: &Path) -> Option<Vec<u8>> {
    let text = std::fs::read_to_string(path).ok()?;
    let b64 = text.split_whitespace().nth(1)?;
    base64::engine::general_purpose::STANDARD.decode(b64).ok()
}

async fn try_agent(
    session: &AsyncSession<TokioTcpStream>,
    user: &str,
    config: &SshHostConfig,
    key_files: &[PathBuf],
    notes: &mut Vec<String>,
    step_timeout: &impl Fn() -> std::time::Duration,
) -> bool {
    let sock = match ensure_ssh_auth_sock() {
        Ok(sock) => sock,
        Err(reason) => {
            notes.push(format!("agent: no socket ({reason})"));
            return false;
        }
    };

    // With IdentitiesOnly, offer only agent keys that match a configured IdentityFile.
    let allowed: Option<Vec<Vec<u8>>> = config.identities_only.then(|| {
        key_files
            .iter()
            .flat_map(|key| {
                [
                    read_pub_blob(&pub_path(key)),
                    read_pub_blob(&cert_path(key)),
                ]
            })
            .flatten()
            .collect()
    });
    if allowed.as_ref().is_some_and(Vec::is_empty) {
        notes.push("agent: skipped (IdentitiesOnly and no matching .pub file)".to_string());
        return false;
    }

    let mut offered = 0usize;
    let attempt = tokio::time::timeout(
        step_timeout(),
        session.userauth_agent_with_try_next_with_callback(user, |identities| {
            let kept: Vec<_> = match &allowed {
                Some(blobs) => identities
                    .into_iter()
                    .filter(|id| blobs.iter().any(|b| b.as_slice() == id.blob()))
                    .collect(),
                None => identities,
            };
            offered = kept.len();
            kept
        }),
    )
    .await;

    match attempt {
        Ok(Ok(())) if session.authenticated() => true,
        Ok(Ok(())) => {
            notes.push("agent: server did not accept the session".to_string());
            false
        }
        Ok(Err(error)) => {
            notes.push(format!(
                "agent {}: {error} ({offered} key(s) offered; the agent may lack the OpenSSH certificate)",
                sock.display()
            ));
            false
        }
        Err(_) => {
            notes.push(format!("agent {}: timed out", sock.display()));
            false
        }
    }
}

async fn try_key_files(
    session: &AsyncSession<TokioTcpStream>,
    user: &str,
    key_files: &[PathBuf],
    notes: &mut Vec<String>,
    step_timeout: &impl Fn() -> std::time::Duration,
) -> bool {
    for key in key_files {
        let cert = cert_path(key);
        let variants: Vec<Option<PathBuf>> = if cert.exists() {
            vec![Some(cert), None]
        } else {
            vec![None]
        };
        for pubkey in variants {
            let label = match &pubkey {
                Some(cert) => format!("{} with certificate {}", key.display(), cert.display()),
                None => key.display().to_string(),
            };
            let attempt = tokio::time::timeout(
                step_timeout(),
                session.userauth_pubkey_file(user, pubkey.as_deref(), key, None),
            )
            .await;
            match attempt {
                Ok(Ok(())) if session.authenticated() => return true,
                Ok(Ok(())) => notes.push(format!("file {label}: not accepted")),
                Ok(Err(error)) => {
                    notes.push(format!("file {label}: {}", explain(&error.to_string())))
                }
                Err(_) => notes.push(format!("file {label}: timed out")),
            }
        }
    }
    false
}

fn explain(error: &str) -> String {
    if error.contains("Wrong passphrase") || error.contains("passphrase") {
        format!("{error} (key needs a passphrase)")
    } else {
        error.to_string()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn cert_and_pub_paths_append_suffix() {
        let key = Path::new("/h/.ssh/id_ed25519");
        assert_eq!(cert_path(key), Path::new("/h/.ssh/id_ed25519-cert.pub"));
        assert_eq!(pub_path(key), Path::new("/h/.ssh/id_ed25519.pub"));
    }

    #[test]
    fn pub_blob_decodes_second_field() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("k.pub");
        std::fs::write(&file, "ssh-ed25519 AAEC k@h\n").unwrap();
        assert_eq!(read_pub_blob(&file), Some(vec![0, 1, 2]));
    }

    #[test]
    fn identities_only_without_file_is_noted() {
        let config = SshHostConfig {
            host: "h".into(),
            hostname: Some("h".into()),
            user: Some("u".into()),
            port: None,
            identity_file: None,
            proxy_command: None,
            proxy_use_fdpass: false,
            identities_only: true,
        };
        let mut notes = Vec::new();
        assert!(candidate_key_files(&config, &mut notes).is_empty());
        assert_eq!(notes.len(), 1);
    }
}
