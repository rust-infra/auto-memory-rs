//! End-to-end test of the MCP Streamable HTTP transport.
//!
//! The transport itself is `rmcp`'s, so the interesting assertions are about the
//! *wiring*: the session header the client must echo back, the tool surface being the
//! same one the stdio transport advertises, and read-only mode being enforced on
//! `tools/call` as well as hidden from `tools/list`.
//!
//! Requests are written by hand over a `std::net::TcpStream` instead of through an
//! HTTP client crate, so the test exercises the actual wire format (JSON-RPC body,
//! `Mcp-Session-Id`, SSE framing) with no extra dependency.

mod common;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use auto_memory::adapters::mcp::http::{HttpServer, bind, serve_on};
use common::indexed_store;
use serde_json::{Value, json};
use tokio::sync::Mutex;

/// One HTTP response, decoded enough to assert on.
struct Response {
    status: u16,
    headers: HashMap<String, String>,
    body: String,
}

impl Response {
    /// The JSON-RPC frame with `id` carried by the SSE body.
    fn frame(&self, id: u64) -> Value {
        for line in self.body.lines() {
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(data.trim()) else {
                continue;
            };
            if value["id"] == id {
                return value;
            }
        }
        panic!("no frame for id {id} in body:\n{}", self.body);
    }
}

/// A minimal HTTP/1.1 client: one connection per request, `Connection: close`.
struct Client {
    addr: std::net::SocketAddr,
    session: Option<String>,
}

impl Client {
    /// POST a JSON-RPC `body` to `path`, keeping any `Mcp-Session-Id` the server hands back.
    fn post(&mut self, path: &str, body: &Value) -> Response {
        let body = body.to_string();
        let mut request = format!(
            "POST {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\n\
             Accept: application/json, text/event-stream\r\nContent-Length: {}\r\n\
             Connection: close\r\n",
            self.addr,
            body.len()
        );
        if let Some(session) = &self.session {
            request.push_str(&format!("Mcp-Session-Id: {session}\r\n"));
        }
        request.push_str("\r\n");
        request.push_str(&body);

        let mut stream = TcpStream::connect(self.addr).expect("connect");
        stream.write_all(request.as_bytes()).expect("write request");
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).expect("read response");

        let split = raw
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("header terminator");
        let head = String::from_utf8_lossy(&raw[..split]).into_owned();
        let mut lines = head.split("\r\n");
        let status = lines
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .expect("status line");
        let headers: HashMap<String, String> = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim().to_owned()))
            .collect();
        let body = if headers.get("transfer-encoding").map(String::as_str) == Some("chunked") {
            dechunk(&raw[split + 4..])
        } else {
            String::from_utf8_lossy(&raw[split + 4..]).into_owned()
        };
        if let Some(session) = headers.get("mcp-session-id") {
            self.session = Some(session.clone());
        }
        Response {
            status,
            headers,
            body,
        }
    }

    /// `initialize` and the follow-up `notifications/initialized` every client sends.
    fn initialize(&mut self) -> Response {
        let response = self.post(
            "/mcp",
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": { "name": "mcp-http-test", "version": "0.1.0" },
                },
            }),
        );
        assert_eq!(response.status, 200, "initialize status");
        assert!(
            response.headers.contains_key("mcp-session-id"),
            "initialize must return a session id"
        );
        let notified = self.post(
            "/mcp",
            &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        );
        assert_eq!(notified.status, 202, "notification status");
        response
    }
}

/// Decode an HTTP/1.1 chunked body.
fn dechunk(raw: &[u8]) -> String {
    let mut out = Vec::new();
    let mut rest = raw;
    while let Some(end) = rest.windows(2).position(|window| window == b"\r\n") {
        let size = usize::from_str_radix(String::from_utf8_lossy(&rest[..end]).trim(), 16)
            .expect("chunk size");
        if size == 0 {
            break;
        }
        let start = end + 2;
        out.extend_from_slice(&rest[start..start + size]);
        rest = &rest[start + size + 2..];
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A server over the fixture vault, listening on an ephemeral port.
async fn spawn(read_only: bool, tag: &str) -> (common::Scratch, std::net::SocketAddr) {
    let (scratch, store, project_id) = indexed_store(tag);
    let external_id = common::block_on(store.project_by_permalink("oracle"))
        .expect("read project")
        .expect("project row")
        .external_id;
    let vault = scratch.join("vault");
    let server = HttpServer::new(
        Arc::new(Mutex::new(store)),
        project_id,
        "oracle",
        external_id,
        "oracle",
        vault,
    )
    .with_read_only(read_only);
    let listener = bind("127.0.0.1", 0).await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = serve_on(server, listener, "/mcp", async move {
            let _ = shutdown_rx.await;
        })
        .await;
    });
    // The sender is leaked so the listener's shutdown future never resolves: the
    // server must outlive the test body, and the task dies with the process.
    std::mem::forget(shutdown);
    (scratch, addr)
}

#[tokio::test(flavor = "multi_thread")]
async fn session_lists_and_calls_tools_over_streamable_http() {
    let (_scratch, addr) = spawn(false, "mcp-http-session").await;
    let mut client = Client {
        addr,
        session: None,
    };
    let initialized = client.initialize();
    let result = initialized.frame(1);
    assert_eq!(
        result["result"]["serverInfo"]["name"],
        json!("auto-memory-rs")
    );
    assert_eq!(result["result"]["protocolVersion"], json!("2024-11-05"));

    let listed = client.post(
        "/mcp",
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} }),
    );
    let tools = listed.frame(2)["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name").to_owned())
        .collect::<Vec<_>>();
    assert!(tools.contains(&"read_note".to_owned()), "{tools:?}");
    assert!(tools.contains(&"write_note".to_owned()), "{tools:?}");

    let called = client.post(
        "/mcp",
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "read_note",
                "arguments": { "identifier": "notes/welcome" },
            },
        }),
    );
    let call = called.frame(3);
    assert_eq!(call["result"]["isError"], json!(false));
    assert!(call["result"]["structuredContent"].is_object());
}

#[tokio::test(flavor = "multi_thread")]
async fn read_only_mode_hides_and_refuses_mutating_tools() {
    let (_scratch, addr) = spawn(true, "mcp-http-readonly").await;
    let mut client = Client {
        addr,
        session: None,
    };
    client.initialize();

    let listed = client.post(
        "/mcp",
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} }),
    );
    let tools = listed.frame(2)["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name").to_owned())
        .collect::<Vec<_>>();
    assert!(!tools.contains(&"write_note".to_owned()), "{tools:?}");
    assert!(tools.contains(&"read_note".to_owned()), "{tools:?}");

    let refused = client.post(
        "/mcp",
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "write_note",
                "arguments": { "title": "x", "content": "y" },
            },
        }),
    );
    let frame = refused.frame(3);
    assert!(
        frame["error"].is_object(),
        "write_note must be refused in read-only mode: {frame}"
    );
}

/// The stdio and HTTP transports advertise the same tools.
///
/// The HTTP path builds its `tools/list` from the same `advertised_tools` call the
/// stdio path answers with, so this pins that the two surfaces actually agree rather
/// than drifting silently.
#[tokio::test(flavor = "multi_thread")]
async fn http_tool_surface_matches_stdio() {
    use common::Session;

    let (scratch, addr) = spawn(false, "mcp-http-parity").await;
    let index = scratch.join("index.db");
    let stdio = Session::new(&scratch.join("vault"), &index)
        .run(&[json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })]);
    let mut stdio_names = common::frame_of(&stdio.frames, 1)["result"]["tools"]
        .as_array()
        .expect("stdio tools")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name").to_owned())
        .collect::<Vec<_>>();
    stdio_names.sort();

    let mut client = Client {
        addr,
        session: None,
    };
    client.initialize();
    let listed = client.post(
        "/mcp",
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} }),
    );
    let mut http_names = listed.frame(2)["result"]["tools"]
        .as_array()
        .expect("http tools")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name").to_owned())
        .collect::<Vec<_>>();
    http_names.sort();

    assert_eq!(http_names, stdio_names);
}
