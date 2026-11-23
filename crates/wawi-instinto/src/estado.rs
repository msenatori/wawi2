//! The state of each question, one file per question in
//! `<home>/instinto/estado/<id>.json`. Written to a temp file and renamed,
//! so a crash never leaves half a file. The question itself rides inside,
//! so a restart needs nothing else.
//!
//! ```text
//! pendiente ──► activa ──► cumplida   (true, without repetir)
//!     │           │  └───► vencida    (vence or hasta passed)
//!     └───────────┴──────► rota       (fallos_max failures in a row)
//!                    └───► cancelada  (cancelar_seguimiento)
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cuando::Memoria;

pub const DIR: &str = "instinto/estado";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Fase {
    /// Taken from the queue, never looked at yet.
    Pendiente,
    /// Looked at least once; still watching.
    Activa,
    Cumplida,
    Vencida,
    Rota,
    Cancelada,
}

impl Fase {
    pub fn terminal(self) -> bool {
        !matches!(self, Fase::Pendiente | Fase::Activa)
    }

    pub fn nombre(self) -> &'static str {
        match self {
            Fase::Pendiente => "pendiente",
            Fase::Activa => "activa",
            Fase::Cumplida => "cumplida",
            Fase::Vencida => "vencida",
            Fase::Rota => "rota",
            Fase::Cancelada => "cancelada",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Estado {
    pub id: String,
    pub fase: Fase,
    /// The question as it came in.
    pub pregunta: Value,
    #[serde(flatten)]
    pub memoria: Memoria,
    /// Good looks so far.
    #[serde(default)]
    pub corridas: u64,
    /// Failures in a row.
    #[serde(default)]
    pub fallos: u32,
    /// Times it came true (more than one only with `repetir`).
    #[serde(default)]
    pub cumplidas: u64,
    /// Epoch seconds of the next look.
    pub proxima: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ultimo_error: Option<String>,
    /// Epoch seconds of the last change.
    pub actualizado: i64,
    /// Why it ended, for the terminal phases.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motivo: Option<String>,
}

pub fn dir(home: &Path) -> PathBuf {
    home.join(DIR)
}

pub fn path(home: &Path, id: &str) -> PathBuf {
    dir(home).join(format!("{id}.json"))
}

/// Writes `text` to `path` through a temp file in the same folder.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

pub fn save(home: &Path, estado: &Estado) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(estado).map_err(std::io::Error::other)?;
    write_atomic(&path(home, &estado.id), &text)
}

pub fn load(home: &Path, id: &str) -> Option<Estado> {
    let text = std::fs::read_to_string(path(home, id)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Every state file, sorted by id. Unreadable files are skipped.
pub fn load_all(home: &Path) -> Vec<Estado> {
    let Ok(entries) = std::fs::read_dir(dir(home)) else {
        return Vec::new();
    };
    let mut all: Vec<Estado> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|e| e == "json")
                && !p
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        })
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .filter_map(|t| serde_json::from_str(&t).ok())
        .collect();
    all.sort_by(|a: &Estado, b: &Estado| a.id.cmp(&b.id));
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trips_through_a_temp_file() {
        let home = std::env::temp_dir().join(format!("instinto-estado-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let estado = Estado {
            id: "a".into(),
            fase: Fase::Activa,
            pregunta: json!({ "id": "a" }),
            memoria: Memoria {
                ultimo: Some(json!("x")),
                vistos: vec!["1".into()],
            },
            corridas: 2,
            fallos: 0,
            cumplidas: 0,
            proxima: 10,
            ultimo_error: None,
            actualizado: 5,
            motivo: None,
        };
        save(&home, &estado).unwrap();
        assert_eq!(load(&home, "a"), Some(estado.clone()));
        assert_eq!(load_all(&home), vec![estado]);
        let leftovers: Vec<_> = std::fs::read_dir(dir(&home)).unwrap().collect();
        assert_eq!(leftovers.len(), 1);
        let _ = std::fs::remove_dir_all(&home);
    }
}
