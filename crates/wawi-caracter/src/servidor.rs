//! `wawi-caracter mcp`: the stdio MCP server Wawi loads as a plugin. Its
//! tools are fast and local:
//!
//! - `trato()`: how Wawi talks to the owner, for «¿Cómo me tratás?».
//! - `quitar_regla(regla)`: the owner takes a rule out.
//! - `olvidar_trato()`: starts over.
//! - `observar_charla(...)`: the host's turn feed. Never offered to the
//!   brain; it only queues the turn for the learner and returns.

use std::io::{BufRead, Write};
use std::sync::Arc;

use serde_json::{Value, json};
use wawi_tools::WallClock;

use crate::aprender::Turno;
use crate::libro::Libro;

pub fn tools() -> Value {
    json!([
        {
            "name": "trato",
            "description": "Cómo le hablás al dueño: las reglas de estilo que aprendiste de él. Usala si te pregunta cómo lo tratás o qué aprendiste de él.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "quitar_regla",
            "description": "Saca una regla del trato porque el dueño lo pidió. Pasá la regla tal cual la dice trato.",
            "inputSchema": { "type": "object", "properties": { "regla": { "type": "string" } }, "required": ["regla"] },
            "annotations": { "readOnlyHint": false }
        },
        {
            "name": "olvidar_trato",
            "description": "Borra todo el trato aprendido y empieza de cero. Solo si el dueño lo pide.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": false }
        },
        {
            "name": wawi_plugins::manifest::FEED_TOOL,
            "description": "El resumen de cada turno, para aprender el trato. Solo lo usa el host.",
            "inputSchema": { "type": "object" },
            "annotations": { "readOnlyHint": true }
        }
    ])
}

pub struct Servidor {
    pub libro: Arc<Libro>,
    pub clock: Box<dyn WallClock + Send + Sync>,
    /// Where `observar_charla` turns go.
    pub observar: Box<dyn Fn(Turno) + Send + Sync>,
}

impl Servidor {
    pub fn handle(&self, message: &Value) -> Option<Value> {
        let id = message.get("id")?.clone();
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => json!({
                "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or(wawi_plugins::mcp::PROTOCOL_VERSION),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "wawi-caracter", "version": env!("CARGO_PKG_VERSION") }
            }),
            "ping" => json!({}),
            "tools/list" => json!({ "tools": tools() }),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                let (text, is_error) = self.call(name, &args);
                json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
            }
            other => {
                return Some(json!({ "jsonrpc": "2.0", "id": id,
                    "error": { "code": -32601, "message": format!("no sé hacer {other}") } }));
            }
        };
        Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }

    pub fn call(&self, name: &str, args: &Value) -> (String, bool) {
        match name {
            "trato" => (self.libro.describir(), false),
            "quitar_regla" => {
                let regla = args.get("regla").and_then(Value::as_str).unwrap_or("");
                match self
                    .libro
                    .quitar(regla, &self.clock.local_now().iso_minute())
                {
                    Ok(text) => (text, false),
                    Err(text) => (text, true),
                }
            }
            "olvidar_trato" => (self.libro.olvidar_todo(), false),
            n if n == wawi_plugins::manifest::FEED_TOOL => {
                (self.observar)(Turno::from_args(args));
                ("ok".into(), false)
            }
            other => (format!("no tengo la herramienta {other}"), true),
        }
    }

    /// Serves JSON-RPC lines until stdin closes.
    pub fn serve(&self, input: impl BufRead, mut output: impl Write) {
        for line in input.lines() {
            let Ok(line) = line else { break };
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Some(reply) = self.handle(&message)
                && writeln!(output, "{reply}")
                    .and_then(|()| output.flush())
                    .is_err()
            {
                break;
            }
        }
    }
}
