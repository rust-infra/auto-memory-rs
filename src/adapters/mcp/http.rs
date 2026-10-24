//! MCP Streamable HTTP transport, served by the official `rmcp` SDK.
//!
//! The protocol side — sessions, `Mcp-Session-Id` handling, SSE framing, JSON-RPC
//! over HTTP, `Accept`/`Content-Type` negotiation — belongs to
//! [`rmcp::transport::streamable_http_server`]; `axum` owns the listener. Only the
//! tool surface stays ours: `HttpSession::call_tool` hands the request to the same
//! `McpServer::call_tool` the stdio transport uses, and `tools/list` is built from
//! `advertised_tools`, so the two transports cannot drift.
//!
//! ## Sessions
//!
//! The default `rmcp` configuration is stateful: the first `initialize` creates a
//! session (returned in the `Mcp-Session-Id` header) and the factory in
//! [`serve_on`] builds one `HttpSession` per session. That session owns the
//! `clientInfo` it recorded during `initialize`, so two HTTP clients no longer share
//! one identity the way the earlier single-identity port did.
//!
//! ## Async core work
//!
//! Tool dispatch awaits the async application services and the tokio-rusqlite bridge.
//! CPU-heavy ONNX work can still move to the blocking pool on the multi-thread runtime
//! built by [`crate::runtime::executor`], which is what `main` uses.

use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, PoisonError};

use axum::Router;
use axum::extract::Request;
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use rmcp::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Implementation, InitializeRequestParams,
    InitializeResult, ListToolsResult, PaginatedRequestParams, ProtocolVersion, ServerCapabilities,
    Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::Mutex as AsyncMutex;

use super::server::{ClientInfo, McpServer, SERVER_NAME, advertised_tools};
use crate::error::{Error, Result};
use crate::runtime::rerank::{
    DEFAULT_RERANKER_CANDIDATES, DEFAULT_RERANKER_MAX_DOCUMENT_CHARS, RerankProvider,
};
use crate::search::embedding::EmbeddingProvider;
use crate::storage::Store;

/// Default `--host`: loopback only.
///
/// The endpoint is unauthenticated, so it should not be reachable from the
/// network unless the operator explicitly opts in with `--host 0.0.0.0`.
pub const DEFAULT_HTTP_HOST: &str = "127.0.0.1";
/// Default `--port`.
///
/// Deliberately not `8000`: that is the reference CLI's default, and picking a
/// distinct port keeps this transport from colliding with a locally running
/// reference server.
pub const DEFAULT_HTTP_PORT: u16 = 8765;
/// Default `--path`, mirroring the reference CLI.
pub const DEFAULT_HTTP_PATH: &str = "/mcp";

/// Shared backend behind every HTTP session.
///
/// One instance is built in `main` and cloned (as an `Arc`) into each session's
/// `HttpSession`. The [`Store`] sits behind an async mutex because tool dispatch needs
/// `&mut Store` and its SQLite methods are asynchronous; the provider and reranker are
/// already `Arc`-shared with the stdio server, so both transports use the same
/// instances.
pub struct HttpServer {
    store: Arc<AsyncMutex<Store>>,
    project_id: i64,
    project_name: String,
    project_external_id: String,
    permalink: String,
    vault: PathBuf,
    read_only: bool,
    provider: Option<Arc<dyn EmbeddingProvider + Send + Sync>>,
    reranker: Option<Arc<dyn RerankProvider + Send + Sync>>,
    reranker_candidates: usize,
    reranker_max_document_chars: usize,
}

impl HttpServer {
    /// Bind the HTTP transport to one indexed project.
    pub fn new(
        store: Arc<AsyncMutex<Store>>,
        project_id: i64,
        project_name: impl Into<String>,
        project_external_id: impl Into<String>,
        permalink: impl Into<String>,
        vault: impl Into<PathBuf>,
    ) -> Self {
        Self {
            store,
            project_id,
            project_name: project_name.into(),
            project_external_id: project_external_id.into(),
            permalink: permalink.into(),
            vault: vault.into(),
            read_only: false,
            provider: None,
            reranker: None,
            reranker_candidates: DEFAULT_RERANKER_CANDIDATES,
            reranker_max_document_chars: DEFAULT_RERANKER_MAX_DOCUMENT_CHARS,
        }
    }

    /// Serve a read-only session: the mutating tools are hidden from `tools/list` and
    /// refused by `tools/call`.
    #[must_use]
    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Attach the embedding runtime shared with the stdio transport.
    #[must_use]
    pub fn with_provider(
        mut self,
        provider: impl Into<Arc<dyn EmbeddingProvider + Send + Sync>>,
    ) -> Self {
        self.provider = Some(provider.into());
        self
    }

    /// Attach the cross-encoder reranker shared with the stdio transport.
    #[must_use]
    pub fn with_reranker(
        mut self,
        reranker: impl Into<Arc<dyn RerankProvider + Send + Sync>>,
    ) -> Self {
        self.reranker = Some(reranker.into());
        self
    }

    /// How many top candidates the reranker rescores.
    #[must_use]
    pub fn with_reranker_candidates(mut self, candidates: usize) -> Self {
        self.reranker_candidates = candidates;
        self
    }

    /// Per-candidate character cap for the reranker.
    #[must_use]
    pub fn with_reranker_max_document_chars(mut self, max_chars: usize) -> Self {
        self.reranker_max_document_chars = max_chars;
        self
    }

    /// The tool definitions this session advertises, as rmcp's typed [`Tool`].
    ///
    /// Derived from [`advertised_tools`] on every call rather than cached, so the
    /// read-only filter and the wire definitions have exactly one source of truth.
    fn tools(&self) -> std::result::Result<Vec<Tool>, rmcp::ErrorData> {
        advertised_tools(self.read_only)
            .into_iter()
            .map(|definition| {
                serde_json::from_value(definition).map_err(|error| {
                    // The definitions are compile-time constants; this can only mean
                    // our JSON and rmcp's model drifted, which is a bug, not input.
                    rmcp::ErrorData::internal_error(
                        format!("the advertised tool definitions are not valid: {error}"),
                        None,
                    )
                })
            })
            .collect()
    }

    /// Dispatch one `tools/call` through the same [`McpServer`] the stdio transport
    /// uses.
    async fn dispatch_tool(
        &self,
        params: &Value,
        client_info: Option<ClientInfo>,
    ) -> std::result::Result<CallToolResult, rmcp::ErrorData> {
        // Recovery, not propagation: a poisoned mutex only means one earlier request
        // panicked, and refusing every later call would turn that into an outage.
        let mut store = self.store.lock().await;
        let mut server = McpServer::new(
            &mut store,
            self.project_id,
            &self.project_name,
            &self.project_external_id,
            &self.permalink,
            &self.vault,
        )
        .with_read_only(self.read_only)
        .with_client_info(client_info);
        if let Some(provider) = &self.provider {
            server = server.with_provider(Arc::clone(provider));
        }
        if let Some(reranker) = &self.reranker {
            server = server
                .with_reranker(Arc::clone(reranker))
                .with_reranker_candidates(self.reranker_candidates)
                .with_reranker_max_document_chars(self.reranker_max_document_chars);
        }
        let name = params["name"].as_str().unwrap_or_default();
        let result = server.call_tool(params).await.map_err(|error| {
            tracing::warn!(%error, tool = name, "mcp http: tool call failed");
            rmcp::ErrorData::internal_error(error.to_string(), None)
        })?;
        tracing::debug!(tool = name, "mcp http: tool call answered");
        serde_json::from_value(result).map_err(|error| {
            rmcp::ErrorData::internal_error(
                format!("the tool result is not a valid MCP result: {error}"),
                None,
            )
        })
    }
}

/// One MCP session, as `rmcp` drives it.
///
/// The `clientInfo` from `initialize` lives here rather than on [`HttpServer`] so it
/// is scoped to the client that reported it.
#[derive(Clone)]
struct HttpSession {
    backend: Arc<HttpServer>,
    client_info: Arc<StdMutex<Option<ClientInfo>>>,
}

impl HttpSession {
    fn new(backend: Arc<HttpServer>) -> Self {
        Self {
            backend,
            client_info: Arc::new(StdMutex::new(None)),
        }
    }

    /// The identity this session recorded, if the client reported one.
    fn client_info(&self) -> Option<ClientInfo> {
        self.client_info
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl ServerHandler for HttpSession {
    fn get_info(&self) -> InitializeResult {
        InitializeResult {
            // Pinned to the stdio transport's `MCP_PROTOCOL_VERSION` so a client sees
            // the same `initialize` result whichever transport it connected over.
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: SERVER_NAME.to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
                ..Default::default()
            },
            instructions: None,
        }
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<InitializeResult, rmcp::ErrorData> {
        // rmcp's typed request replaces the raw `params.clientInfo` the stdio
        // transport parses; both funnel into `ClientInfo` so identity decisions (the
        // ChatGPT adapters) cannot differ by transport.
        let client_info = ClientInfo::from_parts(
            Some(request.client_info.name.clone()),
            request.client_info.title.clone(),
        );
        tracing::info!(
            client = client_info.as_ref().and_then(ClientInfo::name),
            "mcp http: session initialized"
        );
        *self
            .client_info
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = client_info;
        if context.peer.peer_info().is_none() {
            context.peer.set_peer_info(request);
        }
        Ok(self.get_info())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> std::result::Result<ListToolsResult, rmcp::ErrorData> {
        Ok(ListToolsResult::with_all_items(self.backend.tools()?))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResult, rmcp::ErrorData> {
        let name = request.name.to_string();
        let params = json!({
            "name": name,
            "arguments": request.arguments.unwrap_or_default(),
        });
        let client_info = self.client_info();
        Arc::clone(&self.backend)
            .dispatch_tool(&params, client_info)
            .await
    }
}

/// Resolve `host:port` and bind the listener.
///
/// Kept separate from [`serve_on`] so a caller (and the tests) can bind an ephemeral
/// port, read [`TcpListener::local_addr`], and only then start serving.
pub async fn bind(host: &str, port: u16) -> std::io::Result<TcpListener> {
    let addr = tokio::net::lookup_host((host, port))
        .await?
        .next()
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("could not resolve {host}:{port}"),
            )
        })?;
    TcpListener::bind(addr).await
}

/// Serve the Streamable HTTP transport on an already-bound listener.
///
/// `shutdown` stops the listener and cancels every live session, so SSE streams do
/// not keep the process alive past Ctrl-C.
pub async fn serve_on(
    server: HttpServer,
    listener: TcpListener,
    path: &str,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<()> {
    serve_on_guarded(server, listener, path, HostAllowlist::default(), shutdown).await
}

/// Extra `Host`/`Origin` values a reverse proxy needs, beyond the bound address.
#[derive(Clone, Debug, Default)]
pub struct HostAllowlist {
    /// Accepted `Host` header values.
    pub hosts: Vec<String>,
    /// Accepted `Origin` values.
    pub origins: Vec<String>,
}

impl HostAllowlist {
    /// Whether the operator asked for anything beyond the bound address.
    fn is_empty(&self) -> bool {
        self.hosts.is_empty() && self.origins.is_empty()
    }
}

/// Serve the Streamable HTTP transport with an explicit DNS-rebinding guard.
///
/// The guard is always built from the listener's own address, so the accepted `Host`
/// is the one clients actually dialed. `allow` widens it for a reverse proxy. Exposed
/// separately from [`serve_on`] so the tests can drive both the refused and the
/// accepted paths without reaching into private state.
pub async fn serve_on_guarded(
    server: HttpServer,
    listener: TcpListener,
    path: &str,
    allow: HostAllowlist,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let path = normalize_path(path)?;
    let addr = listener.local_addr()?;
    let loopback = addr.ip().is_loopback();
    tracing::info!(
        %addr,
        path = %path,
        read_only = server.read_only,
        loopback,
        "mcp streamable http transport listening"
    );
    if !loopback {
        // The upstream v0.24.0 breaking change: binding beyond loopback is an explicit
        // opt-in, and the operator should hear about it every time.
        tracing::warn!(
            %addr,
            "mcp http is bound beyond loopback without authentication; anyone who can \
             reach this address can call every tool"
        );
    }
    let config = StreamableHttpServerConfig::default();
    // Cloned before the config is moved into the service; cancelling it terminates
    // the SSE streams the service hands out, which is what lets graceful shutdown
    // finish instead of waiting on an idle client's open stream.
    let cancellation = config.cancellation_token.clone();
    let backend = Arc::new(server);
    let service = StreamableHttpService::new(
        move || -> std::io::Result<HttpSession> { Ok(HttpSession::new(Arc::clone(&backend))) },
        Arc::new(LocalSessionManager::default()),
        config,
    );
    let mut router = Router::new().nest_service(&path, service);
    // Only a loopback bind gets the guard by default: a non-loopback bind is already
    // an explicit decision to expose the endpoint, and an allowlist there would break
    // reverse proxies unless the operator names them.
    let guard = (loopback || !allow.is_empty()).then(|| {
        HostGuard::new(addr.to_string())
            .with_allowed_hosts(allow.hosts)
            .with_allowed_origins(allow.origins)
    });
    if let Some(guard) = guard {
        router = router.layer(middleware::from_fn(move |request: Request, next: Next| {
            let guard = guard.clone();
            async move { enforce_host_guard(guard, request, next).await }
        }));
    }
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown.await;
            cancellation.cancel();
        })
        .await?;
    Ok(())
}

/// Bind `host:port` and serve until `shutdown` resolves.
pub async fn serve(
    server: HttpServer,
    host: &str,
    port: u16,
    path: &str,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let listener = bind(host, port).await?;
    serve_on(server, listener, path, shutdown).await
}

/// DNS-rebinding guard for the unauthenticated HTTP transports.
///
/// A browser on the machine can resolve an attacker's hostname to `127.0.0.1` and
/// then speak to this server with the browser's own credentials-free `fetch`, calling
/// every MCP tool including the mutating ones. Binding to loopback does not stop that,
/// so a loopback bind additionally requires the request's `Host` to name the bound
/// listener and its `Origin` (when present) to be same-origin with that host. This
/// mirrors upstream's GHSA-4gfv-c2mm-mvj2 fix and FastMCP's `Host`/`Origin` guard.
///
/// A non-loopback bind is an explicit opt-in to network exposure (the container image
/// passes `--host 0.0.0.0`), so the guard is not installed there: the operator already
/// decided the endpoint is reachable, and an allowlist would break reverse proxies.
#[derive(Clone, Debug)]
pub struct HostGuard {
    /// `host:port` the listener is bound to, lowercased.
    bound: String,
    /// Extra `Host` values accepted verbatim, from `--allow-host`.
    allowed_hosts: Vec<String>,
    /// Extra `Origin` values accepted verbatim, from `--allow-origin`.
    allowed_origins: Vec<String>,
}

impl HostGuard {
    /// Build a guard for a listener bound to `bound` (`host:port`).
    pub fn new(bound: impl Into<String>) -> Self {
        Self {
            bound: bound.into().to_ascii_lowercase(),
            allowed_hosts: Vec::new(),
            allowed_origins: Vec::new(),
        }
    }

    /// Accept these additional `Host` header values (a reverse proxy's public name).
    #[must_use]
    pub fn with_allowed_hosts(mut self, hosts: impl IntoIterator<Item = String>) -> Self {
        self.allowed_hosts = hosts
            .into_iter()
            .map(|host| host.trim().to_ascii_lowercase())
            .filter(|host| !host.is_empty())
            .collect();
        self
    }

    /// Accept these additional `Origin` values.
    #[must_use]
    pub fn with_allowed_origins(mut self, origins: impl IntoIterator<Item = String>) -> Self {
        self.allowed_origins = origins
            .into_iter()
            .map(|origin| origin.trim().trim_end_matches('/').to_ascii_lowercase())
            .filter(|origin| !origin.is_empty())
            .collect();
        self
    }

    /// Whether a request may proceed.
    ///
    /// A missing `Host` is accepted: HTTP/1.0 clients and some test harnesses omit it,
    /// and the rebinding attack needs a *present* attacker-controlled `Host`.
    pub fn permits(&self, headers: &HeaderMap) -> bool {
        let host = headers
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.trim().to_ascii_lowercase());
        if let Some(host) = &host
            && !self.host_matches(host)
        {
            return false;
        }

        // `Origin` is only sent by browsers, so its presence is exactly the case the
        // guard exists for. A non-browser client omits it and stays unaffected.
        match headers
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok())
        {
            None => true,
            Some(origin) => self.origin_matches(origin.trim().trim_end_matches('/')),
        }
    }

    fn host_matches(&self, host: &str) -> bool {
        if host == self.bound || self.allowed_hosts.iter().any(|allowed| allowed == host) {
            return true;
        }
        // A port-less `Host` names the same listener on the default port.
        match host.rsplit_once(':') {
            Some((name, _)) => {
                let bare = self
                    .bound
                    .rsplit_once(':')
                    .map_or(self.bound.as_str(), |(h, _)| h);
                name == bare && self.allowed_hosts.is_empty()
            }
            None => false,
        }
    }

    fn origin_matches(&self, origin: &str) -> bool {
        if self
            .allowed_origins
            .iter()
            .any(|allowed| allowed == &origin.to_ascii_lowercase())
        {
            return true;
        }
        let Some(rest) = origin
            .strip_prefix("http://")
            .or_else(|| origin.strip_prefix("https://"))
        else {
            // `Origin: null` (sandboxed iframe, file://) never comes from this origin.
            return false;
        };
        rest.to_ascii_lowercase() == self.bound
            || self.allowed_hosts.iter().any(|host| host == rest)
    }
}

/// Reject a request the [`HostGuard`] refuses.
///
/// The response body is deliberately terse: a probing page learns only that it was
/// refused, not which value would have been accepted.
async fn enforce_host_guard(guard: HostGuard, request: Request, next: Next) -> Response {
    if guard.permits(request.headers()) {
        return next.run(request).await;
    }
    (
        StatusCode::FORBIDDEN,
        "Forbidden: foreign Host or Origin header (DNS-rebinding protection)",
    )
        .into_response()
}

/// Validate and normalize the mount path.
///
/// `axum` panics when nesting at `/`, so an unusable `--path` is reported as an
/// error here rather than crashing the server on startup. A trailing slash is
/// dropped, which is the one malformation that has an obvious correction.
fn normalize_path(path: &str) -> Result<String> {
    let normalized = path.trim_end_matches('/');
    if normalized.is_empty() || !normalized.starts_with('/') {
        return Err(Error::InvalidArgument {
            message: format!("--path must be an absolute HTTP path such as /mcp (got {path:?})"),
        });
    }
    Ok(normalized.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::permalink::generate_permalink;
    use crate::indexing::{RebuildOptions, rebuild_vault};

    /// A server over the fixture vault, backed by an in-memory index.
    async fn server(read_only: bool, tag: &str) -> (tempfile::TempDir, HttpServer) {
        let dir = tempfile::Builder::new()
            .prefix(&format!("auto-memory-rs-http-{tag}-"))
            .tempdir()
            .expect("scratch dir");
        let vault = dir.path().join("vault");
        copy_dir(&repo_fixtures_vault(), &vault);
        let mut store = Store::open_in_memory().await.expect("store");
        let permalink = generate_permalink("oracle");
        let project_id = store
            .upsert_project("oracle", &permalink, &vault.to_string_lossy())
            .await
            .expect("project");
        rebuild_vault(
            &mut store,
            project_id,
            &vault,
            &RebuildOptions::new(&permalink),
        )
        .await
        .expect("rebuild");
        let external_id = store
            .project_by_permalink(&permalink)
            .await
            .expect("read project")
            .expect("project row")
            .external_id;
        let server = HttpServer::new(
            Arc::new(AsyncMutex::new(store)),
            project_id,
            "oracle",
            external_id,
            permalink,
            &vault,
        )
        .with_read_only(read_only);
        (dir, server)
    }

    fn repo_fixtures_vault() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vault")
    }

    fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
        std::fs::create_dir_all(to).expect("create dir");
        for entry in std::fs::read_dir(from).expect("read dir") {
            let entry = entry.expect("entry");
            let target = to.join(entry.file_name());
            if entry.path().is_dir() {
                copy_dir(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), &target).expect("copy file");
            }
        }
    }

    #[test]
    fn path_must_be_absolute_and_is_normalized() {
        assert!(normalize_path("/mcp").is_ok());
        assert_eq!(normalize_path("/mcp/").expect("normalized"), "/mcp");
        assert!(normalize_path("").is_err());
        assert!(normalize_path("/").is_err());
        assert!(normalize_path("mcp").is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tools_match_the_stdio_definitions_and_hide_mutating_tools_when_read_only() {
        let (_full_dir, full) = server(false, "tools-full").await;
        let names: Vec<String> = full
            .tools()
            .expect("tools")
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        assert!(names.contains(&"write_note".to_owned()));

        let (_ro_dir, read_only) = server(true, "tools-readonly").await;
        let names: Vec<String> = read_only
            .tools()
            .expect("tools")
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        assert!(!names.contains(&"write_note".to_owned()));
        assert!(names.contains(&"read_note".to_owned()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dispatch_tool_reaches_the_core_and_shapes_an_mcp_result() {
        let (_dir, server) = server(false, "dispatch").await;
        let params = json!({ "name": "read_note", "arguments": { "identifier": "notes/welcome" } });
        let result = server
            .dispatch_tool(&params, None)
            .await
            .expect("tool result");
        assert_eq!(result.is_error, Some(false));
        assert_eq!(result.content.len(), 1);
        assert!(result.structured_content.is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dispatch_tool_refuses_a_mutating_tool_in_read_only_mode() {
        let (_dir, server) = server(true, "dispatch-readonly").await;
        let params = json!({
            "name": "write_note",
            "arguments": { "title": "x", "content": "y" },
        });
        assert!(server.dispatch_tool(&params, None).await.is_err());
    }

    /// A `HeaderMap` carrying the headers a browser would send.
    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).expect("header name"),
                value.parse().expect("header value"),
            );
        }
        map
    }

    #[test]
    fn host_guard_accepts_the_bound_loopback_address() {
        let guard = HostGuard::new("127.0.0.1:8765");
        assert!(guard.permits(&headers(&[("host", "127.0.0.1:8765")])));
        // A browser posting from a page served by the same origin.
        assert!(guard.permits(&headers(&[
            ("host", "127.0.0.1:8765"),
            ("origin", "http://127.0.0.1:8765"),
        ])));
        // No `Host` at all: an HTTP/1.0 client, not the rebinding attack.
        assert!(guard.permits(&HeaderMap::new()));
    }

    #[test]
    fn host_guard_refuses_a_foreign_host() {
        let guard = HostGuard::new("127.0.0.1:8765");
        // The attack: a page on evil.example resolves to 127.0.0.1 and the browser
        // sends its own hostname in `Host`.
        assert!(!guard.permits(&headers(&[("host", "evil.example")])));
        assert!(!guard.permits(&headers(&[("host", "evil.example:8765")])));
    }

    #[test]
    fn host_guard_refuses_a_foreign_origin_even_with_a_matching_host() {
        let guard = HostGuard::new("127.0.0.1:8765");
        assert!(!guard.permits(&headers(&[
            ("host", "127.0.0.1:8765"),
            ("origin", "http://evil.example"),
        ])));
        // A sandboxed iframe's opaque origin never comes from this listener.
        assert!(!guard.permits(&headers(&[("host", "127.0.0.1:8765"), ("origin", "null"),])));
    }

    #[test]
    fn host_guard_allowlist_widens_the_accepted_values() {
        let guard = HostGuard::new("127.0.0.1:8765")
            .with_allowed_hosts(vec!["memory.internal".to_owned()])
            .with_allowed_origins(vec!["https://memory.internal".to_owned()]);
        assert!(guard.permits(&headers(&[
            ("host", "memory.internal"),
            ("origin", "https://memory.internal"),
        ])));
        // The bound address keeps working alongside the allowlist.
        assert!(guard.permits(&headers(&[("host", "127.0.0.1:8765")])));
        assert!(!guard.permits(&headers(&[("host", "other.internal")])));
    }

    #[test]
    fn client_info_round_trips_into_the_session_identity() {
        let info =
            ClientInfo::from_parts(Some("openai-mcp".to_owned()), None).expect("client info");
        assert_eq!(info.name(), Some("openai-mcp"));
        assert!(ClientInfo::from_parts(Some("  ".to_owned()), None).is_none());
    }
}
