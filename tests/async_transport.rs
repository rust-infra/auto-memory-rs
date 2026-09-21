//! The tokio edges: the async MCP transport and the async watch loop.
//!
//! Both surfaces hand their synchronous core work (SQLite, file reads, parsing) to
//! the blocking pool, so these tests run on a multi-thread runtime — the same shape
//! `auto_memory::runtime::executor` builds for the CLI. What is pinned here is the
//! behavior the async rewrite had to preserve: stdout stays a sequence of compact
//! newline-delimited frames, frame order follows request order, a shutdown request
//! stops the loop between frames, and the watcher still applies the pending debounce
//! window when it is asked to stop.

use std::fs;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, BufReader, duplex};

use auto_memory::adapters::mcp::McpServer;
use auto_memory::indexing::{IndexOptions, IndexService, VaultWatcher, shutdown_when, watch_vault};
use auto_memory::runtime::block_on;
use auto_memory::storage::Store;

mod common;
use common::{Scratch, fixture};

/// Run a scripted session through `serve_async` over an in-memory duplex.
///
/// The client half is written on a task so a frame larger than the duplex buffer
/// cannot deadlock against the reader.
async fn serve_session<'a>(server: &mut McpServer<'a>, requests: &[Value]) -> String {
    let (mut client, server_input) = duplex(8 * 1024);
    let script: Vec<u8> = requests
        .iter()
        .flat_map(|request| format!("{request}\n").into_bytes())
        .collect();
    let writer = tokio::spawn(async move {
        client.write_all(&script).await.expect("write requests");
        client.shutdown().await.expect("close stdin");
    });

    let mut stdout = Vec::new();
    server
        .serve_async(
            BufReader::new(server_input),
            &mut stdout,
            std::future::pending(),
        )
        .await
        .expect("serve session");
    writer.await.expect("client task");
    String::from_utf8(stdout).expect("stdout is UTF-8")
}

/// Parse the newline-delimited frames `serve_async` produced.
fn frames(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|_| panic!("bad frame: {line}")))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn async_transport_emits_one_compact_frame_per_request() {
    let (_dir, vault, mut store, project_id) = fixture("async-serve");
    let mut server = McpServer::new(&mut store, project_id, "oracle", "oracle", "oracle", &vault);

    let stdout = serve_session(
        &mut server,
        &[
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                   "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
            // A notification carries no id and must not produce a frame.
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                   "params": {"name": "read_note", "arguments": {"identifier": "notes/simple.md"}}}),
            json!({"jsonrpc": "2.0", "id": 4, "method": "nope"}),
        ],
    )
    .await;

    let frames = frames(&stdout);
    assert_eq!(
        frames.len(),
        4,
        "one frame per request with an id: {stdout}"
    );
    assert_eq!(
        frames
            .iter()
            .map(|frame| frame["id"].as_u64().expect("id"))
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4],
        "responses keep request order"
    );
    assert_eq!(frames[0]["result"]["serverInfo"]["name"], "auto-memory-rs");
    assert_eq!(frames[3]["error"]["message"], "unsupported method: nope");
    // Tool results keep the reference's text surface as `content[0].text`, and the
    // frame itself stays compact so it renders on exactly one line.
    assert!(
        frames[2]["result"]["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("Simple Note")),
        "read_note answers on the text surface: {}",
        frames[2]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn async_transport_skips_blank_and_malformed_lines_without_losing_the_stream() {
    let (_dir, vault, mut store, project_id) = fixture("async-serve-noise");
    let mut server = McpServer::new(&mut store, project_id, "oracle", "oracle", "oracle", &vault);

    let (mut client, server_input) = duplex(4 * 1024);
    let writer = tokio::spawn(async move {
        client.write_all(b"\n").await.expect("blank line");
        client.write_all(b"{not json}\n").await.expect("junk");
        client
            .write_all(b"{\"jsonrpc\": \"2.0\", \"id\": 9, \"method\": \"ping\"}\n")
            .await
            .expect("ping");
        client.shutdown().await.expect("close stdin");
    });

    let mut stdout = Vec::new();
    server
        .serve_async(
            BufReader::new(server_input),
            &mut stdout,
            std::future::pending(),
        )
        .await
        .expect("serve session");
    writer.await.expect("client task");

    let stdout = String::from_utf8(stdout).expect("stdout is UTF-8");
    let frames = frames(&stdout);
    assert_eq!(frames.len(), 1, "only the valid request answers: {stdout}");
    assert_eq!(frames[0]["id"], 9);
}

#[tokio::test(flavor = "multi_thread")]
async fn async_transport_stops_on_shutdown_between_frames() {
    let (_dir, vault, mut store, project_id) = fixture("async-serve-shutdown");
    let mut server = McpServer::new(&mut store, project_id, "oracle", "oracle", "oracle", &vault);

    // The input stays open: only the shutdown request can end the loop.
    let (_client, server_input) = duplex(4 * 1024);
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let mut stdout = Vec::new();
    let served = server.serve_async(BufReader::new(server_input), &mut stdout, async {
        let _ = stopped.await;
    });
    let trigger = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = stop.send(());
    });

    tokio::time::timeout(Duration::from_secs(5), served)
        .await
        .expect("shutdown stops the loop")
        .expect("serve session");
    trigger.await.expect("trigger task");
    assert!(stdout.is_empty(), "a shutdown writes no frame");
}

/// The async watch loop indexes a write, then flushes the pending window on shutdown.
///
/// `stop` is never set until after the note is indexed, so the shutdown arm of the
/// `select!` is what releases the second file — the graceful-stop property the
/// blocking loop could not offer (a killed process dropped the debounce window).
#[tokio::test(flavor = "multi_thread")]
async fn async_watch_loop_indexes_then_flushes_on_shutdown() {
    let dir = Scratch::new("async-watch");
    let vault = dir.join("vault");
    let index_path = dir.join("memory.db");
    fs::create_dir_all(&vault).expect("vault");

    let vault_for_task = vault.clone();
    let index_for_task = index_path.clone();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let handle = std::thread::spawn(move || {
        block_on(async move {
            let mut store = Store::open(&index_for_task).await.expect("store");
            let project_id = store
                .upsert_project("oracle", "oracle", &vault_for_task.to_string_lossy())
                .await
                .expect("project");
            let service = IndexService::new(
                &mut store,
                project_id,
                &vault_for_task,
                IndexOptions::new("oracle"),
            );
            let watcher =
                VaultWatcher::new(service, &vault_for_task).with_window(Duration::from_millis(50));
            watch_vault(watcher, async {
                let _ = stopped.await;
            })
            .await
        })
        .expect("runtime")
        .expect("watch loop")
    });

    // Give the loop a moment to install the notify watch before writing.
    std::thread::sleep(Duration::from_millis(300));
    fs::write(
        vault.join("watched.md"),
        "# Watched\n\n- [fact] the async loop saw this\n",
    )
    .expect("write");

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut indexed = false;
    while Instant::now() < deadline {
        if let Ok(store) = Store::open(&index_path).await
            && let Ok(Some(entity)) = store.entity_by_file_path(1, "watched.md").await
        {
            assert_eq!(entity.file_path, "watched.md");
            indexed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        indexed,
        "the async watcher indexed the note it was told about"
    );

    // A second write lands inside the window; the shutdown flush is what applies it.
    fs::write(
        vault.join("late.md"),
        "# Late\n\n- [fact] still debouncing\n",
    )
    .expect("write");
    std::thread::sleep(Duration::from_millis(10));
    let _ = stop.send(());

    let batches = handle.join().expect("watcher thread");
    assert!(batches >= 2, "one batch per write, got {batches}");
    let store = block_on(Store::open(&index_path))
        .expect("runtime")
        .expect("store");
    assert!(
        block_on(store.entity_by_file_path(1, "late.md"))
            .expect("runtime")
            .expect("read")
            .is_some(),
        "the shutdown flush applied the pending window"
    );
}

/// `shutdown_when` keeps the old `Fn() -> bool` calling convention working.
#[test]
fn shutdown_when_resolves_once_the_flag_flips() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let flag = Arc::new(AtomicBool::new(false));
    let flipped = Arc::clone(&flag);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        flipped.store(true, Ordering::Relaxed);
    });
    block_on(shutdown_when(|| flag.load(Ordering::Relaxed))).expect("runtime");
}

/// `auto-memory watch` stops on Ctrl-C and flushes the window it was holding.
///
/// This is the user-visible payoff of the async loop. The blocking version could only
/// be killed, so a note edited in the last second before Ctrl-C stayed out of the index
/// until the next start's reconcile. The debounce window here is deliberately much
/// longer than the test's patience, so the only way the note can reach the index is the
/// shutdown flush.
#[test]
fn watch_cli_flushes_the_pending_window_on_sigint() {
    let dir = Scratch::new("watch-sigint");
    let vault = dir.join("vault");
    let index = dir.join("memory.db");
    fs::create_dir_all(&vault).expect("vault");
    fs::write(vault.join("seed.md"), "# Seed\n").expect("seed");

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_auto-memory"))
        .args(["watch", "--vault"])
        .arg(&vault)
        .arg("--index")
        .arg(&index)
        .args(["--project", "oracle", "--window-ms", "3000"])
        // Pin the filter: a developer's own RUST_LOG must not decide this test.
        .env("RUST_LOG", "auto_memory=info")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn auto-memory watch");

    // Let the initial reconcile finish and the notify watch install.
    std::thread::sleep(Duration::from_millis(800));
    fs::write(
        vault.join("late.md"),
        "# Late\n\n- [fact] written just before Ctrl-C\n",
    )
    .expect("write");
    std::thread::sleep(Duration::from_millis(200));

    let signalled = std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("send SIGINT");
    assert!(signalled.success(), "kill -INT <watch pid>");

    // Bounded wait: a signal that is ignored must fail the test, not hang it.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut status = None;
    while Instant::now() < deadline {
        if let Some(exited) = child.try_wait().expect("try_wait") {
            status = Some(exited);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let Some(status) = status else {
        let _ = child.kill();
        let _ = child.wait();
        panic!("auto-memory watch kept running after SIGINT");
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    use std::io::Read;
    let _ = child
        .stdout
        .take()
        .expect("stdout pipe")
        .read_to_string(&mut stdout);
    let _ = child
        .stderr
        .take()
        .expect("stderr pipe")
        .read_to_string(&mut stderr);
    assert!(
        status.success(),
        "a handled SIGINT is a clean exit, not a kill ({status}): stdout={stdout:?} stderr={stderr:?}"
    );
    let report: Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|_| panic!("watch report: {stdout}"));
    assert_eq!(
        report["batches"], 1,
        "the shutdown flush applied the pending window: {stdout}"
    );
    // The daemon's only sign of life while it runs is stderr: startup, the initial
    // reconcile summary, and one line per applied batch (here the shutdown flush).
    // stdout must stay the machine-readable report, so nothing below may appear there.
    for expected in [
        "watching the vault",
        "initial reconcile finished",
        "watch batch applied",
        "Ctrl-C received",
        "watch stopped",
    ] {
        assert!(
            stderr.contains(expected),
            "missing {expected:?} in watch stderr: {stderr:?}"
        );
    }

    let store = block_on(Store::open(&index))
        .expect("runtime")
        .expect("store");
    assert!(
        block_on(store.entity_by_file_path(1, "late.md"))
            .expect("runtime")
            .expect("read")
            .is_some(),
        "the note written before Ctrl-C is in the index"
    );
}

/// `auto-memory watch` also stops on SIGTERM, which is what `systemctl stop`, `docker
/// stop` and a plain `kill` send.
///
/// Before this was handled the process died mid-window: no flush, no report, and no
/// log line — a daemon that looked identical to one that had crashed. This pins the
/// clean path instead: a batch applied before the signal is reported on stderr while
/// it happens, the signal itself is logged, and stdout still carries the report.
#[test]
fn watch_cli_stops_on_sigterm_and_logs_its_batches() {
    let dir = Scratch::new("watch-sigterm");
    let vault = dir.join("vault");
    let index = dir.join("memory.db");
    fs::create_dir_all(&vault).expect("vault");
    fs::write(vault.join("seed.md"), "# Seed\n").expect("seed");

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_auto-memory"))
        .args(["watch", "--vault"])
        .arg(&vault)
        .arg("--index")
        .arg(&index)
        .args(["--project", "oracle", "--window-ms", "300"])
        .env("RUST_LOG", "auto_memory=info")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn auto-memory watch");

    std::thread::sleep(Duration::from_millis(800));
    fs::write(vault.join("during.md"), "# During\n\n- [fact] watched\n").expect("write");
    // Longer than the debounce window, so the batch is applied and logged while the
    // daemon is still running (that is the point of the test).
    std::thread::sleep(Duration::from_millis(1200));

    let signalled = std::process::Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .expect("send SIGTERM");
    assert!(signalled.success(), "kill -TERM <watch pid>");

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut status = None;
    while Instant::now() < deadline {
        if let Some(exited) = child.try_wait().expect("try_wait") {
            status = Some(exited);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let Some(status) = status else {
        let _ = child.kill();
        let _ = child.wait();
        panic!("auto-memory watch kept running after SIGTERM");
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    use std::io::Read;
    let _ = child
        .stdout
        .take()
        .expect("stdout pipe")
        .read_to_string(&mut stdout);
    let _ = child
        .stderr
        .take()
        .expect("stderr pipe")
        .read_to_string(&mut stderr);

    assert!(
        status.success(),
        "a handled SIGTERM is a clean exit ({status}): stdout={stdout:?} stderr={stderr:?}"
    );
    let report: Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|_| panic!("watch report: {stdout}"));
    assert!(
        report["batches"].as_u64().unwrap_or_default() >= 1,
        "the edit made while watching was applied: {stdout}"
    );
    for expected in ["SIGTERM received", "watch batch applied", "watch stopped"] {
        assert!(
            stderr.contains(expected),
            "missing {expected:?} in watch stderr: {stderr:?}"
        );
    }
}
