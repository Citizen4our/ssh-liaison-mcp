use anyhow::Result;
use async_ssh2_lite::AsyncChannel;
use async_ssh2_lite::TokioTcpStream;
use regex::Regex;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::sleep;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const READ_BUFFER_SIZE: usize = 4096;
const READ_TIMEOUT_MS: u64 = 100;
const NO_DATA_THRESHOLD: u32 = 10;
const IDLE_TIMEOUT_MS: u64 = 500;
const CONTINUE_READ_ATTEMPTS: u32 = 10;
const CONTINUE_READ_TIMEOUT_MS: u64 = 100;
const CONTINUE_READ_MAX_FAILURES: u32 = 3;
const SLEEP_ON_EOF_MS: u64 = 50;
const SLEEP_ON_ERROR_MS: u64 = 10;

fn generate_marker() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("__SSH_CMD_DONE_{}__", timestamp)
}

#[derive(Debug, Clone, Default)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
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

pub struct ShellChannel {
    channel: AsyncChannel<TokioTcpStream>,
}

/// Marker on own line (preceded by \n or at start) — ignores echoed command, truncates at real marker.
fn find_last_marker_on_own_line(output: &str, marker: &str) -> Option<usize> {
    let mut last_pos = None;
    let mut search_pos = 0;
    while let Some(pos) = output[search_pos..].find(marker) {
        let abs_pos = search_pos + pos;
        let at_line_start =
            abs_pos == 0 || output.as_bytes().get(abs_pos.wrapping_sub(1)) == Some(&b'\n');
        if at_line_start {
            last_pos = Some(abs_pos);
        }
        search_pos = abs_pos + marker.len();
    }
    last_pos
}

fn remove_command_echo(output: &mut String, command: &str, marker: &str) {
    let full_cmd = format!("{}; echo {}", command, marker);
    while let Some(cmd_pos) = output.find(&full_cmd) {
        output.replace_range(cmd_pos..cmd_pos + full_cmd.len(), "");
    }
}

impl ShellChannel {
    pub fn new(channel: AsyncChannel<TokioTcpStream>) -> Self {
        Self { channel }
    }

    pub async fn execute_command(
        &mut self,
        command: &str,
        sudo_password: Option<&str>,
    ) -> Result<CommandOutput> {
        let marker = generate_marker();
        let full_command = format!("{}; echo {}\n", command, marker);

        tracing::debug!(command = %command, "Executing command");
        tracing::trace!(full_command = %full_command.trim(), "Full command with marker");

        self.channel.write_all(full_command.as_bytes()).await?;
        self.channel.flush().await?;

        tracing::trace!("Command sent, starting to read");

        let mut stdout = String::new();
        let mut buffer = vec![0u8; READ_BUFFER_SIZE];
        let start = Instant::now();
        let mut marker_found = false;
        let mut last_read_time = Instant::now();
        let mut no_data_count = 0;
        let mut sudo_password_sent = false;

        loop {
            if start.elapsed() > COMMAND_TIMEOUT {
                tracing::warn!(elapsed = ?start.elapsed(), "Command timeout");
                anyhow::bail!("Command timeout after {:?}", COMMAND_TIMEOUT);
            }

            let read_future = self.channel.read(&mut buffer);
            let timeout_future = sleep(Duration::from_millis(READ_TIMEOUT_MS));

            tokio::select! {
                result = read_future => {
                    match result {
                        Ok(0) => {
                            if marker_found {
                                break;
                            }
                            no_data_count += 1;
                            if no_data_count > NO_DATA_THRESHOLD && last_read_time.elapsed() > Duration::from_millis(IDLE_TIMEOUT_MS) {
                                tracing::trace!(idle_ms = IDLE_TIMEOUT_MS, "No data, assuming command completed");
                                break;
                            }
                            sleep(Duration::from_millis(SLEEP_ON_EOF_MS)).await;
                        }
                        Ok(n) => {
                            last_read_time = Instant::now();
                            no_data_count = 0;
                            let chunk = String::from_utf8_lossy(&buffer[..n]);
                            tracing::trace!(bytes = n, "Read data");
                            stdout.push_str(&chunk);

                            let sudo_prompt = stdout.contains("[sudo] password")
                                || stdout.contains("Password:");
                            if sudo_prompt && !sudo_password_sent {
                                if let Some(pass) = sudo_password {
                                    tracing::trace!("Sudo password prompt detected, sending response");
                                    self.channel
                                        .write_all(format!("{}\n", pass).as_bytes())
                                        .await?;
                                    self.channel.flush().await?;
                                    sudo_password_sent = true;
                                } else {
                                    anyhow::bail!(
                                        "Command requires sudo password. Elicitation support coming soon. \
                                        Please ensure the user has passwordless sudo configured or handle manually."
                                    );
                                }
                            }

                            if let Some(marker_pos) = find_last_marker_on_own_line(&stdout, &marker) {
                                tracing::trace!(position = marker_pos, "Marker found");

                                let mut continue_reading = true;
                                let mut read_attempts = 0;

                                while continue_reading && read_attempts < CONTINUE_READ_ATTEMPTS {
                                    match tokio::time::timeout(Duration::from_millis(CONTINUE_READ_TIMEOUT_MS), self.channel.read(&mut buffer)).await {
                                        Ok(Ok(0)) => {
                                            read_attempts += 1;
                                            sleep(Duration::from_millis(SLEEP_ON_EOF_MS)).await;
                                        }
                                        Ok(Ok(n)) => {
                                            let chunk = String::from_utf8_lossy(&buffer[..n]);
                                            stdout.push_str(&chunk);
                                            read_attempts = 0;

                                            if chunk.contains(&marker) {
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

                                if let Some(pos) = find_last_marker_on_own_line(&stdout, &marker) {
                                    tracing::trace!(position = pos, total_len = stdout.len(), "Using marker on own line");
                                    stdout.truncate(pos);
                                    remove_command_echo(&mut stdout, command, &marker);
                                    marker_found = true;
                                    break;
                                }
                            }
                        }
                        Err(e) => {
                            tracing::trace!(error = %e, "Read error");
                            sleep(Duration::from_millis(SLEEP_ON_ERROR_MS)).await;
                        }
                    }
                }
                _ = timeout_future => {
                    if marker_found {
                        break;
                    }
                    if last_read_time.elapsed() > Duration::from_millis(IDLE_TIMEOUT_MS) && no_data_count > 5 {
                        tracing::trace!(idle_ms = IDLE_TIMEOUT_MS, "No data, breaking");
                        break;
                    }
                }
            }
        }

        tracing::trace!(
            stdout_len = stdout.len(),
            marker_found = marker_found,
            "Loop finished"
        );

        let cleaned = clean_ansi_sequences(&stdout);

        Ok(CommandOutput {
            stdout: cleaned.trim_end().to_string(),
            stderr: String::new(),
        })
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
    pub async fn execute_command_streaming(&mut self, command: &str) -> Result<String> {
        let marker = generate_marker();
        let full_command = format!("{}; echo {}\n", command, marker);

        self.channel.write_all(full_command.as_bytes()).await?;
        self.channel.flush().await?;

        let mut buffer = vec![0u8; READ_BUFFER_SIZE];
        let mut stdout_accumulated = String::new();
        let start = Instant::now();

        loop {
            if start.elapsed() > COMMAND_TIMEOUT {
                anyhow::bail!("Command timeout after {:?}", COMMAND_TIMEOUT);
            }

            match self.channel.read(&mut buffer).await {
                Ok(0) => {
                    sleep(Duration::from_millis(SLEEP_ON_EOF_MS)).await;
                    continue;
                }
                Ok(n) => {
                    let chunk = String::from_utf8_lossy(&buffer[..n]);
                    stdout_accumulated.push_str(&chunk);

                    if let Some(pos) = stdout_accumulated.find(&marker) {
                        stdout_accumulated.truncate(pos);
                        break;
                    }

                    print!("{}", chunk);
                    use std::io::Write;
                    std::io::stdout().flush()?;
                }
                Err(_) => {
                    sleep(Duration::from_millis(SLEEP_ON_ERROR_MS)).await;
                }
            }
        }

        println!();

        Ok(stdout_accumulated)
    }

    #[allow(dead_code)]
    pub async fn write(&mut self, data: &[u8]) -> Result<()> {
        self.channel.write_all(data).await?;
        self.channel.flush().await?;
        Ok(())
    }

    pub async fn close(mut self) -> Result<()> {
        self.channel.close().await?;
        Ok(())
    }
}
