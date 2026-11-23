//! Shared by the integration tests: temp homes and repos, a fake claude
//! and fake gate commands. No test calls a real model.

#![allow(dead_code)]

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

use serde_json::{Value, json};
use wawi_autoarreglo::config::Config;
use wawi_autoarreglo::gates::Corrida;
use wawi_autoarreglo::intento::Contexto;
use wawi_autoarreglo::log::Log;
use wawi_autoarreglo::motor::{Comandos, Falla, Motor, Rol, Salida, Tarea};
use wawi_tools::{Civil, FakeClock};

pub fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aa-test-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A repo with one commit on `main` (or none).
pub fn repo(dir: &Path, with_commit: bool) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.name", "Dueño de prueba"]);
    git(dir, &["config", "user.email", "duenio@example.com"]);
    if with_commit {
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "pub fn uno() -> u32 { 1 }\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "base"]);
    }
}

pub fn commits(dir: &Path) -> usize {
    git(dir, &["rev-list", "--count", "HEAD"])
        .trim()
        .parse()
        .unwrap()
}

pub fn config(repo: &Path) -> Config {
    Config {
        activo: true,
        repo: repo.to_string_lossy().into_owned(),
        ventana: String::new(),
        ..Config::default()
    }
}

pub fn write_config(home: &Path, repo: &Path, extra: &str) {
    let text = format!(
        "activo = true\nrepo = \"{}\"\nventana = \"\"\n{extra}",
        repo.display()
    );
    std::fs::create_dir_all(home.join("autoarreglo")).unwrap();
    std::fs::write(home.join("autoarreglo/config.toml"), text).unwrap();
}

pub fn clock() -> Rc<FakeClock> {
    FakeClock::at(Civil::date(2026, 11, 21).at(3, 0, 0), -3)
}

/// What a fake author does in the worktree.
pub type Edit = Box<dyn Fn(&Path)>;

/// A fake claude: the author runs `edit` in the worktree and answers
/// `author`; the reviewer answers `review`. Every call is recorded.
pub struct FakeMotor {
    pub edit: Edit,
    pub author: Result<Value, Falla>,
    pub review: Result<Value, Falla>,
    pub judge: Value,
    pub cost: f64,
    pub calls: RefCell<Vec<(Rol, String, PathBuf, String)>>,
}

impl FakeMotor {
    pub fn fixing(edit: Edit) -> Self {
        Self {
            edit,
            author: Ok(json!({
                "titulo_commit": "No esperar al cerebro para el primer audio",
                "para_decir": "se me cortaba la voz",
                "causa": "El primer audio esperaba al cerebro.",
                "cambios": "Ahora sale apenas está.",
                "pruebas": "Una prueba nueva lo reproduce.",
                "bloqueado": ""
            })),
            review: Ok(json!({ "aprobado": true, "motivos": ["arregla el problema"] })),
            judge: json!({ "problemas": [] }),
            cost: 1.5,
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl Motor for FakeMotor {
    fn correr(&self, t: &Tarea<'_>) -> Result<Salida, Falla> {
        self.calls.borrow_mut().push((
            t.rol,
            t.modelo.to_owned(),
            t.dir.to_path_buf(),
            t.mensaje.to_owned(),
        ));
        let answer = match t.rol {
            Rol::Autor => {
                (self.edit)(t.dir);
                self.author.clone()
            }
            Rol::Revisor => self.review.clone(),
            Rol::Juez => Ok(self.judge.clone()),
        };
        answer.map(|valor| Salida {
            valor,
            costo_usd: self.cost,
            ms: 1,
        })
    }
}

/// Fake gate commands: everything passes, and every `cargo test` "runs"
/// one test, unless `fail` names a word of the command.
pub struct FakeComandos {
    pub fail: Option<String>,
    pub zero_tests: bool,
    pub runs: RefCell<Vec<String>>,
}

impl FakeComandos {
    pub fn green() -> Self {
        Self {
            fail: None,
            zero_tests: false,
            runs: RefCell::new(Vec::new()),
        }
    }
}

impl Comandos for FakeComandos {
    fn correr(&self, _dir: &Path, argv: &[String], _timeout: Duration) -> Corrida {
        let line = argv.join(" ");
        self.runs.borrow_mut().push(line.clone());
        if self
            .fail
            .as_ref()
            .is_some_and(|f| line.contains(f.as_str()))
        {
            return Corrida {
                code: 101,
                timed_out: false,
                salida: "error[E0308]: mismatched types".into(),
            };
        }
        let n = if self.zero_tests && line.contains("--exact") {
            0
        } else {
            1
        };
        Corrida {
            code: 0,
            timed_out: false,
            salida: format!("test result: ok. {n} passed; 0 failed"),
        }
    }
}

pub fn ctx<'a>(
    home: &'a Path,
    config: &'a Config,
    clock: &'a FakeClock,
    motor: &'a dyn Motor,
    comandos: &'a dyn Comandos,
    log: &'a Log,
) -> Contexto<'a> {
    Contexto {
        home,
        config,
        clock,
        motor,
        comandos,
        log,
        home_dir: "/Users/ana".into(),
        user: "ana".into(),
    }
}

/// The facts waiting for Wawi, as text.
pub fn facts(home: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(home.join("cola/hechos/new")) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .collect();
    out.sort();
    out
}
