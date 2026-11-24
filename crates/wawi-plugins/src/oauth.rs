//! OAuth for remote MCP servers, as the MCP authorization spec asks:
//! OAuth 2.1 with PKCE (S256), protected resource metadata (RFC 9728),
//! authorization server metadata (RFC 8414), dynamic client registration
//! (RFC 7591) when the server offers it, resource indicators (RFC 8707),
//! and a loopback redirect (RFC 8252) to `http://127.0.0.1:<port>/callback`.
//!
//! Blocking, with `ureq`, like the MCP client. Nothing here runs on the host
//! or UI thread: the authorization runs on its own short-lived thread, and
//! refreshes on the plugin's worker thread.
//!
//! Tokens never reach a log. [`Grant`]'s `Debug` hides them, and errors
//! name what failed, never a value.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// How long the owner has to say yes in the browser.
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
/// A token this close to expiring is refreshed before use.
pub const REFRESH_MARGIN_S: i64 = 60;
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_BODY: u64 = 1024 * 1024;

/// The page the browser shows after the redirect.
pub const DONE_PAGE: &str = "<!doctype html><html lang=\"es\"><head><meta charset=\"utf-8\"><title>Wawi</title><style>body{font-family:-apple-system,system-ui,sans-serif;display:flex;align-items:center;justify-content:center;height:90vh;margin:0;background:#faf7f2;color:#2b2b2b}main{text-align:center}h1{font-size:1.6rem;font-weight:600}p{color:#666}</style></head><body><main><h1>Listo, ya podés volver a Wawi</h1><p>Podés cerrar esta pestaña.</p></main></body></html>";
const FAILED_PAGE: &str = "<!doctype html><html lang=\"es\"><head><meta charset=\"utf-8\"><title>Wawi</title></head><body style=\"font-family:-apple-system,system-ui,sans-serif;text-align:center;padding-top:20vh\"><h1>No se pudo conectar</h1><p>Volvé a Wawi y probá de nuevo.</p></body></html>";

/// What Wawi keeps (in the Keychain, as JSON) to use and refresh a token.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Unix seconds; `None` if the server did not say.
    #[serde(default)]
    pub expires_at: Option<i64>,
    pub token_endpoint: String,
    #[serde(default)]
    pub revocation_endpoint: Option<String>,
    pub issuer: String,
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    pub redirect_uri: String,
    pub resource: String,
    #[serde(default)]
    pub scope: Option<String>,
}

impl std::fmt::Debug for Grant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Grant")
            .field("issuer", &self.issuer)
            .field("client_id", &self.client_id)
            .field("expires_at", &self.expires_at)
            .field("refresh", &self.refresh_token.is_some())
            .finish_non_exhaustive()
    }
}

impl Grant {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_json(text: &str) -> Option<Self> {
        serde_json::from_str(text).ok()
    }

    /// Expired, or about to.
    pub fn stale(&self, now: i64) -> bool {
        self.expires_at
            .is_some_and(|at| now + REFRESH_MARGIN_S >= at)
    }
}

/// Why a refresh failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefreshError {
    /// The server says no (revoked, expired for good, client gone): the
    /// plugin is disconnected until the owner connects it again.
    Revoked(String),
    /// Network or server trouble: keep the grant and try later.
    Transient(String),
}

impl std::fmt::Display for RefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RefreshError::Revoked(why) | RefreshError::Transient(why) => f.write_str(why),
        }
    }
}

pub fn now_s() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// -- small URL helpers ------------------------------------------------------

/// `scheme://authority/path?query`, just enough to build the well-known
/// addresses and check where a URL points.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    pub scheme: String,
    /// Host, with the port if any.
    pub authority: String,
    /// Starts with `/`, or is empty.
    pub path: String,
}

impl Url {
    pub fn parse(text: &str) -> Option<Self> {
        let (scheme, rest) = text.split_once("://")?;
        let scheme = scheme.to_ascii_lowercase();
        if scheme != "https" && scheme != "http" {
            return None;
        }
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = rest[..end].to_ascii_lowercase();
        if authority.is_empty() || authority.contains('@') {
            return None;
        }
        let tail = &rest[end..];
        let path_end = tail.find(['?', '#']).unwrap_or(tail.len());
        let path = tail[..path_end].trim_end_matches('/').to_owned();
        Some(Self {
            scheme,
            authority,
            path,
        })
    }

    pub fn origin(&self) -> String {
        format!("{}://{}", self.scheme, self.authority)
    }

    pub fn host(&self) -> &str {
        if let Some(rest) = self.authority.strip_prefix('[') {
            return rest.split(']').next().unwrap_or("");
        }
        self.authority.split(':').next().unwrap_or("")
    }

    pub fn is_loopback(&self) -> bool {
        matches!(self.host(), "127.0.0.1" | "localhost" | "::1")
    }
}

/// HTTPS, or plain HTTP to this machine only.
pub fn check_secure(url: &str) -> Result<Url, String> {
    let parsed = Url::parse(url).ok_or_else(|| format!("dirección inválida: {url}"))?;
    if parsed.scheme == "https" || parsed.is_loopback() {
        Ok(parsed)
    } else {
        Err(format!("{url} no usa https; no la uso"))
    }
}

pub fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

pub fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = |b: u8| (b as char).to_digit(16);
                match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    (Some(high), Some(low)) => {
                        out.push((high * 16 + low) as u8);
                        i += 2;
                    }
                    _ => out.push(b'%'),
                }
            }
            other => out.push(other),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn query_pairs(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(k), decode(v))
        })
        .collect()
}

// -- PKCE and randomness -----------------------------------------------------

pub fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let chars = chunk.len() + 1;
        for i in 0..chars {
            out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

/// `bytes` random bytes from the OS, base64url.
pub fn random_token(bytes: usize) -> Result<String, String> {
    let mut buf = vec![0u8; bytes];
    getrandom::getrandom(&mut buf).map_err(|e| format!("sin azar del sistema: {e}"))?;
    Ok(base64url(&buf))
}

/// The S256 challenge of a verifier.
pub fn challenge(verifier: &str) -> String {
    base64url(&Sha256::digest(verifier.as_bytes()))
}

// -- HTTP ---------------------------------------------------------------------

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(HTTP_TIMEOUT))
        .build()
        .into()
}

fn read_json(mut response: ureq::http::Response<ureq::Body>) -> (u16, Option<Value>) {
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_BODY)
        .read_to_string()
        .unwrap_or_default();
    (status, serde_json::from_str(&body).ok())
}

fn get_json(url: &str) -> Result<Option<Value>, String> {
    let response = agent()
        .get(url)
        .header("Accept", "application/json")
        .header("MCP-Protocol-Version", crate::mcp::PROTOCOL_VERSION)
        .call()
        .map_err(|e| format!("no pude leer {url}: {e}"))?;
    match read_json(response) {
        (200..=299, Some(value)) if value.is_object() => Ok(Some(value)),
        _ => Ok(None),
    }
}

fn post_form(url: &str, body: &str) -> Result<(u16, Option<Value>), String> {
    let response = agent()
        .post(url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .send(body.to_owned())
        .map_err(|e| format!("no pude hablar con {url}: {e}"))?;
    Ok(read_json(response))
}

// -- discovery ----------------------------------------------------------------

/// What discovery found about a protected MCP server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Discovery {
    /// The resource indicator to ask a token for (the MCP server's URL).
    pub resource: String,
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
    pub revocation_endpoint: Option<String>,
    pub scopes: Vec<String>,
    /// The server sends `iss` with the code (RFC 9207): check it.
    pub iss_parameter: bool,
}

/// `name="value"` from a `WWW-Authenticate` header.
pub fn auth_param(header: &str, name: &str) -> Option<String> {
    let lower = header.to_ascii_lowercase();
    let needle = format!("{name}=");
    let mut from = 0;
    while let Some(found) = lower[from..].find(&needle) {
        let start = from + found;
        let ok_before = start == 0 || matches!(lower.as_bytes()[start - 1], b' ' | b',' | b'\t');
        let value_start = start + needle.len();
        if ok_before {
            let rest = &header[value_start..];
            return Some(if let Some(quoted) = rest.strip_prefix('"') {
                quoted.split('"').next().unwrap_or("").to_owned()
            } else {
                rest.split([',', ' ']).next().unwrap_or("").to_owned()
            });
        }
        from = value_start;
    }
    None
}

/// Reads the server's protected resource metadata (from the 401's
/// `WWW-Authenticate` if there was one, else the well-known addresses),
/// then its authorization server's metadata. Without resource metadata
/// (older servers) the MCP server's origin is the authorization server.
pub fn discover(mcp_url: &str, www_authenticate: Option<&str>) -> Result<Discovery, String> {
    let mcp = check_secure(mcp_url)?;
    let mut candidates = Vec::new();
    if let Some(url) = www_authenticate.and_then(|h| auth_param(h, "resource_metadata")) {
        candidates.push(url);
    }
    if !mcp.path.is_empty() {
        candidates.push(format!(
            "{}/.well-known/oauth-protected-resource{}",
            mcp.origin(),
            mcp.path
        ));
    }
    candidates.push(format!(
        "{}/.well-known/oauth-protected-resource",
        mcp.origin()
    ));
    let mut resource_meta = None;
    for url in candidates {
        let Ok(url) = check_secure(&url).map(|_| url) else {
            continue;
        };
        if let Some(meta) = get_json(&url)?
            && meta.get("authorization_servers").is_some()
        {
            resource_meta = Some(meta);
            break;
        }
    }
    let wanted = mcp_url.trim_end_matches('/').to_owned();
    let (issuer, resource, mut scopes) = match &resource_meta {
        Some(meta) => {
            let issuer = meta
                .pointer("/authorization_servers/0")
                .and_then(Value::as_str)
                .ok_or("la metadata del recurso no nombra un servidor de autorización")?
                .trim_end_matches('/')
                .to_owned();
            let declared = meta
                .get("resource")
                .and_then(Value::as_str)
                .map(|r| r.trim_end_matches('/').to_owned());
            if let Some(declared) = &declared
                && !wanted.starts_with(declared.as_str())
            {
                return Err(format!(
                    "la metadata es de otro recurso ({declared}), no de {wanted}"
                ));
            }
            let scopes = strings(meta.get("scopes_supported"));
            (issuer, wanted.clone(), scopes)
        }
        None => (mcp.origin(), wanted.clone(), Vec::new()),
    };
    if let Some(scope) = www_authenticate.and_then(|h| auth_param(h, "scope")) {
        scopes = scope.split_whitespace().map(str::to_owned).collect();
    }
    let as_url = check_secure(&issuer)?;
    let mut meta_urls = Vec::new();
    if as_url.path.is_empty() {
        meta_urls.push(format!(
            "{}/.well-known/oauth-authorization-server",
            as_url.origin()
        ));
        meta_urls.push(format!(
            "{}/.well-known/openid-configuration",
            as_url.origin()
        ));
    } else {
        meta_urls.push(format!(
            "{}/.well-known/oauth-authorization-server{}",
            as_url.origin(),
            as_url.path
        ));
        meta_urls.push(format!(
            "{}/.well-known/openid-configuration{}",
            as_url.origin(),
            as_url.path
        ));
        meta_urls.push(format!("{issuer}/.well-known/openid-configuration"));
    }
    let mut server_meta = None;
    for url in meta_urls {
        if let Some(meta) = get_json(&url)? {
            server_meta = Some(meta);
            break;
        }
    }
    let meta = server_meta.ok_or_else(|| {
        format!(
            "{issuer} no publica la metadata de OAuth (/.well-known/oauth-authorization-server)"
        )
    })?;
    let declared_issuer = meta
        .get("issuer")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim_end_matches('/');
    if declared_issuer != issuer {
        return Err(format!(
            "el emisor no coincide: esperaba {issuer} y dice «{declared_issuer}»"
        ));
    }
    let methods = strings(meta.get("code_challenge_methods_supported"));
    if !methods.iter().any(|m| m == "S256") {
        return Err(format!("{issuer} no ofrece PKCE con S256; no me conecto"));
    }
    let endpoint = |name: &str| -> Result<Option<String>, String> {
        match meta.get(name).and_then(Value::as_str) {
            None => Ok(None),
            Some(url) => check_secure(url).map(|_| Some(url.to_owned())),
        }
    };
    Ok(Discovery {
        resource,
        authorization_endpoint: endpoint("authorization_endpoint")?
            .ok_or("falta authorization_endpoint")?,
        token_endpoint: endpoint("token_endpoint")?.ok_or("falta token_endpoint")?,
        registration_endpoint: endpoint("registration_endpoint")?,
        revocation_endpoint: endpoint("revocation_endpoint")?,
        iss_parameter: meta
            .get("authorization_response_iss_parameter_supported")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        issuer,
        scopes,
    })
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

// -- registration ---------------------------------------------------------------

#[derive(Clone, PartialEq, Eq)]
pub struct ClientId {
    pub client_id: String,
    pub client_secret: Option<String>,
}

/// Dynamic client registration (RFC 7591): a public client with this
/// loopback redirect.
pub fn register(discovery: &Discovery, redirect_uri: &str) -> Result<ClientId, String> {
    let url = discovery
        .registration_endpoint
        .as_deref()
        .ok_or("este servidor no deja registrarse solo; poné client_id en auth, en plugin.toml")?;
    let body = json!({
        "client_name": "Wawi",
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    let response = agent()
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .send(body.to_string())
        .map_err(|e| format!("no pude registrarme en {url}: {e}"))?;
    match read_json(response) {
        (200..=299, Some(reply)) => {
            let client_id = reply
                .get("client_id")
                .and_then(Value::as_str)
                .ok_or("el registro no devolvió client_id")?
                .to_owned();
            Ok(ClientId {
                client_id,
                client_secret: reply
                    .get("client_secret")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
        }
        (status, reply) => Err(format!(
            "el registro falló (HTTP {status}{})",
            oauth_error(reply.as_ref())
        )),
    }
}

fn oauth_error(reply: Option<&Value>) -> String {
    reply
        .and_then(|r| r.get("error"))
        .and_then(Value::as_str)
        .map(|e| format!(": {e}"))
        .unwrap_or_default()
}

// -- the loopback redirect --------------------------------------------------------

/// A one-shot listener on 127.0.0.1 for the browser's redirect.
pub struct Loopback {
    listener: TcpListener,
    port: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Callback {
    pub code: String,
    pub iss: Option<String>,
}

impl Loopback {
    /// On `port` if given and free, else on a random one.
    pub fn bind(port: Option<u16>) -> Result<Self, String> {
        let listener = port
            .and_then(|p| TcpListener::bind(("127.0.0.1", p)).ok())
            .map(Ok)
            .unwrap_or_else(|| TcpListener::bind(("127.0.0.1", 0)))
            .map_err(|e| format!("no pude abrir el puerto local: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        Ok(Self { listener, port })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.port)
    }

    /// Waits for `/callback` with this `state`. Requests for other paths, or
    /// with another state, get an error page and are ignored. Gives up at
    /// `timeout`.
    pub fn wait(&self, state: &str, timeout: Duration) -> Result<Callback, String> {
        let deadline = Instant::now() + timeout;
        loop {
            if Instant::now() >= deadline {
                return Err("pasó el tiempo y no llegó el permiso del navegador".into());
            }
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if let Some(outcome) = handle_callback(stream, state) {
                        return outcome;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(format!("el puerto local falló: {e}")),
            }
        }
    }
}

fn respond(stream: &mut TcpStream, status: &str, page: &str) {
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{page}",
        page.len()
    );
    let _ = stream.write_all(reply.as_bytes());
    let _ = stream.flush();
}

/// `Some` when this request settles the wait.
fn handle_callback(mut stream: TcpStream, state: &str) -> Option<Result<Callback, String>> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 16 * 1024 {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let target = head
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("GET "))
        .and_then(|rest| rest.split(' ').next())
        .unwrap_or("");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/callback" {
        respond(&mut stream, "404 Not Found", "");
        return None;
    }
    let params = query_pairs(query);
    let get = |name: &str| {
        params
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    if get("state").as_deref() != Some(state) {
        respond(&mut stream, "400 Bad Request", FAILED_PAGE);
        return None;
    }
    if let Some(error) = get("error") {
        respond(&mut stream, "200 OK", FAILED_PAGE);
        let why = match error.as_str() {
            "access_denied" => "no diste permiso en el navegador".to_owned(),
            other => format!("el servidor dijo {other}"),
        };
        return Some(Err(why));
    }
    match get("code").filter(|c| !c.is_empty()) {
        Some(code) => {
            respond(&mut stream, "200 OK", DONE_PAGE);
            Some(Ok(Callback {
                code,
                iss: get("iss"),
            }))
        }
        None => {
            respond(&mut stream, "400 Bad Request", FAILED_PAGE);
            Some(Err("el navegador volvió sin código".into()))
        }
    }
}

// -- tokens -----------------------------------------------------------------------

fn token_request(
    endpoint: &str,
    pairs: &[(&str, &str)],
    secret: Option<&str>,
) -> Result<(u16, Option<Value>), String> {
    let mut all: Vec<(&str, &str)> = pairs.to_vec();
    if let Some(secret) = secret {
        all.push(("client_secret", secret));
    }
    post_form(endpoint, &form(&all))
}

fn grant_from(reply: &Value, base: Grant) -> Result<Grant, String> {
    let access_token = reply
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .ok_or("la respuesta no trae access_token")?
        .to_owned();
    let token_type = reply
        .get("token_type")
        .and_then(Value::as_str)
        .unwrap_or("Bearer");
    if !token_type.eq_ignore_ascii_case("bearer") {
        return Err(format!("token de tipo {token_type}, no Bearer"));
    }
    let expires_at = reply
        .get("expires_in")
        .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
        .map(|secs| now_s() + secs);
    Ok(Grant {
        access_token,
        // Rotation: a new refresh token replaces the old one; none keeps it.
        refresh_token: reply
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or(base.refresh_token),
        expires_at,
        scope: reply
            .get("scope")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or(base.scope),
        ..base
    })
}

/// Uses the refresh token. A rotated refresh token replaces the old one.
pub fn refresh(grant: &Grant) -> Result<Grant, RefreshError> {
    let refresh_token = grant
        .refresh_token
        .as_deref()
        .ok_or_else(|| RefreshError::Revoked("el permiso venció y no se puede renovar".into()))?;
    let (status, reply) = token_request(
        &grant.token_endpoint,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", &grant.client_id),
            ("resource", &grant.resource),
        ],
        grant.client_secret.as_deref(),
    )
    .map_err(RefreshError::Transient)?;
    match (status, reply) {
        (200..=299, Some(reply)) => {
            grant_from(&reply, grant.clone()).map_err(RefreshError::Transient)
        }
        (400 | 401, reply) => Err(RefreshError::Revoked(format!(
            "el servidor ya no acepta el permiso (HTTP {status}{})",
            oauth_error(reply.as_ref())
        ))),
        (status, _) => Err(RefreshError::Transient(format!(
            "renovar el permiso falló (HTTP {status})"
        ))),
    }
}

/// Best effort (RFC 7009): tells the server to forget the grant.
pub fn revoke(grant: &Grant) {
    let Some(endpoint) = &grant.revocation_endpoint else {
        return;
    };
    let (token, hint) = match &grant.refresh_token {
        Some(token) => (token.as_str(), "refresh_token"),
        None => (grant.access_token.as_str(), "access_token"),
    };
    let _ = token_request(
        endpoint,
        &[
            ("token", token),
            ("token_type_hint", hint),
            ("client_id", &grant.client_id),
        ],
        grant.client_secret.as_deref(),
    );
}

// -- the whole login --------------------------------------------------------------

/// What the login needs besides the URL.
pub struct Login<'a> {
    pub mcp_url: &'a str,
    /// From plugin.toml, for servers without dynamic registration.
    pub client_id: Option<&'a str>,
    /// From plugin.toml; else what the server suggests.
    pub scopes: Option<&'a [String]>,
    /// The last grant, to reuse its registered client and port.
    pub previous: Option<&'a Grant>,
    pub www_authenticate: Option<&'a str>,
    pub timeout: Duration,
    /// Opens the browser at the URL.
    pub open: &'a dyn Fn(&str) -> Result<(), String>,
    /// Called once the browser is open, before waiting for the owner.
    pub opened: &'a mut dyn FnMut(),
}

/// Discovery, registration, the browser, the redirect and the token. The
/// whole thing blocks until the owner answers or `timeout`: run it on its
/// own thread.
pub fn login(login: Login<'_>) -> Result<Grant, String> {
    let discovery = discover(login.mcp_url, login.www_authenticate)?;
    // Reuse the registered client (and its port) when the server is the same.
    let previous = login
        .previous
        .filter(|g| g.issuer == discovery.issuer && login.client_id.is_none());
    let previous_port = previous
        .and_then(|g| Url::parse(&g.redirect_uri))
        .and_then(|u| u.authority.rsplit(':').next()?.parse::<u16>().ok());
    let loopback = Loopback::bind(previous_port)?;
    let redirect_uri = loopback.redirect_uri();
    let client = match (login.client_id, previous) {
        (Some(id), _) => ClientId {
            client_id: id.to_owned(),
            client_secret: None,
        },
        (None, Some(grant)) if grant.redirect_uri == redirect_uri => ClientId {
            client_id: grant.client_id.clone(),
            client_secret: grant.client_secret.clone(),
        },
        _ => register(&discovery, &redirect_uri)?,
    };
    let verifier = random_token(32)?;
    let state = random_token(24)?;
    let scopes = match login.scopes {
        Some(scopes) if !scopes.is_empty() => scopes.join(" "),
        _ => discovery.scopes.join(" "),
    };
    let mut pairs = vec![
        ("response_type", "code"),
        ("client_id", client.client_id.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("code_challenge", ""),
        ("code_challenge_method", "S256"),
        ("state", state.as_str()),
        ("resource", discovery.resource.as_str()),
    ];
    let code_challenge = challenge(&verifier);
    pairs[3].1 = &code_challenge;
    if !scopes.is_empty() {
        pairs.push(("scope", &scopes));
    }
    let separator = if discovery.authorization_endpoint.contains('?') {
        '&'
    } else {
        '?'
    };
    let url = format!(
        "{}{separator}{}",
        discovery.authorization_endpoint,
        form(&pairs)
    );
    (login.open)(&url)?;
    (login.opened)();
    let callback = loopback.wait(&state, login.timeout)?;
    if let Some(iss) = &callback.iss {
        if iss.trim_end_matches('/') != discovery.issuer {
            return Err("el permiso volvió de otro emisor; lo descarto".into());
        }
    } else if discovery.iss_parameter {
        return Err("el permiso volvió sin emisor; lo descarto".into());
    }
    let (status, reply) = token_request(
        &discovery.token_endpoint,
        &[
            ("grant_type", "authorization_code"),
            ("code", &callback.code),
            ("redirect_uri", &redirect_uri),
            ("client_id", &client.client_id),
            ("code_verifier", &verifier),
            ("resource", &discovery.resource),
        ],
        client.client_secret.as_deref(),
    )?;
    let base = Grant {
        access_token: String::new(),
        refresh_token: None,
        expires_at: None,
        token_endpoint: discovery.token_endpoint.clone(),
        revocation_endpoint: discovery.revocation_endpoint.clone(),
        issuer: discovery.issuer.clone(),
        client_id: client.client_id,
        client_secret: client.client_secret,
        redirect_uri,
        resource: discovery.resource.clone(),
        scope: None,
    };
    match (status, reply) {
        (200..=299, Some(reply)) => grant_from(&reply, base),
        (status, reply) => Err(format!(
            "cambiar el código por el permiso falló (HTTP {status}{})",
            oauth_error(reply.as_ref())
        )),
    }
}

/// Opens `url` in the owner's default browser.
pub fn open_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(not(target_os = "macos"))]
    let program = "xdg-open";
    std::process::Command::new(program)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("no pude abrir el navegador: {e}"))
        .and_then(|status| {
            if status.success() {
                Ok(())
            } else {
                Err("no pude abrir el navegador".into())
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_the_rfc_example() {
        // RFC 7636, appendix B.
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        let a = random_token(32).unwrap();
        assert_eq!(a.len(), 43);
        assert_ne!(a, random_token(32).unwrap());
    }

    #[test]
    fn urls_and_security() {
        let u = Url::parse("https://MCP.linear.app/mcp/").unwrap();
        assert_eq!(u.origin(), "https://mcp.linear.app");
        assert_eq!(u.path, "/mcp");
        assert!(check_secure("https://x.com/a").is_ok());
        assert!(check_secure("http://127.0.0.1:9/a").is_ok());
        assert!(check_secure("http://localhost/a").is_ok());
        assert!(check_secure("http://[::1]:80/a").is_ok());
        assert!(check_secure("http://evil.com/a").is_err());
        assert!(check_secure("http://127.0.0.1.evil.com/a").is_err());
        assert!(check_secure("ftp://x").is_err());
        assert!(check_secure("https://user@x.com").is_err());
        assert_eq!(encode("a b/ñ"), "a%20b%2F%C3%B1");
        assert_eq!(decode("a%20b%2F%C3%B1+c"), "a b/ñ c");
        assert_eq!(decode("100%"), "100%");
    }

    #[test]
    fn www_authenticate_params() {
        let h = r#"Bearer error="invalid_token", resource_metadata="https://x/.well-known/oauth-protected-resource", scope="read write""#;
        assert_eq!(
            auth_param(h, "resource_metadata").as_deref(),
            Some("https://x/.well-known/oauth-protected-resource")
        );
        assert_eq!(auth_param(h, "scope").as_deref(), Some("read write"));
        assert_eq!(auth_param(h, "error").as_deref(), Some("invalid_token"));
        assert_eq!(auth_param(h, "realm"), None);
    }

    #[test]
    fn grants_hide_their_tokens_and_know_when_they_are_stale() {
        let grant = Grant {
            access_token: "AT-secreto".into(),
            refresh_token: Some("RT-secreto".into()),
            expires_at: Some(1000),
            token_endpoint: "https://x/token".into(),
            revocation_endpoint: None,
            issuer: "https://x".into(),
            client_id: "c".into(),
            client_secret: None,
            redirect_uri: "http://127.0.0.1:1/callback".into(),
            resource: "https://x/mcp".into(),
            scope: None,
        };
        let shown = format!("{grant:?}");
        assert!(!shown.contains("secreto"), "{shown}");
        assert!(grant.stale(950));
        assert!(!grant.stale(900));
        assert_eq!(Grant::from_json(&grant.to_json()), Some(grant));
    }

    #[test]
    fn the_loopback_checks_state_and_ignores_strays() {
        let loopback = Loopback::bind(None).unwrap();
        let port = loopback.port();
        let browser = std::thread::spawn(move || {
            let get = |path: &str| {
                let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
                write!(s, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
                let mut out = String::new();
                let _ = s.read_to_string(&mut out);
                out
            };
            let stray = get("/favicon.ico");
            let forged = get("/callback?code=malo&state=otro");
            let good = get("/callback?code=bueno&state=s1&iss=https%3A%2F%2Fx");
            (stray, forged, good)
        });
        let got = loopback.wait("s1", Duration::from_secs(10)).unwrap();
        assert_eq!(got.code, "bueno");
        assert_eq!(got.iss.as_deref(), Some("https://x"));
        let (stray, forged, good) = browser.join().unwrap();
        assert!(stray.starts_with("HTTP/1.1 404"));
        assert!(forged.starts_with("HTTP/1.1 400"));
        assert!(good.contains("Listo, ya podés volver a Wawi"));
    }

    #[test]
    fn the_loopback_gives_up() {
        let loopback = Loopback::bind(None).unwrap();
        let error = loopback.wait("s", Duration::from_millis(120)).unwrap_err();
        assert!(error.contains("tiempo"));
    }

    #[test]
    fn a_denied_login_says_so() {
        let loopback = Loopback::bind(None).unwrap();
        let port = loopback.port();
        std::thread::spawn(move || {
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(
                s,
                "GET /callback?error=access_denied&state=s HTTP/1.1\r\n\r\n"
            )
            .unwrap();
        });
        let error = loopback.wait("s", Duration::from_secs(10)).unwrap_err();
        assert!(error.contains("permiso"));
    }
}
