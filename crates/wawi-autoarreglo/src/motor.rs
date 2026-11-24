//! The engines: the headless `claude` CLI (author, reviewer, the nightly
//! judge) and the gate commands (cargo). Both are traits, so the tests use
//! a fake claude and fake commands; no test ever calls a real model.
//!
//! Every child runs in its own process group, at a lower CPU priority
//! (`nice`, so a night build never slows a turn), with a hard timeout that
//! kills the whole group. The prompt goes in on stdin, never on the
//! command line, where `ps` would show it.
//!
//! The author's flags (checked against `claude --help`, Claude Code 2.1):
//! `-p --output-format json --model claude-opus-5-5 --permission-mode
//! acceptEdits --permission-prompts none --tools Read,Edit,Write,Grep,Glob,Bash
//! --allowedTools "Bash(cargo *)" "Bash(git diff *)" "Bash(git diff)"
//! --setting-sources= --strict-mcp-config --disable-slash-commands --no-chrome
//! --no-session-persistence --max-budget-usd N --json-schema S
//! --append-system-prompt RULES`. No MCP server is configured, so
//! `--strict-mcp-config` means none at all. Reads inside the worktree need
//! no approval and `acceptEdits` accepts edits inside it; anything else
//! (a file outside it, another command) would need one, and with
//! `--permission-prompts none` it is denied instead of asked. The reviewer
//! gets only Read, Grep and Glob, and nothing pre-approved.

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::gates::Corrida;

/// Who the model is in this call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rol {
    /// Edits the worktree and runs cargo.
    Autor,
    /// Reads the worktree; never edits.
    Revisor,
    /// No tools at all: the nightly review's one call.
    Juez,
}

pub struct Tarea<'a> {
    pub rol: Rol,
    /// Where it runs: the worktree (or an empty folder for the judge).
    pub dir: &'a Path,
    pub modelo: &'a str,
    pub presupuesto_usd: f64,
    pub timeout: Duration,
    /// The rules, added to the CLI's own system prompt (author, reviewer)
    /// or as the whole system prompt (judge).
    pub sistema: &'a str,
    /// The brief, on stdin.
    pub mensaje: &'a str,
    pub esquema: &'a Value,
    /// Extra environment (CARGO_TARGET_DIR and friends).
    pub env: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Salida {
    pub valor: Value,
    pub costo_usd: f64,
    pub ms: u128,
}

/// A failed call, with what it cost anyway (a budget hit is not free).
#[derive(Clone, Debug, PartialEq)]
pub struct Falla {
    pub motivo: String,
    pub costo_usd: f64,
}

impl Falla {
    pub fn new(motivo: impl Into<String>) -> Self {
        Self {
            motivo: motivo.into(),
            costo_usd: 0.0,
        }
    }
}

pub trait Motor {
    fn correr(&self, tarea: &Tarea<'_>) -> Result<Salida, Falla>;
}

/// Runs the gate commands.
pub trait Comandos {
    fn correr(&self, dir: &Path, argv: &[String], timeout: Duration) -> Corrida;
}

/// Process groups still running, so a plugin that is closing can kill them.
static RUNNING: Mutex<Vec<i32>> = Mutex::new(Vec::new());

/// Kills every child group still running (the plugin is going away).
pub fn kill_all() {
    if let Ok(mut groups) = RUNNING.lock() {
        for pgid in groups.drain(..) {
            // SAFETY: killpg only sends a signal to a process group.
            unsafe { libc::killpg(pgid, libc::SIGKILL) };
        }
    }
}

fn forget(pgid: i32) {
    if let Ok(mut groups) = RUNNING.lock() {
        groups.retain(|g| *g != pgid);
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

/// What a bounded child gave.
pub struct Fin {
    pub code: i32,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    pub ms: u128,
}

fn reader(mut pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

/// Runs `program args` in `dir`, at `nice`, in its own group, killed with
/// the whole group at `timeout`.
pub fn bounded(
    program: &Path,
    args: &[String],
    dir: &Path,
    env: &[(String, String)],
    stdin: Option<&str>,
    timeout: Duration,
    nice: i32,
) -> Result<Fin, String> {
    let started = Instant::now();
    let mut command = if nice > 0 {
        let mut c = Command::new("nice");
        c.arg("-n").arg(nice.to_string()).arg(program);
        c
    } else {
        Command::new(program)
    };
    command
        .args(args)
        .current_dir(dir)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    for (key, _) in std::env::vars_os() {
        let key = key.to_string_lossy();
        if key == "CLAUDECODE"
            || key == "CLAUDE_PID"
            || key == "CLAUDE_EFFORT"
            || key.starts_with("CLAUDE_CODE_")
            || key.starts_with("ANTHROPIC_")
            || key.starts_with("GIT_")
        {
            command.env_remove(key.as_ref());
        }
    }
    for (k, v) in env {
        command.env(k, v);
    }
    let mut child: Child = command
        .spawn()
        .map_err(|e| format!("no pude arrancar {}: {e}", program.display()))?;
    let pgid = child.id() as i32;
    if let Ok(mut groups) = RUNNING.lock() {
        groups.push(pgid);
    }
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let _ = pipe.write_all(text.as_bytes());
    }
    let out = reader(child.stdout.take().ok_or("sin stdout")?);
    let err = reader(child.stderr.take().ok_or("sin stderr")?);
    let deadline = started + timeout;
    let (code, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (status.code().unwrap_or(-1), false),
            Ok(None) if Instant::now() >= deadline => {
                // SAFETY: killpg only sends a signal to the child's group.
                unsafe { libc::killpg(pgid, libc::SIGKILL) };
                let _ = child.wait();
                break (-1, true);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => {
                forget(pgid);
                return Err(format!("esperando el proceso: {e}"));
            }
        }
    };
    // Anything the group left behind (cargo's children) goes too.
    // SAFETY: as above.
    unsafe { libc::killpg(pgid, libc::SIGKILL) };
    forget(pgid);
    Ok(Fin {
        code,
        timed_out,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
        ms: started.elapsed().as_millis(),
    })
}

pub struct ClaudeCli {
    pub programa: PathBuf,
    pub nice: i32,
    /// Wawi's home, to note the usage limit in `<home>/cupo.json` when a
    /// call runs into it. `None`: not noted.
    pub home: Option<PathBuf>,
}

impl ClaudeCli {
    pub fn new(nice: i32) -> Self {
        Self {
            programa: default_program(),
            nice,
            home: None,
        }
    }

    pub fn con_cupo(mut self, home: &Path) -> Self {
        self.home = Some(home.to_path_buf());
        self
    }
}

const AUTHOR_TOOLS: &str = "Read,Edit,Write,Grep,Glob,Bash";
/// Pre-approved: only cargo and git diff. Reading inside the worktree
/// needs no approval and `acceptEdits` accepts edits inside it; anything
/// outside it would need one, and `--permission-prompts none` denies it.
const AUTHOR_ALLOWED: &[&str] = &["Bash(cargo *)", "Bash(git diff *)", "Bash(git diff)"];
const REVIEW_TOOLS: &str = "Read,Grep,Glob";

/// The flags, in order. Public so a test can check the isolation.
pub fn args(t: &Tarea<'_>) -> Vec<String> {
    let mut a: Vec<String> = [
        "-p",
        "--output-format",
        "json",
        "--model",
        t.modelo,
        "--setting-sources=",
        "--strict-mcp-config",
        "--disable-slash-commands",
        "--no-chrome",
        "--no-session-persistence",
        "--permission-prompts",
        "none",
        "--max-budget-usd",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    a.push(format!("{:.2}", t.presupuesto_usd));
    match t.rol {
        Rol::Autor => {
            a.extend(
                ["--permission-mode", "acceptEdits", "--tools", AUTHOR_TOOLS].map(str::to_owned),
            );
            a.push("--allowedTools".into());
            a.extend(AUTHOR_ALLOWED.iter().map(|s| (*s).to_owned()));
        }
        Rol::Revisor => a.extend(["--tools", REVIEW_TOOLS].map(str::to_owned)),
        Rol::Juez => a.extend(["--tools", ""].map(str::to_owned)),
    }
    a.push("--json-schema".into());
    a.push(t.esquema.to_string());
    a.push(
        if t.rol == Rol::Juez {
            "--system-prompt"
        } else {
            "--append-system-prompt"
        }
        .into(),
    );
    a.push(t.sistema.to_owned());
    a
}

impl Motor for ClaudeCli {
    fn correr(&self, t: &Tarea<'_>) -> Result<Salida, Falla> {
        std::fs::create_dir_all(t.dir).map_err(|e| Falla::new(format!("carpeta: {e}")))?;
        let fin = bounded(
            &self.programa,
            &args(t),
            t.dir,
            &t.env,
            Some(t.mensaje),
            t.timeout,
            self.nice,
        )
        .map_err(Falla::new)?;
        if let Some(limit) = wawi_brain_claude::cupo::in_output(&fin.stdout) {
            let now = wawi_brain_claude::cupo::now();
            let motivo = match &self.home {
                Some(home) => {
                    let held = wawi_brain_claude::cupo::note(home, limit, now, "autoarreglo");
                    wawi_brain_claude::cupo::skipped(&held, now)
                }
                None => "sin cupo de Claude".into(),
            };
            return Err(Falla::new(motivo));
        }
        let mut salida = parse_output(&fin.stdout);
        if fin.timed_out {
            let cost = salida
                .as_ref()
                .map_or_else(|f| f.costo_usd, |s| s.costo_usd);
            return Err(Falla {
                motivo: format!(
                    "se le acabó el tiempo ({} min)",
                    t.timeout.as_secs().div_ceil(60)
                ),
                costo_usd: cost,
            });
        }
        if let Ok(s) = &mut salida {
            s.ms = fin.ms;
        }
        salida
    }
}

/// The CLI's `--output-format json` result: `structured_output` when it
/// worked, and the cost either way.
pub fn parse_output(text: &str) -> Result<Salida, Falla> {
    let value: Value =
        serde_json::from_str(text.trim()).map_err(|_| Falla::new("claude no devolvió JSON"))?;
    let costo_usd = value
        .get("total_cost_usd")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    if value.get("is_error").and_then(Value::as_bool) == Some(true)
        || value
            .get("subtype")
            .and_then(Value::as_str)
            .is_some_and(|s| s.starts_with("error"))
    {
        let subtype = value.get("subtype").and_then(Value::as_str).unwrap_or("");
        let why = if subtype.contains("budget") {
            "se pasó del presupuesto".to_owned()
        } else {
            value
                .get("result")
                .and_then(Value::as_str)
                .map(|r| crate::truncate(r, 200))
                .unwrap_or_else(|| format!("error ({subtype})"))
        };
        return Err(Falla {
            motivo: format!("claude falló: {}", crate::scrub::scrub(&why)),
            costo_usd,
        });
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
        .ok_or(Falla {
            motivo: "claude no devolvió la salida estructurada".into(),
            costo_usd,
        })?;
    Ok(Salida {
        valor,
        costo_usd,
        ms: 0,
    })
}

/// The real gate commands: cargo in the worktree, offline (no new crates
/// can come in), into the shared target folder, at low priority.
pub struct Cargo {
    pub target: PathBuf,
    pub nice: i32,
}

impl Comandos for Cargo {
    fn correr(&self, dir: &Path, argv: &[String], timeout: Duration) -> Corrida {
        let Some((program, rest)) = argv.split_first() else {
            return Corrida {
                code: -1,
                timed_out: false,
                salida: "comando vacío".into(),
            };
        };
        let env = cargo_env(&self.target);
        match bounded(
            Path::new(program),
            rest,
            dir,
            &env,
            None,
            timeout,
            self.nice,
        ) {
            Ok(fin) => Corrida {
                code: fin.code,
                timed_out: fin.timed_out,
                salida: format!("{}\n{}", fin.stdout, fin.stderr),
            },
            Err(e) => Corrida {
                code: -1,
                timed_out: false,
                salida: e,
            },
        }
    }
}

pub fn cargo_env(target: &Path) -> Vec<(String, String)> {
    vec![
        (
            "CARGO_TARGET_DIR".into(),
            target.to_string_lossy().into_owned(),
        ),
        ("CARGO_NET_OFFLINE".into(), "true".into()),
        ("CARGO_TERM_COLOR".into(), "never".into()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tarea<'a>(rol: Rol, schema: &'a Value) -> Tarea<'a> {
        Tarea {
            rol,
            dir: Path::new("/tmp"),
            modelo: "claude-opus-5-5",
            presupuesto_usd: 4.0,
            timeout: Duration::from_secs(1800),
            sistema: "reglas",
            mensaje: "texto privado del brief",
            esquema: schema,
            env: Vec::new(),
        }
    }

    #[test]
    fn the_author_is_boxed_in() {
        let schema = json!({ "type": "object" });
        let a = args(&tarea(Rol::Autor, &schema));
        for flag in [
            "-p",
            "--setting-sources=",
            "--strict-mcp-config",
            "--no-session-persistence",
            "--disable-slash-commands",
            "acceptEdits",
            "Bash(cargo *)",
            "Bash(git diff *)",
            "claude-opus-5-5",
            "4.00",
        ] {
            assert!(a.contains(&flag.to_owned()), "{flag}: {a:?}");
        }
        let tools = a.iter().position(|x| x == "--tools").unwrap();
        assert_eq!(a[tools + 1], "Read,Edit,Write,Grep,Glob,Bash");
        assert!(
            !a.iter().any(|x| x == "Bash" || x == "Bash(*)"),
            "no free Bash"
        );
        assert!(
            !a.iter().any(|x| x.contains("privado")),
            "the brief goes on stdin"
        );
        assert!(
            !a.iter()
                .any(|x| x.contains("dangerously") || x.contains("bypass"))
        );
    }

    #[test]
    fn the_reviewer_only_reads_and_the_judge_has_nothing() {
        let schema = json!({ "type": "object" });
        let r = args(&tarea(Rol::Revisor, &schema));
        let tools = r.iter().position(|x| x == "--tools").unwrap();
        assert_eq!(r[tools + 1], "Read,Grep,Glob");
        assert!(
            !r.iter()
                .any(|x| x.contains("Edit") || x.contains("Write") || x.contains("Bash"))
        );
        assert!(!r.contains(&"acceptEdits".to_owned()));
        let j = args(&tarea(Rol::Juez, &schema));
        let tools = j.iter().position(|x| x == "--tools").unwrap();
        assert_eq!(j[tools + 1], "");
        assert!(j.contains(&"--system-prompt".to_owned()));
    }

    #[test]
    fn reads_results_and_costs() {
        let ok =
            parse_output(r#"{"is_error":false,"structured_output":{"a":1},"total_cost_usd":1.25}"#)
                .unwrap();
        assert_eq!(ok.valor, json!({ "a": 1 }));
        assert_eq!(ok.costo_usd, 1.25);
        let budget = parse_output(
            r#"{"is_error":true,"subtype":"error_max_budget_usd","total_cost_usd":4.01}"#,
        )
        .unwrap_err();
        assert!(budget.motivo.contains("presupuesto"));
        assert_eq!(budget.costo_usd, 4.01);
        assert!(parse_output("nada").is_err());
    }

    #[test]
    fn a_child_past_its_time_is_killed() {
        let fin = bounded(
            Path::new("sleep"),
            &["5".to_owned()],
            Path::new("/tmp"),
            &[],
            None,
            Duration::from_millis(300),
            0,
        )
        .unwrap();
        assert!(fin.timed_out);
        assert!(fin.ms < 3000);
    }
}
