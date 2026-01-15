use anyhow::Result;
use rmcp::{
    ErrorData as McpError,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, Content},
    schemars::JsonSchema,
};
use serde::{Deserialize, Serialize};

use crate::ssh::SessionManager;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "SSH connection parameters")]
pub struct SshConnectParams {
    #[schemars(description = "Host alias from ~/.ssh/config (e.g., 'dev-1', 'prod-server')")]
    pub host_alias: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "SSH command execution parameters")]
pub struct SshRunCommandParams {
    #[schemars(description = "Host alias to execute command on (must be connected first)")]
    pub host: String,
    #[schemars(description = "Command to execute on remote host")]
    pub command: String,
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
    #[schemars(description = "Host alias to read log from (must be connected first)")]
    pub host: String,
    #[schemars(description = "Path to log file on remote host")]
    pub file_path: String,
    #[schemars(description = "Number of lines to read from log file")]
    pub lines: i32,
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
        Err(e) => {
            let msg = Box::leak(e.to_string().into_boxed_str());
            Err(McpError::invalid_params(&*msg, None))
        }
    }
}

pub async fn ssh_connect_direct_impl(
    session_manager: &SessionManager,
    params: Parameters<SshConnectDirectParams>,
) -> Result<CallToolResult, McpError> {
    let p = &params.0;

    // Try SSH key authentication first (standard SSH behavior)
    match session_manager
        .connect_direct(&p.host_alias, &p.user, &p.hostname, p.port)
        .await
    {
        Ok(()) => {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "Successfully connected to {}@{} using SSH keys",
                p.user, p.hostname
            ))]));
        }
        Err(e) => {
            // SSH keys failed, try password if provided
            let debug =
                std::env::var("SSH_LIAISON_DEBUG").unwrap_or_else(|_| "0".to_string()) == "1";
            if debug {
                eprintln!(
                    "[DEBUG] SSH key authentication failed: {}, trying password...",
                    e
                );
            }
        }
    }

    // Try password authentication if password is provided and not empty
    if let Some(ref password) = p.password {
        if !password.is_empty() {
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
                    let msg = Box::leak(format!("Authentication failed: SSH keys and password both failed. Last error: {}", e).into_boxed_str());
                    return Err(McpError::invalid_params(&*msg, None));
                }
            }
        }
    }

    // No password provided and SSH keys failed
    let msg = Box::leak(
        "SSH key authentication failed and no password provided"
            .to_string()
            .into_boxed_str(),
    );
    Err(McpError::invalid_params(&*msg, None))
}

pub async fn ssh_run_command_impl(
    session_manager: &SessionManager,
    params: Parameters<SshRunCommandParams>,
) -> Result<CallToolResult, McpError> {
    let host = &params.0.host;
    let command = &params.0.command;

    // Check for sudo password prompt in command
    if command.contains("sudo") {
        // Note: Full elicitation support would be added here
        // For now, we'll execute and detect password prompts in output
    }

    match session_manager.execute_command(host, command).await {
        Ok(output) => {
            // Check for sudo password prompt in both stdout and stderr
            let combined = output.combined();
            if combined.contains("[sudo] password") || combined.contains("Password:") {
                // In a full implementation, this would trigger elicitation
                // For now, return an error suggesting the user handle it manually
                return Err(McpError::invalid_params(
                    "Command requires sudo password. Elicitation support coming soon. Please ensure the user has passwordless sudo configured or handle manually.",
                    None,
                ));
            }

            // Combine stdout and stderr for MCP response
            let mut result_text = String::new();
            if !output.stdout.trim().is_empty() {
                result_text.push_str(&output.stdout);
            }
            if !output.stderr.trim().is_empty() {
                if !result_text.is_empty() && !result_text.ends_with('\n') {
                    result_text.push('\n');
                }
                result_text.push_str("STDERR:\n");
                result_text.push_str(&output.stderr);
            }

            Ok(CallToolResult::success(vec![Content::text(result_text)]))
        }
        Err(e) => {
            let msg = Box::leak(e.to_string().into_boxed_str());
            Err(McpError::invalid_params(&*msg, None))
        }
    }
}

pub async fn ssh_read_log_impl(
    session_manager: &SessionManager,
    params: Parameters<SshReadLogParams>,
) -> Result<CallToolResult, McpError> {
    let host = &params.0.host;
    let file_path = &params.0.file_path;
    let lines = params.0.lines;

    let command = format!("tail -n {} {}", lines, file_path);

    match session_manager.execute_command(host, &command).await {
        Ok(output) => {
            // Combine stdout and stderr for MCP response
            let mut result_text = String::new();
            if !output.stdout.trim().is_empty() {
                result_text.push_str(&output.stdout);
            }
            if !output.stderr.trim().is_empty() {
                if !result_text.is_empty() && !result_text.ends_with('\n') {
                    result_text.push('\n');
                }
                result_text.push_str("STDERR:\n");
                result_text.push_str(&output.stderr);
            }

            Ok(CallToolResult::success(vec![Content::text(result_text)]))
        }
        Err(e) => {
            let msg = Box::leak(e.to_string().into_boxed_str());
            Err(McpError::invalid_params(&*msg, None))
        }
    }
}
