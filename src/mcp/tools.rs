use anyhow::Result;
use rmcp::{
    ErrorData as McpError,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, Content},
    schemars::JsonSchema,
};
use serde::{Deserialize, Serialize};

use crate::ssh::{CommandTimeouts, JobSignal, JobState, SessionManager, SshCommandError};

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "SSH connection parameters")]
pub struct SshConnectParams {
    #[schemars(description = "Host alias from ~/.ssh/config (e.g., 'dev-1', 'prod-server')")]
    pub host_alias: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "SSH command execution parameters")]
pub struct SshRunCommandParams {
    #[schemars(
        description = "Host alias to execute command on. Auto-connects via ~/.ssh/config or a prior ssh_connect_direct profile if needed."
    )]
    pub host: String,
    #[schemars(description = "Command to execute on remote host")]
    pub command: String,
    #[schemars(
        description = "Optional sudo password when command requires it. Use with caution; prefer passwordless sudo."
    )]
    pub sudo_password: Option<String>,
    #[schemars(
        description = "Idle timeout in seconds: no output for this long ends the command (default 30). Resets while output is streaming."
    )]
    pub timeout_secs: Option<u64>,
    #[schemars(
        description = "Hard maximum runtime in seconds (default 30, no cap). Command may run longer than timeout_secs if still producing output, up to this limit."
    )]
    pub max_timeout_secs: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Start a long-running command in the background (detached)")]
pub struct SshRunDetachedParams {
    #[schemars(
        description = "Host alias to execute command on. Auto-connects via ~/.ssh/config or a prior ssh_connect_direct profile if needed."
    )]
    pub host: String,
    #[schemars(description = "Command to run detached; output is buffered on the remote host")]
    pub command: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Read status and new output of a detached job")]
pub struct SshJobStatusParams {
    #[schemars(description = "Host alias the job runs on")]
    pub host: String,
    #[schemars(description = "Job id returned by ssh_run_detached")]
    pub job_id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum JobSignalParam {
    Int,
    Term,
    Kill,
}

impl From<JobSignalParam> for JobSignal {
    fn from(value: JobSignalParam) -> Self {
        match value {
            JobSignalParam::Int => JobSignal::Int,
            JobSignalParam::Term => JobSignal::Term,
            JobSignalParam::Kill => JobSignal::Kill,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Send a signal to a detached job")]
pub struct SshJobSignalParams {
    #[schemars(description = "Host alias the job runs on")]
    pub host: String,
    #[schemars(description = "Job id returned by ssh_run_detached")]
    pub job_id: String,
    #[schemars(
        description = "Signal: 'int' (SIGINT to worker, TERM fallback), 'term' (SIGTERM), 'kill' (SIGKILL worker + tmux session cleanup when applicable)"
    )]
    pub signal: JobSignalParam,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Wait (attach) for a detached job up to a timeout")]
pub struct SshJobWaitParams {
    #[schemars(description = "Host alias the job runs on")]
    pub host: String,
    #[schemars(description = "Job id returned by ssh_run_detached")]
    pub job_id: String,
    #[schemars(description = "Maximum seconds to block waiting for completion (default 30)")]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Direct SSH connection parameters")]
pub struct SshConnectDirectParams {
    #[schemars(
        description = "Host alias to identify this connection (used for subsequent commands)"
    )]
    pub host_alias: String,
    #[schemars(description = "SSH username")]
    pub user: String,
    #[schemars(description = "Hostname or IP address")]
    pub hostname: String,
    #[schemars(
        description = "SSH password for authentication (optional, will try SSH keys if not provided or if password fails)"
    )]
    pub password: Option<String>,
    #[schemars(description = "SSH port (default: 22)")]
    pub port: Option<u16>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "SSH log reading parameters")]
pub struct SshReadLogParams {
    #[schemars(
        description = "Host alias to read log from. Auto-connects via ~/.ssh/config or a prior ssh_connect_direct profile if needed."
    )]
    pub host: String,
    #[schemars(description = "Path to log file on remote host")]
    pub file_path: String,
    #[schemars(description = "Number of lines to read from log file")]
    pub lines: i32,
}

fn mcp_error_from_ssh(e: SshCommandError) -> McpError {
    match &e {
        SshCommandError::NotConnected(_)
        | SshCommandError::CommandFailed { .. }
        | SshCommandError::SudoPasswordRequired => McpError::invalid_params(e.to_string(), None),
        SshCommandError::CommandTimeout { .. }
        | SshCommandError::ConnectionLost(_)
        | SshCommandError::Io(_) => McpError::internal_error(e.to_string(), None),
    }
}

async fn ensure_host_connected(
    session_manager: &SessionManager,
    host: &str,
) -> Result<(), McpError> {
    if session_manager.is_connected(host).await {
        return Ok(());
    }
    session_manager.ensure_connected(host).await.map_err(|e| {
        McpError::invalid_params(
            format!(
                "Not connected to host '{host}' and auto-connect failed ({e}). \
                 Use ssh_connect (SSH config) or ssh_connect_direct first for direct hosts."
            ),
            None,
        )
    })
}

async fn register_direct_profile_from_params(
    session_manager: &SessionManager,
    p: &SshConnectDirectParams,
) {
    session_manager
        .register_direct_profile(
            &p.host_alias,
            &p.user,
            &p.hostname,
            p.port,
            p.password.clone(),
        )
        .await;
}

pub async fn ssh_connect_impl(
    session_manager: &SessionManager,
    params: Parameters<SshConnectParams>,
) -> Result<CallToolResult, McpError> {
    let host_alias = &params.0.host_alias;

    match session_manager.connect_by_alias(host_alias).await {
        Ok(()) => Ok(CallToolResult::success(vec![Content::text(format!(
            "Successfully connected to '{}'",
            host_alias
        ))])),
        Err(e) => Err(McpError::invalid_params(e.to_string(), None)),
    }
}

pub async fn ssh_connect_direct_impl(
    session_manager: &SessionManager,
    params: Parameters<SshConnectDirectParams>,
) -> Result<CallToolResult, McpError> {
    let p = &params.0;
    register_direct_profile_from_params(session_manager, p).await;

    let key_result = session_manager
        .connect_direct(&p.host_alias, &p.user, &p.hostname, p.port)
        .await;
    match key_result {
        Ok(()) => {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "Successfully connected to {}@{} using SSH keys",
                p.user, p.hostname
            ))]));
        }
        Err(ref e) => {
            tracing::debug!(error = %e, "SSH key authentication failed, trying password");
        }
    }

    if let Some(ref password) = p.password
        && !password.is_empty()
    {
        match session_manager
            .connect_with_password(&p.host_alias, &p.user, &p.hostname, password, p.port)
            .await
        {
            Ok(()) => {
                return Ok(CallToolResult::success(vec![Content::text(format!(
                    "Successfully connected to {}@{} using password",
                    p.user, p.hostname
                ))]));
            }
            Err(e) => {
                return Err(McpError::invalid_params(
                    format!(
                        "Authentication failed: SSH keys and password both failed. Last error: {e}"
                    ),
                    None,
                ));
            }
        }
    }

    let detail = key_result
        .err()
        .map(|e| e.to_string())
        .unwrap_or_else(|| "unknown key auth failure".to_string());
    Err(McpError::invalid_params(
        format!("SSH key authentication failed and no password provided. Detail: {detail}"),
        None,
    ))
}

pub async fn ssh_run_command_impl(
    session_manager: &SessionManager,
    params: Parameters<SshRunCommandParams>,
) -> Result<CallToolResult, McpError> {
    let p = &params.0;
    ensure_host_connected(session_manager, &p.host).await?;
    let timeouts = CommandTimeouts::from_secs(p.timeout_secs, p.max_timeout_secs);

    match session_manager
        .execute_command(&p.host, &p.command, p.sudo_password.as_deref(), timeouts)
        .await
    {
        Ok(output) => Ok(CallToolResult::success(vec![Content::text(
            output.combined_with_stderr_label(),
        )])),
        Err(e) => Err(mcp_error_from_ssh(e)),
    }
}

fn format_state(state: JobState) -> String {
    match state {
        JobState::Running => "running".to_string(),
        JobState::Exited(code) => format!("exited (code {code})"),
    }
}

pub async fn ssh_run_detached_impl(
    session_manager: &SessionManager,
    params: Parameters<SshRunDetachedParams>,
) -> Result<CallToolResult, McpError> {
    let p = &params.0;
    ensure_host_connected(session_manager, &p.host).await?;
    match session_manager.run_detached(&p.host, &p.command).await {
        Ok((job_id, use_tmux)) => {
            let backend = if use_tmux { "tmux" } else { "native" };
            Ok(CallToolResult::success(vec![Content::text(format!(
                "Started detached job '{job_id}' on '{}' ({backend} backend).\n\
                 Use ssh_job_status / ssh_job_wait to read output, ssh_job_signal to interrupt or kill.",
                p.host
            ))]))
        }
        Err(e) => Err(mcp_error_from_ssh(e)),
    }
}

pub async fn ssh_job_status_impl(
    session_manager: &SessionManager,
    params: Parameters<SshJobStatusParams>,
) -> Result<CallToolResult, McpError> {
    let p = &params.0;
    ensure_host_connected(session_manager, &p.host).await?;
    match session_manager.job_status(&p.host, &p.job_id).await {
        Ok(report) => Ok(CallToolResult::success(vec![Content::text(format!(
            "[job {}] {}\n{}",
            report.job_id,
            format_state(report.state),
            report.output
        ))])),
        Err(e) => Err(mcp_error_from_ssh(e)),
    }
}

pub async fn ssh_job_signal_impl(
    session_manager: &SessionManager,
    params: Parameters<SshJobSignalParams>,
) -> Result<CallToolResult, McpError> {
    let p = &params.0;
    ensure_host_connected(session_manager, &p.host).await?;
    match session_manager
        .job_signal(&p.host, &p.job_id, p.signal.into())
        .await
    {
        Ok(()) => Ok(CallToolResult::success(vec![Content::text(format!(
            "Signal sent to job '{}'",
            p.job_id
        ))])),
        Err(e) => Err(mcp_error_from_ssh(e)),
    }
}

pub async fn ssh_job_wait_impl(
    session_manager: &SessionManager,
    params: Parameters<SshJobWaitParams>,
) -> Result<CallToolResult, McpError> {
    let p = &params.0;
    ensure_host_connected(session_manager, &p.host).await?;
    let timeout = p.timeout_secs.unwrap_or(30);
    match session_manager.job_wait(&p.host, &p.job_id, timeout).await {
        Ok(report) => {
            let header = if report.timed_out {
                format!(
                    "[job {}] still {} after {timeout}s (output so far)",
                    report.job_id,
                    format_state(report.state)
                )
            } else {
                format!("[job {}] {}", report.job_id, format_state(report.state))
            };
            Ok(CallToolResult::success(vec![Content::text(format!(
                "{header}\n{}",
                report.output
            ))]))
        }
        Err(e) => Err(mcp_error_from_ssh(e)),
    }
}

pub async fn ssh_read_log_impl(
    session_manager: &SessionManager,
    params: Parameters<SshReadLogParams>,
) -> Result<CallToolResult, McpError> {
    let host = &params.0.host;
    ensure_host_connected(session_manager, host).await?;
    let file_path = &params.0.file_path;
    let lines = params.0.lines;

    let command = format!("tail -n {} {}", lines, file_path);

    match session_manager
        .execute_command(host, &command, None, CommandTimeouts::default())
        .await
    {
        Ok(output) => Ok(CallToolResult::success(vec![Content::text(
            output.combined_with_stderr_label(),
        )])),
        Err(e) => Err(mcp_error_from_ssh(e)),
    }
}
