//! What the agents read: tight, scrubbed, with the rules in it. Everything
//! that comes from the room (the problem, its samples, a past failure)
//! goes through [`crate::scrub::scrub`] first: it leaves the Mac.

use serde_json::{Value, json};

use crate::diario::Problema;
use crate::gates::FORBIDDEN;
use crate::scrub::scrub;

/// The rules, appended to the author's system prompt.
pub fn reglas_autor(max_lineas: u32, max_archivos: u32) -> String {
    format!(
        "Sos el agente que arregla a Wawi 2, un asistente de voz en Rust para macOS. \
Trabajás SOLO en la carpeta actual, que es un worktree de git aparte. \
Reglas, sin excepción:\n\
- Primero escribí una prueba que reproduzca el problema y FALLE. Después arreglalo y dejala pasando.\n\
- No agregues dependencias ni toques Cargo.toml ni Cargo.lock.\n\
- La velocidad es la regla del dueño: nada que haga esperar a un turno (ni sleep, ni esperas, ni I/O en el núcleo puro). Las pruebas de velocidad (a_turn_never_waits_for_a_respawn y las de punta a punta de wawi-app) tienen que seguir pasando.\n\
- Nunca toques: {}, secrets.toml, .env, LICENSE, .git, ni los permisos de un plugin.toml.\n\
- Cambio chico: como mucho {max_lineas} líneas y {max_archivos} archivos.\n\
- Solo podés usar Read, Edit, Write, Grep, Glob, y Bash para cargo y git diff. No hagas commit: lo hace Wawi después de revisarlo.\n\
- Antes de terminar corré `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings` y `cargo test --workspace`; tienen que quedar verdes. Estás sin red: cargo corre offline.\n\
- Código y comentarios en inglés, textos para el dueño en castellano, como el resto del repo.\n\
- Nada de claves, tokens, mails ni rutas personales (/Users/...) en el código.\n\
- Si para arreglarlo hace falta algo del dueño (un permiso, instalar algo, una decisión), no lo hagas: decilo en `bloqueado`.\n\
Al final contestá con la salida estructurada que se te pide.",
        FORBIDDEN.join(", ")
    )
}

/// The author's answer.
pub fn esquema_autor() -> Value {
    json!({
        "type": "object",
        "properties": {
            "titulo_commit": { "type": "string", "description": "una línea, en castellano" },
            "para_decir": { "type": "string", "description": "qué andaba mal, en primera persona y pocas palabras, como lo diría Wawi (por ejemplo «se me cortaba la voz»)" },
            "causa": { "type": "string", "description": "por qué pasaba, en dos o tres oraciones" },
            "cambios": { "type": "string", "description": "qué cambiaste, en dos o tres oraciones" },
            "pruebas": { "type": "string", "description": "qué prueba nueva reproduce el problema y qué corriste" },
            "bloqueado": { "type": "string", "description": "vacío, o qué necesita del dueño" }
        },
        "required": ["titulo_commit", "para_decir", "causa", "cambios", "pruebas", "bloqueado"]
    })
}

/// The brief, on stdin.
pub fn brief(p: &Problema, fallos_previos: &[String]) -> String {
    let mut out = vec![
        format!("## El problema ({})", p.tipo),
        format!("Título: {}", scrub(&p.titulo)),
        format!("Veces que pasó: {}", p.cuenta),
    ];
    if !p.muestras.is_empty() {
        out.push(String::new());
        out.push("Muestras (datos personales ya tapados):".into());
        for m in p.muestras.iter().rev().take(5) {
            out.push(format!("- {}", crate::truncate(&scrub(m), 400)));
        }
    }
    if p.tipo == "turno_lento" {
        out.push(String::new());
        out.push("Es lentitud: el primer audio tardó mucho más que la mediana. Mirá docs/LATENCIA.md, el camino del turno (crates/wawi-app/src/voice/host.rs, crates/wawi-brain-claude) y qué pudo bloquear o esperar. Una prueba determinista (con relojes o piezas de mentira) vale más que una medición.".into());
    }
    if !fallos_previos.is_empty() {
        out.push(String::new());
        out.push("Intentos anteriores fallaron por esto; no repitas el error:".into());
        for f in fallos_previos.iter().rev().take(5) {
            out.push(format!("- {}", crate::truncate(&scrub(f), 300)));
        }
    }
    out.push(String::new());
    out.push("Leé README.md y docs/ARCHITECTURE.md si te hace falta el contexto. Encontrá la causa, escribí la prueba que falla, arreglalo y dejá todo verde.".into());
    out.join("\n") + "\n"
}

pub fn reglas_revisor() -> &'static str {
    "Sos el revisor de un cambio que otro agente hizo en Wawi 2 (Rust). Solo podés leer: Read, Grep y Glob, en la carpeta actual. Juzgá SOLO corrección y riesgos: ¿arregla el problema de verdad? ¿la prueba nueva lo reproduce? ¿puede romper algo, hacer más lento un turno, colgar un hilo, filtrar datos o saltear un permiso? No opines de estilo. Ante la duda, rechazá. Contestá con la salida estructurada."
}

pub fn esquema_revisor() -> Value {
    json!({
        "type": "object",
        "properties": {
            "aprobado": { "type": "boolean" },
            "motivos": { "type": "array", "items": { "type": "string" }, "maxItems": 8 }
        },
        "required": ["aprobado", "motivos"]
    })
}

pub fn pedido_revisor(p: &Problema, autor: &Value, diff: &str) -> String {
    let field = |k: &str| scrub(autor.get(k).and_then(Value::as_str).unwrap_or(""));
    format!(
        "## El problema\n{}\n\n## Lo que dice el autor\nCausa: {}\nCambios: {}\nPruebas: {}\n\n## El diff (ya está aplicado en esta carpeta)\n```diff\n{}\n```\n",
        scrub(&p.titulo),
        crate::truncate(&field("causa"), 800),
        crate::truncate(&field("cambios"), 800),
        crate::truncate(&field("pruebas"), 800),
        crate::truncate(&scrub(diff), 60_000)
    )
}

/// `(approved, reasons)`. Anything but an explicit `true` is a no.
pub fn veredicto(valor: &Value) -> (bool, Vec<String>) {
    let aprobado = valor.get("aprobado").and_then(Value::as_bool) == Some(true);
    let motivos = valor
        .get("motivos")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(|s| crate::truncate(&scrub(s), 200))
                .take(8)
                .collect()
        })
        .unwrap_or_default();
    (aprobado, motivos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diario::Estado;

    #[test]
    fn the_brief_is_scrubbed_and_names_the_rules() {
        let p = Problema {
            id: "reportado-1".into(),
            tipo: "reportado".into(),
            clave: "x".into(),
            titulo: "se corta la voz, mi mail es ana@gmail.com".into(),
            cuenta: 3,
            primero_ts: 0,
            ultimo_ts: 0,
            muestras: vec!["en /Users/ana/Work con sk-ant-abcdefghijklmnopqrstuvwx".into()],
            estado: Estado::Abierto,
            intentos: Vec::new(),
            propuesta: None,
            cuenta_al_aplicar: 0,
        };
        let b = brief(&p, &["los tests fallan".into()]);
        assert!(
            !b.contains("ana@gmail.com") && !b.contains("sk-ant-abc") && !b.contains("/Users/ana")
        );
        assert!(b.contains("no repitas"));
        let r = reglas_autor(400, 12);
        assert!(
            r.contains("crates/wawi-autoarreglo/")
                && r.contains("prueba")
                && r.contains("dependencias")
        );
    }

    #[test]
    fn only_an_explicit_yes_approves() {
        assert!(veredicto(&json!({ "aprobado": true, "motivos": ["ok"] })).0);
        assert!(!veredicto(&json!({ "aprobado": "sí" })).0);
        assert!(!veredicto(&json!({})).0);
    }
}
