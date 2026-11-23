//! The «carácter» plugin: Wawi learns the TRATO, how to talk to THIS owner.
//! Style, not facts (facts stay in MEMORY.md).
//!
//! - The host sends every finished turn (`observar_charla`, with the
//!   `interrumpida` flag). Only turns with a cheap signal (an interruption,
//!   a correction cue, laughter) reach the model ([`aprender::vale_mirar`]).
//! - One headless `claude -p` call per such turn: no tools, structured
//!   output only, proposing `{op: agregar|cambiar|quitar, regla, evidencia}`.
//! - CODE validates every proposal ([`aprender::validar`]): the evidence is
//!   the owner's own words or a real interruption, never Wawi's text; a rule
//!   needs repeated evidence unless it is an explicit correction; at most 12
//!   rules and 600 characters; near-duplicates merge; every change is logged
//!   with its evidence.
//! - The result is `<home>/trato.md`, which the owner reads, edits or
//!   deletes. Wawi's memory block carries it to the brain, so a change never
//!   touches the system prompt (see docs/PERSONALIDAD.md).
//!
//! Off by default: the plugin ships with `observa_charla = false`.

pub mod aprender;
pub mod libro;
pub mod log;
pub mod servidor;

use std::path::PathBuf;

/// `$WAWI2_HOME`, else `~/.wawi2`.
pub fn home() -> PathBuf {
    if let Some(home) = std::env::var_os("WAWI2_HOME").filter(|h| !h.is_empty()) {
        return PathBuf::from(home);
    }
    let user = std::env::var_os("HOME").unwrap_or_else(|| ".".into());
    PathBuf::from(user).join(".wawi2")
}

/// Where the plugin keeps its own files: `<home>/caracter`.
pub const DIR: &str = "caracter";
/// The model and effort of the learner.
pub const MODELO: &str = "claude-sonnet-5-5";
pub const ESFUERZO: &str = "low";
