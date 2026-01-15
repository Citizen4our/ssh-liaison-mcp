use anyhow::{Context, Result};
use std::io::{self, BufRead, BufReader, Write};

use crate::ssh::SessionManager;

pub async fn run_cli_mode(
    host_alias: Option<String>,
    user: Option<String>,
    hostname: Option<String>,
    password: Option<String>,
    port: u16,
) -> Result<()> {
    let manager = SessionManager::new();
    let mut current_host: Option<String> = None;
    
    // Connect immediately if credentials provided
    if let (Some(user), Some(hostname)) = (user, hostname) {
        let alias = host_alias.as_deref().unwrap_or("direct");
        eprintln!("Connecting to {}@{}:{}...", user, hostname, port);
        
        if let Some(pass) = password {
            manager.connect_with_password(alias, &user, &hostname, &pass, Some(port))
                .await
                .with_context(|| format!("Failed to connect to {}@{}:{}", user, hostname, port))?;
        } else {
            manager.connect_direct(alias, &user, &hostname, Some(port))
                .await
                .with_context(|| format!("Failed to connect to {}@{}:{}", user, hostname, port))?;
        }
        eprintln!("Connected successfully!");
        current_host = Some(alias.to_string());
    } else if let Some(ref alias) = host_alias {
        // Connect using SSH config alias
        eprintln!("Connecting to {}...", alias);
        manager.connect_by_alias(alias)
            .await
            .with_context(|| format!("Failed to connect to '{}'", alias))?;
        eprintln!("Connected successfully!");
        current_host = Some(alias.clone());
    }

    // Use blocking stdin for CLI mode (simpler for interactive use)
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());

    loop {
        // Show prompt
        if let Some(ref alias) = current_host {
            print!("[{}]> ", alias);
        } else {
            print!("ssh> ");
        }
        io::stdout().flush()?;

        // Read command
        let mut input = String::new();
        reader.read_line(&mut input)?;
        let command = input.trim();

        if command.is_empty() {
            continue;
        }

        // Handle special commands
        match command {
            "exit" | "quit" => {
                if let Some(ref alias) = current_host {
                    let _ = manager.disconnect(alias).await;
                }
                break;
            }
            "disconnect" => {
                if let Some(ref alias) = current_host {
                    manager.disconnect(alias).await?;
                    eprintln!("Disconnected from {}", alias);
                    current_host = None;
                } else {
                    eprintln!("Not connected to any host");
                }
                continue;
            }
            cmd if cmd.starts_with("connect ") => {
                let args: Vec<&str> = cmd[8..].split_whitespace().collect();
                if args.is_empty() {
                    eprintln!("Usage: connect <host-alias>");
                    eprintln!("   or: connect <user> <hostname> [password] [port]");
                    continue;
                }
                
                if let Some(ref old_alias) = current_host {
                    let _ = manager.disconnect(old_alias).await;
                }
                
                // Try direct connection format: connect user hostname [password] [port]
                if args.len() >= 2 {
                    let user = args[0];
                    let hostname = args[1];
                    let password = args.get(2).copied();
                    let port = args.get(3).and_then(|p| p.parse::<u16>().ok()).unwrap_or(22);
                    
                    eprintln!("Connecting to {}@{}:{}...", user, hostname, port);
                    let alias = format!("{}_{}", user, hostname);
                    
                    match password {
                        Some(pass) => {
                            match manager.connect_with_password(&alias, user, hostname, pass, Some(port)).await {
                                Ok(()) => {
                                    eprintln!("Connected successfully!");
                                    current_host = Some(alias);
                                }
                                Err(e) => {
                                    eprintln!("Connection failed: {}", e);
                                }
                            }
                        }
                        None => {
                            match manager.connect_direct(&alias, user, hostname, Some(port)).await {
                                Ok(()) => {
                                    eprintln!("Connected successfully!");
                                    current_host = Some(alias);
                                }
                                Err(e) => {
                                    eprintln!("Connection failed: {}", e);
                                }
                            }
                        }
                    }
                } else {
                    // SSH config alias
                    let alias = args[0];
                    eprintln!("Connecting to {}...", alias);
                    match manager.connect_by_alias(alias).await {
                        Ok(()) => {
                            eprintln!("Connected successfully!");
                            current_host = Some(alias.to_string());
                        }
                        Err(e) => {
                            eprintln!("Connection failed: {}", e);
                        }
                    }
                }
                continue;
            }
            _ => {}
        }

        // Execute command on remote host
        if let Some(ref alias) = current_host {
            match manager.execute_command(alias, command).await {
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
        } else {
            eprintln!("Not connected to any host. Use 'connect <host-alias>' to connect.");
        }
    }

    Ok(())
}
