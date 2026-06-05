//! MCP stdio smoke test: stdout must contain only JSON-RPC (no banner leakage).

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

const INIT: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#;

#[test]
fn mcp_serve_stdout_is_valid_json_rpc() {
    let bin = env!("CARGO_BIN_EXE_ssh-liaison-mcp");
    let mut child = Command::new(bin)
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn ssh-liaison-mcp serve");

    {
        let stdin = child.stdin.as_mut().expect("stdin");
        writeln!(stdin, "{INIT}").expect("write init");
        stdin.flush().expect("flush init");
    }

    let stdout = BufReader::new(child.stdout.take().expect("stdout"));
    let line = stdout
        .lines()
        .next()
        .expect("one response line")
        .expect("read line");
    let value: serde_json::Value =
        serde_json::from_str(&line).expect("stdout line must be valid JSON");
    assert_eq!(value.get("jsonrpc").and_then(|v| v.as_str()), Some("2.0"));
    assert!(value.get("result").is_some());

    let _ = child.kill();
    let _ = child.wait();
}
