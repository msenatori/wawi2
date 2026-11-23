//! `<home>/autoarreglo/config.toml`. Everything is optional, and it is
//! OFF by default: with `activo = false` incidents are still journaled (it
//! costs nothing), but no model is ever called.
//!
//! ```toml
//! activo = false
//! repo = "/Users/vos/Work/wawi2"   # the checkout to fix (or $WAWI2_REPO)
//! modelo = "claude-opus-5-5"
//! max_intentos_dia = 2
//! presupuesto_dia_usd = 10.0       # all model calls of a day together
//! presupuesto_intento_usd = 4.0    # the author's cap (--max-budget-usd)
//! presupuesto_revision_usd = 1.0   # the second-opinion review's cap
//! tiempo_max = "30m"               # the author's time limit
//! ventana = "01:00-07:00"          # when attempts may run ("" = any time)
//! max_lineas = 400
//! max_archivos = 12
//! min_repeticiones = 2             # an observed problem must repeat
//! reintento_dias = 7               # after a failed attempt
//! nice = 10                        # CPU priority of claude and cargo
//! probar_base = true               # the clean base must pass first
//! compilar = ["cargo", "build", "--release", "-p", "wawi-app"]
//!
//! [revision]                       # the nightly review
//! activa = true
//! hora = "04:00"
//! modelo = "claude-sonnet-5-5"
//! presupuesto_usd = 0.3
//! ```

use std::path::{Path, PathBuf};

use serde::Deserialize;

pub const FILE: &str = "autoarreglo/config.toml";

/// The default model for the author and the reviewer.
pub const OPUS: &str = "claude-opus-5-5";

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub activo: bool,
    pub repo: String,
    pub modelo: String,
    pub max_intentos_dia: u32,
    pub presupuesto_dia_usd: f64,
    pub presupuesto_intento_usd: f64,
    pub presupuesto_revision_usd: f64,
    pub tiempo_max: String,
    pub ventana: String,
    pub max_lineas: u32,
    pub max_archivos: u32,
    pub min_repeticiones: u32,
    pub reintento_dias: u32,
    pub nice: i32,
    pub probar_base: bool,
    /// Where cargo builds, shared by every attempt (incremental).
    pub target: String,
    /// What `compilar` runs in the repo after a fix is applied.
    pub compilar: Vec<String>,
    pub revision: Revision,
    pub pruebas: Pruebas,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Revision {
    pub activa: bool,
    pub hora: String,
    pub modelo: String,
    pub presupuesto_usd: f64,
}

/// The commands the gates run in the worktree. Each is argv; the defaults
/// are Wawi 2's. `velocidad` are the speed tests, by name: each must run at
/// least one test and pass, so deleting one fails the gate too.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Pruebas {
    pub fmt: Vec<String>,
    pub clippy: Vec<String>,
    pub test: Vec<String>,
    pub velocidad: Vec<Vec<String>>,
    /// The longest any one of them may take.
    pub tiempo_max: String,
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            activo: false,
            repo: String::new(),
            modelo: OPUS.into(),
            max_intentos_dia: 2,
            presupuesto_dia_usd: 10.0,
            presupuesto_intento_usd: 4.0,
            presupuesto_revision_usd: 1.0,
            tiempo_max: "30m".into(),
            ventana: "01:00-07:00".into(),
            max_lineas: 400,
            max_archivos: 12,
            min_repeticiones: 2,
            reintento_dias: 7,
            nice: 10,
            probar_base: true,
            target: String::new(),
            compilar: argv(&["cargo", "build", "--release", "-p", "wawi-app"]),
            revision: Revision::default(),
            pruebas: Pruebas::default(),
        }
    }
}

impl Default for Revision {
    fn default() -> Self {
        Self {
            activa: true,
            hora: "04:00".into(),
            modelo: "claude-sonnet-5-5".into(),
            presupuesto_usd: 0.3,
        }
    }
}

impl Default for Pruebas {
    fn default() -> Self {
        Self {
            fmt: argv(&["cargo", "fmt", "--all", "--check"]),
            clippy: argv(&[
                "cargo",
                "clippy",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ]),
            test: argv(&["cargo", "test", "--workspace"]),
            velocidad: vec![
                argv(&[
                    "cargo",
                    "test",
                    "-p",
                    "wawi-brain-claude",
                    "--test",
                    "fake_cli",
                    "--",
                    "--exact",
                    "a_turn_never_waits_for_a_respawn",
                ]),
                argv(&[
                    "cargo",
                    "test",
                    "-p",
                    "wawi-app",
                    "--",
                    "--exact",
                    "voice::tests::a_turn_flows_from_the_key_to_the_last_sound",
                ]),
                argv(&[
                    "cargo",
                    "test",
                    "-p",
                    "wawi-app",
                    "--",
                    "--exact",
                    "voice::tests::pressing_the_key_while_she_speaks_cuts_audio_and_the_brain",
                ]),
            ],
            tiempo_max: "25m".into(),
        }
    }
}

/// `90s`, `15m`, `2h`, `1h30m` in seconds.
pub fn seconds(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let bad = || format!("«{text}» no es una duración (90s, 15m, 2h)");
    if text.is_empty() {
        return Err(bad());
    }
    let (mut total, mut number) = (0u64, String::new());
    for c in text.chars() {
        if c.is_ascii_digit() {
            number.push(c);
            continue;
        }
        let n: u64 = number.parse().map_err(|_| bad())?;
        number.clear();
        total += n * match c {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            _ => return Err(bad()),
        };
    }
    if !number.is_empty() {
        return Err(bad());
    }
    Ok(total)
}

/// `HH:MM` as minutes since midnight.
pub fn minute(text: &str) -> Result<u16, String> {
    let bad = || format!("«{text}» no es una hora HH:MM");
    let (h, m) = text.trim().split_once(':').ok_or_else(bad)?;
    let (h, m): (u16, u16) = (h.parse().map_err(|_| bad())?, m.parse().map_err(|_| bad())?);
    if h > 23 || m > 59 {
        return Err(bad());
    }
    Ok(h * 60 + m)
}

/// Whether `minute` is inside `"HH:MM-HH:MM"` (which may wrap midnight).
/// An empty window is always open.
pub fn in_window(window: &str, minute_now: u16) -> Result<bool, String> {
    let window = window.trim();
    if window.is_empty() {
        return Ok(true);
    }
    let (from, until) = window
        .split_once('-')
        .ok_or_else(|| format!("«{window}» no es HH:MM-HH:MM"))?;
    let (from, until) = (minute(from)?, minute(until)?);
    Ok(if from <= until {
        (from..until).contains(&minute_now)
    } else {
        minute_now >= from || minute_now < until
    })
}

impl Config {
    /// The file, or the defaults (with the reason when the file is wrong).
    pub fn load(home: &Path) -> (Self, Option<String>) {
        let Ok(text) = std::fs::read_to_string(home.join(FILE)) else {
            return (Self::default(), None);
        };
        match toml::from_str::<Config>(&text) {
            Ok(config) => match config.check() {
                Ok(()) => (config, None),
                Err(e) => (
                    Self::default(),
                    Some(format!(
                        "{FILE}: {e}; uso los valores por defecto (apagado)"
                    )),
                ),
            },
            Err(e) => (
                Self::default(),
                Some(format!(
                    "{FILE} no se entiende ({}); uso los valores por defecto (apagado)",
                    e.message()
                )),
            ),
        }
    }

    pub fn check(&self) -> Result<(), String> {
        seconds(&self.tiempo_max)?;
        seconds(&self.pruebas.tiempo_max)?;
        in_window(&self.ventana, 0)?;
        minute(&self.revision.hora)?;
        if self.presupuesto_intento_usd <= 0.0 || self.presupuesto_revision_usd <= 0.0 {
            return Err("los presupuestos tienen que ser mayores que cero".into());
        }
        if self.pruebas.test.is_empty() {
            return Err("pruebas.test no puede estar vacío".into());
        }
        Ok(())
    }

    /// The repo to fix: `repo`, else `$WAWI2_REPO`.
    pub fn repo(&self) -> Option<PathBuf> {
        let repo = self.repo.trim();
        if !repo.is_empty() {
            return Some(PathBuf::from(repo));
        }
        std::env::var_os("WAWI2_REPO")
            .filter(|r| !r.is_empty())
            .map(PathBuf::from)
    }

    /// Where cargo builds: `target`, else `<home>/autoarreglo/target`.
    pub fn target_dir(&self, home: &Path) -> PathBuf {
        if self.target.trim().is_empty() {
            crate::dir(home).join("target")
        } else {
            PathBuf::from(self.target.trim())
        }
    }

    /// What one attempt may cost at most: the author and the reviewer.
    pub fn attempt_cap(&self) -> f64 {
        self.presupuesto_intento_usd + self.presupuesto_revision_usd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_by_default_with_opus_and_two_attempts() {
        let c = Config::default();
        assert!(!c.activo);
        assert_eq!(c.modelo, "claude-opus-5-5");
        assert_eq!(c.max_intentos_dia, 2);
        assert_eq!(seconds(&c.tiempo_max).unwrap(), 1800);
        assert!(c.check().is_ok());
        let c: Config =
            toml::from_str("activo = true\nmax_intentos_dia = 1\n[revision]\nhora = \"05:30\"\n")
                .unwrap();
        assert!(c.activo);
        assert_eq!(c.max_intentos_dia, 1);
        assert_eq!(c.revision.modelo, "claude-sonnet-5-5");
        assert!(toml::from_str::<Config>("otra = 1").is_err());
    }

    #[test]
    fn durations_and_windows() {
        assert_eq!(seconds("1h30m").unwrap(), 5400);
        assert!(seconds("30").is_err());
        assert!(in_window("01:00-07:00", 60).unwrap());
        assert!(!in_window("01:00-07:00", 7 * 60).unwrap());
        assert!(in_window("23:00-02:00", 30).unwrap());
        assert!(in_window("", 900).unwrap());
        assert!(in_window("25:00-02:00", 0).is_err());
    }
}
