//! The gates. ALL must pass; the first failure discards the attempt, and
//! its reason goes into the problem's history and the next brief.
//!
//! Cheap and deterministic first (forbidden paths, size, dependencies,
//! secrets, the hot-path guard), then the commands in the worktree (fmt,
//! clippy, the full test suite, the speed tests by name), then the
//! second-opinion review. This file is itself a forbidden path.

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::git::FileStat;
use crate::scrub::scan_added;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Resultado {
    pub nombre: String,
    pub ok: bool,
    #[serde(default)]
    pub motivo: String,
    #[serde(default)]
    pub detalle: Vec<String>,
}

impl Resultado {
    pub fn ok(nombre: &str, motivo: impl Into<String>) -> Self {
        Self {
            nombre: nombre.into(),
            ok: true,
            motivo: motivo.into(),
            detalle: Vec::new(),
        }
    }

    pub fn falla(nombre: &str, motivo: impl Into<String>, detalle: Vec<String>) -> Self {
        Self {
            nombre: nombre.into(),
            ok: false,
            motivo: motivo.into(),
            detalle: detalle.into_iter().take(10).collect(),
        }
    }

    /// One line: "prohibidos: toca archivos prohibidos (a, b)".
    pub fn linea(&self) -> String {
        let mut line = format!("{}: {}", self.nombre, self.motivo);
        if !self.detalle.is_empty() {
            line.push_str(&format!(
                " ({})",
                self.detalle
                    .iter()
                    .take(3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        line
    }
}

/// Paths a fix may never touch. Prefixes end in `/`.
pub const FORBIDDEN: &[&str] = &[
    // Her own guardrails: this crate and its plugin.
    "crates/wawi-autoarreglo/",
    "plugins/autoarreglo/",
    // The detector that would report a bad fix.
    "crates/wawi-app/src/voice/incidents.rs",
    // Policy and permissions.
    "crates/wawi-core/src/policy.rs",
    "crates/wawi-core/src/registry.rs",
    "crates/wawi-plugins/src/manifest.rs",
    "crates/wawi-plugins/src/tool.rs",
    // Secrets handling.
    "crates/wawi-plugins/src/secrets.rs",
    "crates/wawi-memory/src/secrets.rs",
    ".git/",
    ".github/",
];

/// File names that are forbidden anywhere.
const FORBIDDEN_NAMES: &[&str] = &[
    "secrets.toml",
    "LICENSE",
    "LICENSE.md",
    "LICENSE.txt",
    "COPYING",
];

pub fn forbidden(path: &str) -> bool {
    let path = path.trim_start_matches("./");
    let name = path.rsplit('/').next().unwrap_or(path);
    FORBIDDEN.iter().any(|f| {
        if f.ends_with('/') {
            path.starts_with(f) || path == f.trim_end_matches('/')
        } else {
            path == *f
        }
    }) || FORBIDDEN_NAMES.contains(&name)
        || name.starts_with(".env")
        || name.starts_with("LICENSE")
        || path.contains("/.git/")
}

/// The changed lines (`+` and `-`, without the headers) of one file.
fn file_lines<'a>(diff: &'a str, path: &str) -> Vec<&'a str> {
    let mut inside = false;
    let mut lines = Vec::new();
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            inside = line.ends_with(&format!(" b/{path}"));
            continue;
        }
        if inside
            && (line.starts_with('+') || line.starts_with('-'))
            && !line.starts_with("+++")
            && !line.starts_with("---")
        {
            lines.push(line);
        }
    }
    lines
}

fn permission_line() -> Regex {
    Regex::new(r"^[+-]\s*(kind|confirm|observa_charla|expose|enabled)\s*=|^[+-]\s*\[tools")
        .expect("fixed")
}

pub fn prohibidos(stats: &[FileStat], diff: &str) -> Resultado {
    let mut bad: Vec<String> = stats
        .iter()
        .filter(|s| forbidden(&s.path))
        .map(|s| s.path.clone())
        .collect();
    // .gitignore: adding rules is fine; dropping one could publish a secret.
    if file_lines(diff, ".gitignore")
        .iter()
        .any(|l| l.starts_with('-') && !l[1..].trim().is_empty())
    {
        bad.push(".gitignore (saca reglas)".into());
    }
    // A plugin manifest may change, but never what its tools are allowed.
    let permission = permission_line();
    for s in stats {
        if s.path.starts_with("plugins/")
            && s.path.ends_with("/plugin.toml")
            && file_lines(diff, &s.path)
                .iter()
                .any(|l| permission.is_match(l))
        {
            bad.push(format!("{} (cambia permisos)", s.path));
        }
    }
    if bad.is_empty() {
        Resultado::ok("prohibidos", "no toca nada prohibido")
    } else {
        Resultado::falla("prohibidos", "toca archivos prohibidos", bad)
    }
}

pub fn tamano(stats: &[FileStat], max_lineas: u32, max_archivos: u32) -> Resultado {
    if stats.is_empty() {
        return Resultado::falla("tamaño", "no cambió nada", Vec::new());
    }
    let lines: u32 = stats.iter().map(|s| s.added + s.removed).sum();
    if stats.len() as u32 > max_archivos {
        return Resultado::falla(
            "tamaño",
            format!("demasiados archivos ({} > {max_archivos})", stats.len()),
            Vec::new(),
        );
    }
    if lines > max_lineas {
        return Resultado::falla(
            "tamaño",
            format!("diff demasiado grande ({lines} > {max_lineas} líneas)"),
            Vec::new(),
        );
    }
    if let Some(binary) = stats.iter().find(|s| s.binary) {
        return Resultado::falla(
            "tamaño",
            "agrega o cambia un archivo binario",
            vec![binary.path.clone()],
        );
    }
    Resultado::ok(
        "tamaño",
        format!("{lines} líneas en {} archivos", stats.len()),
    )
}

/// No new dependency and no lockfile churn: `Cargo.lock` may not change at
/// all, and a `Cargo.toml` only in comments and blank lines.
pub fn dependencias(stats: &[FileStat], diff: &str) -> Resultado {
    let mut bad = Vec::new();
    for s in stats {
        let name = s.path.rsplit('/').next().unwrap_or(&s.path);
        if name == "Cargo.lock" {
            bad.push(format!("{} cambió", s.path));
        } else if name == "Cargo.toml" {
            for line in file_lines(diff, &s.path) {
                let body = line[1..].trim();
                if !body.is_empty() && !body.starts_with('#') {
                    bad.push(format!("{}: {}", s.path, crate::truncate(body, 80)));
                }
            }
        }
    }
    if bad.is_empty() {
        Resultado::ok("dependencias", "sin dependencias nuevas")
    } else {
        Resultado::falla(
            "dependencias",
            "un arreglo no puede tocar dependencias",
            bad,
        )
    }
}

pub fn secretos(diff: &str, home: &str, user: &str) -> Resultado {
    let found = scan_added(diff, home, user);
    if found.is_empty() {
        Resultado::ok("secretos", "nada secreto ni personal")
    } else {
        Resultado::falla("secretos", "el diff tiene algo secreto o personal", found)
    }
}

/// The files of the turn path, where a blocking call would cost the owner
/// time. Tests are not on the path.
fn on_turn_path(path: &str) -> bool {
    let hot = [
        "crates/wawi-core/src/",
        "crates/wawi-app/src/voice/host.rs",
        "crates/wawi-app/src/voice/turns.rs",
        "crates/wawi-brain-claude/src/",
        "crates/wawi-tts-kokoro/src/",
        "crates/wawi-stt-parakeet/src/",
        "crates/wawi-audio-mac/src/",
    ];
    hot.iter().any(|h| path.starts_with(h))
        && !path.ends_with("/tests.rs")
        && !path.contains("/tests/")
        && !path.contains("/bin/")
}

/// The cheap latency guard: no new blocking call on the turn path (a
/// sleep, a blocking receive, a subprocess, a wait or join, network, or
/// file I/O inside the pure core). Speed is the owner's hard rule, and this
/// catches the usual way a fix makes a turn slower before any test runs.
pub fn camino_rapido(diff: &str) -> Resultado {
    let blocking = Regex::new(
        r"thread::sleep\(|\.recv\(\)|Command::new\(|\.wait\(\)|\.join\(\)|ureq::|reqwest|TcpStream|block_on\(",
    )
    .expect("fixed");
    let core_io = Regex::new(r"std::fs|File::open|File::create|read_to_string\(").expect("fixed");
    let mut bad = Vec::new();
    for (path, line) in crate::scrub::added_lines(diff) {
        if !on_turn_path(&path) {
            continue;
        }
        let code = line.trim();
        if code.starts_with("//") {
            continue;
        }
        if blocking.is_match(code)
            || (path.starts_with("crates/wawi-core/src/") && core_io.is_match(code))
        {
            bad.push(format!("{path}: {}", crate::truncate(code, 90)));
        }
    }
    if bad.is_empty() {
        Resultado::ok(
            "camino rápido",
            "nada bloqueante nuevo en el camino del turno",
        )
    } else {
        Resultado::falla(
            "camino rápido",
            "agrega algo que puede bloquear un turno",
            bad,
        )
    }
}

/// One command's run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Corrida {
    pub code: i32,
    pub timed_out: bool,
    /// The end of stdout and stderr together.
    pub salida: String,
}

fn tail(text: &str, n: usize) -> Vec<String> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|l| crate::truncate(l, 200))
        .collect()
}

pub fn comando(nombre: &str, corrida: &Corrida) -> Resultado {
    if corrida.timed_out {
        return Resultado::falla(nombre, "no terminó a tiempo", tail(&corrida.salida, 5));
    }
    if corrida.code != 0 {
        return Resultado::falla(nombre, "falla", tail(&corrida.salida, 8));
    }
    Resultado::ok(nombre, "pasa")
}

/// How many tests a `cargo test` run passed, from its summary lines.
pub fn passed(salida: &str) -> u32 {
    let summary = Regex::new(r"test result: ok\. (\d+) passed").expect("fixed");
    summary
        .captures_iter(salida)
        .filter_map(|c| c[1].parse::<u32>().ok())
        .sum()
}

/// The speed tests: each command must pass AND run at least one test, so
/// a fix that deletes or renames one fails here.
pub fn velocidad(corridas: &[(String, Corrida)]) -> Resultado {
    let mut bad = Vec::new();
    for (name, run) in corridas {
        if run.timed_out || run.code != 0 {
            bad.push(format!("{name}: falla"));
        } else if passed(&run.salida) == 0 {
            bad.push(format!("{name}: no corrió ninguna prueba (¿la borraron?)"));
        }
    }
    if bad.is_empty() {
        Resultado::ok(
            "velocidad",
            format!("{} pruebas de velocidad pasan", corridas.len()),
        )
    } else {
        Resultado::falla("velocidad", "una prueba de velocidad no pasa", bad)
    }
}

pub fn revision(aprobado: bool, motivos: &[String]) -> Resultado {
    if aprobado {
        Resultado {
            nombre: "revisión".into(),
            ok: true,
            motivo: "la aprobó una segunda opinión (Opus, solo lectura)".into(),
            detalle: motivos.iter().take(3).cloned().collect(),
        }
    } else {
        Resultado::falla(
            "revisión",
            "la segunda opinión la rechazó",
            motivos.to_vec(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(path: &str, added: u32) -> FileStat {
        FileStat {
            path: path.into(),
            added,
            removed: 0,
            binary: false,
        }
    }

    #[test]
    fn forbidden_paths() {
        for p in [
            "crates/wawi-autoarreglo/src/gates.rs",
            "plugins/autoarreglo/plugin.toml",
            "crates/wawi-core/src/policy.rs",
            "crates/wawi-plugins/src/secrets.rs",
            "LICENSE",
            "x/.env.local",
            "a/secrets.toml",
        ] {
            assert!(forbidden(p), "{p}");
        }
        for p in [
            "crates/wawi-core/src/dialogue.rs",
            "docs/X.md",
            "crates/wawi-autoarreglo-x/a.rs",
        ] {
            assert!(!forbidden(p), "{p}");
        }
    }

    #[test]
    fn manifests_may_change_but_not_their_permissions() {
        let diff = "diff --git a/plugins/linear/plugin.toml b/plugins/linear/plugin.toml\n--- a/plugins/linear/plugin.toml\n+++ b/plugins/linear/plugin.toml\n@@ -1 +1 @@\n-kind = \"act\"\n+kind = \"read\"\n";
        let r = prohibidos(&[stat("plugins/linear/plugin.toml", 1)], diff);
        assert!(!r.ok);
        let diff = "diff --git a/plugins/linear/plugin.toml b/plugins/linear/plugin.toml\n+++ b/plugins/linear/plugin.toml\n+say = \"otra frase\"\n";
        assert!(prohibidos(&[stat("plugins/linear/plugin.toml", 1)], diff).ok);
        let diff = "diff --git a/.gitignore b/.gitignore\n+++ b/.gitignore\n-/secrets\n";
        assert!(!prohibidos(&[stat(".gitignore", 0)], diff).ok);
    }

    #[test]
    fn no_dependency_and_no_lockfile() {
        let diff = "diff --git a/crates/x/Cargo.toml b/crates/x/Cargo.toml\n+++ b/crates/x/Cargo.toml\n+rand = \"0.9\"\n";
        assert!(!dependencias(&[stat("crates/x/Cargo.toml", 1)], diff).ok);
        assert!(!dependencias(&[stat("Cargo.lock", 3)], "").ok);
        let diff = "diff --git a/crates/x/Cargo.toml b/crates/x/Cargo.toml\n+++ b/crates/x/Cargo.toml\n+# a note\n";
        assert!(dependencias(&[stat("crates/x/Cargo.toml", 1)], diff).ok);
    }

    #[test]
    fn size_limits() {
        assert!(!tamano(&[], 400, 12).ok);
        assert!(!tamano(&[stat("a", 401)], 400, 12).ok);
        assert!(tamano(&[stat("a", 40)], 400, 12).ok);
    }

    #[test]
    fn the_hot_path_guard() {
        let diff = "diff --git a/crates/wawi-app/src/voice/host.rs b/crates/wawi-app/src/voice/host.rs\n+++ b/crates/wawi-app/src/voice/host.rs\n+        std::thread::sleep(Duration::from_millis(50));\n";
        assert!(!camino_rapido(diff).ok);
        let diff = "diff --git a/crates/wawi-core/src/dialogue.rs b/crates/wawi-core/src/dialogue.rs\n+++ b/crates/wawi-core/src/dialogue.rs\n+    let s = std::fs::read_to_string(p);\n";
        assert!(!camino_rapido(diff).ok);
        let diff = "diff --git a/crates/wawi-instinto/src/worker.rs b/crates/wawi-instinto/src/worker.rs\n+++ b/crates/wawi-instinto/src/worker.rs\n+        std::thread::sleep(TICK);\n";
        assert!(camino_rapido(diff).ok, "off the turn path it is fine");
    }

    #[test]
    fn a_speed_test_must_run_and_pass() {
        let ok = Corrida {
            code: 0,
            timed_out: false,
            salida: "test result: ok. 1 passed; 0 failed".into(),
        };
        let none = Corrida {
            code: 0,
            timed_out: false,
            salida: "test result: ok. 0 passed; 0 failed; 0 ignored".into(),
        };
        assert!(velocidad(&[("a".into(), ok.clone())]).ok);
        assert!(!velocidad(&[("a".into(), ok), ("b".into(), none)]).ok);
    }
}
