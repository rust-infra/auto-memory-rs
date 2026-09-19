//! stdout is protocol-only.
//!
//! MCP clients read the child's stdout as newline-delimited JSON-RPC, so a single
//! stray log line breaks the session. Diagnostics therefore go to stderr, and only
//! a real terminal gets ANSI colour — a client that captures stderr (or `journalctl`,
//! or a redirected file) must not see escape codes either.

mod common;

use std::io::Write;
use std::process::{Command, Stdio};

use common::{Scratch, copy_dir_with_mtimes, fixtures_vault};
use serde_json::Value;

fn request(id: u64) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"basic_memory_diagnostics","arguments":{{}}}}}}"#
    )
}

#[test]
fn mcp_logs_stay_on_stderr_and_stdout_stays_protocol_only() {
    let dir = Scratch::new("mcp-stdout-purity");
    let vault = dir.join("vault");
    copy_dir_with_mtimes(&fixtures_vault(), &vault);

    let mut child = Command::new(env!("CARGO_BIN_EXE_auto-memory"))
        .args(["mcp", "--vault"])
        .arg(&vault)
        .args(["--index"])
        .arg(dir.join("memory.db"))
        .args(["--project", "oracle"])
        // Pin the filter: the assertions below must not depend on the developer's own
        // RUST_LOG, and this is the default a user gets.
        .env("RUST_LOG", "auto_memory=info")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn auto-memory mcp");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        for id in [1, 2] {
            writeln!(stdin, "{}", request(id)).expect("write request");
        }
    }
    let output = child.wait_with_output().expect("mcp session");
    assert!(output.status.success(), "mcp server failed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let frames: Vec<Value> = stdout
        .lines()
        .enumerate()
        .map(|(number, line)| {
            serde_json::from_str(line).unwrap_or_else(|error| {
                panic!("stdout line {number} is not JSON ({error}): {line}")
            })
        })
        .collect();
    assert_eq!(frames.len(), 2, "one frame per request: {stdout}");
    for frame in &frames {
        assert_eq!(
            frame["jsonrpc"], "2.0",
            "stdout carries protocol frames only"
        );
        assert!(frame["result"]["content"].is_array(), "{frame}");
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("mcp server starting"),
        "the startup line belongs on stderr: {stderr:?}"
    );
    assert!(
        !stderr.contains('\u{1b}'),
        "stderr is a pipe here, so the subscriber must not colour it: {stderr:?}"
    );
}
