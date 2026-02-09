use anyhow::{Context, Result};
use async_ssh2_lite::{AsyncSession, TokioTcpStream};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use super::channel::ShellChannel;
use super::config::{SshHostConfig, parse_ssh_config};

pub struct SessionState {
    session: AsyncSession<TokioTcpStream>,
    channel: ShellChannel,
}

pub struct SessionManager {
    sessions: Arc<Mutex<HashMap<String, SessionState>>>,
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn connect_by_alias(&self, host_alias: &str) -> Result<()> {
        let config = parse_ssh_config(host_alias)?;
        self.connect_with_config(host_alias, &config).await
    }

    pub async fn connect_with_config(
        &self,
        host_alias: &str,
        config: &SshHostConfig,
    ) -> Result<()> {
        let hostname = config
            .hostname
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Hostname not specified for host '{}'", host_alias))?;
        let port = config.port.unwrap_or(22);
        let user = config
            .user
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("User not specified for host '{}'", host_alias))?;

        if let Some(ref proxy_cmd) = config.proxy_command {
            tracing::debug!(proxy_command = %proxy_cmd, "ProxyCommand specified");
            tracing::debug!(hostname = %hostname, port = %port, "Attempting direct connection");
        }

        let addr = tokio::net::lookup_host(format!("{}:{}", hostname, port))
            .await
            .context("Failed to resolve hostname")?
            .next()
            .ok_or_else(|| anyhow::anyhow!("No address found for {}", hostname))?;

        let mut session = AsyncSession::<TokioTcpStream>::connect(addr, None)
            .await
            .context("Failed to connect")?;

        session.handshake().await.context("SSH handshake failed")?;

        let mut authenticated = false;

        if !config.identities_only {
            tracing::debug!("Attempting SSH agent authentication");
            match session.userauth_agent(user).await {
                Ok(_) => {
                    if session.authenticated() {
                        authenticated = true;
                        tracing::debug!("SSH agent authentication successful");
                    } else {
                        tracing::debug!("SSH agent returned OK but session not authenticated");
                    }
                }
                Err(e) => {
                    tracing::debug!(error = %e, "SSH agent authentication failed");
                }
            }
        } else {
            tracing::debug!("IdentitiesOnly is set, skipping SSH agent");
        }

        if !authenticated {
            if let Some(ref identity_file) = config.identity_file {
                tracing::debug!(path = %identity_file.display(), "Trying identity file");
                if !identity_file.exists() {
                    anyhow::bail!(
                        "Identity file not found: {}. Check that the file exists and path is correct.",
                        identity_file.display()
                    );
                }

                #[cfg(unix)]
                {
                    if let Ok(metadata) = std::fs::metadata(identity_file) {
                        use std::os::unix::fs::PermissionsExt;
                        let mode = metadata.permissions().mode();
                        if mode & 0o077 != 0 {
                            tracing::warn!(
                                path = %identity_file.display(),
                                mode = format!("{:o}", mode & 0o777),
                                "Identity file has insecure permissions, should be 600"
                            );
                        }
                    }
                }

                match session
                    .userauth_pubkey_file(user, None, identity_file, None)
                    .await
                {
                    Ok(_) => {
                        if session.authenticated() {
                            authenticated = true;
                            tracing::debug!("Identity file authentication successful");
                        } else {
                            tracing::debug!("Identity file auth returned OK but not authenticated");
                        }
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "Identity file authentication failed");
                        anyhow::bail!(
                            "Authentication failed with identity file {}. Error: {}. Make sure the key is added to authorized_keys on the remote host.",
                            identity_file.display(),
                            e
                        );
                    }
                }
            } else if !config.identities_only {
                let home = std::env::var("HOME").unwrap_or_else(|_| "~".to_string());
                let key_paths = vec![
                    format!("{}/.ssh/id_ed25519", home),
                    format!("{}/.ssh/id_rsa", home),
                    format!("{}/.ssh/id_ecdsa", home),
                    format!("{}/.ssh/id_dsa", home),
                ];

                tracing::debug!("Trying common SSH key files");
                for key_path in key_paths {
                    let path = PathBuf::from(&key_path);
                    if path.exists() {
                        tracing::trace!(path = %path.display(), "Trying key file");
                        match session.userauth_pubkey_file(user, None, &path, None).await {
                            Ok(_) => {
                                if session.authenticated() {
                                    authenticated = true;
                                    tracing::debug!(path = %path.display(), "Key file authentication successful");
                                    break;
                                } else {
                                    tracing::trace!(path = %path.display(), "Key returned OK but not authenticated");
                                }
                            }
                            Err(e) => {
                                tracing::trace!(path = %path.display(), error = %e, "Key file auth failed");
                            }
                        }
                    } else {
                        tracing::trace!(path = %path.display(), "Key file not found");
                    }
                }
            } else {
                tracing::debug!("IdentitiesOnly set but no IdentityFile specified");
            }
        }

        if !authenticated {
            let mut error_msg = String::from("SSH key authentication failed.");

            if config.identities_only {
                if config.identity_file.is_some() {
                    error_msg.push_str(
                        " IdentitiesOnly is set but the specified identity file failed authentication.",
                    );
                } else {
                    error_msg.push_str(" IdentitiesOnly is set but no IdentityFile was specified.");
                }
            } else {
                error_msg.push_str(" No valid keys found or agent not available.");
            }

            if config.proxy_command.is_some() {
                error_msg.push_str(" ProxyCommand was specified but connection failed.");
            }

            error_msg.push_str(" Check that:");
            if !config.identities_only {
                error_msg.push_str(" SSH agent is running,");
            }
            if config.identity_file.is_some() {
                error_msg.push_str(" the identity file exists and has correct permissions (600),");
            } else if !config.identities_only {
                error_msg.push_str(" keys exist in ~/.ssh/,");
            }
            error_msg
                .push_str(" and the public key is added to authorized_keys on the remote host.");

            anyhow::bail!("{}", error_msg);
        }

        if !session.authenticated() {
            anyhow::bail!("Authentication failed for {}@{}", user, hostname);
        }

        let mut channel = session
            .channel_session()
            .await
            .context("Failed to open channel")?;

        channel
            .request_pty("xterm", None, None)
            .await
            .context("Failed to request PTY")?;

        channel.shell().await.context("Failed to open shell")?;

        let shell_channel = ShellChannel::new(channel);

        let state = SessionState {
            session,
            channel: shell_channel,
        };

        let mut sessions = self.sessions.lock().await;
        sessions.insert(host_alias.to_string(), state);

        Ok(())
    }

    pub async fn connect_direct(
        &self,
        host_alias: &str,
        user: &str,
        host: &str,
        port: Option<u16>,
    ) -> Result<()> {
        let config = SshHostConfig {
            host: host_alias.to_string(),
            hostname: Some(host.to_string()),
            user: Some(user.to_string()),
            port,
            identity_file: None,
            proxy_command: None,
            proxy_use_fdpass: false,
            identities_only: false,
        };
        self.connect_with_config(host_alias, &config).await
    }

    pub async fn connect_with_password(
        &self,
        host_alias: &str,
        user: &str,
        host: &str,
        password: &str,
        port: Option<u16>,
    ) -> Result<()> {
        let port = port.unwrap_or(22);
        let addr = tokio::net::lookup_host(format!("{}:{}", host, port))
            .await
            .context("Failed to resolve hostname")?
            .next()
            .ok_or_else(|| anyhow::anyhow!("No address found for {}", host))?;

        let mut session = AsyncSession::<TokioTcpStream>::connect(addr, None)
            .await
            .context("Failed to connect")?;

        session.handshake().await.context("SSH handshake failed")?;

        session
            .userauth_password(user, password)
            .await
            .context("Password authentication failed")?;

        if !session.authenticated() {
            anyhow::bail!("Authentication failed for {}@{}", user, host);
        }

        let mut channel = session
            .channel_session()
            .await
            .context("Failed to open channel")?;

        channel
            .request_pty("xterm", None, None)
            .await
            .context("Failed to request PTY")?;

        channel.shell().await.context("Failed to open shell")?;

        let shell_channel = ShellChannel::new(channel);

        let state = SessionState {
            session,
            channel: shell_channel,
        };

        let mut sessions = self.sessions.lock().await;
        sessions.insert(host_alias.to_string(), state);

        Ok(())
    }

    #[allow(dead_code)]
    pub async fn is_connected(&self, host_alias: &str) -> bool {
        let sessions = self.sessions.lock().await;
        sessions.contains_key(host_alias)
    }

    pub async fn execute_command(
        &self,
        host_alias: &str,
        command: &str,
        sudo_password: Option<&str>,
    ) -> Result<crate::ssh::channel::CommandOutput> {
        let mut sessions = self.sessions.lock().await;
        let state = sessions
            .get_mut(host_alias)
            .ok_or_else(|| anyhow::anyhow!("Not connected to host '{}'", host_alias))?;

        state.channel.execute_command(command, sudo_password).await
    }

    #[allow(dead_code)]
    pub async fn execute_command_streaming(
        &self,
        host_alias: &str,
        command: &str,
    ) -> Result<String> {
        let mut sessions = self.sessions.lock().await;
        let state = sessions
            .get_mut(host_alias)
            .ok_or_else(|| anyhow::anyhow!("Not connected to host '{}'", host_alias))?;

        state.channel.execute_command_streaming(command).await
    }

    pub async fn disconnect(&self, host_alias: &str) -> Result<()> {
        let mut sessions = self.sessions.lock().await;
        if let Some(state) = sessions.remove(host_alias) {
            state.channel.close().await?;
            state.session.disconnect(None, "Goodbye", None).await?;
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub async fn list_connections(&self) -> Vec<String> {
        let sessions = self.sessions.lock().await;
        sessions.keys().cloned().collect()
    }
}

impl Clone for SessionManager {
    fn clone(&self) -> Self {
        Self {
            sessions: Arc::clone(&self.sessions),
        }
    }
}
