use std::fmt;

/// SSH command execution failure.
#[derive(Clone, Debug)]
pub enum SshCommandError {
    /// Idle or hard-duration limit exceeded while waiting for command output.
    CommandTimeout {
        /// Timeout limit that was exceeded.
        limit: std::time::Duration,

        /// Indicator whether the idle (no-output) limit was hit rather than max duration.
        idle: bool,

        /// Indicator whether the persistent shell was restored after the timeout.
        session_recovered: bool,
    },

    /// SSH transport or channel failure.
    ConnectionLost(String),

    /// Remote command finished with a non-zero exit code.
    CommandFailed {
        /// Process exit code.
        exit_code: i32,

        /// Captured stdout before the failure was reported.
        stdout: String,
    },

    /// No live session for the requested host alias.
    NotConnected(String),

    /// Remote sudo prompted for a password and none was supplied.
    SudoPasswordRequired,

    /// Local I/O failure on the SSH channel.
    Io(String),
}

impl SshCommandError {
    /// Indicates whether the managed session should be dropped after this error.
    pub fn should_evict_session(&self) -> bool {
        matches!(
            self,
            SshCommandError::ConnectionLost(_)
                | SshCommandError::Io(_)
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
                    "Command timeout ({kind} limit {limit:?}). Session {}",
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
