use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use std::time::{SystemTime, UNIX_EPOCH};

pub const JOBS_ROOT: &str = "/tmp/ssh-liaison-jobs";
pub const JOBS_RETENTION_DAYS: u64 = 7;
pub const CONTROL_IDLE_SECS: u64 = 15;
pub const CONTROL_MAX_SECS: u64 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobSignal {
    Int,
    Term,
    Kill,
}

impl JobSignal {
    fn name(self) -> &'static str {
        match self {
            JobSignal::Int => "INT",
            JobSignal::Term => "TERM",
            JobSignal::Kill => "KILL",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Running,
    Exited(i32),
}

pub fn new_job_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("slj_{nanos}")
}

pub fn job_dir(id: &str) -> String {
    format!("{JOBS_ROOT}/{id}")
}

fn session_name(id: &str) -> String {
    id.to_string()
}

pub fn detect_tmux_command() -> &'static str {
    "command -v tmux >/dev/null 2>&1 && echo TMUX_YES || echo TMUX_NO"
}

pub fn tmux_available(output: &str) -> bool {
    output.contains("TMUX_YES")
}

pub fn backend_is_tmux(output: &str) -> bool {
    output.trim() == "tmux"
}

fn cleanup_old_jobs_cmd() -> String {
    format!(
        "find {JOBS_ROOT} -mindepth 1 -maxdepth 1 -type d -mtime +{JOBS_RETENTION_DAYS} -exec rm -rf {{}} + 2>/dev/null; "
    )
}

/// Script run on the remote: records pgid and worker pid, runs the user command
/// in the background, waits for it, then records the exit code.
fn build_script(command: &str, dir: &str) -> String {
    format!(
        "echo $$ > \"{dir}/pgid\"\n\
         {{\n{command}\n}} > \"{dir}/log\" 2>&1 &\n\
         wpid=$!\n\
         echo $wpid > \"{dir}/wpid\"\n\
         wait $wpid\n\
         echo $? > \"{dir}/rc\"\n"
    )
}

pub fn build_detach_command(command: &str, id: &str, use_tmux: bool) -> String {
    let dir = job_dir(id);
    let b64 = STANDARD.encode(build_script(command, &dir));
    let backend = if use_tmux { "tmux" } else { "native" };
    let setup = format!(
        "{}mkdir -p \"{dir}\" && echo {backend} > \"{dir}/backend\" && printf %s '{b64}' | base64 -d > \"{dir}/cmd.sh\"",
        cleanup_old_jobs_cmd(),
    );
    if use_tmux {
        let name = session_name(id);
        format!("{setup} && tmux new-session -d -s {name} sh \"{dir}/cmd.sh\" && echo {id}")
    } else {
        format!("{setup} && {{ setsid sh \"{dir}/cmd.sh\" >/dev/null 2>&1 & }} && echo {id}")
    }
}

pub fn build_status_command(id: &str, read_from: u64) -> String {
    let dir = job_dir(id);
    format!(
        "d=\"{dir}\"; \
         rc=; [ -f \"$d/rc\" ] && rc=$(cat \"$d/rc\"); \
         sz=$(wc -c < \"$d/log\" 2>/dev/null || echo 0); \
         be=$(cat \"$d/backend\" 2>/dev/null || echo native); \
         if [ -n \"$rc\" ]; then echo SLJ_META exited \"$rc\" \"$sz\" \"$be\"; \
         else echo SLJ_META running 0 \"$sz\" \"$be\"; fi; \
         tail -c +{read_from} \"$d/log\" 2>/dev/null"
    )
}

pub fn build_signal_command(id: &str, signal: JobSignal, use_tmux: bool) -> String {
    let dir = job_dir(id);
    let name = session_name(id);
    let sig = signal.name();
    let mut cmd = format!(
        "d=\"{dir}\"; wpid=$(cat \"$d/wpid\" 2>/dev/null); \
         [ -n \"$wpid\" ] && kill -{sig} \"$wpid\" 2>/dev/null; "
    );
    if matches!(signal, JobSignal::Int) {
        cmd.push_str("[ -n \"$wpid\" ] && kill -TERM \"$wpid\" 2>/dev/null; ");
    }
    if use_tmux && matches!(signal, JobSignal::Int) {
        cmd.push_str(&format!("tmux send-keys -t {name} C-c 2>/dev/null; "));
    } else if use_tmux && matches!(signal, JobSignal::Kill) {
        cmd.push_str(&format!("tmux kill-session -t {name} 2>/dev/null; "));
    }
    cmd.push_str("echo SLJ_SIGNALED");
    cmd
}

/// Returns (state, total_log_size, incremental_output, use_tmux_backend).
pub fn parse_status(raw: &str) -> (JobState, u64, String, bool) {
    let mut state = JobState::Running;
    let mut size = 0u64;
    let mut use_tmux = false;
    let mut log_lines: Vec<&str> = Vec::new();
    let mut past_meta = false;

    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("SLJ_META ") {
            let parts: Vec<&str> = rest.split_whitespace().collect();
            match parts.first().copied() {
                Some("exited") => {
                    if let Some(code) = parts.get(1).and_then(|s| s.parse().ok()) {
                        state = JobState::Exited(code);
                    }
                    if let Some(sz) = parts.get(2).and_then(|s| s.parse().ok()) {
                        size = sz;
                    }
                }
                Some("running") => {
                    state = JobState::Running;
                    if let Some(sz) = parts.get(2).and_then(|s| s.parse().ok()) {
                        size = sz;
                    }
                }
                _ => {}
            }
            if let Some(be) = parts.get(3) {
                use_tmux = backend_is_tmux(be);
            }
            past_meta = true;
            continue;
        }
        if past_meta {
            log_lines.push(line);
        }
    }

    let output = log_lines.join("\n");
    (state, size, output, use_tmux)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detach_command_native_is_single_line() {
        let cmd = build_detach_command("echo hi", "slj_1", false);
        assert!(!cmd.contains('\n'));
        assert!(cmd.contains("setsid"));
        assert!(cmd.ends_with("echo slj_1"));
    }

    #[test]
    fn detach_command_tmux_uses_session() {
        let cmd = build_detach_command("echo hi", "slj_2", true);
        assert!(cmd.contains("tmux new-session -d -s slj_2"));
    }

    #[test]
    fn parse_status_running_and_exited() {
        let raw = "SLJ_META running 0 5 tmux\nhello";
        let (state, size, out, tmux) = parse_status(raw);
        assert_eq!(state, JobState::Running);
        assert_eq!(size, 5);
        assert_eq!(out, "hello");
        assert!(tmux);

        let raw = "SLJ_META exited 42 0 native\n";
        let (state, _, out, tmux) = parse_status(raw);
        assert_eq!(state, JobState::Exited(42));
        assert!(out.is_empty());
        assert!(!tmux);
    }

    #[test]
    fn parse_status_ignores_junk_before_meta_line() {
        let raw = "root# d=/tmp/foo\nSLJ_META exited 0 10 native\noutput\n";
        let (state, size, out, _) = parse_status(raw);
        assert_eq!(state, JobState::Exited(0));
        assert_eq!(size, 10);
        assert_eq!(out.trim_end(), "output");
    }

    #[test]
    fn detach_command_prunes_old_job_dirs() {
        let cmd = build_detach_command("echo hi", "slj_1", false);
        assert!(cmd.contains("mtime +7"));
    }

    #[test]
    fn signal_targets_wpid_not_pgid() {
        let cmd = build_signal_command("slj_3", JobSignal::Int, true);
        assert!(cmd.contains("cat \"$d/wpid\""));
        assert!(cmd.contains("kill -INT"));
        assert!(cmd.contains("kill -TERM"));
        assert!(!cmd.contains("pgid"));
    }
}
