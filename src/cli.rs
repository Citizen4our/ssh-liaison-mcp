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

    if let (Some(user), Some(hostname)) = (user, hostname) {
        let alias = host_alias.as_deref().unwrap_or("direct");
        tracing::info!(user = %user, hostname = %hostname, port = %port, "Connecting");

        match manager
            .connect_direct(alias, &user, &hostname, Some(port))
            .await
        {
            Ok(()) => {
                tracing::info!("Connected using SSH keys");
                current_host = Some(alias.to_string());
            }
            Err(e) => {
                tracing::warn!(error = %e, "SSH key authentication failed");
                if let Some(pass) = password {
                    if !pass.is_empty() {
                        tracing::info!("Trying password authentication");
                        match manager
                            .connect_with_password(alias, &user, &hostname, &pass, Some(port))
                            .await
                        {
                            Ok(()) => {
                                tracing::info!("Connected using password");
                                current_host = Some(alias.to_string());
                            }
                            Err(e2) => {
                                anyhow::bail!(
                                    "Failed to connect to {}@{}:{} - SSH keys failed: {}, password failed: {}",
                                    user,
                                    hostname,
                                    port,
                                    e,
                                    e2
                                );
                            }
                        }
                    } else {
                        anyhow::bail!(
                            "Failed to connect to {}@{}:{} - {}",
                            user,
                            hostname,
                            port,
                            e
                        );
                    }
                } else {
                    anyhow::bail!(
                        "Failed to connect to {}@{}:{} - {}",
                        user,
                        hostname,
                        port,
                        e
                    );
                }
            }
        }
    } else if let Some(ref alias) = host_alias {
        tracing::info!(alias = %alias, "Connecting");
        manager
            .connect_by_alias(alias)
            .await
            .with_context(|| format!("Failed to connect to '{}'", alias))?;
        tracing::info!("Connected successfully");
        current_host = Some(alias.clone());
    }

    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());

    loop {
        if let Some(ref alias) = current_host {
            print!("[{}]> ", alias);
        } else {
            print!("ssh> ");
        }
        io::stdout().flush()?;

        let mut input = String::new();
        reader.read_line(&mut input)?;
        let command = input.trim();

        if command.is_empty() {
            continue;
        }

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
                    tracing::info!(host = %alias, "Disconnected");
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

                if args.len() >= 2 {
                    let user = args[0];
                    let hostname = args[1];
                    let password = args.get(2).copied();
                    let port = args
                        .get(3)
                        .and_then(|p| p.parse::<u16>().ok())
                        .unwrap_or(22);

                    tracing::info!(user = %user, hostname = %hostname, port = %port, "Connecting");
                    let alias = format!("{}_{}", user, hostname);

                    match manager
                        .connect_direct(&alias, user, hostname, Some(port))
                        .await
                    {
                        Ok(()) => {
                            tracing::info!("Connected using SSH keys");
                            current_host = Some(alias.clone());
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "SSH key authentication failed");
                            if let Some(pass) = password {
                                if !pass.is_empty() {
                                    tracing::info!("Trying password authentication");
                                    match manager
                                        .connect_with_password(
                                            &alias,
                                            user,
                                            hostname,
                                            pass,
                                            Some(port),
                                        )
                                        .await
                                    {
                                        Ok(()) => {
                                            tracing::info!("Connected using password");
                                            current_host = Some(alias);
                                        }
                                        Err(e2) => {
                                            tracing::error!(
                                                ssh_error = %e,
                                                password_error = %e2,
                                                "Connection failed"
                                            );
                                        }
                                    }
                                } else {
                                    tracing::error!(error = %e, "Connection failed");
                                }
                            } else {
                                tracing::error!(error = %e, "Connection failed");
                            }
                        }
                    }
                } else {
                    let alias = args[0];
                    tracing::info!(alias = %alias, "Connecting");
                    match manager.connect_by_alias(alias).await {
                        Ok(()) => {
                            tracing::info!("Connected successfully");
                            current_host = Some(alias.to_string());
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "Connection failed");
                        }
                    }
                }
                continue;
            }
            _ => {}
        }

        if let Some(ref alias) = current_host {
            match manager.execute_command(alias, command, None).await {
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
        } else {
            eprintln!("Not connected to any host. Use 'connect <host-alias>' to connect.");
        }
    }

    Ok(())
}
