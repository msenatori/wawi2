//! Autoarreglo: Wawi fixes herself, but only by proposing.
//!
//! - **Detection.** The host drops incidents into `<home>/cola/incidentes`
//!   (maildir: `tmp/`, `new/`, `cur/`): a failed turn, the brain dying, a
//!   plugin crash loop, transcripts that keep coming back empty, a turn far
//!   over its latency baseline, an audio failure. The owner reports one with
//!   `reportar_problema`. A nightly review reads the day's logs and may file
//!   up to two more. The worker folds them into a journal ([`diario`]),
//!   deduplicated by kind and fingerprint, with a counter.
//! - **Work** ([`intento`]). The most useful open problem, a git worktree
//!   outside the repo on `autoarreglo/<id>`, one headless Opus run limited
//!   to that worktree and cargo ([`motor`]), with a tight, scrubbed brief
//!   ([`brief`], [`scrub`]). The agent never commits; the service does.
//! - **Gates** ([`gates`]). Forbidden paths, size, no dependencies, a secret
//!   scan, a static hot-path guard, fmt, clippy, the full test suite, the
//!   speed tests by name, and a second, read-only Opus review. The first
//!   failure discards the attempt and is remembered.
//! - **Proposal** ([`propuestas`]). `<home>/autoarreglo/propuestas/<id>/`
//!   and a fact in `cola/hechos`. Applying is the owner's call: one squash
//!   commit with an `Autoarreglo:` trailer, only on a clean tree. A watchdog
//!   ([`vigia`]) offers a revert if the next run looks bad.
//!
//! The pure parts take a clock and fakes; docs/AUTOARREGLO.md is the
//! reference.

pub mod brief;
pub mod clock;
pub mod config;
pub mod diario;
pub mod elegir;
pub mod gates;
pub mod git;
pub mod incidentes;
pub mod intento;
pub mod log;
pub mod motor;
pub mod propuestas;
pub mod revision;
pub mod scrub;
pub mod servidor;
pub mod vigia;
pub mod worker;

use std::path::{Path, PathBuf};

/// `$WAWI2_HOME`, else `~/.wawi2`.
pub fn home() -> PathBuf {
    if let Some(home) = std::env::var_os("WAWI2_HOME").filter(|h| !h.is_empty()) {
        return PathBuf::from(home);
    }
    let user = std::env::var_os("HOME").unwrap_or_else(|| ".".into());
    PathBuf::from(user).join(".wawi2")
}

/// Where the service keeps its own files: `<home>/autoarreglo`.
pub const DIR: &str = "autoarreglo";

pub fn dir(home: &Path) -> PathBuf {
    home.join(DIR)
}

/// Writes `text` to `path` through a temp file and a rename, so a reader
/// never sees half a file.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.tmp"));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// At most `max` characters, with "…" when cut.
pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}
