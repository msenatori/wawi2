//! The last failure of each plugin, for the Settings window:
//! `<home>/plugins/.fallos/<plugin>.json`. One small file per plugin, so
//! every writer (the plugin host, the instinto worker, autoarreglo) only
//! ever touches its own, written whole through a temporary file. The folder
//! starts with a dot, so the plugin loader skips it.
//!
//! ```json
//! { "plugin": "linear", "cuando": "2026-11-24T15:10:02", "tipo": "permiso",
//!   "detalle": "el servidor pide permiso (HTTP 401)" }
//! ```
//!
//! `tipo` is one of [`Tipo`]. The file stays after the plugin recovers: it
//! is the *last* failure, and `cuando` says how old it is. Compare it with
//! the plugin's state (`PluginInfo::state`) to know whether it still holds.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use wawi_tools::Civil;

use crate::manifest::truncate;

pub const DIR: &str = "plugins/.fallos";

/// What kind of failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tipo {
    /// It needs the owner to sign in again (token revoked, key refused).
    Permiso,
    /// It went down (one crash or a failed connection).
    Caida,
    /// It keeps going down: a crash loop.
    Caidas,
    /// An instinto follow-up broke (`rota`).
    Rota,
    /// Autoarreglo could not run.
    NoCorrio,
}

impl Tipo {
    pub fn as_str(self) -> &'static str {
        match self {
            Tipo::Permiso => "permiso",
            Tipo::Caida => "caida",
            Tipo::Caidas => "caidas",
            Tipo::Rota => "rota",
            Tipo::NoCorrio => "no_corrio",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fallo {
    pub plugin: String,
    /// Local time, `YYYY-MM-DDTHH:MM:SS`.
    pub cuando: String,
    pub tipo: String,
    /// Why, in a short line, without secrets.
    pub detalle: String,
}

fn path(home: &Path, plugin: &str) -> Option<PathBuf> {
    crate::inbox::valid_id(plugin).then(|| home.join(DIR).join(format!("{plugin}.json")))
}

/// Records the plugin's last failure. Errors are ignored: this is a note
/// for a screen, never worth failing over.
pub fn registrar(home: &Path, plugin: &str, tipo: Tipo, detalle: &str, now: &Civil) {
    let Some(path) = path(home, plugin) else {
        return;
    };
    let fallo = Fallo {
        plugin: plugin.to_owned(),
        cuando: format!("{}:{:02}", now.iso_minute(), now.second),
        tipo: tipo.as_str().to_owned(),
        detalle: truncate(detalle.trim(), 300),
    };
    let Ok(text) = serde_json::to_string(&fallo) else {
        return;
    };
    let dir = home.join(DIR);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let tmp = dir.join(format!(".{plugin}.{}.tmp", std::process::id()));
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// The plugin's last failure, if any.
pub fn leer(home: &Path, plugin: &str) -> Option<Fallo> {
    let text = std::fs::read_to_string(path(home, plugin)?).ok()?;
    serde_json::from_str(&text).ok()
}

/// Every plugin's last failure, by plugin name.
pub fn todos(home: &Path) -> Vec<Fallo> {
    let Ok(entries) = std::fs::read_dir(home.join(DIR)) else {
        return Vec::new();
    };
    let mut all: Vec<Fallo> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .filter_map(|p| serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok())
        .collect();
    all.sort_by(|a: &Fallo, b| a.plugin.cmp(&b.plugin));
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_failure_per_plugin_is_kept_and_read_back() {
        let home = std::env::temp_dir().join(format!("wawi-fallos-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let now = Civil::date(2026, 11, 24).at(15, 10, 2);
        assert_eq!(leer(&home, "linear"), None);
        registrar(&home, "linear", Tipo::Caida, "se cerró", &now);
        registrar(
            &home,
            "linear",
            Tipo::Permiso,
            "pide permiso (HTTP 401)",
            &now,
        );
        registrar(
            &home,
            "instinto",
            Tipo::Rota,
            "Dejé de poder mirar el PR",
            &now,
        );
        registrar(&home, "../x", Tipo::Rota, "nunca", &now);
        let linear = leer(&home, "linear").unwrap();
        assert_eq!(linear.tipo, "permiso");
        assert_eq!(linear.cuando, "2026-11-24T15:10:02");
        let all = todos(&home);
        assert_eq!(
            all.iter().map(|f| f.plugin.as_str()).collect::<Vec<_>>(),
            ["instinto", "linear"]
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}
