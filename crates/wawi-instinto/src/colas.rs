//! The two file queues, maildir style, shared with Wawi's host:
//! `<home>/cola/preguntas` (questions in) and `<home>/cola/hechos` (facts
//! out). Write to `tmp/`, rename into `new/`; whoever consumes moves the
//! file to `cur/`. Renames are atomic, so nobody sees half a file.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use wawi_plugins::inbox::{FACTS, QUESTIONS, make_queue, valid_id};

use crate::motor::Hecho;

/// Marks a question for cancelling: `<home>/instinto/cancelar/<id>`.
pub const CANCEL_DIR: &str = "instinto/cancelar";

pub fn preguntas(home: &Path) -> PathBuf {
    home.join(QUESTIONS)
}

pub fn hechos(home: &Path) -> PathBuf {
    home.join(FACTS)
}

pub fn ensure(home: &Path) -> std::io::Result<()> {
    make_queue(&preguntas(home))?;
    make_queue(&hechos(home))?;
    std::fs::create_dir_all(home.join(CANCEL_DIR))?;
    std::fs::create_dir_all(crate::estado::dir(home))
}

fn drop_into(root: &Path, id: &str, text: &str) -> std::io::Result<()> {
    make_queue(root)?;
    let tmp = root.join("tmp").join(format!("{id}.json"));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, root.join("new").join(format!("{id}.json")))
}

/// Leaves a question for the worker.
pub fn dejar_pregunta(home: &Path, pregunta: &Value) -> Result<(), String> {
    let id = pregunta
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| valid_id(id))
        .ok_or("la pregunta necesita un id válido")?;
    drop_into(&preguntas(home), id, &pregunta.to_string())
        .map_err(|e| format!("no pude escribir en la cola de preguntas: {e}"))
}

/// Questions waiting in `new/`: `(id from the file name, text)`.
pub fn nuevas(home: &Path) -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir(preguntas(home).join("new")) else {
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

/// Moves a question from `new/` to `cur/`, with what happened to it.
pub fn archivar(home: &Path, stem: &str, original: &str, resultado: &str, motivo: &str, now: &str) {
    let root = preguntas(home);
    let mut value = serde_json::from_str::<Value>(original)
        .ok()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({ "original": crate::texto::truncate(original, 2000) }));
    value["resultado"] = json!(resultado);
    if !motivo.is_empty() {
        value["motivo"] = json!(motivo);
    }
    value["procesado"] = json!(now);
    let tmp = root.join("tmp").join(format!("{stem}.ack.json"));
    let cur = root.join("cur").join(format!("{stem}.json"));
    if std::fs::write(&tmp, value.to_string())
        .and_then(|()| std::fs::rename(&tmp, &cur))
        .is_ok()
    {
        let _ = std::fs::remove_file(root.join("new").join(format!("{stem}.json")));
    }
}

/// Writes a fact for Wawi, once: if a fact with that id is already in
/// `new/` or `cur/`, nothing is written (`Ok(false)`). The id is the
/// host's dedup key, so a retry after a crash never speaks twice.
pub fn escribir_hecho(home: &Path, hecho: &Hecho, creado: &str) -> std::io::Result<bool> {
    let root = hechos(home);
    let name = format!("{}.json", hecho.id);
    if root.join("new").join(&name).exists() || root.join("cur").join(&name).exists() {
        return Ok(false);
    }
    let mut value = json!({
        "id": hecho.id,
        "origen": "instinto",
        "decir": hecho.decir,
        "contexto": hecho.contexto,
        "urgencia": "normal",
        "creado": creado,
    });
    if let Some(vence) = &hecho.vence {
        value["vence"] = json!(vence);
    }
    drop_into(&root, &hecho.id, &value.to_string())?;
    Ok(true)
}

pub fn pedir_cancelar(home: &Path, id: &str) -> std::io::Result<()> {
    let dir = home.join(CANCEL_DIR);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(id), "")
}

/// Ids marked for cancelling.
pub fn cancelaciones(home: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(home.join(CANCEL_DIR)) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|id| valid_id(id))
        .collect()
}

pub fn cancelada(home: &Path, id: &str) {
    let _ = std::fs::remove_file(home.join(CANCEL_DIR).join(id));
}
