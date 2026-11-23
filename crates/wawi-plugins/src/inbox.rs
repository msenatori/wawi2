//! The file inbox: any process, in any language, can hand Wawi something
//! worth saying by dropping a JSON file in `<home>/cola/hechos/`, maildir
//! style. Write `tmp/<id>.json`, then rename it into `new/`. The host polls
//! `new/` every couple of seconds, checks each item and offers it to the
//! core's instinct gate. When Wawi said it, or when it will never be said
//! (refused by the gate, expired, invalid, a repeat), the file moves to
//! `cur/` with the outcome written into it: that is the acknowledgement.
//!
//! Delivery is at least once: an item still in `new/` when Wawi closes is
//! read again at the next start. An id already in `cur/` is a repeat.
//!
//! `<home>/cola/preguntas/` has the same layout for the other direction
//! (follow-up questions for an outside worker). The host only creates it.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use wawi_core::{DiagnosticReason, InstinctCandidate, Priority, TurnId};
use wawi_tools::clock::parse_local;
use wawi_tools::{Civil, WallClock};

use crate::manifest::truncate;

pub const FACTS: &str = "cola/hechos";
pub const QUESTIONS: &str = "cola/preguntas";
/// The incident journal's inbox (the host's problems, for autoarreglo).
/// The host only writes it; docs/AUTOARREGLO.md has the format.
pub const INCIDENTS: &str = "cola/incidentes";
/// What the brain is told to do with an inbox item. Fixed on purpose: the
/// item's own text is data, never the instruction.
pub const INBOX_INSTRUCTION: &str = "Un proceso de fondo dejó este aviso para el dueño. Si vale la pena, decíselo en una o dos frases, con tus palabras. Si no, no digas nada.";
/// The longest `decir` plus `contexto` that reaches the brain.
const MAX_TEXT: usize = 4000;

/// A valid item, as the producer wrote it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub origin: String,
    pub say: String,
    pub context: String,
    pub priority: Priority,
    /// `urgencia: "alta"`: skips the gate's gap and hourly budget.
    pub urgent: bool,
    pub expires: Option<i64>,
}

/// Item ids: letters, digits, `.`, `_` and `-`, up to 64.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Reads one item. `vence` is a local time (`YYYY-MM-DDTHH:MM[:SS]`).
pub fn parse_item(text: &str, clock: &dyn WallClock) -> Result<Item, String> {
    let value: Value = serde_json::from_str(text).map_err(|_| "no es JSON".to_owned())?;
    let field = |name: &str| value.get(name).and_then(Value::as_str).map(str::trim);
    let id = field("id").ok_or("falta id")?;
    if !valid_id(id) {
        return Err("id inválido".into());
    }
    let say = field("decir")
        .filter(|s| !s.is_empty())
        .ok_or("falta decir")?;
    let origin = field("origen")
        .filter(|s| !s.is_empty())
        .unwrap_or("externo");
    let priority = match field("urgencia").unwrap_or("normal") {
        "alta" | "normal" => Priority::High,
        "baja" => Priority::Low,
        _ => return Err("urgencia tiene que ser alta, normal o baja".into()),
    };
    let context = match value.get("contexto") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    };
    let expires = match field("vence") {
        None | Some("") => None,
        Some(text) => {
            let local = parse_local(text).ok_or("vence no es una hora local AAAA-MM-DDTHH:MM")?;
            Some(
                clock
                    .epoch_of(local)
                    .ok_or("vence no existe en este huso")?,
            )
        }
    };
    Ok(Item {
        urgent: field("urgencia") == Some("alta"),
        id: id.to_owned(),
        origin: origin.to_owned(),
        say: say.to_owned(),
        context,
        priority,
        expires,
    })
}

impl Item {
    /// `now` is the Unix second, for the deadline.
    pub fn candidate(&self, local_minute: u16, now: i64) -> InstinctCandidate {
        let mut data = format!("Aviso de {}: {}", self.origin, self.say);
        if !self.context.trim().is_empty() {
            data.push_str(&format!("\nContexto: {}", self.context.trim()));
        }
        InstinctCandidate {
            id: format!("cola:{}", self.id),
            key: format!("cola/{}/{}", self.origin, self.id),
            say: INBOX_INSTRUCTION.into(),
            context: truncate(&data, MAX_TEXT),
            priority: self.priority,
            cooldown_ms: None,
            local_minute,
            urgent: self.urgent,
            expires_in_ms: self
                .expires
                .map(|at| (at.saturating_sub(now).max(0) as u64) * 1000),
        }
    }
}

/// Creates `<root>/{tmp,new,cur}`.
pub fn make_queue(root: &Path) -> std::io::Result<()> {
    for sub in ["tmp", "new", "cur"] {
        std::fs::create_dir_all(root.join(sub))?;
    }
    Ok(())
}

pub struct Inbox {
    root: PathBuf,
    /// Candidate id to item id, for items the gate still has.
    offered: HashMap<String, String>,
    /// Initiative turns started from an item, waiting to speak.
    turns: HashMap<TurnId, String>,
    /// Items the gate held back for timing: item id to the Unix second to
    /// try again and why. Kept in `espera.json`, so a restart honors it.
    waiting: BTreeMap<String, Retry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Retry {
    pub at: i64,
    pub reason: String,
}

/// Where postponed items' retry times live.
pub const WAITING_FILE: &str = "espera.json";

/// What a scan or an outcome left in the log.
pub type Notes = Vec<String>;

impl Inbox {
    /// Opens `<home>/cola/hechos` and creates both queues.
    pub fn open(home: &Path) -> std::io::Result<Self> {
        let root = home.join(FACTS);
        make_queue(&root)?;
        make_queue(&home.join(QUESTIONS))?;
        let waiting = std::fs::read_to_string(root.join(WAITING_FILE))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Ok(Self {
            root,
            offered: HashMap::new(),
            turns: HashMap::new(),
            waiting,
        })
    }

    /// New items, as candidates for the gate. Invalid, expired and
    /// repeated ones go straight to `cur/`; postponed ones wait for their
    /// retry time.
    pub fn scan(&mut self, clock: &dyn WallClock, notes: &mut Notes) -> Vec<InstinctCandidate> {
        let Ok(entries) = std::fs::read_dir(self.root.join("new")) else {
            return Vec::new();
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        paths.sort();
        let now = clock.now();
        let minute = crate::schedule::minute_of_day(&clock.local_now());
        let mut found = Vec::new();
        for path in paths {
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if self.offered.values().any(|id| *id == stem)
                || self.turns.values().any(|id| *id == stem)
            {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let item = match parse_item(&text, clock) {
                Ok(item) if item.id != stem => {
                    Err("el id no coincide con el nombre del archivo".into())
                }
                other => other,
            };
            match item {
                Err(reason) => {
                    notes.push(format!("cola: {stem} inválido ({reason})"));
                    self.finish_file(&stem, &text, "invalido", &reason, clock.local_now());
                }
                Ok(item)
                    if self
                        .root
                        .join("cur")
                        .join(format!("{}.json", item.id))
                        .exists() =>
                {
                    notes.push(format!("cola: {} repetido", item.id));
                    let _ = std::fs::remove_file(&path);
                }
                Ok(item) if item.expires.is_some_and(|at| at <= now) => {
                    notes.push(format!("cola: {} vencido", item.id));
                    let why = self
                        .waiting
                        .get(&item.id)
                        .map_or_else(String::new, |w| format!("esperaba por {}", w.reason));
                    self.finish_file(&item.id, &text, "vencido", &why, clock.local_now());
                }
                Ok(item) if self.waiting.get(&item.id).is_some_and(|w| w.at > now) => {}
                Ok(item) => {
                    let candidate = item.candidate(minute, now);
                    self.offered.insert(candidate.id.clone(), item.id.clone());
                    found.push(candidate);
                }
            }
        }
        found
    }

    /// Whether `candidate_id` is one of the inbox's.
    pub fn owns(&self, candidate_id: &str) -> bool {
        self.offered.contains_key(candidate_id)
    }

    /// The gate started an initiative turn for it.
    pub fn started(&mut self, candidate_id: &str, turn: TurnId) {
        if let Some(id) = self.offered.remove(candidate_id) {
            self.turns.insert(turn, id);
        }
    }

    /// The gate held it back for timing: it stays in `new/` and is offered
    /// again at `retry_at` (a Unix second), until its `vence`.
    pub fn postponed(&mut self, candidate_id: &str, reason: DiagnosticReason, retry_at: i64) {
        if let Some(id) = self.offered.remove(candidate_id) {
            self.waiting.insert(
                id,
                Retry {
                    at: retry_at,
                    reason: format!("{reason:?}"),
                },
            );
            self.save_waiting();
        }
    }

    /// The retry times of postponed items.
    pub fn waiting(&self) -> &BTreeMap<String, Retry> {
        &self.waiting
    }

    /// The gate dropped it for good: acknowledged with the reason. A passed
    /// deadline is `vencido`; a repeat or an empty item, `descartado`.
    pub fn dropped(&mut self, candidate_id: &str, reason: DiagnosticReason, now: Civil) {
        if let Some(id) = self.offered.remove(candidate_id) {
            let result = if reason == DiagnosticReason::InstinctPastDue {
                "vencido"
            } else {
                "descartado"
            };
            self.finish(&id, result, &format!("{reason:?}"), now);
        }
    }

    fn save_waiting(&self) {
        let path = self.root.join(WAITING_FILE);
        let tmp = self.root.join("tmp").join("espera.json.tmp");
        if let Ok(text) = serde_json::to_string_pretty(&self.waiting)
            && std::fs::write(&tmp, text).is_ok()
        {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    /// Wawi said something in `turn`: the item it came from is done.
    pub fn spoke(&mut self, turn: TurnId, now: Civil) -> Option<String> {
        let id = self.turns.remove(&turn)?;
        self.finish(&id, "dicho", "", now);
        Some(id)
    }

    /// The dialogue is idle and these turns never spoke (the brain judged
    /// it not worth it, or failed): done too, with that reason.
    pub fn all_silent(&mut self, now: Civil) -> Vec<String> {
        let ids: Vec<String> = self.turns.drain().map(|(_, id)| id).collect();
        for id in &ids {
            self.finish(id, "callado", "el turno terminó sin hablar", now);
        }
        ids
    }

    fn finish(&mut self, id: &str, result: &str, reason: &str, now: Civil) {
        let path = self.root.join("new").join(format!("{id}.json"));
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        self.finish_file(id, &text, result, reason, now);
    }

    /// Writes the item, plus its outcome, into `cur/` (through `tmp/`) and
    /// takes it out of `new/`.
    fn finish_file(&mut self, id: &str, original: &str, result: &str, reason: &str, now: Civil) {
        if self.waiting.remove(id).is_some() {
            self.save_waiting();
        }
        let mut value = serde_json::from_str::<Value>(original)
            .ok()
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({ "original": truncate(original, 2000) }));
        value["resultado"] = json!(result);
        if !reason.is_empty() {
            value["motivo"] = json!(reason);
        }
        value["procesado"] = json!(format!("{}:{:02}", now.iso_minute(), now.second));
        let tmp = self.root.join("tmp").join(format!("{id}.ack.json"));
        let cur = self.root.join("cur").join(format!("{id}.json"));
        let written =
            std::fs::write(&tmp, value.to_string()).and_then(|()| std::fs::rename(&tmp, &cur));
        if written.is_ok() {
            let _ = std::fs::remove_file(self.root.join("new").join(format!("{id}.json")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wawi_tools::FakeClock;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wawi-inbox-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn drop_item(home: &Path, id: &str, body: &str) {
        let root = home.join(FACTS);
        let tmp = root.join("tmp").join(format!("{id}.json"));
        std::fs::write(&tmp, body).unwrap();
        std::fs::rename(&tmp, root.join("new").join(format!("{id}.json"))).unwrap();
    }

    fn cur(home: &Path, id: &str) -> Value {
        let text = std::fs::read_to_string(home.join(FACTS).join("cur").join(format!("{id}.json")))
            .unwrap();
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn items_flow_from_new_to_cur_with_their_outcome() {
        let home = temp("flow");
        let clock = FakeClock::at(Civil::date(2026, 11, 23).at(10, 0, 0), -3);
        let mut inbox = Inbox::open(&home).unwrap();
        assert!(home.join(QUESTIONS).join("new").is_dir());
        drop_item(
            &home,
            "a1",
            r#"{"id":"a1","origen":"instinto","decir":"Terminó el build","contexto":{"rama":"main"},"urgencia":"alta","creado":"2026-11-23T09:59"}"#,
        );
        drop_item(
            &home,
            "b2",
            r#"{"id":"b2","decir":"Viejo","vence":"2026-11-23T09:00"}"#,
        );
        drop_item(&home, "c3", r#"{"id":"c3"}"#);
        drop_item(&home, "d4", r#"{"id":"otro","decir":"x"}"#);
        let mut notes = Vec::new();
        let found = inbox.scan(clock.as_ref(), &mut notes);
        assert_eq!(found.len(), 1);
        let candidate = &found[0];
        assert_eq!(candidate.id, "cola:a1");
        assert_eq!(candidate.say, INBOX_INSTRUCTION);
        assert!(
            candidate.context.contains("Terminó el build") && candidate.context.contains("main")
        );
        assert_eq!(cur(&home, "b2")["resultado"], "vencido");
        assert_eq!(cur(&home, "c3")["resultado"], "invalido");
        assert!(
            cur(&home, "d4")["motivo"]
                .as_str()
                .unwrap()
                .contains("no coincide")
        );
        // Still in flight: a second scan does not offer it again.
        assert!(inbox.scan(clock.as_ref(), &mut notes).is_empty());
        inbox.started("cola:a1", TurnId(5));
        assert_eq!(
            inbox.spoke(TurnId(5), clock.local_now()).as_deref(),
            Some("a1")
        );
        assert_eq!(cur(&home, "a1")["resultado"], "dicho");
        assert!(!home.join(FACTS).join("new").join("a1.json").exists());
        // A repeat of an acknowledged id is dropped.
        drop_item(&home, "a1", r#"{"id":"a1","decir":"otra vez"}"#);
        assert!(inbox.scan(clock.as_ref(), &mut notes).is_empty());
        assert!(!home.join(FACTS).join("new").join("a1.json").exists());
        assert_eq!(cur(&home, "a1")["decir"], "Terminó el build");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_dropped_item_is_acknowledged_with_the_reason_and_a_restart_rereads_the_rest() {
        let home = temp("drop");
        let clock = FakeClock::at(Civil::date(2026, 11, 23).at(10, 0, 0), -3);
        let mut inbox = Inbox::open(&home).unwrap();
        drop_item(&home, "x", r#"{"id":"x","decir":"uno"}"#);
        let mut notes = Vec::new();
        assert_eq!(inbox.scan(clock.as_ref(), &mut notes).len(), 1);
        // Wawi closes before deciding: the next start reads it again.
        let mut again = Inbox::open(&home).unwrap();
        assert_eq!(again.scan(clock.as_ref(), &mut notes).len(), 1);
        again.dropped(
            "cola:x",
            DiagnosticReason::InstinctDuplicate,
            clock.local_now(),
        );
        assert_eq!(cur(&home, "x")["resultado"], "descartado");
        assert_eq!(cur(&home, "x")["motivo"], "InstinctDuplicate");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_postponed_item_waits_in_new_until_its_retry_then_expires_as_vencido() {
        let home = temp("postpone");
        let clock = FakeClock::at(Civil::date(2026, 11, 23).at(23, 30, 0), -3);
        let mut inbox = Inbox::open(&home).unwrap();
        drop_item(
            &home,
            "entrevista",
            r#"{"id":"entrevista","origen":"instinto","decir":"Mañana es la entrevista","urgencia":"alta","vence":"2026-11-24T10:00"}"#,
        );
        let mut notes = Vec::new();
        let found = inbox.scan(clock.as_ref(), &mut notes);
        assert!(found[0].urgent);
        assert_eq!(found[0].expires_in_ms, Some(630 * 60_000));
        // The gate says: quiet hours, try again at 8:00.
        inbox.postponed(
            "cola:entrevista",
            DiagnosticReason::InstinctQuiet,
            clock.now() + 8 * 3600 + 1800,
        );
        assert!(home.join(FACTS).join("new/entrevista.json").exists());
        assert!(inbox.scan(clock.as_ref(), &mut notes).is_empty());
        // A restart keeps the retry time.
        let mut again = Inbox::open(&home).unwrap();
        assert!(again.scan(clock.as_ref(), &mut notes).is_empty());
        assert_eq!(again.waiting()["entrevista"].reason, "InstinctQuiet");
        clock.advance(8 * 3600 + 1800);
        assert_eq!(again.scan(clock.as_ref(), &mut notes).len(), 1);
        // Held again (say, the owner is talking), and then it passes its
        // deadline: acknowledged as vencido.
        again.postponed(
            "cola:entrevista",
            DiagnosticReason::InstinctExpired,
            clock.now() + 60,
        );
        clock.advance(3 * 3600);
        assert!(again.scan(clock.as_ref(), &mut notes).is_empty());
        let ack = cur(&home, "entrevista");
        assert_eq!(ack["resultado"], "vencido");
        assert!(ack["motivo"].as_str().unwrap().contains("InstinctExpired"));
        assert!(again.waiting().is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }
}
