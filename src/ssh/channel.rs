use crate::ssh::error::SshCommandError;
use async_ssh2_lite::AsyncChannel;
use async_ssh2_lite::TokioTcpStream;
use regex::Regex;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::sleep;

const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 30;
const DEFAULT_MAX_TIMEOUT_SECS: u64 = 30;
const READ_BUFFER_SIZE: usize = 4096;
const READ_POLL_MS: u64 = 100;
const CONTINUE_READ_ATTEMPTS: u32 = 10;
const CONTINUE_READ_TIMEOUT_MS: u64 = 100;
const CONTINUE_READ_MAX_FAILURES: u32 = 3;
const SLEEP_ON_EOF_MS: u64 = 50;
const DRAIN_IDLE_MS: u64 = 2000;
const DRAIN_MAX_ATTEMPTS: u32 = 40;
const WRITE_TIMEOUT_MS: u64 = 10_000;
const INTERRUPT_SETTLE_MS: u64 = 150;
const CTRL_C: &[u8] = b"\x03";
const CTRL_BACKSLASH: &[u8] = b"\x1c";

/// Idle and hard-duration limits for a synchronous SSH command.
#[derive(Clone, Copy, Debug)]
pub struct CommandTimeouts {
    /// Maximum silence between output chunks before the command is interrupted.
    pub idle: Duration,

    /// Absolute maximum runtime regardless of output activity.
    pub max: Duration,
}

impl Default for CommandTimeouts {
    fn default() -> Self {
        Self::from_secs(None, None)
    }
}

impl CommandTimeouts {
    /// Builds timeouts from optional second counts, applying defaults and flooring max to idle.
    pub fn from_secs(idle_secs: Option<u64>, max_secs: Option<u64>) -> Self {
        let idle = Duration::from_secs(idle_secs.unwrap_or(DEFAULT_IDLE_TIMEOUT_SECS));
        let max_raw = max_secs.unwrap_or(DEFAULT_MAX_TIMEOUT_SECS);
        let max = Duration::from_secs(max_raw.max(idle.as_secs()));
        Self { idle, max }
    }
}

fn generate_marker() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("__SSH_CMD_DONE_{timestamp}__")
}

/// Captured result of a remote shell command.
#[derive(Clone, Debug, Default)]
pub struct CommandOutput {
    /// Command stdout (ANSI sequences stripped).
    pub stdout: String,

    /// Command stderr when available.
    pub stderr: String,

    /// Remote exit code when a completion marker was observed.
    pub exit_code: Option<i32>,
}

impl CommandOutput {
    /// Formats stdout and labeled stderr into a single MCP-facing string.
    pub fn combined_with_stderr_label(&self) -> String {
        let mut result = String::new();
        if !self.stdout.trim().is_empty() {
            result.push_str(&self.stdout);
        }
        if !self.stderr.trim().is_empty() {
            if !result.is_empty() && !result.ends_with('\n') {
                result.push('\n');
            }
            result.push_str("STDERR:\n");
            result.push_str(&self.stderr);
        }
        result
    }
}

/// Persistent interactive shell channel used for marker-wrapped commands.
pub struct ShellChannel {
    channel: AsyncChannel<TokioTcpStream>,
}

/// Finds the last completed marker line (`MARKER:0`), ignoring PTY echo (`MARKER:$?`).
fn find_last_completed_marker(output: &str, marker: &str) -> Option<usize> {
    let mut last_pos = None;
    let mut search_pos = 0;
    while let Some(pos) = output[search_pos..].find(marker) {
        let abs_pos = search_pos + pos;
        let at_line_start =
            abs_pos == 0 || output.as_bytes().get(abs_pos.wrapping_sub(1)) == Some(&b'\n');
        if at_line_start {
            let after = abs_pos + marker.len();
            if output.as_bytes().get(after) == Some(&b':') {
                let code = &output[after + 1..];
                let digits = code.chars().take_while(|c| c.is_ascii_digit()).count();
                if digits > 0 {
                    let next = code.chars().nth(digits);
                    if next.is_none_or(|c| c == '\n' || c == '\r') {
                        last_pos = Some(abs_pos);
                    }
                }
            }
        }
        search_pos = abs_pos + marker.len();
    }
    last_pos
}

fn parse_exit_code_after_marker(output: &str, marker: &str, marker_pos: usize) -> Option<i32> {
    let tail = &output[marker_pos + marker.len()..];
    let line_end = tail.find('\n').unwrap_or(tail.len());
    let after = &tail[..line_end];
    let code_str = after.strip_prefix(':')?;
    code_str.trim().parse().ok()
}

fn wrap_command_for_marker(command: &str, marker: &str) -> String {
    format!("set +e; {command}; echo {marker}:$?")
}

fn remove_command_echo(output: &mut String, command: &str, marker: &str) {
    for suffix in ["", ":0", ":1", ":7"] {
        for wrapper in [
            wrap_command_for_marker(command, marker),
            format!("{command}; echo {marker}{suffix}"),
        ] {
            while let Some(cmd_pos) = output.find(&wrapper) {
                output.replace_range(cmd_pos..cmd_pos + wrapper.len(), "");
            }
        }
    }
    let legacy = format!("{command}; echo {marker}:$?");
    while let Some(cmd_pos) = output.find(&legacy) {
        output.replace_range(cmd_pos..cmd_pos + legacy.len(), "");
    }
}

impl ShellChannel {
    pub fn new(channel: AsyncChannel<TokioTcpStream>) -> Self {
        Self { channel }
    }

    pub(crate) async fn send_raw(&mut self, data: &str) -> Result<(), SshCommandError> {
        self.write_bytes(data.as_bytes()).await
    }

    pub async fn drain_pending(&mut self, max_ms: u64) {
        let started = Instant::now();
        let mut buffer = vec![0u8; READ_BUFFER_SIZE];
        while started.elapsed() < Duration::from_millis(max_ms) {
            match tokio::time::timeout(
                Duration::from_millis(READ_POLL_MS),
                self.channel.read(&mut buffer),
            )
            .await
            {
                Ok(Ok(0)) => sleep(Duration::from_millis(SLEEP_ON_EOF_MS)).await,
                Ok(Ok(_)) => {}
                _ => break,
            }
        }
    }

    async fn write_bytes(&mut self, data: &[u8]) -> Result<(), SshCommandError> {
        let write = async {
            self.channel
                .write_all(data)
                .await
                .map_err(|e| SshCommandError::Io(e.to_string()))?;
            self.channel
                .flush()
                .await
                .map_err(|e| SshCommandError::Io(e.to_string()))?;
            Ok(())
        };
        match tokio::time::timeout(Duration::from_millis(WRITE_TIMEOUT_MS), write).await {
            Ok(result) => result,
            Err(_) => Err(SshCommandError::Io(format!(
                "SSH channel write timed out after {WRITE_TIMEOUT_MS}ms"
            ))),
        }
    }

    async fn send_signal_raw(&mut self, signal: &[u8]) {
        _ = tokio::time::timeout(Duration::from_millis(WRITE_TIMEOUT_MS), async {
            _ = self.channel.write_all(signal).await;
            _ = self.channel.flush().await;
        })
        .await;
        sleep(Duration::from_millis(INTERRUPT_SETTLE_MS)).await;
    }

    async fn probe_shell_alive(&mut self) -> bool {
        let probe = generate_marker();
        let probe_cmd = format!("echo {probe}:$?\n");
        if self.write_bytes(probe_cmd.as_bytes()).await.is_err() {
            return false;
        }

        let mut buffer = vec![0u8; READ_BUFFER_SIZE];
        let mut accumulated = String::new();
        let started = Instant::now();
        let mut attempts = 0u32;

        while started.elapsed() < Duration::from_millis(DRAIN_IDLE_MS)
            && attempts < DRAIN_MAX_ATTEMPTS
        {
            attempts += 1;
            match tokio::time::timeout(
                Duration::from_millis(READ_POLL_MS),
                self.channel.read(&mut buffer),
            )
            .await
            {
                Ok(Ok(0)) => {
                    if self.channel.eof() {
                        return false;
                    }
                    sleep(Duration::from_millis(SLEEP_ON_EOF_MS)).await;
                }
                Ok(Ok(n)) => {
                    accumulated.push_str(&String::from_utf8_lossy(&buffer[..n]));
                    if find_last_completed_marker(&accumulated, &probe).is_some() {
                        return true;
                    }
                }
                Ok(Err(_)) => return false,
                Err(_) => sleep(Duration::from_millis(SLEEP_ON_EOF_MS)).await,
            }
        }

        false
    }

    async fn interrupt_and_drain(&mut self) -> bool {
        self.send_signal_raw(CTRL_C).await;
        if self.probe_shell_alive().await {
            return true;
        }

        self.send_signal_raw(CTRL_BACKSLASH).await;
        self.probe_shell_alive().await
    }

    async fn finish_after_timeout(
        &mut self,
        limit: Duration,
        idle: bool,
    ) -> Result<CommandOutput, SshCommandError> {
        let recovered = self.interrupt_and_drain().await;
        Err(SshCommandError::CommandTimeout {
            limit,
            idle,
            session_recovered: recovered,
        })
    }

    pub async fn execute_command(
        &mut self,
        command: &str,
        sudo_password: Option<&str>,
        timeouts: CommandTimeouts,
    ) -> Result<CommandOutput, SshCommandError> {
        let marker = generate_marker();
        let full_command = format!("{};\n", wrap_command_for_marker(command, &marker));

        tracing::debug!(command = %command, ?timeouts, "Executing command");
        tracing::trace!(full_command = %full_command.trim(), "Full command with marker");

        self.write_bytes(full_command.as_bytes()).await?;

        let mut stdout = String::new();
        let mut buffer = vec![0u8; READ_BUFFER_SIZE];
        let started = Instant::now();
        let mut last_activity = Instant::now();
        let mut sudo_password_sent = false;

        loop {
            if started.elapsed() >= timeouts.max {
                return self.finish_after_timeout(timeouts.max, false).await;
            }
            if last_activity.elapsed() >= timeouts.idle {
                return self.finish_after_timeout(timeouts.idle, true).await;
            }

            let read_future = self.channel.read(&mut buffer);
            let poll = sleep(Duration::from_millis(READ_POLL_MS));

            tokio::select! {
                result = read_future => {
                    match result {
                        Ok(0) => {
                            if self.channel.eof() {
                                return Err(SshCommandError::ConnectionLost(
                                    "SSH channel closed".into(),
                                ));
                            }
                            sleep(Duration::from_millis(SLEEP_ON_EOF_MS)).await;
                        }
                        Ok(n) => {
                            last_activity = Instant::now();
                            let chunk = String::from_utf8_lossy(&buffer[..n]);
                            tracing::trace!(bytes = n, "Read data");
                            stdout.push_str(&chunk);

                            let sudo_prompt = stdout.contains("[sudo] password")
                                || stdout.contains("Password:");
                            if sudo_prompt && !sudo_password_sent {
                                if let Some(pass) = sudo_password {
                                    tracing::trace!("Sudo password prompt detected, sending response");
                                    self.write_bytes(format!("{pass}\n").as_bytes()).await?;
                                    sudo_password_sent = true;
                                    last_activity = Instant::now();
                                } else {
                                    return Err(SshCommandError::SudoPasswordRequired);
                                }
                            }

                            if find_last_completed_marker(&stdout, &marker).is_some() {
                                let mut continue_reading = true;
                                let mut read_attempts = 0;

                                while continue_reading && read_attempts < CONTINUE_READ_ATTEMPTS {
                                    match tokio::time::timeout(
                                        Duration::from_millis(CONTINUE_READ_TIMEOUT_MS),
                                        self.channel.read(&mut buffer),
                                    )
                                    .await
                                    {
                                        Ok(Ok(0)) => {
                                            read_attempts += 1;
                                            sleep(Duration::from_millis(SLEEP_ON_EOF_MS)).await;
                                        }
                                        Ok(Ok(n)) => {
                                            let chunk = String::from_utf8_lossy(&buffer[..n]);
                                            stdout.push_str(&chunk);
                                            read_attempts = 0;
                                            if find_last_completed_marker(&stdout, &marker).is_some() {
                                                continue_reading = false;
                                            }
                                        }
                                        _ => {
                                            read_attempts += 1;
                                            if read_attempts > CONTINUE_READ_MAX_FAILURES {
                                                continue_reading = false;
                                            }
                                        }
                                    }
                                }

                                if let Some(pos) = find_last_completed_marker(&stdout, &marker) {
                                    let exit_code = parse_exit_code_after_marker(&stdout, &marker, pos);
                                    stdout.truncate(pos);
                                    remove_command_echo(&mut stdout, command, &marker);
                                    let cleaned = clean_ansi_sequences(&stdout);
                                    return Ok(CommandOutput {
                                        stdout: cleaned.trim_end().to_string(),
                                        stderr: String::new(),
                                        exit_code,
                                    });
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "SSH channel read failed");
                            return Err(SshCommandError::ConnectionLost(e.to_string()));
                        }
                    }
                }
                _ = poll => {}
            }
        }
    }

    pub async fn shutdown(&mut self) -> Result<(), SshCommandError> {
        self.channel
            .close()
            .await
            .map_err(|e| SshCommandError::Io(e.to_string()))?;
        Ok(())
    }
}

static ANSI_REGEX: OnceLock<Regex> = OnceLock::new();
static OSC_REGEX: OnceLock<Regex> = OnceLock::new();
static OTHER_ESCAPE_REGEX: OnceLock<Regex> = OnceLock::new();

fn clean_ansi_sequences(text: &str) -> String {
    let ansi_re = ANSI_REGEX
        .get_or_init(|| Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]").expect("ANSI regex should be valid"));
    let mut cleaned = ansi_re.replace_all(text, "").to_string();

    let osc_re = OSC_REGEX
        .get_or_init(|| Regex::new(r"\x1b\][^\x07]*\x07").expect("OSC regex should be valid"));
    cleaned = osc_re.replace_all(&cleaned, "").to_string();

    let other_re = OTHER_ESCAPE_REGEX.get_or_init(|| {
        Regex::new(r"\x1b[P^_].*?\x1b\\").expect("Other escape regex should be valid")
    });
    cleaned = other_re.replace_all(&cleaned, "").to_string();

    cleaned
}

impl ShellChannel {
    #[allow(dead_code)]
    pub async fn execute_command_streaming(
        &mut self,
        command: &str,
    ) -> Result<String, SshCommandError> {
        let output = self
            .execute_command(command, None, CommandTimeouts::default())
            .await?;
        Ok(output.stdout)
    }

    #[allow(dead_code)]
    pub async fn write(&mut self, data: &[u8]) -> Result<(), SshCommandError> {
        self.write_bytes(data).await
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn command_timeouts_default_and_cap() {
        let t = CommandTimeouts::from_secs(None, None);

        assert_eq!(t.idle, Duration::from_secs(30));
        assert_eq!(t.max, Duration::from_secs(30));

        let explicit = CommandTimeouts::from_secs(Some(10), Some(600));

        assert_eq!(explicit.max, Duration::from_secs(600));
        assert!(explicit.max >= explicit.idle);

        let floor = CommandTimeouts::from_secs(Some(45), Some(10));

        assert_eq!(floor.max, Duration::from_secs(45));
    }

    #[test]
    fn completed_marker_ignores_pty_echo_with_dollar_question() {
        let marker = "__SSH_CMD_DONE_1__";
        let out = format!("{marker}:$?\noutput\n{marker}:0\n");
        let pos = find_last_completed_marker(&out, marker).unwrap();
        assert_eq!(pos, out.rfind(marker).unwrap());
    }

    #[test]
    fn test_parse_exit_code_after_marker() {
        let marker = "__SSH_CMD_DONE_99__";
        let out = format!("ok\n{marker}:42\n");
        let pos = find_last_completed_marker(&out, marker).unwrap();
        assert_eq!(parse_exit_code_after_marker(&out, marker, pos), Some(42));
    }

    #[test]
    fn remove_command_echo_strips_sent_line() {
        let marker = "__SSH_CMD_DONE_1__";
        let mut out = format!(
            "{marker}:0\n{wrapped}\noutput\n",
            marker = marker,
            wrapped = wrap_command_for_marker("pwd", marker)
        );
        remove_command_echo(&mut out, "pwd", marker);
        assert!(!out.contains("echo"));
        assert!(out.contains("output"));
    }
}
