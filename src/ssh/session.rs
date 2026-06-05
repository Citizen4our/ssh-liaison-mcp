use anyhow::{Context, Result};
use async_ssh2_lite::{AsyncSession, SessionConfiguration, TokioTcpStream};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use super::channel::{CommandTimeouts, ShellChannel};
use super::config::{SshHostConfig, parse_ssh_config};
use super::error::SshCommandError;
use super::jobs::{self, JobSignal, JobState};

const KEEPALIVE_INTERVAL_SECS: u32 = 30;
const JOB_POLL_INTERVAL_MS: u64 = 1000;

#[derive(Debug, Clone)]
struct DirectProfile {
    user: String,
    hostname: String,
    port: Option<u16>,
    password: Option<String>,
}

#[derive(Debug, Clone)]
struct JobMeta {
    host: String,
    read_bytes: u64,
    use_tmux: bool,
}

#[derive(Debug, Clone)]
pub struct JobReport {
    pub job_id: String,
    pub state: JobState,
    pub output: String,
    pub timed_out: bool,
}

fn connect_config() -> SessionConfiguration {
    let mut config = SessionConfiguration::new();
    config.set_keepalive(true, KEEPALIVE_INTERVAL_SECS);
    config
}

pub struct SessionState {
    session: AsyncSession<TokioTcpStream>,
    channel: ShellChannel,
}

struct ManagedSession {
    inner: Mutex<SessionState>,
}

pub struct SessionManager {
    sessions: Arc<Mutex<HashMap<String, Arc<ManagedSession>>>>,
    jobs: Arc<Mutex<HashMap<String, JobMeta>>>,
    tmux_cache: Arc<Mutex<HashMap<String, bool>>>,
    direct_profiles: Arc<Mutex<HashMap<String, DirectProfile>>>,
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
            jobs: Arc::new(Mutex::new(HashMap::new())),
            tmux_cache: Arc::new(Mutex::new(HashMap::new())),
            direct_profiles: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn connect_by_alias(&self, host_alias: &str) -> Result<()> {
        let config = parse_ssh_config(host_alias)?;
        self.connect_with_config(host_alias, &config).await
    }

    async fn initialize_shell(state: &mut SessionState) -> Result<()> {
        state.channel.drain_pending(2000).await;
        state
            .channel
            .send_raw("stty -echo 2>/dev/null; set +e\n")
            .await
            .map_err(|e| anyhow::anyhow!("Shell init failed: {e}"))?;
        state.channel.drain_pending(500).await;
        Ok(())
    }

    async fn open_shell_session(session: AsyncSession<TokioTcpStream>) -> Result<SessionState> {
        let mut channel = session
            .channel_session()
            .await
            .context("Failed to open channel")?;

        channel
            .request_pty("dumb", None, None)
            .await
            .context("Failed to request PTY")?;

        channel.shell().await.context("Failed to open shell")?;

        Ok(SessionState {
            session,
            channel: ShellChannel::new(channel),
        })
    }

    async fn insert_session(&self, host_alias: &str, state: SessionState) {
        self.evict(host_alias).await;
        let managed = Arc::new(ManagedSession {
            inner: Mutex::new(state),
        });
        let mut sessions = self.sessions.lock().await;
        sessions.insert(host_alias.to_string(), managed);
    }

    async fn evict(&self, host_alias: &str) {
        let removed = {
            let mut sessions = self.sessions.lock().await;
            sessions.remove(host_alias)
        };
        if let Some(managed) = removed {
            let mut state = managed.inner.lock().await;
            let _ = state.channel.shutdown().await;
            let _ = state.session.disconnect(None, "Goodbye", None).await;
        }
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

        let mut session = AsyncSession::<TokioTcpStream>::connect(addr, connect_config())
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

        let mut state = Self::open_shell_session(session).await?;
        Self::initialize_shell(&mut state).await?;
        self.insert_session(host_alias, state).await;

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

        let mut session = AsyncSession::<TokioTcpStream>::connect(addr, connect_config())
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

        let mut state = Self::open_shell_session(session).await?;
        Self::initialize_shell(&mut state).await?;
        self.insert_session(host_alias, state).await;

        Ok(())
    }

    pub async fn register_direct_profile(
        &self,
        host_alias: &str,
        user: &str,
        hostname: &str,
        port: Option<u16>,
        password: Option<String>,
    ) {
        self.direct_profiles.lock().await.insert(
            host_alias.to_string(),
            DirectProfile {
                user: user.to_string(),
                hostname: hostname.to_string(),
                port,
                password,
            },
        );
    }

    pub async fn is_connected(&self, host_alias: &str) -> bool {
        let sessions = self.sessions.lock().await;
        sessions.contains_key(host_alias)
    }

    /// Reconnects via ~/.ssh/config or a registered direct profile (after MCP process restart).
    pub async fn ensure_connected(&self, host_alias: &str) -> Result<()> {
        if self.is_connected(host_alias).await {
            return Ok(());
        }
        if parse_ssh_config(host_alias).is_ok() {
            return self.connect_by_alias(host_alias).await;
        }
        let profile = self.direct_profiles.lock().await.get(host_alias).cloned();
        if let Some(profile) = profile {
            if self
                .connect_direct(host_alias, &profile.user, &profile.hostname, profile.port)
                .await
                .is_ok()
            {
                return Ok(());
            }
            if let Some(ref password) = profile.password.filter(|p| !p.is_empty()) {
                return self
                    .connect_with_password(
                        host_alias,
                        &profile.user,
                        &profile.hostname,
                        password,
                        profile.port,
                    )
                    .await;
            }
            anyhow::bail!(
                "Direct profile reconnect failed for '{host_alias}' (keys failed, no password stored)"
            );
        }
        self.connect_by_alias(host_alias).await
    }

    pub async fn execute_command(
        &self,
        host_alias: &str,
        command: &str,
        sudo_password: Option<&str>,
        timeouts: CommandTimeouts,
    ) -> Result<crate::ssh::channel::CommandOutput, SshCommandError> {
        let managed = {
            let sessions = self.sessions.lock().await;
            sessions
                .get(host_alias)
                .cloned()
                .ok_or_else(|| SshCommandError::NotConnected(host_alias.to_string()))?
        };

        let mut state = managed.inner.lock().await;
        match state
            .channel
            .execute_command(command, sudo_password, timeouts)
            .await
        {
            Ok(output) => {
                if let Some(code) = output.exit_code
                    && code != 0
                {
                    return Err(SshCommandError::CommandFailed {
                        exit_code: code,
                        stdout: output.stdout,
                    });
                }
                Ok(output)
            }
            Err(e) => {
                if e.should_evict_session() {
                    drop(state);
                    self.evict(host_alias).await;
                }
                Err(e)
            }
        }
    }

    #[allow(dead_code)]
    pub async fn execute_command_streaming(
        &self,
        host_alias: &str,
        command: &str,
    ) -> Result<String, SshCommandError> {
        let output = self
            .execute_command(host_alias, command, None, CommandTimeouts::default())
            .await?;
        Ok(output.stdout)
    }

    fn control_timeouts() -> CommandTimeouts {
        CommandTimeouts::from_secs(Some(jobs::CONTROL_IDLE_SECS), Some(jobs::CONTROL_MAX_SECS))
    }

    /// Runs a short control command and returns raw stdout, ignoring the
    /// command's non-zero exit code (control scripts always echo a sentinel).
    async fn run_control(
        &self,
        host_alias: &str,
        command: &str,
    ) -> Result<String, SshCommandError> {
        match self
            .execute_command(host_alias, command, None, Self::control_timeouts())
            .await
        {
            Ok(output) => Ok(output.stdout),
            Err(SshCommandError::CommandFailed { stdout, .. }) => Ok(stdout),
            Err(e) => Err(e),
        }
    }

    async fn host_has_tmux(&self, host_alias: &str) -> bool {
        if let Some(cached) = self.tmux_cache.lock().await.get(host_alias) {
            return *cached;
        }
        let available = match self
            .run_control(host_alias, jobs::detect_tmux_command())
            .await
        {
            Ok(out) => jobs::tmux_available(&out),
            Err(_) => false,
        };
        self.tmux_cache
            .lock()
            .await
            .insert(host_alias.to_string(), available);
        available
    }

    pub async fn run_detached(
        &self,
        host_alias: &str,
        command: &str,
    ) -> Result<(String, bool), SshCommandError> {
        let use_tmux = self.host_has_tmux(host_alias).await;
        let job_id = jobs::new_job_id();
        let detach_cmd = jobs::build_detach_command(command, &job_id, use_tmux);
        self.run_control(host_alias, &detach_cmd).await?;

        self.jobs.lock().await.insert(
            job_id.clone(),
            JobMeta {
                host: host_alias.to_string(),
                read_bytes: 0,
                use_tmux,
            },
        );
        Ok((job_id, use_tmux))
    }

    async fn resolve_job_host(&self, host_alias: &str, job_id: &str) -> String {
        match self.jobs.lock().await.get(job_id) {
            Some(meta) => meta.host.clone(),
            None => host_alias.to_string(),
        }
    }

    pub async fn job_status(
        &self,
        host_alias: &str,
        job_id: &str,
    ) -> Result<JobReport, SshCommandError> {
        let host = self.resolve_job_host(host_alias, job_id).await;
        let read_from = {
            let jobs = self.jobs.lock().await;
            jobs.get(job_id).map(|m| m.read_bytes).unwrap_or(0) + 1
        };

        let status_cmd = jobs::build_status_command(job_id, read_from);
        let raw = self.run_control(&host, &status_cmd).await?;
        let (state, size, output, use_tmux) = jobs::parse_status(&raw);

        let mut jobs = self.jobs.lock().await;
        let meta = jobs.entry(job_id.to_string()).or_insert_with(|| JobMeta {
            host: host.clone(),
            read_bytes: 0,
            use_tmux,
        });
        meta.read_bytes = size;
        meta.use_tmux = use_tmux;

        Ok(JobReport {
            job_id: job_id.to_string(),
            state,
            output,
            timed_out: false,
        })
    }

    async fn resolve_job_use_tmux(&self, host: &str, job_id: &str) -> bool {
        if let Some(m) = self.jobs.lock().await.get(job_id) {
            return m.use_tmux;
        }
        let cmd = format!(
            "cat \"{}/backend\" 2>/dev/null || echo native",
            jobs::job_dir(job_id)
        );
        match self.run_control(host, &cmd).await {
            Ok(out) => jobs::backend_is_tmux(&out),
            Err(_) => false,
        }
    }

    pub async fn job_signal(
        &self,
        host_alias: &str,
        job_id: &str,
        signal: JobSignal,
    ) -> Result<(), SshCommandError> {
        let host = self.resolve_job_host(host_alias, job_id).await;
        let use_tmux = self.resolve_job_use_tmux(&host, job_id).await;
        let signal_cmd = jobs::build_signal_command(job_id, signal, use_tmux);
        self.run_control(&host, &signal_cmd).await?;
        Ok(())
    }

    pub async fn job_wait(
        &self,
        host_alias: &str,
        job_id: &str,
        timeout_secs: u64,
    ) -> Result<JobReport, SshCommandError> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
        let mut accumulated = String::new();

        loop {
            let report = self.job_status(host_alias, job_id).await?;
            accumulated.push_str(&report.output);

            if let JobState::Exited(_) = report.state {
                return Ok(JobReport {
                    output: accumulated,
                    ..report
                });
            }
            if std::time::Instant::now() >= deadline {
                return Ok(JobReport {
                    output: accumulated,
                    timed_out: true,
                    ..report
                });
            }
            tokio::time::sleep(std::time::Duration::from_millis(JOB_POLL_INTERVAL_MS)).await;
        }
    }

    pub async fn disconnect(&self, host_alias: &str) -> Result<()> {
        self.evict(host_alias).await;
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
            jobs: Arc::clone(&self.jobs),
            tmux_cache: Arc::clone(&self.tmux_cache),
            direct_profiles: Arc::clone(&self.direct_profiles),
        }
    }
}
