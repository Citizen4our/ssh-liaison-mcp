use anyhow::{Context, Result};
use std::collections::HashMap;
use std::collections::HashSet;
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
    pub proxy_command: Option<String>,
    pub proxy_use_fdpass: bool,
    pub identities_only: bool,
}

fn expand_path(path_str: &str, home: &str) -> PathBuf {
    if let Some(stripped) = path_str.strip_prefix("~/") {
        PathBuf::from(home).join(stripped)
    } else if path_str == "~" {
        PathBuf::from(home)
    } else {
        PathBuf::from(path_str)
    }
}

fn parse_include_directive(line: &str, home: &str) -> Vec<PathBuf> {
    if !line.starts_with("Include ") {
        return Vec::new();
    }

    let include_paths = line[8..].trim();
    let mut paths = Vec::new();

    for path_str in include_paths.split_whitespace() {
        let expanded = expand_path(path_str, home);
        if expanded.exists() {
            paths.push(expanded);
        }
    }

    paths
}

fn read_config_file(path: &PathBuf, home: &str, visited: &mut HashSet<PathBuf>) -> Result<String> {
    if visited.contains(path) {
        return Ok(String::new());
    }
    visited.insert(path.clone());

    if !path.exists() {
        return Ok(String::new());
    }

    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read SSH config from {}", path.display()))?;

    let mut result = String::new();
    let mut includes = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with('#') {
            result.push_str(line);
            result.push('\n');
            continue;
        }

        if trimmed.starts_with("Include ") {
            let include_paths = parse_include_directive(trimmed, home);
            for include_path in include_paths {
                if let Ok(included_content) = read_config_file(&include_path, home, visited) {
                    includes.push(included_content);
                }
            }
            continue;
        }

        result.push_str(line);
        result.push('\n');
    }

    let mut final_content = String::new();
    for included in includes {
        final_content.push_str(&included);
    }
    final_content.push_str(&result);

    Ok(final_content)
}

pub fn parse_ssh_config(host_alias: &str) -> Result<SshHostConfig> {
    let home = std::env::var("HOME").context("HOME environment variable not set")?;
    let config_path = PathBuf::from(&home).join(".ssh").join("config");

    if !config_path.exists() {
        anyhow::bail!("SSH config file not found at {}", config_path.display());
    }

    let mut visited = HashSet::new();
    let content = read_config_file(&config_path, &home, &mut visited)
        .with_context(|| format!("Failed to read SSH config from {}", config_path.display()))?;

    tracing::trace!(config_length = content.len(), "Parsed SSH config");

    let mut current_host: Option<String> = None;
    let mut hosts: HashMap<String, SshHostConfig> = HashMap::new();

    tracing::trace!(lines = content.lines().count(), "Starting config parsing");

    for line in content.lines() {
        let line = line.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if line.starts_with("Include ") {
            continue;
        }

        if line.to_lowercase().starts_with("host ") {
            let host = line[5..].trim();
            if !host.is_empty() {
                current_host = Some(host.to_string());
                if !hosts.contains_key(host) {
                    hosts.insert(
                        host.to_string(),
                        SshHostConfig {
                            host: host.to_string(),
                            hostname: None,
                            user: None,
                            port: None,
                            identity_file: None,
                            proxy_command: None,
                            proxy_use_fdpass: false,
                            identities_only: false,
                        },
                    );
                }
            }
            continue;
        }

        if let Some(ref host) = current_host
            && let Some(config) = hosts.get_mut(host)
        {
            let line_lower = line.to_lowercase();
            if line_lower.starts_with("hostname ") {
                let hostname = line[9..].trim().to_string();
                tracing::trace!(host = %host, hostname = %hostname, "Setting hostname");
                config.hostname = Some(hostname);
            } else if line_lower.starts_with("user ") {
                config.user = Some(line[5..].trim().to_string());
            } else if line_lower.starts_with("port ") {
                if let Ok(port) = line[5..].trim().parse::<u16>() {
                    config.port = Some(port);
                }
            } else if line_lower.starts_with("identityfile ") {
                let path_str = line[13..].trim();
                let expanded_path = expand_path(path_str, &home);
                config.identity_file = Some(expanded_path);
            } else if line_lower.starts_with("proxycommand ") {
                let cmd = line[13..].trim();
                let cmd = if (cmd.starts_with('"') && cmd.ends_with('"'))
                    || (cmd.starts_with('\'') && cmd.ends_with('\''))
                {
                    &cmd[1..cmd.len() - 1]
                } else {
                    cmd
                };
                config.proxy_command = Some(cmd.to_string());
            } else if line_lower.starts_with("proxyusefdpass ") {
                let value = line[15..].trim().to_lowercase();
                config.proxy_use_fdpass = value == "yes" || value == "true" || value == "1";
            } else if line_lower.starts_with("identitiesonly ") {
                let value = line[15..].trim().to_lowercase();
                config.identities_only = value == "yes" || value == "true" || value == "1";
            }
        }
    }

    tracing::debug!(hosts_count = hosts.len(), "Found hosts in config");

    if let Some(config) = hosts.get(host_alias) {
        tracing::debug!(
            host = %host_alias,
            hostname = ?config.hostname,
            user = ?config.user,
            port = ?config.port,
            "Found exact match"
        );
        return Ok(config.clone());
    }

    for (host_pattern, config) in &hosts {
        if host_pattern.contains('*') {
            let pattern = host_pattern.replace("*", ".*");
            if let Ok(re) = regex::Regex::new(&format!("^{}$", pattern))
                && re.is_match(host_alias)
            {
                return Ok(config.clone());
            }
        }
    }

    anyhow::bail!("Host '{}' not found in SSH config", host_alias)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_path() {
        let home = "/home/user";
        assert_eq!(
            expand_path("~/test", home),
            PathBuf::from("/home/user/test")
        );
        assert_eq!(expand_path("~", home), PathBuf::from("/home/user"));
        assert_eq!(
            expand_path("/absolute/path", home),
            PathBuf::from("/absolute/path")
        );
        assert_eq!(expand_path("relative", home), PathBuf::from("relative"));
    }

    #[test]
    fn test_parse_include_directive_helper() {
        let home = "/home/user";
        let result = parse_include_directive("Include /nonexistent/path", home);
        assert!(result.is_empty());

        let result = parse_include_directive("NotInclude something", home);
        assert!(result.is_empty());
    }

    #[test]
    fn test_proxy_command_quote_stripping() {
        let cmd_with_both_quotes = "\"quoted command\"";
        let stripped = if (cmd_with_both_quotes.starts_with('"')
            && cmd_with_both_quotes.ends_with('"'))
            || (cmd_with_both_quotes.starts_with('\'') && cmd_with_both_quotes.ends_with('\''))
        {
            &cmd_with_both_quotes[1..cmd_with_both_quotes.len() - 1]
        } else {
            cmd_with_both_quotes
        };
        assert_eq!(stripped, "quoted command");

        let cmd_single_quotes = "'single quoted'";
        let stripped = if (cmd_single_quotes.starts_with('"') && cmd_single_quotes.ends_with('"'))
            || (cmd_single_quotes.starts_with('\'') && cmd_single_quotes.ends_with('\''))
        {
            &cmd_single_quotes[1..cmd_single_quotes.len() - 1]
        } else {
            cmd_single_quotes
        };
        assert_eq!(stripped, "single quoted");

        let cmd_mixed = "'/path/to/cmd' args";
        let stripped = if (cmd_mixed.starts_with('"') && cmd_mixed.ends_with('"'))
            || (cmd_mixed.starts_with('\'') && cmd_mixed.ends_with('\''))
        {
            &cmd_mixed[1..cmd_mixed.len() - 1]
        } else {
            cmd_mixed
        };
        assert_eq!(stripped, "'/path/to/cmd' args");
    }

    #[test]
    fn test_boolean_parsing() {
        let values_true = ["yes", "Yes", "YES", "true", "True", "1"];
        let values_false = ["no", "No", "NO", "false", "False", "0", "other"];

        for v in values_true {
            let lower = v.to_lowercase();
            assert!(
                lower == "yes" || lower == "true" || lower == "1",
                "Expected true for {}",
                v
            );
        }

        for v in values_false {
            let lower = v.to_lowercase();
            assert!(
                !(lower == "yes" || lower == "true" || lower == "1"),
                "Expected false for {}",
                v
            );
        }
    }
}
