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

/// Output from an executed SSH command.
#[derive(Debug, Clone)]
pub struct CommandOutput {
    /// Standard output from the command.
    pub stdout: String,
    /// Standard error output from the command.
    pub stderr: String,
}

impl Default for CommandOutput {
    fn default() -> Self {
        Self {
            stdout: String::new(),
            stderr: String::new(),
        }
    }
}

impl CommandOutput {
    /// Returns combined stdout and stderr, with proper newline handling.
    pub fn combined(&self) -> String {
        let mut result = String::new();
        if !self.stdout.trim().is_empty() {
            result.push_str(&self.stdout);
        }
        if !self.stderr.trim().is_empty() {
            if !result.is_empty() && !result.ends_with('\n') {
                result.push('\n');
            }
            result.push_str(&self.stderr);
        }
        result
    }
}

/// Persistent shell channel for executing commands over SSH.
///
/// Maintains shell state (current directory, environment variables) between commands.
pub struct ShellChannel {
    channel: AsyncChannel<TokioTcpStream>,
}

fn find_last_marker_position(output: &str, marker: &str) -> Option<usize> {
    let mut last_marker_pos = None;
    let mut search_pos = 0;
    while let Some(pos) = output[search_pos..].find(marker) {
        let abs_pos = search_pos + pos;
        last_marker_pos = Some(abs_pos);
        search_pos = abs_pos + marker.len();
    }
    last_marker_pos
}

fn remove_command_echo(output: &mut String, command: &str, marker: &str) {
    let full_cmd = format!("{}; echo {}", command, marker);
    while let Some(cmd_pos) = output.find(&full_cmd) {
        output.replace_range(cmd_pos..cmd_pos + full_cmd.len(), "");
    }
}

impl ShellChannel {
    /// Creates a new `ShellChannel` from an SSH channel.
    pub fn new(channel: AsyncChannel<TokioTcpStream>) -> Self {
        Self { channel }
    }

    /// Executes a command on the remote shell and returns its output.
    ///
    /// Uses a unique marker to detect command completion. The marker is appended
    /// to the command and detected in the output stream to determine when execution finishes.
    ///
    /// # Arguments
    ///
    /// * `command` - The shell command to execute
    ///
    /// # Returns
    ///
    /// Returns `CommandOutput` containing stdout and stderr. In PTY mode, stderr
    /// is typically empty as streams are merged.
    ///
    /// # Errors
    ///
    /// Returns an error if the command times out, the channel fails, or I/O errors occur.
    pub async fn execute_command(&mut self, command: &str) -> Result<CommandOutput> {
        let debug = std::env::var("SSH_LIAISON_DEBUG").unwrap_or_else(|_| "0".to_string()) == "1";

        let marker = generate_marker();
        let full_command = format!("{}; echo {}\n", command, marker);

        if debug {
            eprintln!("[DEBUG] Executing command: {}", command);
            eprintln!("[DEBUG] Full command with marker: {}", full_command.trim());
        }

        self.channel.write_all(full_command.as_bytes()).await?;
        self.channel.flush().await?;

        if debug {
            eprintln!("[DEBUG] Command sent, starting to read...");
        }

        let mut stdout = String::new();
        let mut buffer = vec![0u8; READ_BUFFER_SIZE];
        let start = Instant::now();
        let mut marker_found = false;
        let mut last_read_time = Instant::now();
        let mut no_data_count = 0;

        loop {
            if start.elapsed() > COMMAND_TIMEOUT {
                if debug {
                    eprintln!("[DEBUG] TIMEOUT after {:?}", start.elapsed());
                }
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
                                if debug {
                                    eprintln!("[DEBUG] No data for {}ms, assuming command completed", IDLE_TIMEOUT_MS);
                                }
                                break;
                            }
                            sleep(Duration::from_millis(SLEEP_ON_EOF_MS)).await;
                        }
                        Ok(n) => {
                            last_read_time = Instant::now();
                            no_data_count = 0;
                            let chunk = String::from_utf8_lossy(&buffer[..n]);
                            if debug {
                                eprintln!("[DEBUG] Read {} bytes: {:?}", n, chunk.chars().take(100).collect::<String>());
                            }
                            stdout.push_str(&chunk);

                            if let Some(marker_pos) = stdout.find(&marker) {
                                if debug {
                                    eprintln!("[DEBUG] Marker found at position {}", marker_pos);
                                }

                                // Continue reading to capture all output after initial marker detection.
                                // The marker may appear in the command echo, but the actual completion
                                // marker comes after command execution.
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

                                if let Some(pos) = find_last_marker_position(&stdout, &marker) {
                                    if debug {
                                        eprintln!("[DEBUG] Using last marker at position {}, total len={}", pos, stdout.len());
                                    }
                                    stdout.truncate(pos);
                                    remove_command_echo(&mut stdout, command, &marker);
                                    marker_found = true;
                                    break;
                                }
                            }
                        }
                        Err(e) => {
                            if debug {
                                eprintln!("[DEBUG] Read error: {:?}", e);
                            }
                            sleep(Duration::from_millis(SLEEP_ON_ERROR_MS)).await;
                        }
                    }
                }
                _ = timeout_future => {
                    if marker_found {
                        break;
                    }
                    if last_read_time.elapsed() > Duration::from_millis(IDLE_TIMEOUT_MS) && no_data_count > 5 {
                        if debug {
                            eprintln!("[DEBUG] No data for {}ms, breaking", IDLE_TIMEOUT_MS);
                        }
                        break;
                    }
                }
            }
        }

        if debug {
            eprintln!(
                "[DEBUG] Loop finished, stdout.len={}, marker_found={}",
                stdout.len(),
                marker_found
            );
        }

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
    let ansi_re = ANSI_REGEX.get_or_init(|| {
        Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]").expect("ANSI regex should be valid")
    });
    let mut cleaned = ansi_re.replace_all(text, "").to_string();

    let osc_re = OSC_REGEX.get_or_init(|| {
        Regex::new(r"\x1b\][^\x07]*\x07").expect("OSC regex should be valid")
    });
    cleaned = osc_re.replace_all(&cleaned, "").to_string();

    let other_re = OTHER_ESCAPE_REGEX.get_or_init(|| {
        Regex::new(r"\x1b[P^_].*?\x1b\\").expect("Other escape regex should be valid")
    });
    cleaned = other_re.replace_all(&cleaned, "").to_string();

    cleaned
}

impl ShellChannel {
    /// Executes a command and streams output to stdout in real-time.
    ///
    /// Similar to `execute_command`, but prints output as it arrives rather than
    /// collecting it all before returning.
    ///
    /// # Arguments
    ///
    /// * `command` - The shell command to execute
    ///
    /// # Returns
    ///
    /// Returns the accumulated stdout output as a string.
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

    /// Writes data directly to the shell channel.
    #[allow(dead_code)]
    pub async fn write(&mut self, data: &[u8]) -> Result<()> {
        self.channel.write_all(data).await?;
        self.channel.flush().await?;
        Ok(())
    }

    /// Closes the shell channel.
    pub async fn close(mut self) -> Result<()> {
        self.channel.close().await?;
        Ok(())
    }
}
