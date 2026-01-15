use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct SshHostConfig {
    #[allow(dead_code)]
    pub host: String,
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_file: Option<PathBuf>,
}

pub fn parse_ssh_config(host_alias: &str) -> Result<SshHostConfig> {
    let home = std::env::var("HOME")
        .context("HOME environment variable not set")?;
    let config_path = PathBuf::from(&home).join(".ssh").join("config");

    if !config_path.exists() {
        anyhow::bail!("SSH config file not found at {}", config_path.display());
    }

    let content = fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read SSH config from {}", config_path.display()))?;

    let mut current_host: Option<String> = None;
    let mut hosts: HashMap<String, SshHostConfig> = HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        
        // Skip comments and empty lines
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Parse Host directive
        if line.starts_with("Host ") {
            let host = line[5..].trim();
            if !host.is_empty() {
                current_host = Some(host.to_string());
                if !hosts.contains_key(host) {
                    hosts.insert(host.to_string(), SshHostConfig {
                        host: host.to_string(),
                        hostname: None,
                        user: None,
                        port: None,
                        identity_file: None,
                    });
                }
            }
            continue;
        }

        // Parse other directives if we're in a Host block
        if let Some(ref host) = current_host {
            if let Some(config) = hosts.get_mut(host) {
                if line.starts_with("HostName ") {
                    config.hostname = Some(line[9..].trim().to_string());
                } else if line.starts_with("User ") {
                    config.user = Some(line[5..].trim().to_string());
                } else if line.starts_with("Port ") {
                    if let Ok(port) = line[5..].trim().parse::<u16>() {
                        config.port = Some(port);
                    }
                } else if line.starts_with("IdentityFile ") {
                    let path_str = line[13..].trim();
                    let path = PathBuf::from(path_str);
                    // Expand ~ to home directory
                    let expanded_path = if path_str.starts_with("~/") {
                        PathBuf::from(&home).join(&path_str[2..])
                    } else {
                        path
                    };
                    config.identity_file = Some(expanded_path);
                }
            }
        }
    }

    // Check for exact match first
    if let Some(config) = hosts.get(host_alias) {
        return Ok(config.clone());
    }

    // Check for wildcard patterns (simple implementation)
    for (host_pattern, config) in &hosts {
        if host_pattern.contains('*') {
            // Simple wildcard matching
            let pattern = host_pattern.replace("*", ".*");
            if let Ok(re) = regex::Regex::new(&format!("^{}$", pattern)) {
                if re.is_match(host_alias) {
                    return Ok(config.clone());
                }
            }
        }
    }

    anyhow::bail!("Host '{}' not found in SSH config", host_alias)
}
