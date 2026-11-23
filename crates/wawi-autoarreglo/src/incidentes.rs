//! The incident journal's inbox: `<home>/cola/incidentes`, maildir style.
//! The host writes one file per incident (`tmp/` then a rename into
//! `new/`), `reportar_problema` writes the owner's reports, and the worker
//! folds each one into the journal ([`crate::diario`]) and moves it to
//! `cur/` with what happened.
//!
//! ```json
//! {"id": "1763730604817-3", "version": 1, "tipo": "turno_lento",
//!  "clave": "cerebro", "resumen": "primer audio 4210 ms; la mediana es 1830 ms",
//!  "detalle": {"felt_ms": 4210, "mediana_ms": 1830},
//!  "creado": "2026-11-21T10:50:04", "origen": "host"}
//! ```
//!
//! `clave` is what makes two incidents the same problem (with `tipo`).
//! Nothing here carries what the owner said, except a report's own words.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wawi_plugins::inbox::{INCIDENTS, make_queue, valid_id};

/// The kinds there are. A closed list keeps the dedup key from splitting.
pub const KINDS: &[&str] = &[
    "turno_fallido",
    "cerebro_caido",
    "plugin_caido",
    "transcripcion_vacia",
    "turno_lento",
    "audio",
    "reportado",
    "revision",
];

/// What an incident is about, for the owner, in a few words.
pub fn kind_title(kind: &str) -> &'static str {
    match kind {
        "turno_fallido" => "un turno que falló",
        "cerebro_caido" => "el cerebro se cayó",
        "plugin_caido" => "un plugin que se cae y vuelve",
        "transcripcion_vacia" => "no te entiendo seguido",
        "turno_lento" => "tardo en contestar",
        "audio" => "un problema con el audio",
        "reportado" => "algo que me reportaste",
        "revision" => "algo que vi en la revisión de la noche",
        _ => "un problema",
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Incidente {
    pub tipo: String,
    pub clave: String,
    pub resumen: String,
    #[serde(default)]
    pub detalle: Value,
    /// Local `AAAA-MM-DDTHH:MM[:SS]`.
    #[serde(default)]
    pub creado: String,
    #[serde(default)]
    pub origen: String,
}

pub fn queue(home: &Path) -> PathBuf {
    home.join(INCIDENTS)
}

pub fn ensure(home: &Path) -> std::io::Result<()> {
    make_queue(&queue(home))
}

/// Checks an incident from a file.
pub fn parse(text: &str) -> Result<Incidente, String> {
    let incidente: Incidente =
        serde_json::from_str(text).map_err(|e| format!("no se entiende: {e}"))?;
    if !KINDS.contains(&incidente.tipo.as_str()) {
        return Err(format!("tipo «{}» desconocido", incidente.tipo));
    }
    if incidente.resumen.trim().is_empty() && incidente.clave.trim().is_empty() {
        return Err("sin resumen ni clave".into());
    }
    Ok(incidente)
}

/// Drops an incident in the queue, the same way the host does.
pub fn dejar(home: &Path, id: &str, incidente: &Incidente) -> std::io::Result<()> {
    if !valid_id(id) {
        return Err(std::io::Error::other("id inválido"));
    }
    let root = queue(home);
    make_queue(&root)?;
    let mut value = serde_json::to_value(incidente).map_err(std::io::Error::other)?;
    value["id"] = json!(id);
    value["version"] = json!(1);
    let tmp = root.join("tmp").join(format!("{id}.json"));
    std::fs::write(&tmp, value.to_string())?;
    std::fs::rename(&tmp, root.join("new").join(format!("{id}.json")))
}

/// Files waiting in `new/`, oldest name first: `(stem, text)`.
pub fn nuevos(home: &Path) -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir(queue(home).join("new")) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|p| {
            let stem = p.file_stem()?.to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&p).ok()?;
            Some((stem, text))
        })
        .collect()
}

/// Moves a file from `new/` to `cur/` with what happened: the ack.
pub fn archivar(home: &Path, stem: &str, original: &str, resultado: &str, problema: &str) {
    let root = queue(home);
    let mut value = serde_json::from_str::<Value>(original)
        .ok()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({ "original": crate::truncate(original, 500) }));
    value["resultado"] = json!(resultado);
    if !problema.is_empty() {
        value["problema"] = json!(problema);
    }
    let tmp = root.join("tmp").join(format!("{stem}.ack.json"));
    let cur = root.join("cur").join(format!("{stem}.json"));
    if std::fs::write(&tmp, value.to_string())
        .and_then(|()| std::fs::rename(&tmp, &cur))
        .is_ok()
    {
        let _ = std::fs::remove_file(root.join("new").join(format!("{stem}.json")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_round_trip_through_the_queue() {
        let home = std::env::temp_dir().join(format!("aa-inc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let inc = Incidente {
            tipo: "turno_lento".into(),
            clave: "cerebro".into(),
            resumen: "primer audio 4210 ms".into(),
            detalle: json!({ "felt_ms": 4210 }),
            creado: "2026-11-21T10:50:04".into(),
            origen: "host".into(),
        };
        dejar(&home, "1-1", &inc).unwrap();
        let found = nuevos(&home);
        assert_eq!(found.len(), 1);
        assert_eq!(parse(&found[0].1).unwrap(), inc);
        archivar(&home, "1-1", &found[0].1, "anotado", "abc");
        assert!(nuevos(&home).is_empty());
        let cur = std::fs::read_to_string(queue(&home).join("cur/1-1.json")).unwrap();
        assert!(cur.contains("\"resultado\":\"anotado\""));
        assert!(parse(r#"{"tipo":"otro","clave":"x","resumen":"y"}"#).is_err());
        let _ = std::fs::remove_dir_all(&home);
    }
}
