//! The MCP tools on a temporary home: `vigilar` validates and dry-runs
//! (bounded), `seguimientos` lists, `cancelar_seguimiento` cancels, and
//! `observar_charla` only queues. Local test servers only.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use wawi_instinto::clock::SystemClock;
use wawi_instinto::fuentes::Registro;
use wawi_instinto::log::Log;
use wawi_instinto::observador::Turno;
use wawi_instinto::servidor::Servidor;
use wawi_tools::WallClock;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("instinto-srv-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn server(home: &Path, seen: Arc<Mutex<Vec<Turno>>>) -> Servidor {
    Servidor {
        home: home.to_path_buf(),
        fuentes: Registro::base(),
        log: Log::none(),
        dry_run: Duration::from_millis(800),
        observar: Box::new(move |t| seen.lock().unwrap().push(t)),
    }
}

/// Answers every connection with `body` after `delay`.
fn http(status: u16, body: &'static str, delay: Duration) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            thread::sleep(delay);
            let _ = stream.write_all(
                format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes(),
            );
        }
    });
    url
}

fn call(s: &Servidor, name: &str, args: Value) -> (String, bool) {
    let reply = s
        .handle(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": args } }))
        .unwrap();
    (
        reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned(),
        reply["result"]["isError"].as_bool().unwrap(),
    )
}

fn dolar(url: &str) -> Value {
    json!({
        "que": "el dólar",
        "vigia": { "fuente": { "http": { "url": url } }, "extraer": { "jsonpath": "$.venta" }, "cuando": { "menor": 1000 }, "cada": "30m" },
        "al_cumplirse": { "decir": "El dólar bajó de mil." }
    })
}

fn queued(home: &Path) -> Vec<String> {
    std::fs::read_dir(home.join("cola/preguntas/new"))
        .map(|d| {
            d.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn handshake_and_tool_list() {
    let home = temp("list");
    let s = server(&home, Arc::default());
    let init = s.handle(&json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } })).unwrap();
    assert_eq!(init["result"]["serverInfo"]["name"], "wawi-instinto");
    assert!(
        s.handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .is_none()
    );
    let list = s
        .handle(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }))
        .unwrap();
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "vigilar",
            "seguimientos",
            "cancelar_seguimiento",
            "observar_charla"
        ]
    );
}

#[test]
fn vigilar_dry_runs_and_says_what_it_is_worth_now() {
    let home = temp("dry");
    let s = server(&home, Arc::default());
    let (text, error) = call(
        &s,
        "vigilar",
        dolar(&http(200, r#"{"venta": 1200}"#, Duration::ZERO)),
    );
    assert!(!error, "{text}");
    assert!(
        text.contains("Ahora vale «1200»: todavía no se cumple"),
        "{text}"
    );
    assert!(text.contains("cada 30 minutos"), "{text}");
    assert_eq!(queued(&home).len(), 1);
    // Already true: not left watching.
    let (text, error) = call(
        &s,
        "vigilar",
        dolar(&http(200, r#"{"venta": 950}"#, Duration::ZERO)),
    );
    assert!(
        error && text.contains("ya se cumple ahora (vale «950»)"),
        "{text}"
    );
    // A failing dry run is rejected with the reason.
    let (text, error) = call(&s, "vigilar", dolar(&http(500, "{}", Duration::ZERO)));
    assert!(
        error && text.contains("la probé y falló: HTTP 500"),
        "{text}"
    );
    // A missing field is not true yet, but says so.
    let (text, error) = call(
        &s,
        "vigilar",
        json!({
        "vigia": { "fuente": { "http": { "url": http(200, r#"{"x": 1}"#, Duration::ZERO) } }, "extraer": { "jsonpath": "$.estado" }, "cuando": { "igual": "listo" } },
        "al_cumplirse": { "decir": "Listo." } }),
    );
    assert!(!error && text.contains("Ojo"), "{text}");
    assert_eq!(queued(&home).len(), 2);
}

#[test]
fn a_slow_dry_run_is_accepted_as_pending() {
    let home = temp("slow");
    let s = server(&home, Arc::default());
    let started = std::time::Instant::now();
    let (text, error) = call(
        &s,
        "vigilar",
        dolar(&http(200, r#"{"venta": 1}"#, Duration::from_secs(3))),
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "bounded by the dry-run timeout"
    );
    assert!(!error && text.contains("pendiente de prueba"), "{text}");
    assert_eq!(queued(&home).len(), 1);
}

#[test]
fn bad_specs_are_refused_in_spanish() {
    let home = temp("bad");
    let s = server(&home, Arc::default());
    let clock = SystemClock::new();
    let past = clock.local(clock.now() - 3600).iso_minute();
    for (vigia, needle) in [
        (json!({ "fuente": { "reloj": { "at": past } } }), "ya pasó"),
        (
            json!({ "fuente": { "http": { "url": "file:///etc/passwd" } } }),
            "solo http",
        ),
        (
            json!({ "fuente": { "reloj": { "at": "2099-01-01T00:00" } }, "cuando": { "parecido": "x" } }),
            "no conoce",
        ),
        (
            json!({ "fuente": { "mcp": { "plugin": "github", "tool": "x" } } }),
            "no hay un plugin",
        ),
        (json!({ "fuente": { "ssh": {} } }), "no conozco la fuente"),
    ] {
        let (text, error) = call(
            &s,
            "vigilar",
            json!({ "vigia": vigia, "al_cumplirse": { "decir": "x" } }),
        );
        assert!(error, "{text}");
        assert!(
            text.starts_with("No la dejé vigilando: ") && text.contains(needle),
            "{text}"
        );
    }
    assert!(queued(&home).is_empty());
}

#[test]
fn list_cancel_and_the_feed() {
    let home = temp("list-cancel");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = server(&home, seen.clone());
    let clock = SystemClock::new();
    let later = clock.local(clock.now() + 7200).iso_minute();
    let (text, error) = call(
        &s,
        "vigilar",
        json!({
        "que": "el turno", "vigia": { "fuente": { "reloj": { "at": later } } },
        "al_cumplirse": { "decir": "Ya es la hora del turno." } }),
    );
    assert!(!error, "{text}");
    assert!(
        text.starts_with("Listo, queda anotado (") && text.contains("Aviso "),
        "{text}"
    );
    let id = text
        .split('(')
        .nth(1)
        .unwrap()
        .split(')')
        .next()
        .unwrap()
        .to_owned();
    assert!(id.starts_with("el-turno-"));
    let (list, _) = call(&s, "seguimientos", json!({}));
    assert!(
        list.contains(&format!("{id} (en cola): el turno")),
        "{list}"
    );
    let (text, error) = call(&s, "cancelar_seguimiento", json!({ "id": id }));
    assert!(!error && text.contains("cancelé"), "{text}");
    assert!(queued(&home).is_empty());
    let (text, error) = call(&s, "cancelar_seguimiento", json!({ "id": "no-existe" }));
    assert!(error && text.contains("No hay"), "{text}");
    assert_eq!(
        call(&s, "seguimientos", json!({})).0,
        "No hay seguimientos."
    );
    let (ok, _) = call(
        &s,
        "observar_charla",
        json!({ "turno": 3, "dueno": "hola", "wawi": "¡Hola!", "iniciativa": false }),
    );
    assert_eq!(ok, "ok");
    assert_eq!(seen.lock().unwrap()[0].dueno, "hola");
}
