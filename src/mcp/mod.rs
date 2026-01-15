use rmcp::{
    handler::server::{
        router::tool::ToolRouter,
        wrapper::Parameters,
    },
    model::{CallToolResult, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router, ServiceExt, transport::stdio,
    ErrorData as McpError,
};
use anyhow::Result;

use crate::ssh::SessionManager;

pub mod tools;
use tools::{SshConnectParams, SshReadLogParams, SshRunCommandParams};

pub struct SshMcpServer {
    session_manager: SessionManager,
    tool_router: ToolRouter<Self>,
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
        name = "ssh_run_command",
        description = "Execute a command on a connected SSH host. Commands run in a persistent shell session, so state (like current directory) is preserved between commands. WARNING: Destructive operations (rm, mv, etc.) should be avoided. Prefer read-only commands."
    )]
    pub async fn ssh_run_command(
        &self,
        params: Parameters<SshRunCommandParams>,
    ) -> Result<CallToolResult, McpError> {
        tools::ssh_run_command_impl(&self.session_manager, params).await
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
    
    // Log startup information to stderr (stdout is used for MCP protocol)
    let version = env!("CARGO_PKG_VERSION");
    let name = env!("CARGO_PKG_NAME");
    
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    eprintln!("🚀 {} v{}", name, version);
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    eprintln!("📡 MCP Server starting...");
    eprintln!("");
    eprintln!("📦 Available tools:");
    eprintln!("   • ssh_connect      - Connect to SSH host via ~/.ssh/config");
    eprintln!("   • ssh_run_command  - Execute commands on connected host");
    eprintln!("   • ssh_read_log     - Read log files from remote host");
    eprintln!("");
    eprintln!("💡 Usage in Cursor/Claude:");
    eprintln!("   Ask AI to connect to a host and run commands");
    eprintln!("   Example: \"Connect to rpi and show disk usage\"");
    eprintln!("");
    eprintln!("⚠️  Security: Prefer read-only operations");
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    eprintln!("✅ Server ready, waiting for MCP requests...");
    eprintln!("");
    std::io::stderr().flush()?;
    
    let server = SshMcpServer::new();
    let service = match server.serve(stdio()).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("❌ Failed to start MCP server: {}", e);
            return Err(e.into());
        }
    };
    
    // Wait for service to complete (or be interrupted)
    match service.waiting().await {
        Ok(_) => {
            eprintln!("");
            eprintln!("👋 Server shutting down gracefully...");
        }
        Err(e) => {
            // Don't show connection closed errors - they're normal when client disconnects
            let err_msg = e.to_string();
            if !err_msg.contains("connection closed") && !err_msg.contains("broken pipe") {
                eprintln!("");
                eprintln!("⚠️  Server error: {}", e);
            } else {
                eprintln!("");
                eprintln!("👋 Client disconnected, shutting down...");
            }
        }
    }
    
    Ok(())
}
