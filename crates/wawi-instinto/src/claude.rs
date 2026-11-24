//! One headless `claude -p` call with structured output and nothing else:
//! no tools, no MCP servers, no user settings, no session kept. The text
//! goes in on stdin (never on the command line, where `ps` would show it)
//! and the answer is the CLI's `structured_output`.
//!
//! Used by the judge (`vigia.juzgar`), the observer and the bench. Never
//! on Wawi's turn path.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use wawi_brain_claude::cupo;

pub struct Pedido<'a> {
    pub modelo: &'a str,
    pub esfuerzo: Option<&'a str>,
    pub sistema: &'a str,
    pub mensaje: &'a str,
    pub esquema: &'a Value,
    pub timeout: Duration,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Respuesta {
    pub valor: Value,
    pub costo_usd: f64,
    pub ms: u128,
}

/// A model that answers with JSON. The real one is [`ClaudeCli`]; tests use
/// fakes.
pub trait Modelo: Send + Sync {
    fn preguntar(&self, pedido: &Pedido<'_>) -> Result<Respuesta, String>;
}

pub struct ClaudeCli {
    pub programa: PathBuf,
    /// The folder it runs in: empty, so no project file is near.
    pub dir: PathBuf,
    /// Wawi's home and who is calling, to honor and note the usage limit
    /// (`<home>/cupo.json`). `None`: no check (the benches).
    pub cupo: Option<(PathBuf, String)>,
}

impl ClaudeCli {
    pub fn new(dir: &Path) -> Self {
        Self {
            programa: default_program(),
            dir: dir.to_path_buf(),
            cupo: None,
        }
    }

    /// While `<home>/cupo.json` says the subscription is out of quota, no
    /// call is made (it fails at once, saying so); a call that runs into the
    /// limit writes it, so every other service waits too.
    pub fn con_cupo(mut self, home: &Path, origen: &str) -> Self {
        self.cupo = Some((home.to_path_buf(), origen.to_owned()));
        self
    }
}

/// `$WAWI2_CLAUDE_BIN`, else `~/.local/bin/claude` if it exists, else
/// `claude` from the PATH. The same rule as Wawi's brain.
pub fn default_program() -> PathBuf {
    if let Some(program) = std::env::var_os("WAWI2_CLAUDE_BIN") {
        return program.into();
    }
    if let Some(home) = std::env::var_os("HOME") {
        let local = PathBuf::from(home).join(".local/bin/claude");
        if local.is_file() {
            return local;
        }
    }
    PathBuf::from("claude")
}

/// The flags, in order. Public so a test can check the isolation.
pub fn args(pedido: &Pedido<'_>) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--output-format",
        "json",
        "--tools",
        "",
        "--setting-sources=",
        "--strict-mcp-config",
        "--disable-slash-commands",
        "--no-chrome",
        "--no-session-persistence",
        "--model",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    args.push(pedido.modelo.to_owned());
    if let Some(effort) = pedido.esfuerzo {
        args.extend(["--effort".to_owned(), effort.to_owned()]);
    }
    args.extend([
        "--json-schema".to_owned(),
        pedido.esquema.to_string(),
        "--system-prompt".to_owned(),
        pedido.sistema.to_owned(),
    ]);
    args
}

fn inherited(key: &str) -> bool {
    key == "CLAUDECODE"
        || key == "CLAUDE_PID"
        || key == "CLAUDE_EFFORT"
        || key.starts_with("CLAUDE_CODE_")
        || key.starts_with("ANTHROPIC_")
}

impl Modelo for ClaudeCli {
    fn preguntar(&self, pedido: &Pedido<'_>) -> Result<Respuesta, String> {
        if let Some((home, _)) = &self.cupo
            && let Some(held) = cupo::active(home, cupo::now())
        {
            return Err(cupo::skipped(&held, cupo::now()));
        }
        let started = Instant::now();
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("no pude crear la carpeta: {e}"))?;
        let mut command = Command::new(&self.programa);
        command
            .args(args(pedido))
            .current_dir(&self.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (key, _) in std::env::vars_os() {
            if inherited(&key.to_string_lossy()) {
                command.env_remove(&key);
            }
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("no pude arrancar claude: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(pedido.mensaje.as_bytes());
        }
        let mut stdout = child.stdout.take().ok_or("claude sin salida")?;
        let reader = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stdout.read_to_string(&mut text);
            text
        });
        let deadline = started + pedido.timeout;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("claude tardó demasiado".into());
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => return Err(format!("claude: {e}")),
            }
        }
        let text = reader.join().unwrap_or_default();
        if let Some((home, origen)) = &self.cupo
            && let Some(limit) = cupo::in_output(&text)
        {
            let held = cupo::note(home, limit, cupo::now(), origen);
            return Err(cupo::skipped(&held, cupo::now()));
        }
        let mut respuesta = parse_output(&text)?;
        respuesta.ms = started.elapsed().as_millis();
        Ok(respuesta)
    }
}

/// The CLI's `--output-format json` result: `structured_output` when it
/// worked.
pub fn parse_output(text: &str) -> Result<Respuesta, String> {
    let value: Value =
        serde_json::from_str(text.trim()).map_err(|_| "claude no devolvió JSON".to_owned())?;
    if value.get("is_error").and_then(Value::as_bool) == Some(true) {
        let why = value
            .get("result")
            .and_then(Value::as_str)
            .unwrap_or("error sin detalle");
        return Err(format!(
            "claude falló: {}",
            crate::texto::truncate(why, 200)
        ));
    }
    let valor = value
        .get("structured_output")
        .cloned()
        .filter(|v| !v.is_null())
        .or_else(|| {
            value
                .get("result")
                .and_then(Value::as_str)
                .and_then(|r| serde_json::from_str(r).ok())
        })
        .ok_or("claude no devolvió la salida estructurada")?;
    Ok(Respuesta {
        valor,
        costo_usd: value
            .get("total_cost_usd")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        ms: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn isolated_flags() {
        let schema = json!({ "type": "object" });
        let a = args(&Pedido {
            modelo: "claude-haiku-5-5",
            esfuerzo: Some("low"),
            sistema: "s",
            mensaje: "secreto del dueño",
            esquema: &schema,
            timeout: Duration::from_secs(1),
        });
        for flag in [
            "--setting-sources=",
            "--strict-mcp-config",
            "--no-session-persistence",
            "--json-schema",
        ] {
            assert!(a.contains(&flag.to_owned()), "{flag}");
        }
        let tools = a.iter().position(|x| x == "--tools").unwrap();
        assert_eq!(a[tools + 1], "");
        assert!(
            !a.iter().any(|x| x.contains("secreto")),
            "the text goes on stdin"
        );
    }

    #[test]
    fn reads_the_structured_output() {
        let r = parse_output(
            r#"{"is_error":false,"structured_output":{"si":true},"total_cost_usd":0.001}"#,
        )
        .unwrap();
        assert_eq!(r.valor, json!({ "si": true }));
        assert!(
            parse_output(r#"{"is_error":true,"result":"sin cupo"}"#)
                .unwrap_err()
                .contains("sin cupo")
        );
        assert!(parse_output("nada").is_err());
    }
}
