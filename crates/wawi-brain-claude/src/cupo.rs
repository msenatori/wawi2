//! `<home>/cupo.json`: the usage limit, shared with the background services
//! (instinto's judge and observer, carácter, curiosidad, autoarreglo), so
//! they stop calling `claude -p` until the reset instead of failing over and
//! over. A file and not a message: they are other processes, and they only
//! look at it right before a call they were going to make anyway (no
//! polling).
//!
//! ```json
//! { "hasta": 1764007260, "sabido": true, "desde": 1764001800, "origen": "wawi" }
//! ```
//!
//! `hasta` is when to try again (Unix seconds); `sabido` says whether that is
//! the reset the CLI gave or a guess. The voice host writes it when the limit
//! starts and removes it when it ends; a service that runs into the limit on
//! its own writes it too (unless a later `hasta` is already there). A file
//! whose `hasta` passed means nothing, so a stale one never blocks anyone.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::limit::{self, Limit};

pub const FILE: &str = "cupo.json";
/// The wait a service writes when the CLI did not say when it resets.
pub const GUESS_S: u64 = 30 * 60;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cupo {
    /// When to try again, Unix seconds.
    pub hasta: u64,
    /// `hasta` is the CLI's reset, not a guess.
    pub sabido: bool,
    pub desde: u64,
    /// Who wrote it: `wawi`, `instinto`, `caracter`…
    pub origen: String,
}

pub fn path(home: &Path) -> PathBuf {
    home.join(FILE)
}

pub fn read(home: &Path) -> Option<Cupo> {
    let text = std::fs::read_to_string(path(home)).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    Some(Cupo {
        hasta: value.get("hasta")?.as_u64()?,
        sabido: value
            .get("sabido")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        desde: value.get("desde").and_then(Value::as_u64).unwrap_or(0),
        origen: value
            .get("origen")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
    })
}

/// The limit, if one holds at `now` (Unix seconds).
pub fn active(home: &Path, now: u64) -> Option<Cupo> {
    read(home).filter(|cupo| cupo.hasta > now)
}

/// Writes it whole (a temporary file, then a rename).
pub fn write(home: &Path, cupo: &Cupo) -> std::io::Result<()> {
    let text = json!({
        "hasta": cupo.hasta,
        "sabido": cupo.sabido,
        "desde": cupo.desde,
        "origen": cupo.origen,
    })
    .to_string();
    let tmp = home.join(format!(".{FILE}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path(home))
}

pub fn clear(home: &Path) {
    let _ = std::fs::remove_file(path(home));
}

/// A service ran into the limit: write it, unless the file already holds
/// a later try. Returns what holds now.
pub fn note(home: &Path, limit: Limit, now: u64, origen: &str) -> Cupo {
    let cupo = Cupo {
        hasta: limit
            .resets_at
            .filter(|at| *at > now)
            .unwrap_or(now + GUESS_S),
        sabido: limit.resets_at.is_some_and(|at| at > now),
        desde: now,
        origen: origen.to_owned(),
    };
    match active(home, now) {
        Some(held) if held.hasta >= cupo.hasta => held,
        _ => {
            let _ = write(home, &cupo);
            cupo
        }
    }
}

/// The usage limit in the output of `claude -p --output-format json` (one
/// `result` object), if that is why it failed.
pub fn in_output(text: &str) -> Option<Limit> {
    let value: Value = serde_json::from_str(text.trim()).ok()?;
    limit::from_value(&value)
}

/// Now, in Unix seconds.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// What a service tells its log (and the brain, as a tool failure) when it
/// skips a call for the limit.
pub fn skipped(cupo: &Cupo, now: u64) -> String {
    let minutes = cupo.hasta.saturating_sub(now).div_ceil(60);
    format!("sin cupo de Claude: vuelvo a probar en {minutes} min")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "wawi-cupo-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_service_notes_the_limit_and_others_see_it_until_it_passes() {
        let home = home();
        assert_eq!(active(&home, 100), None);
        let noted = note(
            &home,
            Limit {
                resets_at: Some(5_000),
            },
            1_000,
            "instinto",
        );
        assert_eq!(noted.hasta, 5_000);
        assert!(noted.sabido);
        assert_eq!(active(&home, 4_999).unwrap().origen, "instinto");
        assert_eq!(active(&home, 5_000), None, "a passed one means nothing");
        // A guess never shortens a later known reset.
        let kept = note(&home, Limit::default(), 1_100, "caracter");
        assert_eq!((kept.hasta, kept.origen.as_str()), (5_000, "instinto"));
        clear(&home);
        let guessed = note(&home, Limit::default(), 1_100, "caracter");
        assert_eq!(guessed.hasta, 1_100 + GUESS_S);
        assert!(!guessed.sabido);
        assert_eq!(
            skipped(&guessed, 1_100),
            "sin cupo de Claude: vuelvo a probar en 30 min"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn reads_the_limit_in_a_json_output() {
        assert_eq!(
            in_output(
                r#"{"type":"result","is_error":true,"api_error":"usage_limit_reached","result":"x"}"#
            ),
            Some(Limit::default())
        );
        assert_eq!(
            in_output(r#"{"type":"result","is_error":true,"result":"boom"}"#),
            None
        );
        assert_eq!(in_output("no json"), None);
    }
}
