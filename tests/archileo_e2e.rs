//! Live e2e against a host from ~/.ssh/config. Run:
//!   ARCHILEO_E2E=1 cargo test --test archileo_e2e -- --ignored --nocapture --test-threads=1
//!   E2E_HOST=eva-cloud ARCHILEO_E2E=1 cargo test --test archileo_e2e -- --ignored --nocapture --test-threads=1

use ssh_liaison_mcp::ssh::{CommandTimeouts, JobState, SessionManager, SshCommandError};
use std::time::Duration;

fn e2e_enabled() -> bool {
    std::env::var("ARCHILEO_E2E").ok().as_deref() == Some("1")
}

fn e2e_host() -> String {
    std::env::var("E2E_HOST").unwrap_or_else(|_| "archileo".to_string())
}

#[tokio::test]
#[ignore = "live SSH host"]
async fn e2e_auto_connect_without_prior_connect() {
    if !e2e_enabled() {
        return;
    }
    let host = e2e_host();
    let mgr = SessionManager::new();
    assert!(
        !mgr.is_connected(&host).await,
        "fresh manager should not be connected"
    );
    mgr.ensure_connected(&host).await.expect("ensure_connected");
    assert!(mgr.is_connected(&host).await);

    let out = mgr
        .execute_command(
            &host,
            "echo auto-connect-e2e",
            None,
            CommandTimeouts::default(),
        )
        .await
        .expect("echo after ensure_connected");
    assert!(
        out.stdout.contains("auto-connect-e2e"),
        "stdout: {}",
        out.stdout
    );

    mgr.disconnect(&host).await.ok();
}

#[tokio::test]
#[ignore = "live SSH host"]
async fn e2e_sync_command_and_exit_code() {
    if !e2e_enabled() {
        return;
    }
    let host = e2e_host();
    let mgr = SessionManager::new();
    mgr.connect_by_alias(&host).await.expect("connect");

    let out = mgr
        .execute_command(&host, "echo hello-e2e", None, CommandTimeouts::default())
        .await
        .expect("echo");
    assert!(out.stdout.contains("hello-e2e"), "stdout: {}", out.stdout);

    let err = mgr
        .execute_command(
            &host,
            "grep -q __ssh_liaison_no_match__ /etc/hosts",
            None,
            CommandTimeouts::default(),
        )
        .await
        .expect_err("non-zero exit");
    match err {
        SshCommandError::CommandFailed { exit_code, .. } => assert_eq!(exit_code, 1),
        other => panic!("expected CommandFailed, got {other}"),
    }

    mgr.disconnect(&host).await.ok();
}

#[tokio::test]
#[ignore = "live SSH host"]
async fn e2e_long_streaming_within_max_timeout() {
    if !e2e_enabled() {
        return;
    }
    let host = e2e_host();
    let mgr = SessionManager::new();
    mgr.connect_by_alias(&host).await.expect("connect");

    let timeouts = CommandTimeouts::from_secs(Some(5), Some(45));
    let out = mgr
        .execute_command(
            &host,
            "for i in $(seq 1 35); do echo line-$i; sleep 1; done",
            None,
            timeouts,
        )
        .await
        .expect("streaming command should finish within 45s max");
    assert!(
        out.stdout.contains("line-35"),
        "stdout tail: {}",
        out.stdout
    );

    mgr.disconnect(&host).await.ok();
}

#[tokio::test]
#[ignore = "live SSH host"]
async fn e2e_detached_quick_echo() {
    if !e2e_enabled() {
        return;
    }
    let host = e2e_host();
    let mgr = SessionManager::new();
    mgr.connect_by_alias(&host).await.expect("connect");
    let (job_id, _) = mgr
        .run_detached(&host, "echo detached-quick")
        .await
        .expect("run_detached");
    tokio::time::sleep(Duration::from_secs(2)).await;
    let report = mgr.job_status(&host, &job_id).await.expect("status");
    eprintln!("quick job state={:?} out={}", report.state, report.output);
    assert!(matches!(report.state, JobState::Exited(0)));
    mgr.disconnect(&host).await.ok();
}

#[tokio::test]
#[ignore = "live SSH host"]
async fn e2e_detached_job_lifecycle_tmux() {
    if !e2e_enabled() {
        return;
    }
    let host = e2e_host();
    let mgr = SessionManager::new();
    mgr.connect_by_alias(&host).await.expect("connect");

    let (job_id, use_tmux) = mgr
        .run_detached(
            &host,
            "for i in $(seq 1 8); do echo detached-$i; sleep 1; done",
        )
        .await
        .expect("run_detached");
    assert!(use_tmux, "{host} should use tmux backend");

    tokio::time::sleep(Duration::from_secs(3)).await;
    let status = mgr.job_status(&host, &job_id).await.expect("job_status");
    assert_eq!(status.state, JobState::Running);
    assert!(
        status.output.contains("detached-"),
        "expected partial output, got: {}",
        status.output
    );

    let waited = mgr.job_wait(&host, &job_id, 20).await.expect("job_wait");
    assert!(
        matches!(waited.state, JobState::Exited(0)),
        "state: {:?}",
        waited.state
    );
    assert!(
        waited.output.contains("detached-8"),
        "output: {}",
        waited.output
    );

    mgr.disconnect(&host).await.ok();
}

#[tokio::test]
#[ignore = "live SSH host"]
async fn e2e_detached_job_signal_int() {
    if !e2e_enabled() {
        return;
    }
    let host = e2e_host();
    let mgr = SessionManager::new();
    mgr.connect_by_alias(&host).await.expect("connect");

    let (job_id, _) = mgr
        .run_detached(&host, "sleep 600")
        .await
        .expect("run_detached");

    tokio::time::sleep(Duration::from_secs(2)).await;
    mgr.job_signal(&host, &job_id, ssh_liaison_mcp::ssh::JobSignal::Int)
        .await
        .expect("signal int");

    let report = mgr
        .job_wait(&host, &job_id, 15)
        .await
        .expect("job_wait after int");
    assert!(
        matches!(report.state, JobState::Exited(_)),
        "expected exited after SIGINT, got {:?}",
        report.state
    );

    mgr.disconnect(&host).await.ok();
}

#[tokio::test]
#[ignore = "live SSH host"]
async fn e2e_stateful_shell_preserved() {
    if !e2e_enabled() {
        return;
    }
    let host = e2e_host();
    let mgr = SessionManager::new();
    mgr.connect_by_alias(&host).await.expect("connect");

    mgr.execute_command(
        &host,
        "cd /tmp && mkdir -p ssh-liaison-e2e-test && cd ssh-liaison-e2e-test",
        None,
        CommandTimeouts::default(),
    )
    .await
    .expect("mkdir");

    let out = mgr
        .execute_command(&host, "pwd", None, CommandTimeouts::default())
        .await
        .expect("pwd");
    assert!(
        out.stdout.contains("/tmp/ssh-liaison-e2e-test"),
        "stateful cwd lost: {}",
        out.stdout
    );

    mgr.execute_command(
        &host,
        "rm -rf /tmp/ssh-liaison-e2e-test",
        None,
        CommandTimeouts::default(),
    )
    .await
    .ok();

    mgr.disconnect(&host).await.ok();
}
