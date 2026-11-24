//! A small MCP client: JSON-RPC 2.0 over stdio (one JSON message per line)
//! or over streamable HTTP (POST, answered with JSON or with a short SSE
//! stream). Only what Wawi uses: `initialize`, `tools/list`, `tools/call`,
//! `resources/subscribe`, and notifications from the server.
//!
//! Why not the official `rmcp` SDK: it is async on tokio, and its HTTP
//! client brings reqwest. Wawi runs on threads with blocking I/O and
//! already ships `ureq`; this file is the whole client.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio as Piped};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

/// The protocol version Wawi asks for. Servers answer with theirs.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
/// The largest reply read from a remote server.
const MAX_BODY: u64 = 8 * 1024 * 1024;

/// The OAuth access token an [`HttpTransport`] sends, shared with whoever
/// refreshes it (the plugin's worker), so a refresh needs no reconnect.
pub type Bearer = Arc<Mutex<Option<String>>>;

/// Called with `(method, params)` for every notification the server sends.
pub type OnNotify = Arc<dyn Fn(&str, &Value) + Send + Sync>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpError {
    /// The call took longer than its timeout. The connection may be fine.
    Timeout,
    /// The connection is gone (process exited, HTTP failed); reconnect.
    Dead(String),
    /// The server answered with a JSON-RPC error.
    Rpc(String),
    /// HTTP 401 or 403: the credential is missing, expired or revoked. Holds
    /// the `WWW-Authenticate` header (for OAuth discovery), if any.
    Unauthorized {
        status: u16,
        www_authenticate: String,
    },
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            McpError::Timeout => write!(f, "tardó demasiado"),
            McpError::Dead(why) => write!(f, "se cortó la conexión: {why}"),
            McpError::Rpc(why) => write!(f, "{why}"),
            McpError::Unauthorized { status, .. } => {
                write!(f, "el servidor rechazó la credencial (HTTP {status})")
            }
        }
    }
}

pub trait Transport: Send {
    fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, McpError>;
    fn notify(&mut self, method: &str, params: Value) -> Result<(), McpError>;
}

fn rpc_outcome(message: &Value) -> Result<Value, McpError> {
    if let Some(error) = message.get("error") {
        let text = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("error sin mensaje");
        return Err(McpError::Rpc(text.to_owned()));
    }
    Ok(message.get("result").cloned().unwrap_or(Value::Null))
}

type Pending = Arc<Mutex<HashMap<u64, Sender<Value>>>>;

/// A local server: a child process, JSON lines on stdin and stdout. Its
/// stderr goes to `on_stderr`, a line at a time.
pub struct StdioTransport {
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Pending,
    dead: Arc<AtomicBool>,
    next_id: u64,
}

pub struct StdioSpec<'a> {
    pub command: &'a str,
    pub args: &'a [String],
    pub env: &'a [(String, String)],
    pub dir: &'a Path,
}

impl StdioTransport {
    /// `on_closed` runs once, from the reader thread, when stdout closes.
    pub fn spawn(
        spec: StdioSpec<'_>,
        on_notify: OnNotify,
        on_stderr: Box<dyn Fn(&str) + Send>,
        on_closed: Box<dyn FnOnce() + Send>,
    ) -> Result<Self, McpError> {
        // `./server` is relative to the plugin's folder; a bare name is
        // looked up on the PATH.
        let program = if spec.command.starts_with("./") || spec.command.starts_with("../") {
            spec.dir.join(spec.command)
        } else {
            spec.command.into()
        };
        let mut command = Command::new(&program);
        command
            .args(spec.args)
            .current_dir(spec.dir)
            .stdin(Piped::piped())
            .stdout(Piped::piped())
            .stderr(Piped::piped());
        // The brain's own settings and API keys are none of a plugin's business.
        for (key, _) in std::env::vars_os() {
            let key = key.to_string_lossy();
            if key.starts_with("ANTHROPIC_") || key.starts_with("CLAUDE") {
                command.env_remove(key.as_ref());
            }
        }
        for (key, value) in spec.env {
            command.env(key, value);
        }
        let mut child = command
            .spawn()
            .map_err(|e| McpError::Dead(format!("no pude arrancar {}: {e}", program.display())))?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(McpError::Dead("arrancó sin sus tuberías".into()));
        };
        let stdin = Arc::new(Mutex::new(stdin));
        let pending: Pending = Arc::default();
        let dead = Arc::new(AtomicBool::new(false));
        {
            let (pending, dead, stdin) = (pending.clone(), dead.clone(), stdin.clone());
            thread::Builder::new()
                .name("wawi-plugin-stdout".into())
                .spawn(move || {
                    for line in BufReader::new(stdout).lines() {
                        let Ok(line) = line else { break };
                        let Ok(message) = serde_json::from_str::<Value>(&line) else {
                            continue;
                        };
                        dispatch(&message, &pending, &on_notify, &stdin);
                    }
                    dead.store(true, Ordering::SeqCst);
                    if let Ok(mut pending) = pending.lock() {
                        pending.clear();
                    }
                    on_closed();
                })
                .map_err(|e| McpError::Dead(e.to_string()))?;
        }
        thread::Builder::new()
            .name("wawi-plugin-stderr".into())
            .spawn(move || {
                for line in BufReader::new(stderr).lines() {
                    let Ok(line) = line else { break };
                    on_stderr(&line);
                }
            })
            .map_err(|e| McpError::Dead(e.to_string()))?;
        Ok(Self {
            child,
            stdin,
            pending,
            dead,
            next_id: 0,
        })
    }

    fn write(&self, message: &Value) -> Result<(), McpError> {
        write_line(&self.stdin, message)
    }
}

fn write_line(stdin: &Mutex<ChildStdin>, message: &Value) -> Result<(), McpError> {
    let mut stdin = stdin
        .lock()
        .map_err(|_| McpError::Dead("stdin envenenado".into()))?;
    writeln!(stdin, "{message}")
        .and_then(|()| stdin.flush())
        .map_err(|e| McpError::Dead(e.to_string()))
}

fn dispatch(message: &Value, pending: &Pending, on_notify: &OnNotify, stdin: &Mutex<ChildStdin>) {
    let method = message.get("method").and_then(Value::as_str);
    match (message.get("id"), method) {
        // A reply to one of ours.
        (Some(id), None) => {
            if let Some(id) = id.as_u64()
                && let Some(waiter) = pending.lock().ok().and_then(|mut p| p.remove(&id))
            {
                let _ = waiter.send(message.clone());
            }
        }
        // A request from the server: only `ping` is supported.
        (Some(id), Some(method)) => {
            let reply = if method == "ping" {
                json!({ "jsonrpc": "2.0", "id": id, "result": {} })
            } else {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("Wawi no soporta {method}") } })
            };
            let _ = write_line(stdin, &reply);
        }
        (None, Some(method)) => {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            on_notify(method, &params);
        }
        (None, None) => {}
    }
}

impl Transport for StdioTransport {
    fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, McpError> {
        if self.dead.load(Ordering::SeqCst) {
            return Err(McpError::Dead("el proceso terminó".into()));
        }
        self.next_id += 1;
        let id = self.next_id;
        let (tx, rx) = mpsc::channel();
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(id, tx);
        }
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        match rx.recv_timeout(timeout) {
            Ok(message) => rpc_outcome(&message),
            Err(RecvTimeoutError::Timeout) => {
                if let Ok(mut pending) = self.pending.lock() {
                    pending.remove(&id);
                }
                let _ = self.notify(
                    "notifications/cancelled",
                    json!({ "requestId": id, "reason": "timeout" }),
                );
                Err(McpError::Timeout)
            }
            Err(RecvTimeoutError::Disconnected) => Err(McpError::Dead("el proceso terminó".into())),
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), McpError> {
        self.write(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A remote server over streamable HTTP. Each message is a POST; the reply
/// is JSON or a short SSE stream that may carry notifications first.
/// Server-initiated streams (a GET kept open) are not used.
pub struct HttpTransport {
    agent: ureq::Agent,
    url: String,
    headers: Vec<(String, String)>,
    bearer: Option<Bearer>,
    session: Option<String>,
    protocol: Option<String>,
    on_notify: OnNotify,
    next_id: u64,
}

impl HttpTransport {
    /// With a token slot: every request carries `Authorization: Bearer`
    /// with whatever the slot holds then.
    pub fn with_bearer(mut self, bearer: Bearer) -> Self {
        self.bearer = Some(bearer);
        self
    }

    pub fn new(url: &str, headers: Vec<(String, String)>, on_notify: OnNotify) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            agent,
            url: url.to_owned(),
            headers,
            bearer: None,
            session: None,
            protocol: None,
            on_notify,
            next_id: 0,
        }
    }

    fn post(
        &mut self,
        message: &Value,
        timeout: Duration,
        id: Option<u64>,
    ) -> Result<Value, McpError> {
        let mut request = self
            .agent
            .post(&self.url)
            .config()
            .timeout_global(Some(timeout))
            .build()
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        for (name, value) in &self.headers {
            request = request.header(name.as_str(), value.as_str());
        }
        let token = self
            .bearer
            .as_ref()
            .and_then(|slot| slot.lock().ok().and_then(|t| t.clone()));
        if let Some(token) = token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        if let Some(session) = &self.session {
            request = request.header("Mcp-Session-Id", session.as_str());
        }
        if let Some(protocol) = &self.protocol {
            request = request.header("MCP-Protocol-Version", protocol.as_str());
        }
        let mut response = request
            .send(message.to_string())
            .map_err(|error| match error {
                ureq::Error::Timeout(_) => McpError::Timeout,
                other => McpError::Dead(other.to_string()),
            })?;
        let status = response.status().as_u16();
        if let Some(session) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            self.session = Some(session.to_owned());
        }
        match status {
            200..=299 => {}
            401 | 403 => {
                let www_authenticate = response
                    .headers()
                    .get("www-authenticate")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_owned();
                return Err(McpError::Unauthorized {
                    status,
                    www_authenticate,
                });
            }
            404 if self.session.is_some() => {
                self.session = None;
                return Err(McpError::Dead("la sesión venció (HTTP 404)".into()));
            }
            _ => return Err(McpError::Dead(format!("HTTP {status}"))),
        }
        let is_sse = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("text/event-stream"));
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_BODY)
            .read_to_string()
            .map_err(|e| match e {
                ureq::Error::Timeout(_) => McpError::Timeout,
                other => McpError::Dead(other.to_string()),
            })?;
        let Some(id) = id else {
            return Ok(Value::Null);
        };
        let messages: Vec<Value> = if is_sse {
            sse_messages(&body)
        } else {
            match serde_json::from_str::<Value>(&body) {
                Ok(Value::Array(items)) => items,
                Ok(value) => vec![value],
                Err(e) => return Err(McpError::Dead(format!("respuesta que no es JSON: {e}"))),
            }
        };
        let mut reply = None;
        for message in messages {
            if message.get("id").and_then(Value::as_u64) == Some(id)
                && message.get("method").is_none()
            {
                reply = Some(message);
            } else if let (None, Some(method)) = (
                message.get("id"),
                message.get("method").and_then(Value::as_str),
            ) {
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                (self.on_notify)(method, &params);
            }
        }
        match reply {
            Some(message) => rpc_outcome(&message),
            None => Err(McpError::Dead("el servidor no contestó el pedido".into())),
        }
    }
}

/// The JSON of every `data:` event in an SSE body.
pub fn sse_messages(body: &str) -> Vec<Value> {
    let mut messages = Vec::new();
    let mut data = String::new();
    for line in body.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if !data.is_empty() {
                if let Ok(value) = serde_json::from_str(&data) {
                    messages.push(value);
                }
                data.clear();
            }
        } else if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    messages
}

impl Transport for HttpTransport {
    fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, McpError> {
        self.next_id += 1;
        let id = self.next_id;
        let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let result = self.post(&message, timeout, Some(id))?;
        if method == "initialize" {
            self.protocol = result
                .get("protocolVersion")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        Ok(result)
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), McpError> {
        let message = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.post(&message, Duration::from_secs(10), None)
            .map(|_| ())
    }
}

/// A tool as the server lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    /// The server's `annotations.readOnlyHint`, if it set one.
    pub read_only_hint: Option<bool>,
}

/// What a `tools/call` gave back, as text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallResult {
    pub text: String,
    pub is_error: bool,
}

pub struct Client {
    transport: Box<dyn Transport>,
    pub server_name: String,
}

impl Client {
    /// The handshake: `initialize`, then `notifications/initialized`.
    pub fn connect(mut transport: Box<dyn Transport>, timeout: Duration) -> Result<Self, McpError> {
        let result = transport.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "wawi2", "version": env!("CARGO_PKG_VERSION") },
            }),
            timeout,
        )?;
        let server_name = result
            .pointer("/serverInfo/name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        transport.notify("notifications/initialized", json!({}))?;
        Ok(Self {
            transport,
            server_name,
        })
    }

    pub fn list_tools(&mut self, timeout: Duration) -> Result<Vec<RemoteTool>, McpError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..20 {
            let params = match &cursor {
                Some(cursor) => json!({ "cursor": cursor }),
                None => json!({}),
            };
            let result = self.transport.request("tools/list", params, timeout)?;
            for tool in result
                .get("tools")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(name) = tool.get("name").and_then(Value::as_str) else {
                    continue;
                };
                tools.push(RemoteTool {
                    name: name.to_owned(),
                    description: tool
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    input_schema: tool
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({ "type": "object" })),
                    read_only_hint: tool
                        .pointer("/annotations/readOnlyHint")
                        .and_then(Value::as_bool),
                });
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        Ok(tools)
    }

    pub fn call(
        &mut self,
        name: &str,
        args: Value,
        timeout: Duration,
    ) -> Result<CallResult, McpError> {
        let result = self.transport.request(
            "tools/call",
            json!({ "name": name, "arguments": args }),
            timeout,
        )?;
        Ok(CallResult {
            text: result_text(&result),
            is_error: result
                .get("isError")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    pub fn subscribe(&mut self, uri: &str, timeout: Duration) -> Result<(), McpError> {
        self.transport
            .request("resources/subscribe", json!({ "uri": uri }), timeout)
            .map(|_| ())
    }
}

/// The text parts of a tool result, joined. Other content is named, not
/// inlined. With no text at all, the structured content as JSON.
pub fn result_text(result: &Value) -> String {
    let mut parts = Vec::new();
    for item in result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match item.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    parts.push(text.to_owned());
                }
            }
            Some("resource") => {
                if let Some(text) = item.pointer("/resource/text").and_then(Value::as_str) {
                    parts.push(text.to_owned());
                }
            }
            Some(other) => parts.push(format!("[{other}]")),
            None => {}
        }
    }
    if parts.is_empty()
        && let Some(structured) = result.get("structuredContent")
    {
        return structured.to_string();
    }
    parts.join("\n")
}

/// Whether a watch result says nothing worth telling: blank, `null`, an
/// empty list or object, an object whose lists are all empty, or a word
/// that means "nothing" (plus the watch's own `empty` texts).
pub fn says_nothing(text: &str, extra: &[String]) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return true;
    }
    let lower = trimmed.to_lowercase();
    let lower = lower.trim_end_matches('.');
    const NOTHING: &[&str] = &[
        "nothing",
        "none",
        "nada",
        "ninguno",
        "ninguna",
        "sin resultados",
        "no results",
        "no hay nada",
        "[]",
        "{}",
        "null",
    ];
    if NOTHING.contains(&lower) || extra.iter().any(|e| e.trim().to_lowercase() == lower) {
        return true;
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(Value::Null) => true,
        Ok(Value::Array(items)) => items.is_empty(),
        Ok(Value::Object(map)) => {
            let lists: Vec<&Vec<Value>> = map.values().filter_map(Value::as_array).collect();
            map.is_empty() || (!lists.is_empty() && lists.iter().all(|list| list.is_empty()))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_results() {
        for nothing in [
            "",
            "  ",
            "[]",
            "{}",
            "null",
            "Nada.",
            "nothing",
            r#"{"issues": [], "hasNextPage": false}"#,
        ] {
            assert!(says_nothing(nothing, &[]), "{nothing}");
        }
        for something in [
            "Tres tareas",
            r#"[{"id": 1}]"#,
            r#"{"issues": [{"t": "x"}]}"#,
            r#"{"count": 0}"#,
            "0",
        ] {
            assert!(!says_nothing(something, &[]), "{something}");
        }
        assert!(says_nothing("No issues found", &["no issues found".into()]));
    }

    #[test]
    fn result_text_joins_text_parts() {
        let result = json!({ "content": [
            { "type": "text", "text": "uno" },
            { "type": "image", "data": "..." },
            { "type": "text", "text": "dos" },
        ]});
        assert_eq!(result_text(&result), "uno\n[image]\ndos");
        assert_eq!(
            result_text(&json!({ "content": [], "structuredContent": { "a": 1 } })),
            r#"{"a":1}"#
        );
    }

    /// A local HTTP server that answers like a streamable MCP server: JSON
    /// with a session id, 202 for notifications, SSE for the tool list.
    #[test]
    fn the_http_transport_speaks_streamable_http() {
        use std::io::{Read, Write as _};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = stream.read(&mut chunk).unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let len = text[..end]
                            .lines()
                            .find_map(|l| {
                                l.to_lowercase()
                                    .strip_prefix("content-length: ")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if buf.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                let request = String::from_utf8_lossy(&buf).to_string();
                let response = if request.contains("\"initialize\"") {
                    let body = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","serverInfo":{"name":"remoto"}}}"#;
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: ses-1\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                } else if request.contains("notifications/initialized") {
                    "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n".to_owned()
                } else {
                    let body = "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/wawi/instinto\",\"params\":{\"decir\":\"hola\"}}\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"leer\",\"annotations\":{\"readOnlyHint\":true}}]}}\n\n";
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                };
                stream.write_all(response.as_bytes()).unwrap();
                seen.push(request);
            }
            seen
        });
        let pushed = Arc::new(Mutex::new(Vec::new()));
        let sink = pushed.clone();
        let on_notify: OnNotify = Arc::new(move |method: &str, params: &Value| {
            sink.lock()
                .unwrap()
                .push(format!("{method} {}", params["decir"]));
        });
        let transport = HttpTransport::new(
            &url,
            vec![("Authorization".into(), "Bearer k-123".into())],
            on_notify,
        );
        let mut client = Client::connect(Box::new(transport), Duration::from_secs(5)).unwrap();
        assert_eq!(client.server_name, "remoto");
        let tools = client.list_tools(Duration::from_secs(5)).unwrap();
        assert_eq!(tools[0].name, "leer");
        assert_eq!(tools[0].read_only_hint, Some(true));
        assert_eq!(
            *pushed.lock().unwrap(),
            ["notifications/wawi/instinto \"hola\""]
        );
        let seen = server.join().unwrap();
        assert!(
            seen.iter()
                .all(|r| r.to_lowercase().contains("authorization: bearer k-123"))
        );
        assert!(!seen[0].to_lowercase().contains("mcp-session-id"));
        assert!(seen[2].to_lowercase().contains("mcp-session-id: ses-1"));
        assert!(
            seen[2]
                .to_lowercase()
                .contains("mcp-protocol-version: 2025-06-18")
        );
    }

    #[test]
    fn sse_bodies() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"n\"}\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n";
        let messages = sse_messages(body);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["id"], 1);
    }
}
