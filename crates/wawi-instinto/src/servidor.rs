//! `wawi-instinto mcp`: a stdio MCP server Wawi loads as a plugin. Its
//! tools are fast and local:
//!
//! - `vigilar(vigia, al_cumplirse, vence?, que?)`: validates the spec, tries
//!   it once (at most 5 s) and leaves the question in the queue.
//! - `seguimientos()`: lists them.
//! - `cancelar_seguimiento(id)`.
//! - `observar_charla(...)`: the host's turn feed, for the observer. Never
//!   offered to the brain; it only queues the turn and returns.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};
use wawi_tools::WallClock;

use crate::clock::SystemClock;
use crate::colas;
use crate::cuando::Memoria;
use crate::estado::{self, Fase};
use crate::extraer::as_text;
use crate::fuentes::{Ctx, Registro};
use crate::log::Log;
use crate::motor::Lectura;
use crate::observador::Turno;
use crate::spec::{MAX_ID, parse_pregunta};
use crate::texto::{slug, truncate};
use crate::worker::mirar;

/// How long `vigilar`'s dry run may take.
pub const DRY_RUN: Duration = Duration::from_secs(5);

pub const VIGILAR: &str = "Deja algo vigilando en segundo plano, sin gastar: cuando la condición se cumple, Wawi lo dice sola. Lo prueba una vez al toque (hasta 5 s) y dice cuánto vale ahora. El formato de vigia está en las instrucciones del plugin.";

pub fn tools() -> Value {
    json!([
        {
            "name": "vigilar",
            "description": VIGILAR,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "vigia": { "type": "object", "description": "fuente, extraer, cuando, juzgar, cada, hasta, repetir" },
                    "al_cumplirse": {
                        "type": "object",
                        "properties": { "decir": { "type": "string" }, "contexto": {} },
                        "required": ["decir"]
                    },
                    "vence": { "type": "string", "description": "AAAA-MM-DDTHH:MM, hora local" },
                    "que": { "type": "string", "description": "qué se vigila, en pocas palabras" }
                },
                "required": ["vigia", "al_cumplirse"]
            },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "seguimientos",
            "description": "Lista lo que está vigilando el instinto: id, estado, qué, próxima mirada y último valor.",
            "inputSchema": { "type": "object", "properties": { "todos": { "type": "boolean", "description": "también los terminados" } } },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "cancelar_seguimiento",
            "description": "Deja de vigilar un seguimiento, por su id.",
            "inputSchema": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] },
            "annotations": { "readOnlyHint": false }
        },
        {
            "name": crate::observador_tool(),
            "description": "El resumen de cada turno, para el observador. Solo lo usa el host.",
            "inputSchema": { "type": "object" },
            "annotations": { "readOnlyHint": true }
        }
    ])
}

/// Cancels a question: straight from the queue if it was never taken,
/// else through a marker the worker applies (it may be another process).
pub fn cancelar(home: &Path, id: &str, now: &str) -> Result<String, String> {
    if !wawi_plugins::inbox::valid_id(id) {
        return Err(format!("«{id}» no es un id"));
    }
    if let Some((stem, text)) = colas::nuevas(home).into_iter().find(|(s, _)| s == id) {
        colas::archivar(
            home,
            &stem,
            &text,
            "cancelada",
            "la cancelaron antes de empezar",
            now,
        );
        return Ok(format!("Listo, cancelé {id}."));
    }
    match estado::load(home, id) {
        Some(e) if e.fase.terminal() => Ok(format!("{id} ya estaba {}.", e.fase.nombre())),
        Some(_) => {
            colas::pedir_cancelar(home, id).map_err(|e| format!("no pude cancelarlo: {e}"))?;
            Ok(format!("Listo, cancelo {id}."))
        }
        None => Err(format!("No hay un seguimiento {id}.")),
    }
}

pub struct Servidor {
    pub home: PathBuf,
    pub fuentes: Registro,
    pub log: Log,
    pub dry_run: Duration,
    /// Where `observar_charla` turns go.
    pub observar: Box<dyn Fn(Turno) + Send + Sync>,
}

/// A tool's answer: text, and whether it is an error.
pub type Answer = (String, bool);

fn describe(valor: &Value) -> String {
    match valor {
        Value::Array(list) if list.is_empty() => "una lista vacía".into(),
        Value::Array(list) => format!(
            "una lista de {}; la primera: {}",
            list.len(),
            truncate(&as_text(&list[0]), 120)
        ),
        Value::Null => "nada (el camino no encontró el dato)".into(),
        other => format!("«{}»", truncate(&as_text(other), 160)),
    }
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
                "serverInfo": { "name": "wawi-instinto", "version": env!("CARGO_PKG_VERSION") }
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

    pub fn call(&self, name: &str, args: &Value) -> Answer {
        match name {
            "vigilar" => self.vigilar(args),
            "seguimientos" => (self.seguimientos(args), false),
            "cancelar_seguimiento" => {
                let id = args.get("id").and_then(Value::as_str).unwrap_or("").trim();
                match cancelar(&self.home, id, &SystemClock::new().local_now().iso_minute()) {
                    Ok(text) => (text, false),
                    Err(text) => (text, true),
                }
            }
            n if n == crate::observador_tool() => {
                (self.observar)(Turno::from_args(args));
                ("ok".into(), false)
            }
            other => (format!("no tengo la herramienta {other}"), true),
        }
    }

    fn vigilar(&self, args: &Value) -> Answer {
        let clock = SystemClock::new();
        match self.vigilar_con(args, &clock) {
            Ok(text) => (text, false),
            Err(text) => (format!("No la dejé vigilando: {text}"), true),
        }
    }

    pub fn vigilar_con(&self, args: &Value, clock: &dyn WallClock) -> Result<String, String> {
        let vigia = args.get("vigia").ok_or("falta vigia")?;
        let al = args.get("al_cumplirse").ok_or("falta al_cumplirse")?;
        let decir = al.get("decir").and_then(Value::as_str).unwrap_or("");
        let que = args
            .get("que")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|q| !q.is_empty());
        let now = clock.now();
        let base = slug(que.unwrap_or(decir), MAX_ID - 8);
        let id = format!("{base}-{}", short_id(now));
        let mut pregunta = json!({
            "id": id,
            "version": 1,
            "origen": "wawi",
            "creado": clock.local_now().iso_minute(),
            "vigia": vigia,
            "al_cumplirse": al,
        });
        if let Some(que) = que {
            pregunta["que"] = json!(que);
        }
        if let Some(vence) = args.get("vence").filter(|v| !v.is_null()) {
            pregunta["vence"] = vence.clone();
        }
        let parsed = parse_pregunta(&pregunta)?;
        let ctx = Ctx {
            home: &self.home,
            clock,
            timeout: self.dry_run,
        };
        self.fuentes.validar(&parsed.vigia.fuente, &ctx)?;
        if let Some(limite) = crate::motor::limite(&parsed, clock)
            && limite <= now
        {
            return Err("vence o hasta ya pasó".into());
        }
        if parsed.vigia.fuente.tipo == "reloj"
            && !self.fuentes.repite(&parsed.vigia.fuente)
            && self.fuentes.proxima(&parsed.vigia.fuente, &ctx).is_none()
        {
            return Err("esa hora ya pasó".into());
        }
        // The dry run, on its own thread, bounded.
        let (tx, rx) = mpsc::channel();
        {
            let (fuentes, home, log, p) = (
                self.fuentes.clone(),
                self.home.clone(),
                self.log.clone(),
                parsed.clone(),
            );
            // The source gets the worker's own timeout: the bound here is
            // the wait below, so a slow source is "pendiente de prueba",
            // not a failure.
            let timeout = crate::worker::SOURCE_TIMEOUT;
            std::thread::spawn(move || {
                let clock = SystemClock::new();
                let ctx = Ctx {
                    home: &home,
                    clock: &clock,
                    timeout,
                };
                let _ = tx.send(mirar(&fuentes, &ctx, &p, &Memoria::default(), None, &log));
            });
        }
        let every = parsed
            .vigia
            .cada
            .map(|s| format!("cada {}", wawi_tools::clock::duration(s as i64)))
            .unwrap_or_else(|| "cada 15 minutos".into());
        let repetir = parsed.vigia.repetir || self.fuentes.repite(&parsed.vigia.fuente);
        let note = if parsed.vigia.fuente.tipo == "reloj" {
            // A clock needs no trial: say when.
            match self.fuentes.proxima(&parsed.vigia.fuente, &ctx) {
                Some(at) if !repetir => format!(
                    "Aviso {}.",
                    wawi_tools::clock::when(&clock.local(at), &clock.local_now())
                ),
                Some(at) => format!(
                    "La primera vez es {}.",
                    wawi_tools::clock::when(&clock.local(at), &clock.local_now())
                ),
                None => String::new(),
            }
        } else {
            match rx.recv_timeout(self.dry_run) {
                Err(_) => {
                    self.log.line(&format!(
                        "vigilar {id}: la prueba tardó más de 5 s; queda pendiente de prueba"
                    ));
                    format!(
                        "No la pude probar en {} s: la dejé igual, pendiente de prueba.",
                        self.dry_run.as_secs()
                    )
                }
                Ok((Lectura::Error(e), _)) => return Err(format!("la probé y falló: {e}")),
                Ok((Lectura::Cumple { valor, .. }, _)) if !repetir => {
                    return Err(format!(
                        "ya se cumple ahora (vale {}). No hace falta vigilarlo: decíselo al dueño.",
                        describe(&valor)
                    ));
                }
                Ok((Lectura::Cumple { valor, .. }, _)) => {
                    format!(
                        "Ahora vale {} y ya se cumple; como repite, aviso cada vez.",
                        describe(&valor)
                    )
                }
                Ok((Lectura::NoCumple { .. }, valor)) => {
                    let valor = valor.unwrap_or(Value::Null);
                    let mut text =
                        format!("Ahora vale {}: todavía no se cumple.", describe(&valor));
                    if valor.is_null() && parsed.vigia.fuente.tipo != "reloj" {
                        text.push_str(
                            " Ojo: si el camino de extraer está mal, nunca se va a cumplir.",
                        );
                    }
                    if parsed.vigia.juzgar.is_some() {
                        text.push_str(" El juez no se prueba ahora: mira solo lo nuevo.");
                    }
                    text
                }
            }
        };
        colas::dejar_pregunta(&self.home, &pregunta)?;
        self.log.line(&format!("vigilar: dejé {id} en la cola"));
        let until = parsed
            .vence
            .as_deref()
            .or(parsed.vigia.hasta.as_deref())
            .map(|v| format!(" hasta {}", v.replace('T', " ")))
            .unwrap_or_default();
        if parsed.vigia.fuente.tipo == "reloj" {
            return Ok(format!("Listo, queda anotado ({id}). {note}")
                .trim_end()
                .to_owned());
        }
        Ok(format!(
            "Listo, queda vigilando ({id}), {every}{until}. {note}"
        ))
    }

    fn seguimientos(&self, args: &Value) -> String {
        let todos = args.get("todos").and_then(Value::as_bool).unwrap_or(false);
        let clock = SystemClock::new();
        let mut lines = Vec::new();
        for (id, text) in colas::nuevas(&self.home) {
            let que = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|q| parse_pregunta(&q).ok())
                .map(|p| p.que().to_owned())
                .unwrap_or_default();
            lines.push(format!("- {id} (en cola): {que}"));
        }
        for e in estado::load_all(&self.home) {
            if e.fase.terminal() && !todos {
                continue;
            }
            let que = parse_pregunta(&e.pregunta)
                .map(|p| p.que().to_owned())
                .unwrap_or_default();
            let mut line = format!("- {} ({}): {que}", e.id, e.fase.nombre());
            if matches!(e.fase, Fase::Pendiente | Fase::Activa) {
                line.push_str(&format!(
                    ". Próxima mirada: {}",
                    wawi_tools::clock::when(&clock.local(e.proxima), &clock.local_now())
                ));
            }
            if let Some(u) = &e.memoria.ultimo {
                line.push_str(&format!(". Último valor: {}", describe(u)));
            }
            if let Some(err) = &e.ultimo_error {
                line.push_str(&format!(". Último error: {err}"));
            }
            if let Some(m) = &e.motivo {
                line.push_str(&format!(". {m}"));
            }
            lines.push(line);
        }
        if lines.is_empty() {
            "No hay seguimientos.".into()
        } else {
            lines.join("\n")
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

/// Six base-36 digits from the time, so two `vigilar` calls with the same
/// words get different ids.
fn short_id(now: i64) -> String {
    let mut n = (now.rem_euclid(36_i64.pow(6))) as u64;
    let mut out = Vec::new();
    for _ in 0..6 {
        let d = (n % 36) as u8;
        out.push(if d < 10 { b'0' + d } else { b'a' + d - 10 });
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}
