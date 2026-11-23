//! The real binary on a temporary home: `wawi-instinto mcp` answers MCP on
//! stdio and runs the worker inside; a question in the queue becomes a fact
//! that Wawi's own inbox accepts; `wawi-instinto run` refuses to start a
//! second worker. No network, no model (the claude binary is `false`).

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wawi_instinto::clock::SystemClock;
use wawi_plugins::inbox::Inbox;
use wawi_tools::WallClock;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("instinto-bin-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn command(home: &Path, mode: &str) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_wawi-instinto"));
    c.arg(mode)
        .env("WAWI2_HOME", home)
        .env("WAWI2_CLAUDE_BIN", "/usr/bin/false")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    c
}

#[test]
fn the_plugin_process_serves_mcp_and_turns_questions_into_facts() {
    let home = temp("e2e");
    // No quiet hours here, so the fact is written whatever the time.
    std::fs::create_dir_all(home.join("instinto")).unwrap();
    std::fs::write(home.join("instinto/config.toml"), "silencio = \"\"\n").unwrap();
    let mut child = command(&home, "mcp").spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut ask = |message: Value| -> Value {
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
        serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap()
    };
    let init = ask(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }));
    assert_eq!(init["result"]["serverInfo"]["name"], "wawi-instinto");
    let list = ask(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 4);
    let clock = SystemClock::new();
    let soon = clock.local(clock.now() + 3600).iso_minute();
    let reply = ask(
        json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "vigilar", "arguments": {
        "que": "la reunión", "vigia": { "fuente": { "reloj": { "at": soon } } }, "al_cumplirse": { "decir": "Arranca la reunión." } } } }),
    );
    assert_eq!(reply["result"]["isError"], false, "{reply}");

    // A question already due, dropped by another process.
    let past = clock.local(clock.now() - 60).iso_minute();
    let q = json!({ "id": "ya-es-hora", "version": 1, "origen": "prueba", "que": "la prueba",
        "vigia": { "fuente": { "reloj": { "at": past } } }, "al_cumplirse": { "decir": "Ya es la hora." } });
    std::fs::write(
        home.join("cola/preguntas/tmp/ya-es-hora.json"),
        q.to_string(),
    )
    .unwrap();
    std::fs::rename(
        home.join("cola/preguntas/tmp/ya-es-hora.json"),
        home.join("cola/preguntas/new/ya-es-hora.json"),
    )
    .unwrap();
    let fact = home.join("cola/hechos/new/ya-es-hora.json");
    let deadline = Instant::now() + Duration::from_secs(15);
    while !fact.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(fact.exists(), "the worker wrote the fact");
    // Wawi's own inbox takes it as is.
    let mut inbox = Inbox::open(&home).unwrap();
    let mut notes = Vec::new();
    let found = inbox.scan(&clock, &mut notes);
    assert_eq!(found.len(), 1, "{notes:?}");
    assert!(found[0].context.contains("Ya es la hora."));

    // A second worker for the same home refuses to start.
    let status = command(&home, "run").status().unwrap();
    assert_eq!(status.code(), Some(3));

    drop(stdin);
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        child.try_wait().unwrap().is_some(),
        "closing stdin ends the plugin"
    );
    let _ = std::fs::remove_dir_all(&home);
}
