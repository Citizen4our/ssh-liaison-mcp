use std::fmt;

#[derive(Debug, Clone)]
pub enum SshCommandError {
    CommandTimeout {
        limit: std::time::Duration,
        idle: bool,
        session_recovered: bool,
    },
    ConnectionLost(String),
    CommandFailed {
        exit_code: i32,
        stdout: String,
    },
    NotConnected(String),
    SudoPasswordRequired,
    Io(String),
}

impl SshCommandError {
    pub fn should_evict_session(&self) -> bool {
        matches!(
            self,
            SshCommandError::ConnectionLost(_)
                | SshCommandError::CommandTimeout {
                    session_recovered: false,
                    ..
                }
        )
    }
}

impl fmt::Display for SshCommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SshCommandError::CommandTimeout {
                limit,
                idle,
                session_recovered,
            } => {
                let kind = if *idle {
                    "no output"
                } else {
                    "maximum duration"
                };
                write!(
                    f,
                    "Command timeout ({kind} limit {:?}). Session {}",
                    limit,
                    if *session_recovered {
                        "recovered; retry command."
                    } else {
                        "unusable; reconnect with ssh_connect."
                    }
                )
            }
            SshCommandError::ConnectionLost(msg) => {
                write!(f, "Connection lost: {msg}. Reconnect with ssh_connect.")
            }
            SshCommandError::CommandFailed { exit_code, stdout } => {
                write!(f, "Command failed with exit code {exit_code}")?;
                if !stdout.trim().is_empty() {
                    write!(f, ":\n{stdout}")?;
                }
                Ok(())
            }
            SshCommandError::NotConnected(host) => {
                write!(f, "Not connected to host '{host}'")
            }
            SshCommandError::SudoPasswordRequired => {
                write!(
                    f,
                    "Command requires sudo password. Elicitation support coming soon. \
                     Please ensure the user has passwordless sudo configured or handle manually."
                )
            }
            SshCommandError::Io(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for SshCommandError {}
