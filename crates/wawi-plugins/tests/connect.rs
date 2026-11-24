//! Connecting plugins the easy way, with no network and no real Keychain:
//! a fake OAuth + MCP server on loopback, a fake browser that follows the
//! redirect, a memory secret store and a temporary WAWI2_HOME.
//!
//! - OAuth: add from a suggestion, discovery, dynamic registration, PKCE,
//!   the loopback callback, tokens in the store, refresh with rotation on a
//!   401, revocation leaves it «Desconectado», disconnect forgets it.
//! - A key: the server says 401 with no OAuth metadata, the key dialog's
//!   answer goes to the store and the plugin connects with it.
//! - Wawi's own plugins: installed from the catalog, a missing program is
//!   explained, turning one off takes its tools away.

use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wawi_core::{Event, Registry, ToolCall, ToolOutcome, TurnId};
use wawi_plugins::catalog::Suggestion;
use wawi_plugins::control::{KeyPrompt, PluginInfo, PluginState};
use wawi_plugins::keychain::{MemoryStore, SecretStore};
use wawi_plugins::oauth::{self, challenge};
use wawi_plugins::{HostOptions, PluginEvent, PluginHost, PluginSink, Services};
use wawi_tools::{Civil, FakeClock, WallClock};

const WAIT: Duration = Duration::from_secs(20);

enum Msg {
    Core(Event),
    Plugin(PluginEvent),
}

#[derive(Clone)]
struct Sink(Sender<Msg>);

impl PluginSink for Sink {
    fn core(&self, event: Event) {
        let _ = self.0.send(Msg::Core(event));
    }
    fn plugin(&self, event: PluginEvent) {
        let _ = self.0.send(Msg::Plugin(event));
    }
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wawi-connect-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// -- a fake OAuth authorization server and MCP server, on loopback ----------------

#[derive(Default)]
struct ServerState {
    oauth: bool,
    base: String,
    challenge: Option<String>,
    registered: Vec<Value>,
    access: HashSet<String>,
    refresh: HashSet<String>,
    issued: u32,
    refreshes: u32,
    revocations: u32,
    /// Every Authorization header the MCP endpoint saw.
    bearers: Vec<String>,
}

type Shared = Arc<Mutex<ServerState>>;

fn read_request(stream: &mut TcpStream) -> (String, String, BTreeMap<String, String>, String) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buf).to_string();
        if let Some(end) = text.find("\r\n\r\n") {
            let len = text[..end]
                .lines()
                .find_map(|l| {
                    l.to_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            if buf.len() >= end + 4 + len {
                break;
            }
        }
    }
    let text = String::from_utf8_lossy(&buf).to_string();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let mut lines = head.lines();
    let first = lines.next().unwrap_or("");
    let mut parts = first.split(' ');
    let method = parts.next().unwrap_or("").to_owned();
    let target = parts.next().unwrap_or("").to_owned();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_owned()))
        .collect();
    (method, target, headers, body.to_owned())
}

fn reply(stream: &mut TcpStream, status: &str, extra: &str, body: &str) {
    let text = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(text.as_bytes());
}

fn form(body: &str) -> BTreeMap<String, String> {
    body.split('&')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (oauth::decode(k), oauth::decode(v)))
        .collect()
}

fn issue(state: &mut ServerState) -> Value {
    state.issued += 1;
    let (at, rt) = (
        format!("at-{}", state.issued),
        format!("rt-{}", state.issued),
    );
    state.access.insert(at.clone());
    state.refresh.insert(rt.clone());
    json!({ "access_token": at, "token_type": "Bearer", "expires_in": 3600, "refresh_token": rt })
}

fn serve(stream: &mut TcpStream, shared: &Shared) {
    let (method, target, headers, body) = read_request(stream);
    let mut state = shared.lock().unwrap();
    let base = state.base.clone();
    match (method.as_str(), target.as_str()) {
        ("GET", "/.well-known/oauth-protected-resource/mcp") if state.oauth => {
            let meta = json!({ "resource": format!("{base}/mcp"), "authorization_servers": [base], "scopes_supported": ["read", "write"] });
            reply(stream, "200 OK", "", &meta.to_string());
        }
        ("GET", "/.well-known/oauth-authorization-server") if state.oauth => {
            let meta = json!({
                "issuer": base,
                "authorization_endpoint": format!("{base}/authorize"),
                "token_endpoint": format!("{base}/token"),
                "registration_endpoint": format!("{base}/register"),
                "revocation_endpoint": format!("{base}/revoke"),
                "code_challenge_methods_supported": ["S256"],
                "authorization_response_iss_parameter_supported": true,
            });
            reply(stream, "200 OK", "", &meta.to_string());
        }
        ("POST", "/register") => {
            state
                .registered
                .push(serde_json::from_str(&body).unwrap_or(Value::Null));
            reply(stream, "201 Created", "", r#"{"client_id":"cli-1"}"#);
        }
        ("POST", "/revoke") => {
            state.revocations += 1;
            let token = form(&body).remove("token").unwrap_or_default();
            state.refresh.remove(&token);
            reply(stream, "200 OK", "", "{}");
        }
        ("POST", "/token") => {
            let f = form(&body);
            let ok_resource = f.get("resource") == Some(&format!("{base}/mcp"));
            match f.get("grant_type").map(String::as_str) {
                Some("authorization_code")
                    if f.get("code").map(String::as_str) == Some("code-1")
                        && f.get("client_id").map(String::as_str) == Some("cli-1")
                        && ok_resource
                        && state.challenge.as_deref()
                            == f.get("code_verifier").map(|v| challenge(v)).as_deref() =>
                {
                    let tokens = issue(&mut state);
                    reply(stream, "200 OK", "", &tokens.to_string());
                }
                Some("refresh_token")
                    if ok_resource
                        && state
                            .refresh
                            .remove(f.get("refresh_token").map(String::as_str).unwrap_or("")) =>
                {
                    state.refreshes += 1;
                    let tokens = issue(&mut state);
                    reply(stream, "200 OK", "", &tokens.to_string());
                }
                _ => reply(
                    stream,
                    "400 Bad Request",
                    "",
                    r#"{"error":"invalid_grant"}"#,
                ),
            }
        }
        ("POST", "/mcp") => {
            let auth = headers.get("authorization").cloned().unwrap_or_default();
            state.bearers.push(auth.clone());
            let token = auth.strip_prefix("Bearer ").unwrap_or("");
            let allowed = if state.oauth {
                state.access.contains(token)
            } else {
                token == "k-123"
            };
            if !allowed {
                let header = if state.oauth {
                    format!(
                        "WWW-Authenticate: Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource/mcp\"\r\n"
                    )
                } else {
                    "WWW-Authenticate: Bearer\r\n".to_owned()
                };
                return reply(stream, "401 Unauthorized", &header, "");
            }
            let message: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let id = message.get("id").cloned();
            let result = match message.get("method").and_then(Value::as_str) {
                Some("initialize") => {
                    json!({ "protocolVersion": "2025-06-18", "serverInfo": { "name": "falso" } })
                }
                Some("tools/list") => {
                    json!({ "tools": [{ "name": "leer", "annotations": { "readOnlyHint": true } }] })
                }
                Some("tools/call") => json!({ "content": [{ "type": "text", "text": "hola" }] }),
                _ => json!({}),
            };
            match id {
                Some(id) => reply(
                    stream,
                    "200 OK",
                    "",
                    &json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string(),
                ),
                None => reply(stream, "202 Accepted", "", ""),
            }
        }
        _ => reply(stream, "404 Not Found", "", ""),
    }
}

fn fake_server(oauth: bool) -> (String, Shared) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let shared: Shared = Arc::new(Mutex::new(ServerState {
        oauth,
        base: base.clone(),
        ..ServerState::default()
    }));
    let server = shared.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let server = server.clone();
            thread::spawn(move || serve(&mut stream, &server));
        }
    });
    (base, shared)
}

/// A browser that says yes at once: it remembers the challenge for the
/// server and follows the redirect with the code.
fn fake_browser(server: Shared, opened: Arc<Mutex<Vec<String>>>) -> wawi_plugins::Opener {
    Arc::new(move |url: &str| {
        opened.lock().unwrap().push(url.to_owned());
        let query = url.split_once('?').map(|(_, q)| q).unwrap_or("");
        let params: BTreeMap<String, String> = query
            .split('&')
            .filter_map(|p| p.split_once('='))
            .map(|(k, v)| (oauth::decode(k), oauth::decode(v)))
            .collect();
        let base = {
            let mut state = server.lock().unwrap();
            state.challenge = params.get("code_challenge").cloned();
            state.base.clone()
        };
        let redirect = params["redirect_uri"].clone();
        let state = params["state"].clone();
        thread::spawn(move || {
            let target = format!(
                "/callback?code=code-1&state={}&iss={}",
                oauth::encode(&state),
                oauth::encode(&base)
            );
            let port: u16 = redirect
                .trim_start_matches("http://127.0.0.1:")
                .trim_end_matches("/callback")
                .parse()
                .unwrap();
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(s, "GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
            let mut page = String::new();
            let _ = s.read_to_string(&mut page);
            assert!(page.contains("Listo, ya podés volver a Wawi"));
        });
        Ok(())
    })
}

// -- the world: a host, a registry and the channel ---------------------------------

struct World {
    home: PathBuf,
    host: PluginHost,
    registry: Registry,
    rx: Receiver<Msg>,
    finished: Vec<(String, ToolOutcome)>,
    instincts: usize,
}

impl World {
    fn new(name: &str, services: Services) -> Self {
        let home = temp(name);
        let (tx, rx) = mpsc::channel();
        let clock: Rc<dyn WallClock> = FakeClock::at(Civil::date(2026, 11, 23).at(12, 0, 0), -3);
        let options = HostOptions {
            backoff_initial: Duration::from_millis(100),
            backoff_max: Duration::from_millis(400),
            ..HostOptions::default()
        };
        let host = PluginHost::start_with(
            &home,
            Some(&home.join("logs")),
            clock,
            Arc::new(Sink(tx)),
            options,
            services,
        );
        let mut registry = Registry::default();
        for tool in host.base_tools() {
            registry.register(tool);
        }
        Self {
            home,
            host,
            registry,
            rx,
            finished: Vec::new(),
            instincts: 0,
        }
    }

    fn info(&self, name: &str) -> Option<PluginInfo> {
        self.host
            .control()
            .list()
            .into_iter()
            .find(|p| p.name == name)
    }

    /// Handles what arrives until `done` holds.
    fn until(&mut self, what: &str, done: impl Fn(&World) -> bool) {
        let deadline = Instant::now() + WAIT;
        while !done(self) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "esperando: {what}");
            match self.rx.recv_timeout(left.min(Duration::from_millis(50))) {
                Ok(Msg::Plugin(event)) => {
                    for event in self.host.on_event(event, &mut self.registry) {
                        if matches!(event, Event::Instinct { .. }) {
                            self.instincts += 1;
                        }
                    }
                }
                Ok(Msg::Core(Event::ToolFinished {
                    call_id, outcome, ..
                })) => self.finished.push((call_id, outcome)),
                Ok(Msg::Core(_)) | Err(_) => {}
            }
        }
    }

    fn call(&mut self, tool: &str, id: &str, args: Value) {
        let call = ToolCall {
            id: id.into(),
            name: tool.into(),
            args_json: args.to_string(),
        };
        self.registry
            .tool_mut(tool)
            .unwrap_or_else(|| panic!("no está {tool}"))
            .start(TurnId(1), &call);
    }

    fn outcome(&mut self, id: &str) -> ToolOutcome {
        self.until(&format!("el resultado de {id}"), |w| {
            w.finished.iter().any(|(c, _)| c == id)
        });
        self.finished
            .iter()
            .find(|(c, _)| c == id)
            .map(|(_, o)| o.clone())
            .unwrap()
    }
}

fn services(
    store: Arc<MemoryStore>,
    open: wawi_plugins::Opener,
    suggestions: Vec<Suggestion>,
) -> Services {
    Services {
        store,
        open,
        catalog: None,
        suggestions,
        login_timeout: Duration::from_secs(10),
    }
}

#[test]
fn oauth_from_a_suggestion_to_revoked_and_back_out() {
    let (base, server) = fake_server(true);
    let store = Arc::new(MemoryStore::new());
    let opened = Arc::new(Mutex::new(Vec::new()));
    let suggestion = Suggestion {
        name: "falso".into(),
        title: "Falso".into(),
        url: format!("{base}/mcp"),
        description: "Un servidor de prueba.".into(),
        icon: String::new(),
        template: None,
    };
    let mut world = World::new(
        "oauth",
        services(
            store.clone(),
            fake_browser(server.clone(), opened.clone()),
            vec![suggestion],
        ),
    );
    assert_eq!(world.host.control().suggested().len(), 1);

    // «Wawi, conectá Falso».
    world.call("conectar", "c1", json!({ "plugin": "falso" }));
    assert_eq!(
        world.outcome("c1"),
        ToolOutcome::Success("Te abrí Falso en el navegador para que me des permiso.".into())
    );
    world.until("conectado", |w| {
        w.info("falso")
            .is_some_and(|p| p.state == PluginState::Conectado)
    });
    let info = world.info("falso").unwrap();
    assert_eq!((info.auth, info.tools, info.installed), ("oauth", 1, true));
    assert!(world.host.control().suggested().is_empty());
    assert!(world.host.take_tools_changed());
    assert!(world.registry.tool_mut("falso__leer").is_some());

    // What went where.
    let url = opened.lock().unwrap()[0].clone();
    assert!(url.starts_with(&format!("{base}/authorize?")), "{url}");
    assert!(url.contains("code_challenge_method=S256"));
    assert!(url.contains(&format!(
        "resource={}",
        oauth::encode(&format!("{base}/mcp"))
    )));
    assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A"));
    assert!(url.contains("scope=read%20write"));
    let registered = server.lock().unwrap().registered[0].clone();
    assert_eq!(registered["token_endpoint_auth_method"], "none");
    let toml = std::fs::read_to_string(world.home.join("plugins/falso/plugin.toml")).unwrap();
    assert!(toml.contains("auth = \"oauth\""), "{toml}");
    let grant = store.get("wawi2.plugin.falso", "oauth").unwrap().unwrap();
    assert!(grant.contains("\"rt-1\""));
    assert!(!toml.contains("at-1"), "tokens never go to files");
    assert!(
        server
            .lock()
            .unwrap()
            .bearers
            .iter()
            .any(|b| b == "Bearer at-1")
    );
    world.until("el aviso de conectado", |w| w.instincts == 1);

    // The token dies early: one 401, a refresh (rotated) and the call works.
    server.lock().unwrap().access.clear();
    world.call("falso__leer", "c2", json!({}));
    assert_eq!(world.outcome("c2"), ToolOutcome::Success("hola".into()));
    assert_eq!(server.lock().unwrap().refreshes, 1);
    let grant = store.get("wawi2.plugin.falso", "oauth").unwrap().unwrap();
    assert!(
        grant.contains("\"rt-2\"") && !grant.contains("\"rt-1\""),
        "rotated"
    );

    // Revoked on the server: the refresh fails, the grant goes, and the
    // plugin waits for the owner.
    {
        let mut state = server.lock().unwrap();
        state.access.clear();
        state.refresh.clear();
    }
    world.call("falso__leer", "c3", json!({}));
    assert!(matches!(world.outcome("c3"), ToolOutcome::Failure(_)));
    world.until("desconectado", |w| {
        w.info("falso")
            .is_some_and(|p| p.state == PluginState::Desconectado)
    });
    assert_eq!(store.get("wawi2.plugin.falso", "oauth").unwrap(), None);
    world.call("falso__leer", "c4", json!({}));
    match world.outcome("c4") {
        ToolOutcome::Failure(text) => assert!(text.contains("conectá falso"), "{text}"),
        other => panic!("{other:?}"),
    }

    // Connect again (the same registered client), then disconnect: the
    // grant is revoked on the server and gone from the store.
    world.host.control().connect("Falso");
    world.until("conectado otra vez", |w| {
        w.info("falso")
            .is_some_and(|p| p.state == PluginState::Conectado)
    });
    assert_eq!(server.lock().unwrap().registered.len(), 1, "client reused");
    world.call("conectar", "c5", json!({ "plugin": "falso" }));
    assert_eq!(
        world.outcome("c5"),
        ToolOutcome::Success("Falso ya está conectado.".into())
    );
    world.host.control().disconnect("falso");
    world.until("desconectado por el dueño", |w| {
        w.info("falso")
            .is_some_and(|p| p.state == PluginState::Desconectado)
    });
    assert!(world.registry.tool_mut("falso__leer").is_none());
    let deadline = Instant::now() + WAIT;
    while !store.keys().is_empty() || server.lock().unwrap().revocations == 0 {
        assert!(Instant::now() < deadline, "borrar el permiso");
        thread::sleep(Duration::from_millis(20));
    }
    let _ = std::fs::remove_dir_all(&world.home);
}

#[test]
fn a_key_goes_from_the_dialog_to_the_store_and_the_header() {
    let (base, server) = fake_server(false);
    let store = Arc::new(MemoryStore::new());
    let never: wawi_plugins::Opener = Arc::new(|_: &str| Err("no hay navegador".into()));
    let mut world = World::new("clave", services(store.clone(), never, Vec::new()));
    let asked: Arc<Mutex<Vec<String>>> = Arc::default();
    let answers = Arc::new(Mutex::new(vec![Some("k-123".to_owned()), None]));
    {
        let (asked, answers) = (asked.clone(), answers.clone());
        world
            .host
            .set_key_prompter(Box::new(move |prompt: KeyPrompt| {
                asked.lock().unwrap().push(prompt.title.clone());
                let given = answers.lock().unwrap().pop().flatten();
                // The dialog answers later, from the UI thread.
                thread::spawn(move || prompt.answer.answer(given));
            }));
    }
    let url = format!("{base}/mcp");

    // First the owner cancels.
    world.call("conectar", "k1", json!({ "plugin": url }));
    match world.outcome("k1") {
        ToolOutcome::Success(text) => assert!(text.contains("ventanita"), "{text}"),
        other => panic!("{other:?}"),
    }
    world.until("cancelado", |w| {
        w.info("local")
            .is_some_and(|p| p.state == PluginState::Desconectado)
    });
    assert_eq!(world.info("local").unwrap().auth, "clave");
    assert!(store.keys().is_empty());

    // Then pastes it.
    world.host.control().connect("local");
    world.until("conectado con la clave", |w| {
        w.info("local")
            .is_some_and(|p| p.state == PluginState::Conectado)
    });
    assert_eq!(*asked.lock().unwrap(), ["Clave de Local", "Clave de Local"]);
    assert_eq!(
        store
            .get("wawi2.plugin.local", "LOCAL_API_KEY")
            .unwrap()
            .as_deref(),
        Some("k-123")
    );
    assert!(
        server
            .lock()
            .unwrap()
            .bearers
            .iter()
            .any(|b| b == "Bearer k-123")
    );
    let toml = std::fs::read_to_string(world.home.join("plugins/local/plugin.toml")).unwrap();
    assert!(!toml.contains("k-123"));
    let log = std::fs::read_to_string(world.home.join("logs/plugins.log")).unwrap_or_default();
    assert!(!log.contains("k-123"), "{log}");

    // Removing it forgets the key too.
    world.host.control().remove("local");
    world.until("sacado", |w| w.info("local").is_none());
    let deadline = Instant::now() + WAIT;
    while !store.keys().is_empty() {
        assert!(Instant::now() < deadline, "borrar la clave");
        thread::sleep(Duration::from_millis(20));
    }
    assert!(!world.home.join("plugins/local").exists());
    let _ = std::fs::remove_dir_all(&world.home);
}

fn catalog_with(dir: &Path, name: &str, command: &str) {
    std::fs::create_dir_all(dir.join(name)).unwrap();
    std::fs::write(
        dir.join(name).join("plugin.toml"),
        format!("# de Wawi\nenabled = false\ndescription = \"Prueba.\"\n[server]\ncommand = \"{command}\"\n"),
    )
    .unwrap();
}

#[test]
fn wawis_own_plugins_install_turn_on_and_off() {
    let catalog = temp("catalogo");
    catalog_with(&catalog, "caracter", env!("CARGO_BIN_EXE_fake-mcp"));
    catalog_with(&catalog, "instinto", "wawi-instinto-que-no-existe");
    let store = Arc::new(MemoryStore::new());
    let never: wawi_plugins::Opener = Arc::new(|_: &str| Err("no".into()));
    let mut world = World::new(
        "propios",
        Services {
            catalog: Some(catalog.clone()),
            ..services(store, never, Vec::new())
        },
    );
    let caracter = world.info("caracter").unwrap();
    assert_eq!(
        (caracter.state, caracter.installed),
        (PluginState::Apagado, false)
    );

    // «Conectá carácter»: installed from the catalog, turned on, connected.
    world.call("conectar", "p1", json!({ "plugin": "carácter" }));
    assert!(matches!(world.outcome("p1"), ToolOutcome::Success(_)));
    world.until("caracter conectado", |w| {
        w.info("caracter")
            .is_some_and(|p| p.state == PluginState::Conectado)
    });
    assert!(world.home.join("plugins/caracter/plugin.toml").is_file());
    let toml = std::fs::read_to_string(world.home.join("plugins/caracter/plugin.toml")).unwrap();
    assert!(toml.starts_with("# de Wawi\nenabled = true\n"), "{toml}");
    assert!(world.registry.tool_mut("caracter__estado").is_some());
    let _ = world.host.take_tools_changed();

    // Off: its tools go, and the brain must hear about it.
    world.host.control().set_enabled("caracter", false);
    world.until("apagado", |w| {
        w.info("caracter")
            .is_some_and(|p| p.state == PluginState::Apagado)
    });
    assert!(world.registry.tool_mut("caracter__estado").is_none());
    assert!(world.host.take_tools_changed());

    // A missing program is explained, not retried.
    world.call("conectar", "p2", json!({ "plugin": "el Instinto" }));
    match world.outcome("p2") {
        ToolOutcome::Failure(text) => assert!(text.contains("Falta el programa"), "{text}"),
        other => panic!("{other:?}"),
    }
    let instinto = world.info("instinto").unwrap();
    assert_eq!(instinto.state, PluginState::Error);
    assert!(
        instinto
            .detail
            .unwrap()
            .contains("cargo build --release -p")
    );

    // plugins(): what there is.
    world.call("plugins", "p3", json!({}));
    match world.outcome("p3") {
        ToolOutcome::Success(text) => {
            assert!(text.contains("Apagado: Caracter"), "{text}");
            assert!(text.contains("Error: Instinto"), "{text}");
        }
        other => panic!("{other:?}"),
    }

    // Something unknown.
    world.call("conectar", "p4", json!({ "plugin": "la heladera" }));
    assert!(matches!(world.outcome("p4"), ToolOutcome::Failure(t) if t.contains("No conozco")));
    let _ = std::fs::remove_dir_all(&world.home);
    let _ = std::fs::remove_dir_all(&catalog);
}
