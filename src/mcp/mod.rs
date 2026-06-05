use anyhow::Result;
use rmcp::{
    ErrorData as McpError, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::stdio,
};

use crate::ssh::SessionManager;

pub mod tools;
use tools::{
    SshConnectDirectParams, SshConnectParams, SshJobSignalParams, SshJobStatusParams,
    SshJobWaitParams, SshReadLogParams, SshRunCommandParams, SshRunDetachedParams,
};

pub struct SshMcpServer {
    session_manager: SessionManager,
    tool_router: ToolRouter<Self>,
}

impl Default for SshMcpServer {
    fn default() -> Self {
        Self::new()
    }
}

impl SshMcpServer {
    pub fn new() -> Self {
        let session_manager = SessionManager::new();
        Self {
            session_manager,
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl SshMcpServer {
    #[tool(
        name = "ssh_connect",
        description = "Connect to a remote SSH server using host alias from ~/.ssh/config. Establishes a persistent shell session that preserves state between commands. WARNING: Only use for read-only operations unless explicitly authorized."
    )]
    pub async fn ssh_connect(
        &self,
        params: Parameters<SshConnectParams>,
    ) -> Result<CallToolResult, McpError> {
        tools::ssh_connect_impl(&self.session_manager, params).await
    }

    #[tool(
        name = "ssh_connect_direct",
        description = "Connect to a remote SSH server directly using user, hostname/IP, optional password, and optional port. Establishes a persistent shell session that preserves state between commands. If password is not provided or fails, SSH key authentication will be attempted. WARNING: Only use for read-only operations unless explicitly authorized. Password is transmitted securely over SSH."
    )]
    pub async fn ssh_connect_direct(
        &self,
        params: Parameters<SshConnectDirectParams>,
    ) -> Result<CallToolResult, McpError> {
        tools::ssh_connect_direct_impl(&self.session_manager, params).await
    }

    #[tool(
        name = "ssh_run_command",
        description = "Execute a command on a connected SSH host. Auto-connects via ~/.ssh/config when no session exists in this server process (ssh_connect is optional). Commands run in a persistent shell session, so state (like current directory) is preserved between commands in the same process. Timeouts: idle default 30s (no output) and hard max default 30s; override with timeout_secs / max_timeout_secs (no cap, pass a larger max explicitly when you expect a long command). For commands that may run long, prefer ssh_run_detached so you never block. Hung commands (no output past idle) are interrupted so the next command can proceed. Connection loss requires reconnect. Use sudo_password for sudo. IMPORTANT: Use --no-pager for systemctl, journalctl, git log. Avoid interactive commands (less, vim, top, htop). WARNING: Prefer read-only commands."
    )]
    pub async fn ssh_run_command(
        &self,
        params: Parameters<SshRunCommandParams>,
    ) -> Result<CallToolResult, McpError> {
        tools::ssh_run_command_impl(&self.session_manager, params).await
    }

    #[tool(
        name = "ssh_run_detached",
        description = "Run a long-running command in the background on a connected host without blocking. Returns a job_id. Output is buffered on the remote host (uses tmux if available, otherwise native job control). Use this instead of ssh_run_command when a command may exceed the command timeout. Poll with ssh_job_status, block with ssh_job_wait, and interrupt/kill with ssh_job_signal."
    )]
    pub async fn ssh_run_detached(
        &self,
        params: Parameters<SshRunDetachedParams>,
    ) -> Result<CallToolResult, McpError> {
        tools::ssh_run_detached_impl(&self.session_manager, params).await
    }

    #[tool(
        name = "ssh_job_status",
        description = "Get the current state (running / exited with code) and any new output of a detached job since the last status read. Non-blocking."
    )]
    pub async fn ssh_job_status(
        &self,
        params: Parameters<SshJobStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        tools::ssh_job_status_impl(&self.session_manager, params).await
    }

    #[tool(
        name = "ssh_job_wait",
        description = "Attach to a detached job and block until it finishes or timeout_secs elapses (default 30s). Returns accumulated output. If it times out the job keeps running; call again or use ssh_job_signal."
    )]
    pub async fn ssh_job_wait(
        &self,
        params: Parameters<SshJobWaitParams>,
    ) -> Result<CallToolResult, McpError> {
        tools::ssh_job_wait_impl(&self.session_manager, params).await
    }

    #[tool(
        name = "ssh_job_signal",
        description = "Send a signal to a detached job: 'int' (SIGINT to worker pid, TERM fallback, tmux C-c when applicable), 'term' (SIGTERM to worker), 'kill' (SIGKILL to worker plus tmux session cleanup). Use when a job hangs or is no longer needed."
    )]
    pub async fn ssh_job_signal(
        &self,
        params: Parameters<SshJobSignalParams>,
    ) -> Result<CallToolResult, McpError> {
        tools::ssh_job_signal_impl(&self.session_manager, params).await
    }

    #[tool(
        name = "ssh_read_log",
        description = "Read the last N lines from a log file on a connected SSH host. This is a read-only operation safe for log analysis."
    )]
    pub async fn ssh_read_log(
        &self,
        params: Parameters<SshReadLogParams>,
    ) -> Result<CallToolResult, McpError> {
        tools::ssh_read_log_impl(&self.session_manager, params).await
    }
}

#[tool_handler(router = self.tool_router)]
impl rmcp::ServerHandler for SshMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            instructions: Some(
                "SSH Liaison MCP Server - Provides SSH connection and command execution tools. \
                WARNING: Use with caution. Prefer read-only operations. Destructive commands should \
                be avoided unless explicitly authorized. All commands run in persistent shell sessions \
                that preserve state between commands."
                    .to_string(),
            ),
            ..Default::default()
        }
    }
}

pub async fn run_mcp_server() -> Result<()> {
    use std::io::Write;

    let version = env!("CARGO_PKG_VERSION");
    let name = env!("CARGO_PKG_NAME");

    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    eprintln!("{} v{}", name, version);
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    eprintln!("MCP Server starting...");
    eprintln!();
    eprintln!("Available tools:");
    eprintln!("  - ssh_connect        Connect to SSH host via ~/.ssh/config");
    eprintln!("  - ssh_connect_direct Connect to SSH host directly");
    eprintln!("  - ssh_run_command    Execute commands on connected host");
    eprintln!("  - ssh_run_detached   Run a long command in the background");
    eprintln!("  - ssh_job_status     Read status/output of a detached job");
    eprintln!("  - ssh_job_wait       Attach/wait for a detached job");
    eprintln!("  - ssh_job_signal     Send INT/TERM/KILL to a detached job");
    eprintln!("  - ssh_read_log       Read log files from remote host");
    eprintln!();
    eprintln!("Security: Prefer read-only operations");
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    eprintln!("Server ready, waiting for MCP requests...");
    eprintln!();
    std::io::stderr().flush()?;

    let server = SshMcpServer::new();
    let service = match server.serve(stdio()).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "Failed to start MCP server");
            return Err(e.into());
        }
    };

    match service.waiting().await {
        Ok(_) => {
            eprintln!();
            eprintln!("Server shutting down gracefully...");
        }
        Err(e) => {
            let err_msg = e.to_string();
            if !err_msg.contains("connection closed") && !err_msg.contains("broken pipe") {
                eprintln!();
                tracing::warn!(error = %e, "Server error");
            } else {
                eprintln!();
                eprintln!("Client disconnected, shutting down...");
            }
        }
    }

    Ok(())
}
