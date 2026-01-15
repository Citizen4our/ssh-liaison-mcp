use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

mod cli;
mod mcp;
mod ssh;

#[derive(Parser)]
#[command(name = "ssh-liaison-mcp")]
#[command(about = "SSH Liaison MCP Server - SSH connection and command execution via MCP or CLI")]
struct Cli {
    /// Verbosity level (use -v, -vv, -vvv for more debug output)
    #[arg(short = 'v', long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,
    
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run as MCP server (for Cursor/Claude integration)
    Serve,
    /// Interactive CLI mode for standalone testing
    Cli {
        /// Host alias from ~/.ssh/config to connect to immediately
        #[arg(short = 'H', long)]
        host: Option<String>,
        /// SSH username (for direct connection)
        #[arg(short = 'u', long)]
        user: Option<String>,
        /// SSH hostname or IP (for direct connection)
        #[arg(long)]
        hostname: Option<String>,
        /// SSH password (for direct connection, use with caution)
        #[arg(short = 'p', long)]
        password: Option<String>,
        /// SSH port (default: 22)
        #[arg(short = 'P', long, default_value = "22")]
        port: u16,
    },
    /// Legacy direct connect mode (for backward compatibility)
    Connect {
        /// SSH username
        user: String,
        /// SSH hostname or IP
        host: String,
        /// SSH port (default: 22)
        #[arg(short, long, default_value = "22")]
        port: u16,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    
    // Debug mode based on verbosity
    let debug = cli.verbose >= 3;
    if debug {
        unsafe {
            std::env::set_var("SSH_LIAISON_DEBUG", "1");
        }
    }

    match cli.command {
        Commands::Serve => {
            // MCP server mode - all output goes to stderr to avoid interfering with stdio protocol
            mcp::run_mcp_server().await?;
        }
        Commands::Cli { host, user, hostname, password, port } => {
            cli::run_cli_mode(host, user, hostname, password, port).await?;
        }
        Commands::Connect { user, host, port } => {
            let manager = ssh::SessionManager::new();
            eprintln!("Connecting to {}@{}:{}...", user, host, port);
            manager.connect_direct("direct", &user, &host, Some(port))
                .await
                .with_context(|| format!("Failed to connect to {}@{}:{}", user, host, port))?;
            eprintln!("Connected successfully!");
            
            // Simple command loop
            loop {
                print!("ssh> ");
                std::io::Write::flush(&mut std::io::stdout())?;
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                let command = input.trim();
                
                if command.is_empty() {
                continue;
                }
                
                if command == "exit" || command == "quit" {
                    break;
                }
                
                match manager.execute_command("direct", command).await {
                    Ok(output) => {
                        // Output stdout
                        if !output.stdout.trim().is_empty() {
                            print!("{}", output.stdout.trim_end());
                            if !output.stdout.trim_end().ends_with('\n') {
                                println!();
                            }
                        }
                        // Output stderr
                        if !output.stderr.trim().is_empty() {
                            eprint!("{}", output.stderr.trim_end());
                            if !output.stderr.trim_end().ends_with('\n') {
                                eprintln!();
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Error: {}", e);
                    }
                }
            }
            
            manager.disconnect("direct").await?;
        }
    }
    
    Ok(())
}
