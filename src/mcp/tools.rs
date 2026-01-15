use anyhow::Result;
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, Content},
    ErrorData as McpError,
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
        Ok(()) => {
            Ok(CallToolResult::success(vec![Content::text(format!(
                "Successfully connected to '{}'",
                host_alias
            ))]))
        }
        Err(e) => {
            let msg = Box::leak(e.to_string().into_boxed_str());
            Err(McpError::invalid_params(&*msg, None))
        }
    }
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
