//! Failure notices («no pude terminar»): when background work fails in a
//! way the owner should know about, a fact goes into `<home>/cola/hechos`
//! and Wawi tells the owner once, briefly, through the instinct gate like
//! any other fact (quiet hours, pacing, dedup).
//!
//! - **One per plugin per day**: the fact id is `aviso-<plugin>-<AAAAMMDD>`,
//!   and the inbox never says an id twice.
//! - **What counts**: a plugin that needs the owner to sign in again (a
//!   refused or revoked grant, not one never given), a plugin that keeps
//!   crashing (3 downs in 10 minutes), autoarreglo that could not run.
//!   An instinto follow-up that broke already leaves its own fact
//!   (`<id>.rota`).
//! - **The text is data**: `decir` says what happened; the instruction for
//!   the brain is Wawi's fixed one for `origen: "aviso"`
//!   (`inbox::NOTICE_INSTRUCTION`).
//!
//! Writing the fact is a small file write on whatever thread noticed; the
//! reading and the gate are the inbox's, every 2 s.

use std::collections::{HashMap, VecDeque};
use std::path::Path;

use serde_json::{Value, json};
use wawi_tools::{Civil, WallClock};

use crate::inbox::{FACTS, make_queue, valid_id};

/// `origen` of failure notices.
pub const ORIGIN: &str = "aviso";
/// `origen` of the "I'm back" fact after the usage limit.
pub const BACK_ORIGIN: &str = "cupo";
/// How long a notice may wait for the gate before it is stale.
pub const NOTICE_TTL_S: i64 = 24 * 3600;
/// Downs that make a crash loop, and in how long.
pub const CRASH_DOWNS: usize = 3;
pub const CRASH_WINDOW_S: i64 = 600;

/// A fact for the inbox.
#[derive(Clone, Debug, PartialEq)]
pub struct Fact {
    pub id: String,
    pub origin: String,
    pub say: String,
    pub context: Option<Value>,
    /// `alta`, `normal` or `baja`.
    pub urgency: &'static str,
    /// Not before this Unix second.
    pub not_before: Option<i64>,
    /// Not after this Unix second.
    pub expires: Option<i64>,
}

fn local(clock: &dyn WallClock, epoch: i64) -> String {
    clock.local(epoch).iso_minute()
}

/// Drops the fact in `<home>/cola/hechos/new` (through `tmp/`), unless its
/// id is already there or in `cur/`: never twice. `Ok(true)` if written.
pub fn drop_fact(home: &Path, fact: &Fact, clock: &dyn WallClock) -> std::io::Result<bool> {
    if !valid_id(&fact.id) {
        return Err(std::io::Error::other("id inválido"));
    }
    let root = home.join(FACTS);
    make_queue(&root)?;
    let name = format!("{}.json", fact.id);
    if root.join("new").join(&name).exists() || root.join("cur").join(&name).exists() {
        return Ok(false);
    }
    let now = clock.now();
    let mut value = json!({
        "id": fact.id,
        "origen": fact.origin,
        "decir": fact.say,
        "urgencia": fact.urgency,
        "creado": format!("{}:{:02}", clock.local(now).iso_minute(), clock.local(now).second),
    });
    if let Some(context) = &fact.context {
        value["contexto"] = context.clone();
    }
    if let Some(at) = fact.not_before {
        value["desde"] = json!(local(clock, at));
    }
    if let Some(at) = fact.expires {
        value["vence"] = json!(local(clock, at));
    }
    let tmp = root.join("tmp").join(&name);
    std::fs::write(&tmp, value.to_string())?;
    std::fs::rename(&tmp, root.join("new").join(&name))?;
    Ok(true)
}

/// `aviso-<plugin>-<AAAAMMDD>`: one notice per plugin per local day.
pub fn notice_id(plugin: &str, today: &Civil) -> String {
    format!(
        "aviso-{plugin}-{:04}{:02}{:02}",
        today.year, today.month, today.day
    )
}

/// A notice about `plugin`: `say` is what happened, for the owner. At most
/// one per plugin per day; `Ok(false)` when today's was already left.
pub fn plugin_notice(
    home: &Path,
    plugin: &str,
    say: &str,
    clock: &dyn WallClock,
) -> std::io::Result<bool> {
    let now = clock.now();
    drop_fact(
        home,
        &Fact {
            id: notice_id(plugin, &clock.local(now)),
            origin: ORIGIN.into(),
            say: say.into(),
            context: Some(json!({ "plugin": plugin })),
            urgency: "normal",
            not_before: None,
            expires: Some(now + NOTICE_TTL_S),
        },
        clock,
    )
}

/// The "I'm back" fact after the usage limit, when the owner had been told.
pub fn back_notice(home: &Path, clock: &dyn WallClock) -> std::io::Result<bool> {
    let now = clock.now();
    drop_fact(
        home,
        &Fact {
            id: format!("cupo-volvi-{now}"),
            origin: BACK_ORIGIN.into(),
            say: "Se terminó el límite de uso y ya volví.".into(),
            context: None,
            urgency: "normal",
            not_before: None,
            // Hours later it is no news: she just works again.
            expires: Some(now + 6 * 3600),
        },
        clock,
    )
}

/// Whether a plugin's sign-in failure is worth a notice: refused or
/// revoked, not a grant the owner never gave.
pub fn auth_is_news(reason: &str) -> bool {
    let reason = reason.to_lowercase();
    !(reason.contains("falta la clave") || reason.contains("todavía no le diste permiso"))
}

/// Counts plugin downs to spot a crash loop. Pure: the time comes in.
#[derive(Debug, Default)]
pub struct CrashWatch {
    downs: HashMap<String, VecDeque<i64>>,
}

impl CrashWatch {
    /// A down at `now` (Unix seconds). True when it makes a crash loop;
    /// the count starts over after that.
    pub fn down(&mut self, plugin: &str, now: i64) -> bool {
        let downs = self.downs.entry(plugin.to_owned()).or_default();
        downs.push_back(now);
        while downs
            .front()
            .is_some_and(|at| now - at > CRASH_WINDOW_S || *at > now)
        {
            downs.pop_front();
        }
        if downs.len() >= CRASH_DOWNS {
            downs.clear();
            return true;
        }
        false
    }

    /// It connected: a fresh start.
    pub fn up(&mut self, plugin: &str) {
        self.downs.remove(plugin);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wawi_tools::FakeClock;

    fn home(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wawi-avisos-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn one_notice_per_plugin_per_day() {
        let home = home("dia");
        let clock = FakeClock::at(Civil::date(2026, 11, 24).at(15, 10, 0), -3);
        assert!(
            plugin_notice(
                &home,
                "linear",
                "Linear pide permiso de nuevo.",
                clock.as_ref()
            )
            .unwrap()
        );
        assert!(!plugin_notice(&home, "linear", "otra vez", clock.as_ref()).unwrap());
        assert!(plugin_notice(&home, "github", "GitHub se cae.", clock.as_ref()).unwrap());
        let text = std::fs::read_to_string(home.join(FACTS).join("new/aviso-linear-20261124.json"))
            .unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["origen"], ORIGIN);
        assert_eq!(value["decir"], "Linear pide permiso de nuevo.");
        assert_eq!(value["vence"], "2026-11-25T15:10");
        // Said (moved to cur/): still not again today; tomorrow yes.
        std::fs::rename(
            home.join(FACTS).join("new/aviso-linear-20261124.json"),
            home.join(FACTS).join("cur/aviso-linear-20261124.json"),
        )
        .unwrap();
        assert!(!plugin_notice(&home, "linear", "x", clock.as_ref()).unwrap());
        clock.advance(24 * 3600);
        assert!(plugin_notice(&home, "linear", "x", clock.as_ref()).unwrap());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn only_a_refused_or_revoked_grant_is_news() {
        assert!(auth_is_news("el servidor pide permiso (HTTP 401)"));
        assert!(auth_is_news("el servidor rechazó la clave (HTTP 403)"));
        assert!(!auth_is_news("todavía no le diste permiso"));
        assert!(!auth_is_news("falta la clave"));
    }

    #[test]
    fn three_downs_in_ten_minutes_is_a_crash_loop() {
        let mut watch = CrashWatch::default();
        assert!(!watch.down("x", 0));
        assert!(!watch.down("x", 700));
        assert!(!watch.down("x", 710));
        assert!(watch.down("x", 720));
        assert!(!watch.down("x", 730), "counted from scratch after a loop");
        watch.up("x");
        assert!(!watch.down("x", 740));
        assert!(!watch.down("x", 741));
        assert!(!watch.down("y", 742));
    }
}
