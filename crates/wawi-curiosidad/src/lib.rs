//! The «curiosidad» plugin: now and then, because she feels like it, Wawi
//! looks at the screen and says one short, playful, curious line about it.
//! The owner can also ask («¿qué te parece esto?»).
//!
//! - `mirar_pantalla` (read): a downscaled JPEG of the main display goes to
//!   ONE headless vision call inside this plugin ([`vision`]), and only its
//!   TEXT goes back to the brain: what is on screen, a few plain facts and,
//!   for a spontaneous look, a suggested playful line. The image never
//!   enters Wawi's brain session, its history or its transcript, and the
//!   file is deleted at once.
//! - `momento` (not offered to the brain): the host's `[[watch]]` asks it
//!   every 20 minutes. It says yes rarely and politely ([`momento`]); a yes
//!   becomes an instinct candidate, so the look is always an initiative
//!   turn through the core's gate, never on the owner's turn path.
//! - `no_mirar` and `mirar_de_nuevo`: the owner's switch.
//! - `observar_charla`: only the time of the owner's last turn is kept, so
//!   she never looks right after he spoke. The text is dropped unread.

pub mod log;
pub mod mac;
pub mod momento;
pub mod vision;

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Value, json};
use wawi_tools::{Civil, WallClock};

use crate::log::Log;
use crate::momento::{Dado, Estado, Reglas, Senales};

/// `$WAWI2_HOME`, else `~/.wawi2`.
pub fn home() -> PathBuf {
    if let Some(home) = std::env::var_os("WAWI2_HOME").filter(|h| !h.is_empty()) {
        return PathBuf::from(home);
    }
    let user = std::env::var_os("HOME").unwrap_or_else(|| ".".into());
    PathBuf::from(user).join(".wawi2")
}

pub const ESTADO: &str = "curiosidad/estado.json";
/// The long side of the image the vision call gets, and its JPEG quality:
/// about 900 image tokens for a 16:10 screen. 768 was barely faster
/// (2.3 s against 2.3 to 2.6 s) and 1280 slower (2.8 s); 1024 keeps small
/// text readable.
pub const MAX_EDGE: u32 = 1024;
pub const QUALITY: u32 = 60;

pub const PAUSADA: &str = "El dueño te pidió que no mires la pantalla. Si quiere que mires de nuevo, que te lo diga («ya podés mirar la pantalla»).";
pub const SIN_PERMISO: &str = "No tengo permiso de Grabación de pantalla. Se activa en Ajustes del Sistema, Privacidad y seguridad, Grabación de pantalla, para la app desde la que corre Wawi (por ejemplo, la Terminal), y después hay que reabrir Wawi.";

pub fn tools() -> Value {
    json!([
        {
            "name": "mirar_pantalla",
            "description": "Mira la pantalla principal del dueño ahora y te cuenta qué hay, en texto. Usala cuando te pida tu opinión de lo que está viendo («¿qué te parece esto?», «mirá esto») o cuando una tarea tuya lo diga.",
            "inputSchema": { "type": "object", "properties": { "espontanea": { "type": "boolean", "description": "true si miraste por tu cuenta, por curiosidad; te sugiere un comentario" } } },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "momento",
            "description": "Si es buen momento para mirar la pantalla por curiosidad. Solo lo usa el watch del host.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "no_mirar",
            "description": "El dueño no quiere que mires la pantalla. Sin horas, hasta que diga que podés de nuevo.",
            "inputSchema": { "type": "object", "properties": { "horas": { "type": "number", "description": "por cuántas horas; vacío = hasta nuevo aviso" } } },
            "annotations": { "readOnlyHint": false }
        },
        {
            "name": "mirar_de_nuevo",
            "description": "El dueño te deja mirar la pantalla otra vez.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": false }
        },
        {
            "name": wawi_plugins::manifest::FEED_TOOL,
            "description": "El resumen de cada turno; solo se usa la hora. Lo usa el host.",
            "inputSchema": { "type": "object" },
            "annotations": { "readOnlyHint": true }
        }
    ])
}

/// Standard base64.
pub fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub type Captura = Box<dyn Fn() -> Result<Vec<u8>, String> + Send + Sync>;
pub type Sondas = Box<dyn Fn() -> Senales + Send + Sync>;
/// Looks at a JPEG (base64) and answers with text for the brain; the flag
/// says whether it was a spontaneous look.
pub type Mirada = Box<dyn Fn(&str, bool) -> Result<String, String> + Send + Sync>;

pub struct Servidor {
    pub home: PathBuf,
    pub reglas: Reglas,
    pub clock: Box<dyn WallClock + Send + Sync>,
    pub log: Log,
    /// Takes the screenshot (a JPEG). Tests use a fake.
    pub captura: Captura,
    /// Everything `momento` asks the Mac.
    pub sondas: Sondas,
    /// The quick subset `mirar_pantalla` asks.
    pub sondas_rapidas: Sondas,
    /// The vision call. Tests use a fake.
    pub mirada: Mirada,
    dado: Mutex<Dado>,
    /// When the owner's last turn ended (Unix seconds). Starts at launch:
    /// no look in the first minutes.
    ultimo_turno: Mutex<i64>,
    estado: Mutex<()>,
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

impl Servidor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        home: &Path,
        clock: Box<dyn WallClock + Send + Sync>,
        log: Log,
        captura: Captura,
        sondas: Sondas,
        sondas_rapidas: Sondas,
        mirada: Mirada,
        seed: u64,
    ) -> Self {
        let now = clock.now();
        Self {
            home: home.to_path_buf(),
            reglas: Reglas::default(),
            clock,
            log,
            captura,
            sondas,
            sondas_rapidas,
            mirada,
            dado: Mutex::new(Dado::new(seed)),
            ultimo_turno: Mutex::new(now),
            estado: Mutex::new(()),
        }
    }

    /// The real Mac: screencapture and sips, the probes in [`mac`], and a
    /// vision call with a spare process kept ready.
    pub fn real(home: &Path, clock: Box<dyn WallClock + Send + Sync>, log: Log) -> Self {
        let tmp = std::env::temp_dir();
        // An empty folder: no project file near the model.
        let vista = std::sync::Arc::new(vision::Vista::new(&home.join("curiosidad/claude")));
        vista.mantener();
        let seed = clock.now() as u64 ^ (u64::from(std::process::id()) << 32);
        Self::new(
            home,
            clock,
            log,
            Box::new(move || mac::capturar(&tmp, MAX_EDGE, QUALITY)),
            Box::new(mac::senales),
            Box::new(mac::senales_para_mirar),
            Box::new(move |jpeg, espontanea| vista.ver(jpeg, espontanea)),
            seed,
        )
    }

    fn con_estado<R>(&self, f: impl FnOnce(&mut Estado) -> R) -> R {
        let _guard = self.estado.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.home.join(ESTADO);
        let mut estado: Estado = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let before = estado.clone();
        let out = f(&mut estado);
        if estado != before
            && let Ok(text) = serde_json::to_string_pretty(&estado)
            && let Err(e) = write_atomic(&path, &text)
        {
            self.log.line(&format!("no pude guardar el estado: {e}"));
        }
        out
    }

    pub fn handle(&self, message: &Value) -> Option<Value> {
        let id = message.get("id")?.clone();
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => json!({
                "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or(wawi_plugins::mcp::PROTOCOL_VERSION),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "wawi-curiosidad", "version": env!("CARGO_PKG_VERSION") }
            }),
            "ping" => json!({}),
            "tools/list" => json!({ "tools": tools() }),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                self.call(name, &args)
            }
            other => {
                return Some(json!({ "jsonrpc": "2.0", "id": id,
                    "error": { "code": -32601, "message": format!("no sé hacer {other}") } }));
            }
        };
        Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }

    fn text(text: &str, is_error: bool) -> Value {
        json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
    }

    /// A `tools/call` result.
    pub fn call(&self, name: &str, args: &Value) -> Value {
        let now = self.clock.now();
        match name {
            "mirar_pantalla" => {
                let espontanea = args.get("espontanea").and_then(Value::as_bool) == Some(true);
                self.mirar(now, espontanea)
            }
            "momento" => Self::text(&self.momento(now), false),
            "no_mirar" => {
                let hours = args
                    .get("horas")
                    .and_then(Value::as_f64)
                    .filter(|h| *h > 0.0);
                let until = hours.map_or(i64::MAX, |h| now + (h * 3600.0) as i64);
                self.con_estado(|e| e.pausa_hasta = Some(until));
                self.log.line("pausada por el dueño");
                let text = match hours {
                    Some(h) => format!("Listo, no miro la pantalla por {h} horas."),
                    None => "Listo, no miro más la pantalla hasta que me digas.".into(),
                };
                Self::text(&text, false)
            }
            "mirar_de_nuevo" => {
                self.con_estado(|e| e.pausa_hasta = None);
                self.log.line("el dueño la dejó mirar de nuevo");
                Self::text("Listo, ya puedo mirar la pantalla de nuevo.", false)
            }
            n if n == wawi_plugins::manifest::FEED_TOOL => {
                // Only the time matters; the words are not even read.
                let initiative = args.get("iniciativa").and_then(Value::as_bool) == Some(true);
                if !initiative && let Ok(mut last) = self.ultimo_turno.lock() {
                    *last = now;
                }
                Self::text("ok", false)
            }
            other => Self::text(&format!("no tengo la herramienta {other}"), true),
        }
    }

    fn mirar(&self, now: i64, espontanea: bool) -> Value {
        if self.con_estado(|e| e.pausada(now)) {
            self.log.line("mirar: pausada");
            return Self::text(PAUSADA, true);
        }
        let senales = (self.sondas_rapidas)();
        if !senales.permiso {
            self.log.line("mirar: sin permiso de Grabación de pantalla");
            return Self::text(SIN_PERMISO, true);
        }
        if let Some(why) = senales.impide_mirar() {
            self.log.line(&format!("mirar: {why}"));
            return Self::text(&format!("No puedo mirar: {why}."), true);
        }
        let started = std::time::Instant::now();
        // The bytes live only in this function: never on disk past the
        // capture, never in a log, never sent to the brain.
        let looked = (self.captura)().and_then(|jpeg| {
            let kb = jpeg.len() / 1024;
            (self.mirada)(&base64(&jpeg), espontanea).map(|text| (text, kb))
        });
        match looked {
            Ok((text, kb)) => {
                self.log.line(&format!(
                    "mirar: {kb} KB, {} ms{}",
                    started.elapsed().as_millis(),
                    if espontanea { ", espontánea" } else { "" }
                ));
                Self::text(&text, false)
            }
            Err(why) => {
                self.log.line(&format!("mirar: falló ({why})"));
                Self::text(&format!("No pude mirar la pantalla: {why}."), true)
            }
        }
    }

    /// A short line when it is a good moment for a look, else nothing.
    pub fn momento(&self, now: i64) -> String {
        let local = self.clock.local(now);
        let midnight = self
            .clock
            .epoch_of(Civil::date(local.year, local.month, local.day))
            .unwrap_or(now - i64::from(local.hour) * 3600);
        let minute = u16::from(local.hour) * 60 + u16::from(local.minute);
        let last = self.ultimo_turno.lock().map(|t| *t).unwrap_or(now);
        let dice = self.dado.lock().map(|mut d| d.tirar()).unwrap_or(1.0);
        let decided = self.con_estado(|e| {
            momento::decidir(
                &self.reglas,
                e,
                now,
                minute,
                midnight,
                last,
                dice,
                &self.sondas,
            )
        });
        match decided {
            Ok(()) => {
                self.log.line("momento: sí");
                format!(
                    "Buen momento para mirar ({:02}:{:02}).",
                    local.hour, local.minute
                )
            }
            Err(why) => {
                self.log.line(&format!("momento: no ({why})"));
                String::new()
            }
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
