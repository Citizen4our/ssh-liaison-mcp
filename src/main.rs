use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing::Level;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

mod cli;
mod mcp;
mod ssh;

#[derive(Parser)]
#[command(name = "ssh-liaison-mcp")]
#[command(about = "SSH Liaison MCP Server - SSH connection and command execution via MCP or CLI")]
struct Cli {
    /// Verbosity level (-v: info, -vv: debug, -vvv: trace)
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

fn init_tracing(verbose: u8) {
    let level = match verbose {
        0 => Level::WARN,
        1 => Level::INFO,
        2 => Level::DEBUG,
        _ => Level::TRACE,
    };

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level.as_str()));

    tracing_subscriber::registry()
        .with(fmt::layer().with_writer(std::io::stderr))
        .with(filter)
        .init();
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    init_tracing(cli.verbose);

    match cli.command {
        Commands::Serve => {
            mcp::run_mcp_server().await?;
        }
        Commands::Cli {
            host,
            user,
            hostname,
            password,
            port,
        } => {
            cli::run_cli_mode(host, user, hostname, password, port).await?;
        }
        Commands::Connect { user, host, port } => {
            let manager = ssh::SessionManager::new();
            tracing::info!(user = %user, host = %host, port = %port, "Connecting");
            manager
                .connect_direct("direct", &user, &host, Some(port))
                .await
                .with_context(|| format!("Failed to connect to {}@{}:{}", user, host, port))?;
            tracing::info!("Connected successfully");

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

                match manager.execute_command("direct", command, None).await {
                    Ok(output) => {
                        if !output.stdout.trim().is_empty() {
                            print!("{}", output.stdout.trim_end());
                            if !output.stdout.trim_end().ends_with('\n') {
                                println!();
                            }
                        }
                        if !output.stderr.trim().is_empty() {
                            eprint!("{}", output.stderr.trim_end());
                            if !output.stderr.trim_end().ends_with('\n') {
                                eprintln!();
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "Command execution failed");
                    }
                }
            }

            manager.disconnect("direct").await?;
        }
    }

    Ok(())
}
