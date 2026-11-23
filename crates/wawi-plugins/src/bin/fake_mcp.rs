//! A tiny MCP server over stdio, for the tests only. Its tools:
//!
//! - `estado` (readOnlyHint): "todo bien" if `FAKE_TOKEN` reached it, else
//!   "sin token". It also writes the token to stderr, so the tests can check
//!   the log blanks it out.
//! - `tareas {estado}`: "Lunes" gives a task list, anything else `[]`.
//! - `crear {titulo}`, `borrar`: no hint (act by default).
//! - `lento`: answers after 3 s, on its own thread.
//! - `romper`: exits on the spot, like a crash.
//! - `avisar`: answers, then pushes `notifications/wawi/instinto`.
//! - `observar_charla`: the turn feed; it only records it.
//!
//! `tools/list` comes in two pages. With `FAKE_MCP_LOG` set, every call is
//! appended there as `call NAME ARGS`.

use std::fs::OpenOptions;
use std::io::{self, BufRead, Write};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

fn out(value: &Value) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{value}");
    let _ = stdout.flush();
}

fn log(entry: &str) {
    if let Some(path) = std::env::var_os("FAKE_MCP_LOG")
        && let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path)
    {
        let _ = writeln!(file, "{entry}");
    }
}

fn text(id: &Value, text: &str, is_error: bool) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": { "content": [{ "type": "text", "text": text }], "isError": is_error } })
}

fn tool(name: &str, read_only: Option<bool>) -> Value {
    let mut tool = json!({
        "name": name,
        "description": format!("herramienta {name} de prueba"),
        "inputSchema": { "type": "object" },
    });
    if let Some(read_only) = read_only {
        tool["annotations"] = json!({ "readOnlyHint": read_only });
    }
    tool
}

fn main() {
    eprintln!(
        "fake-mcp arrancó con token {}",
        std::env::var("FAKE_TOKEN").unwrap_or_default()
    );
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = message.get("id").cloned() else {
            continue;
        };
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        match message["method"].as_str().unwrap_or("") {
            "initialize" => out(&json!({ "jsonrpc": "2.0", "id": id, "result": {
                "protocolVersion": "2025-06-18",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "fake-mcp", "version": "0" },
            }})),
            "tools/list" => {
                let page = if params.get("cursor").is_some() {
                    json!({ "tools": [
                        tool("lento", Some(true)),
                        tool("romper", Some(true)),
                        tool("avisar", Some(true)),
                        tool("observar_charla", None),
                    ]})
                } else {
                    json!({ "tools": [
                        tool("estado", Some(true)),
                        tool("tareas", None),
                        tool("crear", None),
                        tool("borrar", None),
                    ], "nextCursor": "2" })
                };
                out(&json!({ "jsonrpc": "2.0", "id": id, "result": page }));
            }
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or("").to_owned();
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                log(&format!("call {name} {args}"));
                match name.as_str() {
                    "estado" => {
                        let ok = std::env::var("FAKE_TOKEN").is_ok_and(|t| t == "s3cr3t-token");
                        out(&text(
                            &id,
                            if ok { "todo bien" } else { "sin token" },
                            false,
                        ));
                    }
                    "tareas" => {
                        let list = if args["estado"] == "Lunes" {
                            r#"[{"titulo":"[Casa] Planificar la semana: tarea clave de cada día"}]"#
                        } else {
                            "[]"
                        };
                        out(&text(&id, list, false));
                    }
                    "crear" => {
                        let title = args["titulo"].as_str().unwrap_or("?");
                        out(&text(&id, &format!("creada: {title}"), false));
                    }
                    "borrar" => out(&text(&id, "borrado", false)),
                    "lento" => {
                        thread::spawn(move || {
                            thread::sleep(Duration::from_secs(3));
                            out(&text(&id, "tarde", false));
                        });
                    }
                    "romper" => std::process::exit(1),
                    "avisar" => {
                        out(&text(&id, "aviso en camino", false));
                        out(
                            &json!({ "jsonrpc": "2.0", "method": "notifications/wawi/instinto", "params": {
                                "id": "build-1",
                                "decir": "Terminó el build y salió verde.",
                                "urgencia": "alta",
                                "contexto": { "rama": "main" },
                            }}),
                        );
                    }
                    "observar_charla" => out(&text(&id, "anotado", false)),
                    _ => out(&text(&id, "no existe", true)),
                }
            }
            "ping" => out(&json!({ "jsonrpc": "2.0", "id": id, "result": {} })),
            other => out(
                &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("no sé {other}") } }),
            ),
        }
    }
}
