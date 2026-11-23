//! The "instinto" service. Intelligence lives only at the two ends; the
//! middle is dumb, declarative and testable:
//!
//! - Wawi (through the `vigilar` tool) or the observer leaves a follow-up
//!   question in `<home>/cola/preguntas` (maildir: `tmp/`, `new/`, `cur/`).
//! - The worker ([`worker`]) runs each question's `vigia` with no model:
//!   a source ([`fuentes`]), an extraction ([`extraer`]), a predicate
//!   ([`cuando`]) and, optionally, a cheap yes/no judge ([`juez`]).
//! - When it comes true, the worker writes a fact into `<home>/cola/hechos`.
//!   Wawi's host already reads that queue, passes it through the instinct
//!   gate, and Wawi speaks.
//!
//! The pure parts (spec parsing, evaluation, state transitions, the
//! observer's validation) have no I/O and are tested with fakes and a fake
//! clock. docs/INSTINTO.md is the reference.

pub mod claude;
pub mod clock;
pub mod colas;
pub mod config;
pub mod cuando;
pub mod estado;
pub mod extraer;
pub mod fuentes;
pub mod jsonpath;
pub mod juez;
pub mod log;
pub mod motor;
pub mod observador;
pub mod servidor;
pub mod spec;
pub mod texto;
pub mod worker;

use std::path::PathBuf;

/// `$WAWI2_HOME`, else `~/.wawi2`.
pub fn home() -> PathBuf {
    if let Some(home) = std::env::var_os("WAWI2_HOME").filter(|h| !h.is_empty()) {
        return PathBuf::from(home);
    }
    let user = std::env::var_os("HOME").unwrap_or_else(|| ".".into());
    PathBuf::from(user).join(".wawi2")
}

/// Where the service keeps its own files: `<home>/instinto`.
pub const DIR: &str = "instinto";

/// The tool the host calls with each finished turn.
pub fn observador_tool() -> &'static str {
    wawi_plugins::manifest::FEED_TOOL
}
