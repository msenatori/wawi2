//! `wawi-autoarreglo mcp`: the stdio MCP server Wawi loads as a plugin.
//! Every tool is quick and local; the slow work is the worker's.
//!
//! | tool | kind |
//! | --- | --- |
//! | `reportar_problema(que_paso)` | read: it only writes the local queue |
//! | `arreglos()` | read |
//! | `contar_arreglo(id)` | read |
//! | `aplicar_arreglo(id)` | act, asks first |
//! | `descartar_arreglo(id)` | act, no question |
//! | `revertir_arreglo(id)` | act, asks first |
//! | `compilar()` | act, asks first |

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use wawi_tools::WallClock;

use crate::clock::SystemClock;
use crate::config::{Config, seconds};
use crate::diario;
use crate::incidentes::{self, Incidente};
use crate::intento::EstadoServicio;
use crate::log::Log;
use crate::propuestas;
use crate::scrub::scrub;

pub fn tools() -> Value {
    let id = json!({ "type": "object", "properties": { "id": { "type": "string", "description": "el id del arreglo" } }, "required": ["id"] });
    json!([
        {
            "name": "reportar_problema",
            "description": "Anota un problema que el dueño te marca («eso está mal, reportalo», «no me entendiste», «tardaste un montón»). Solo lo deja en la cola local; nunca toca código. Pasá en que_paso lo que salió mal, en pocas palabras.",
            "inputSchema": { "type": "object", "properties": { "que_paso": { "type": "string", "description": "qué salió mal, en pocas palabras" } }, "required": ["que_paso"] },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "arreglos",
            "description": "Lista los arreglos que Wawi se armó: id, estado y qué arreglan.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "contar_arreglo",
            "description": "Un resumen corto de un arreglo: qué andaba mal, por qué, qué cambió y cómo se probó.",
            "inputSchema": id,
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "aplicar_arreglo",
            "description": "Aplica un arreglo como un solo commit en la rama actual del dueño, solo si su árbol está limpio.",
            "inputSchema": id,
            "annotations": { "readOnlyHint": false, "destructiveHint": false }
        },
        {
            "name": "descartar_arreglo",
            "description": "Tira un arreglo propuesto y no vuelve a intentar ese problema.",
            "inputSchema": id,
            "annotations": { "readOnlyHint": false }
        },
        {
            "name": "revertir_arreglo",
            "description": "Deshace un arreglo aplicado con un commit nuevo (nunca borra historia).",
            "inputSchema": id,
            "annotations": { "readOnlyHint": false }
        },
        {
            "name": "compilar",
            "description": "Compila a Wawi de nuevo después de aplicar o deshacer un arreglo, en segundo plano; avisa cuando termina.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": false }
        }
    ])
}

pub struct Servidor {
    pub home: PathBuf,
    pub log: Log,
    building: Arc<AtomicBool>,
}

pub type Answer = (String, bool);

fn arg<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or("").trim()
}

impl Servidor {
    pub fn new(home: &Path, log: Log) -> Self {
        Self {
            home: home.to_path_buf(),
            log,
            building: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn handle(&self, message: &Value) -> Option<Value> {
        let id = message.get("id")?.clone();
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => json!({
                "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or(wawi_plugins::mcp::PROTOCOL_VERSION),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "wawi-autoarreglo", "version": env!("CARGO_PKG_VERSION") }
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
        let clock = SystemClock::new();
        self.call_with(name, args, &clock)
    }

    pub fn call_with(&self, name: &str, args: &Value, clock: &dyn WallClock) -> Answer {
        let (config, _) = Config::load(&self.home);
        let result = match name {
            "reportar_problema" => self.reportar(arg(args, "que_paso"), &config, clock),
            "arreglos" => Ok(propuestas::lista(&self.home)),
            "contar_arreglo" => propuestas::contar(&self.home, arg(args, "id")),
            "aplicar_arreglo" => match config.repo() {
                Some(repo) => propuestas::aplicar(&self.home, &repo, arg(args, "id"), clock),
                None => Err(no_repo()),
            },
            "descartar_arreglo" => {
                propuestas::descartar(&self.home, config.repo().as_deref(), arg(args, "id"))
            }
            "revertir_arreglo" => match config.repo() {
                Some(repo) => propuestas::revertir(&self.home, &repo, arg(args, "id"), clock),
                None => Err(no_repo()),
            },
            "compilar" => self.compilar(&config),
            other => Err(format!("no tengo la herramienta {other}")),
        };
        if name != "reportar_problema" {
            self.log.line(&format!(
                "herramienta {name} {}: {}",
                arg(args, "id"),
                if result.is_ok() { "ok" } else { "no" }
            ));
        }
        match result {
            Ok(text) => (text, false),
            Err(text) => (text, true),
        }
    }

    fn reportar(
        &self,
        que_paso: &str,
        config: &Config,
        clock: &dyn WallClock,
    ) -> Result<String, String> {
        if que_paso.is_empty() {
            return Err("Decime qué salió mal y lo anoto.".into());
        }
        let text = crate::truncate(&scrub(que_paso), 500);
        let now = clock.now();
        let incidente = Incidente {
            tipo: "reportado".into(),
            clave: text.clone(),
            resumen: text.clone(),
            detalle: Value::Null,
            creado: crate::clock::stamp(clock, now),
            origen: "dueño".into(),
        };
        let seq = std::process::id();
        incidentes::dejar(&self.home, &format!("dueno-{now}-{seq}"), &incidente)
            .map_err(|e| format!("No pude anotarlo: {e}"))?;
        self.log.line("reportar_problema: anotado");
        let before = diario::load(&self.home, &diario::fingerprint("reportado", &text))
            .map_or(0, |p| p.cuenta);
        let mut reply = if before == 0 {
            "Anotado.".to_owned()
        } else {
            format!("Anotado, ya van {} veces.", before + 1)
        };
        if !config.activo {
            reply.push_str(" El autoarreglo está apagado, así que por ahora solo lo guardo.");
        } else {
            reply.push_str(
                " Lo miro cuando me toque arreglarme y, si encuentro un arreglo, te lo propongo.",
            );
        }
        Ok(reply)
    }

    fn compilar(&self, config: &Config) -> Result<String, String> {
        let repo = config.repo().ok_or_else(no_repo)?;
        let Some((program, rest)) = config.compilar.split_first() else {
            return Err("No sé cómo compilarme (compilar está vacío en config.toml).".into());
        };
        if self.building.swap(true, Ordering::SeqCst) {
            return Err("Ya me estoy compilando; te aviso cuando termine.".into());
        }
        let (program, rest) = (PathBuf::from(program), rest.to_vec());
        let (home, log, flag, nice) = (
            self.home.clone(),
            self.log.clone(),
            self.building.clone(),
            config.nice,
        );
        let timeout = Duration::from_secs(seconds("40m").unwrap_or(2400));
        std::thread::spawn(move || {
            let clock = SystemClock::new();
            let fin = crate::motor::bounded(&program, &rest, &repo, &[], None, timeout, nice);
            let (ok, why) = match &fin {
                Ok(f) if f.code == 0 && !f.timed_out => (true, String::new()),
                Ok(f) if f.timed_out => (false, "tardó demasiado".into()),
                Ok(f) => (
                    false,
                    f.stderr
                        .lines()
                        .rev()
                        .find(|l| l.contains("error"))
                        .map(|l| crate::truncate(&scrub(l), 160))
                        .unwrap_or_else(|| "falló".into()),
                ),
                Err(e) => (false, e.clone()),
            };
            let now = clock.now();
            if ok {
                let mut estado = EstadoServicio::load(&home);
                estado.compilado_ts = Some(now);
                estado.save(&home);
            }
            log.line(&format!("compilar: {}", if ok { "ok" } else { "falló" }));
            let decir = if ok {
                "Ya me compilé con los cambios. Reiniciame cuando quieras y quedan andando."
                    .to_owned()
            } else {
                format!("No me pude compilar: {why}.")
            };
            let _ = propuestas::hecho(
                &home,
                &format!("autoarreglo-compilado-{now}"),
                &decir,
                json!({ "compilado": ok }),
                &clock,
            );
            flag.store(false, Ordering::SeqCst);
        });
        Ok("Arranqué la compilación en segundo plano (puede tardar unos minutos). Te aviso cuando termine; después hay que reiniciarme.".into())
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

fn no_repo() -> String {
    "No sé dónde está mi repo: poné `repo` en autoarreglo/config.toml.".into()
}
