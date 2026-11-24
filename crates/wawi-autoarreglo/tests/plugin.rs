//! The plugin as Wawi sees it: the manifest, the MCP server over stdio,
//! the binary, the real CLI plumbing with a fake `claude` script, and the
//! nightly review with a fake judge.

mod comun;

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use comun::*;
use serde_json::{Value, json};
use wawi_autoarreglo::config::Config;
use wawi_autoarreglo::diario;
use wawi_autoarreglo::log::Log;
use wawi_autoarreglo::motor::{ClaudeCli, Motor, Rol, Tarea};
use wawi_autoarreglo::revision;
use wawi_autoarreglo::servidor::{Servidor, tools};
use wawi_autoarreglo::worker::Worker;
use wawi_plugins::manifest::{self, Kind as ToolKind};

fn plugin_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/autoarreglo")
}

#[test]
fn the_manifest_gives_each_tool_its_kind() {
    let m = manifest::load(&plugin_dir()).unwrap();
    assert_eq!(m.name, "autoarreglo");
    let rule = |name: &str| {
        m.tools
            .get(name)
            .cloned()
            .unwrap_or_else(|| panic!("{name}"))
    };
    for read in ["reportar_problema", "arreglos", "contar_arreglo"] {
        assert_eq!(rule(read).kind, Some(ToolKind::Read), "{read}");
    }
    for (act, confirm) in [
        ("aplicar_arreglo", true),
        ("descartar_arreglo", false),
        ("revertir_arreglo", true),
        ("compilar", true),
    ] {
        let r = rule(act);
        assert_eq!(r.kind, Some(ToolKind::Act), "{act}");
        assert_eq!(r.confirm, Some(confirm), "{act}");
    }
    let served: Vec<String> = tools()
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    for name in m.tools.keys() {
        assert!(served.contains(name), "{name} is served");
    }
    assert!(
        std::fs::read_to_string(plugin_dir().join("instrucciones.md"))
            .unwrap()
            .contains("Nunca apliques")
    );
}

#[test]
fn the_binary_serves_mcp_and_writes_a_report_to_the_queue() {
    let home = temp("bin");
    let mut child = Command::new(env!("CARGO_BIN_EXE_wawi-autoarreglo"))
        .arg("mcp")
        .env("WAWI2_HOME", &home)
        .env("WAWI2_CLAUDE_BIN", "/nonexistent/claude")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut ask = |msg: Value| -> Value {
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    let init = ask(
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } }),
    );
    assert_eq!(init["result"]["serverInfo"]["name"], "wawi-autoarreglo");
    let list = ask(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 7);
    let call = ask(json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": { "name": "reportar_problema", "arguments": { "que_paso": "no me entendiste" } } }));
    let text = call["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.starts_with("Anotado") && text.contains("apagado"),
        "{text}"
    );
    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success());
    // The report is in the queue, or already folded in by the worker.
    let queued = std::fs::read_dir(home.join("cola/incidentes/new"))
        .map(|d| d.count())
        .unwrap_or(0)
        + std::fs::read_dir(home.join("cola/incidentes/cur"))
            .map(|d| d.count())
            .unwrap_or(0);
    assert_eq!(queued, 1);
    let help = Command::new(env!("CARGO_BIN_EXE_wawi-autoarreglo"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&help.stdout).contains("intentar"));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_cli_gets_the_flags_and_the_brief_on_stdin() {
    let dir = temp("cli");
    let script = dir.join("claude");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {0}/args.txt\ncat > {0}/stdin.txt\npwd > {0}/pwd.txt\necho '{{\"is_error\":false,\"structured_output\":{{\"aprobado\":true,\"motivos\":[\"bien\"]}},\"total_cost_usd\":0.42}}'\n",
            dir.display()
        ),
    )
    .unwrap();
    Command::new("chmod")
        .arg("+x")
        .arg(&script)
        .status()
        .unwrap();
    let cli = ClaudeCli {
        programa: script.clone(),
        nice: 5,
        home: None,
    };
    let schema = json!({ "type": "object" });
    let work = dir.join("worktree");
    std::fs::create_dir_all(&work).unwrap();
    let out = cli
        .correr(&Tarea {
            rol: Rol::Revisor,
            dir: &work,
            modelo: "claude-opus-5-5",
            presupuesto_usd: 1.0,
            timeout: Duration::from_secs(20),
            sistema: "reglas",
            mensaje: "el diff, privado",
            esquema: &schema,
            env: vec![("CARGO_NET_OFFLINE".into(), "true".into())],
        })
        .unwrap();
    assert_eq!(out.valor["aprobado"], true);
    assert_eq!(out.costo_usd, 0.42);
    let args = std::fs::read_to_string(dir.join("args.txt")).unwrap();
    for flag in [
        "-p",
        "--model",
        "claude-opus-5-5",
        "--setting-sources=",
        "--strict-mcp-config",
        "Read,Grep,Glob",
        "1.00",
    ] {
        assert!(args.lines().any(|l| l == flag), "{flag} in {args}");
    }
    assert!(!args.contains("privado"));
    assert_eq!(
        std::fs::read_to_string(dir.join("stdin.txt")).unwrap(),
        "el diff, privado"
    );
    assert!(
        std::fs::read_to_string(dir.join("pwd.txt"))
            .unwrap()
            .trim()
            .ends_with("worktree")
    );

    // A CLI that hangs is killed at its timeout, and that is a failure.
    std::fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
    let err = cli
        .correr(&Tarea {
            rol: Rol::Autor,
            dir: &work,
            modelo: "claude-opus-5-5",
            presupuesto_usd: 1.0,
            timeout: Duration::from_millis(500),
            sistema: "",
            mensaje: "",
            esquema: &schema,
            env: Vec::new(),
        })
        .unwrap_err();
    assert!(err.motivo.contains("tiempo"), "{}", err.motivo);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_nightly_review_files_at_most_two_checked_problems() {
    let home = temp("revision");
    std::fs::create_dir_all(home.join("logs")).unwrap();
    std::fs::write(
        home.join("logs/session.log"),
        "2026-11-21 01:00:00.000 event brain_failed turn 3\n2026-11-21 01:10:00.000 event brain_failed turn 8\n2026-11-21 01:11:00.000 state Idle -> Thinking\n",
    )
    .unwrap();
    std::fs::write(
        home.join("logs/turns.log"),
        "x felt_ms=1800 via=\"cerebro\" heard=\"privado\"\n",
    )
    .unwrap();
    let clock = clock();
    let config = Config {
        activo: true,
        ..Config::default()
    };
    let mut motor = FakeMotor::fixing(Box::new(|_: &Path| {}));
    motor.cost = 0.05;
    motor.judge = json!({ "problemas": [
        { "grupo": 1, "titulo": "El cerebro falla seguido", "por_que": "dos veces anoche" },
        { "grupo": 9, "titulo": "Inventado", "por_que": "no existe" }
    ]});
    let n = revision::revisar(&home, &config, &motor, &*clock, &Log::none());
    assert_eq!(n, 1);
    let calls = motor.calls.borrow();
    assert_eq!(calls[0].0, Rol::Juez);
    assert_eq!(calls[0].1, "claude-sonnet-5-5");
    assert!(!calls[0].3.contains("privado"), "never the turn texts");
    drop(calls);
    let w = Worker {
        home: home.clone(),
        clock: clock.clone(),
        motor: Box::new(FakeMotor::fixing(Box::new(|_: &Path| {}))),
        comandos: Box::new(FakeComandos::green()),
        log: Log::none(),
        last_work: None,
    };
    assert_eq!(w.absorber(), 1);
    let p = &diario::load_all(&home)[0];
    assert_eq!(p.tipo, "revision");
    // Once a day: the next sweep starts where this one ended.
    assert!(!revision::toca(
        &config,
        &wawi_autoarreglo::intento::EstadoServicio::load(&home),
        &*clock
    ));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn reports_count_up_and_the_server_refuses_unknown_tools() {
    let home = temp("srv");
    let clock = clock();
    let server = Servidor::new(&home, Log::none());
    let w = Worker {
        home: home.clone(),
        clock: clock.clone(),
        motor: Box::new(FakeMotor::fixing(Box::new(|_: &Path| {}))),
        comandos: Box::new(FakeComandos::green()),
        log: Log::none(),
        last_work: None,
    };
    let (a, _) = server.call_with(
        "reportar_problema",
        &json!({ "que_paso": "tardaste un montón" }),
        &*clock,
    );
    assert!(a.starts_with("Anotado."));
    w.absorber();
    clock.advance(5);
    let (b, _) = server.call_with(
        "reportar_problema",
        &json!({ "que_paso": "tardaste un montón" }),
        &*clock,
    );
    assert!(b.contains("ya van 2 veces"), "{b}");
    let (e, err) = server.call_with("reportar_problema", &json!({}), &*clock);
    assert!(err, "{e}");
    let (_, err) = server.call_with("borrar_todo", &json!({}), &*clock);
    assert!(err);
    let (t, err) = server.call_with("aplicar_arreglo", &json!({ "id": "x" }), &*clock);
    assert!(err && t.contains("repo"), "{t}");
    let _ = std::fs::remove_dir_all(&home);
}
