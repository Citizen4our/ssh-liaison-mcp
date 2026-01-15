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

        let addr = tokio::net::lookup_host(format!("{}:{}", hostname, port))
            .await
            .context("Failed to resolve hostname")?
            .next()
            .ok_or_else(|| anyhow::anyhow!("No address found for {}", hostname))?;
        let mut session = AsyncSession::<TokioTcpStream>::connect(addr, None)
            .await
            .context("Failed to connect")?;

        // Perform SSH handshake
        session.handshake().await.context("SSH handshake failed")?;

        // Try SSH agent first
        let mut authenticated = false;
        let debug = std::env::var("SSH_LIAISON_DEBUG").unwrap_or_else(|_| "0".to_string()) == "1";

        if debug {
            eprintln!("[DEBUG] Attempting SSH agent authentication...");
        }
        match session.userauth_agent(user).await {
            Ok(_) => {
                if session.authenticated() {
                    authenticated = true;
                    if debug {
                        eprintln!("[DEBUG] SSH agent authentication successful");
                    }
                } else if debug {
                    eprintln!(
                        "[DEBUG] SSH agent authentication returned OK but session not authenticated"
                    );
                }
            }
            Err(e) => {
                if debug {
                    eprintln!("[DEBUG] SSH agent authentication failed: {}", e);
                }
            }
        }

        if !authenticated {
            if let Some(ref identity_file) = config.identity_file {
                // Try specified identity file
                if debug {
                    eprintln!("[DEBUG] Trying identity file: {}", identity_file.display());
                }
                if identity_file.exists() {
                    match session
                        .userauth_pubkey_file(user, None, identity_file, None)
                        .await
                    {
                        Ok(_) => {
                            if session.authenticated() {
                                authenticated = true;
                                if debug {
                                    eprintln!("[DEBUG] Identity file authentication successful");
                                }
                            }
                        }
                        Err(e) => {
                            if debug {
                                eprintln!("[DEBUG] Identity file authentication failed: {}", e);
                            }
                        }
                    }
                } else {
                    anyhow::bail!("Identity file not found: {}", identity_file.display());
                }
            } else {
                // Try common SSH key files
                let home = std::env::var("HOME").unwrap_or_else(|_| "~".to_string());
                let key_paths = vec![
                    format!("{}/.ssh/id_ed25519", home),
                    format!("{}/.ssh/id_rsa", home),
                    format!("{}/.ssh/id_ecdsa", home),
                    format!("{}/.ssh/id_dsa", home),
                ];

                if debug {
                    eprintln!("[DEBUG] Trying common SSH key files...");
                }
                for key_path in key_paths {
                    let path = PathBuf::from(&key_path);
                    if path.exists() {
                        if debug {
                            eprintln!("[DEBUG] Trying key file: {}", path.display());
                        }
                        match session.userauth_pubkey_file(user, None, &path, None).await {
                            Ok(_) => {
                                if session.authenticated() {
                                    authenticated = true;
                                    if debug {
                                        eprintln!(
                                            "[DEBUG] Key file authentication successful: {}",
                                            path.display()
                                        );
                                    }
                                    break;
                                } else if debug {
                                    eprintln!(
                                        "[DEBUG] Key file authentication returned OK but session not authenticated: {}",
                                        path.display()
                                    );
                                }
                            }
                            Err(e) => {
                                if debug {
                                    eprintln!(
                                        "[DEBUG] Key file authentication failed: {} - {}",
                                        path.display(),
                                        e
                                    );
                                }
                            }
                        }
                    } else if debug {
                        eprintln!("[DEBUG] Key file not found: {}", path.display());
                    }
                }
            }
        }

        if !authenticated {
            anyhow::bail!(
                "SSH key authentication failed. No valid keys found or agent not available. Check that SSH agent is running or keys exist in ~/.ssh/"
            );
        }

        if !session.authenticated() {
            anyhow::bail!("Authentication failed for {}@{}", user, hostname);
        }

        // Open persistent shell channel with PTY
        let mut channel = session
            .channel_session()
            .await
            .context("Failed to open channel")?;

        // Request PTY before opening shell
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

        // Perform SSH handshake
        session.handshake().await.context("SSH handshake failed")?;

        // Authenticate with password
        session
            .userauth_password(user, password)
            .await
            .context("Password authentication failed")?;

        if !session.authenticated() {
            anyhow::bail!("Authentication failed for {}@{}", user, host);
        }

        // Open persistent shell channel with PTY
        let mut channel = session
            .channel_session()
            .await
            .context("Failed to open channel")?;

        // Request PTY before opening shell
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
    ) -> Result<crate::ssh::channel::CommandOutput> {
        let mut sessions = self.sessions.lock().await;
        let state = sessions
            .get_mut(host_alias)
            .ok_or_else(|| anyhow::anyhow!("Not connected to host '{}'", host_alias))?;

        // Use persistent shell channel to preserve state between commands
        state.channel.execute_command(command).await
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
