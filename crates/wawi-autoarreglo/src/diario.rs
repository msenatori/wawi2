//! The journal: one problem per file, `<home>/autoarreglo/problemas/<id>.json`,
//! where `id` is a fingerprint of kind plus key. A repeat bumps the counter
//! instead of making another problem (Wawi 1's rule: «esto pasó 40 veces
//! hoy» is a different report from «pasó una vez»). Everything stored is
//! scrubbed, and only a few short samples are kept.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::incidentes::Incidente;
use crate::scrub::scrub;

pub const DIR: &str = "autoarreglo/problemas";
/// Samples kept per problem.
pub const MAX_MUESTRAS: usize = 5;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Estado {
    #[default]
    Abierto,
    /// A tested fix waits for the owner.
    Propuesto,
    Aplicado,
    /// The owner said no: never again.
    Descartado,
}

impl Estado {
    pub fn nombre(self) -> &'static str {
        match self {
            Estado::Abierto => "abierto",
            Estado::Propuesto => "propuesto",
            Estado::Aplicado => "aplicado",
            Estado::Descartado => "descartado",
        }
    }
}

/// One attempt on a problem, and how it ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Intento {
    pub id: String,
    pub ts: i64,
    /// "propuesto", "rechazado", "error", "base_roja".
    pub resultado: String,
    #[serde(default)]
    pub motivo: String,
    #[serde(default)]
    pub costo_usd: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Problema {
    pub id: String,
    pub tipo: String,
    pub clave: String,
    pub titulo: String,
    pub cuenta: u32,
    pub primero_ts: i64,
    pub ultimo_ts: i64,
    #[serde(default)]
    pub muestras: Vec<String>,
    #[serde(default)]
    pub estado: Estado,
    #[serde(default)]
    pub intentos: Vec<Intento>,
    #[serde(default)]
    pub propuesta: Option<String>,
    /// The count when a fix went in: it counts as back only past this.
    #[serde(default)]
    pub cuenta_al_aplicar: u32,
}

impl Problema {
    /// The last failed attempt's time.
    pub fn ultimo_fallo(&self) -> Option<i64> {
        self.intentos
            .iter()
            .filter(|i| i.resultado != "propuesto")
            .map(|i| i.ts)
            .max()
    }

    pub fn reportado(&self) -> bool {
        self.tipo == "reportado"
    }
}

/// Lowercase, spaces collapsed, digits as `#`: so "turno 12" and "turno 13"
/// are one problem.
pub fn normalize(text: &str) -> String {
    let lower: String = text
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_digit() { '#' } else { c })
        .collect();
    lower.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// FNV-1a, 64 bits: stable across runs and platforms, no dependency.
fn fnv1a(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// The problem's id: `<tipo>-<12 hex>`.
pub fn fingerprint(tipo: &str, clave: &str) -> String {
    let hash = fnv1a(&format!("{tipo}|{}", normalize(clave)));
    format!("{tipo}-{:012x}", hash & 0xffff_ffff_ffff)
}

pub fn dir(home: &Path) -> PathBuf {
    home.join(DIR)
}

fn path(home: &Path, id: &str) -> PathBuf {
    dir(home).join(format!("{id}.json"))
}

pub fn load(home: &Path, id: &str) -> Option<Problema> {
    let text = std::fs::read_to_string(path(home, id)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn save(home: &Path, problema: &Problema) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(problema).map_err(std::io::Error::other)?;
    crate::write_atomic(&path(home, &problema.id), &text)
}

pub fn load_all(home: &Path) -> Vec<Problema> {
    let Ok(entries) = std::fs::read_dir(dir(home)) else {
        return Vec::new();
    };
    let mut out: Vec<Problema> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .filter_map(|t| serde_json::from_str(&t).ok())
        .collect();
    out.sort_by(|a: &Problema, b| a.id.cmp(&b.id));
    out
}

/// The title of a new problem: the kind and the gist, scrubbed.
fn title(incidente: &Incidente) -> String {
    let gist = if incidente.resumen.trim().is_empty() {
        incidente.clave.trim()
    } else {
        incidente.resumen.trim()
    };
    let head = crate::incidentes::kind_title(&incidente.tipo);
    crate::truncate(&scrub(&format!("{head}: {gist}")), 120)
}

/// Folds one incident in: a new problem, or one more on an existing one.
/// Returns the problem as it is now.
pub fn absorb(home: &Path, incidente: &Incidente, ts: i64) -> std::io::Result<Problema> {
    let id = fingerprint(&incidente.tipo, &incidente.clave);
    let mut detail = scrub(incidente.resumen.trim());
    if !incidente.detalle.is_null() {
        detail.push_str(&format!(" {}", scrub(&incidente.detalle.to_string())));
    }
    let sample = crate::truncate(&format!("{}: {detail}", incidente.creado), 400);
    let problema = match load(home, &id) {
        Some(mut p) => {
            p.cuenta = p.cuenta.saturating_add(1);
            p.ultimo_ts = p.ultimo_ts.max(ts);
            p.muestras.push(sample);
            let extra = p.muestras.len().saturating_sub(MAX_MUESTRAS);
            p.muestras.drain(..extra);
            p
        }
        None => Problema {
            id: id.clone(),
            tipo: incidente.tipo.clone(),
            clave: crate::truncate(&scrub(&normalize(&incidente.clave)), 200),
            titulo: title(incidente),
            cuenta: 1,
            primero_ts: ts,
            ultimo_ts: ts,
            muestras: vec![sample],
            estado: Estado::Abierto,
            intentos: Vec::new(),
            propuesta: None,
            cuenta_al_aplicar: 0,
        },
    };
    save(home, &problema)?;
    Ok(problema)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn incident(resumen: &str) -> Incidente {
        Incidente {
            tipo: "turno_fallido".into(),
            clave: "claude falló en el turno 12".into(),
            resumen: resumen.into(),
            detalle: json!({ "turno": 12 }),
            creado: "2026-11-21T10:00".into(),
            origen: "host".into(),
        }
    }

    #[test]
    fn repeats_bump_one_counter() {
        let home = std::env::temp_dir().join(format!("aa-diario-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let a = absorb(&home, &incident("falló"), 10).unwrap();
        let mut other = incident("falló otra vez, clave sk-ant-abcdefghijklmnopqrstu");
        other.clave = "claude falló en el turno 13".into();
        let b = absorb(&home, &other, 20).unwrap();
        assert_eq!(a.id, b.id, "digits do not split a problem");
        assert_eq!(b.cuenta, 2);
        assert_eq!((b.primero_ts, b.ultimo_ts), (10, 20));
        assert!(b.muestras.iter().all(|m| !m.contains("sk-ant-abc")));
        for i in 0..10 {
            absorb(&home, &incident("x"), 30 + i).unwrap();
        }
        assert_eq!(load(&home, &a.id).unwrap().muestras.len(), MAX_MUESTRAS);
        assert_eq!(load_all(&home).len(), 1);
        assert_ne!(
            fingerprint("turno_lento", "cerebro"),
            fingerprint("audio", "cerebro")
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}
