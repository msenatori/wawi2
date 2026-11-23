//! Timers and reminders: `avisame`, `recordatorios` and
//! `cancelar_recordatorio`, and the store they share, persisted in
//! `<home>/reminders.json` so they survive a restart.
//!
//! The tools only write the list. Saying a reminder out loud is the host's
//! job: on every tick it takes the due ones ([`Reminders::take_due`]) and asks
//! the core for the floor, which speaks at once when idle and queues the
//! notice otherwise (never over the owner).

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde_json::{Value, json};
use wawi_core::{EventSink, PermissionLevel};
use wawi_memory::looks_like_secret;

use crate::clock::{self, WallClock};
use crate::tool::{InstantTool, known_args, optional_integer, optional_text, spec};

pub const SET: &str = "avisame";
pub const LIST: &str = "recordatorios";
pub const CANCEL: &str = "cancelar_recordatorio";

pub const FILE: &str = "reminders.json";

/// The furthest ahead a reminder may be.
pub const MAX_AHEAD_S: i64 = 366 * 86_400;
/// How many may be pending at once.
pub const MAX_PENDING: usize = 50;
/// The longest `texto`, in characters.
pub const MAX_TEXT: usize = 200;
/// A reminder said this late (the Mac was asleep) owns up to it.
const LATE_S: i64 = 120;

const SET_DESCRIPTION: &str = "Programa un aviso que vas a decir en voz alta vos sola \
cuando llegue el momento, aunque el dueño no esté hablando: un temporizador («avisame en \
diez minutos») o un recordatorio a una hora o fecha («recordame mañana a las ocho que llame \
a mamá»). Pasá exactamente uno de los dos: en_segundos, para «en tanto tiempo», o a_las, \
para una hora o fecha, como hora local en el formato 2026-11-20T08:00, calculada a partir de \
[Ahora: ...]. Si el dueño dijo solo una hora y esa hora ya pasó hoy, es mañana. Si no queda \
claro cuándo, preguntale antes de llamarla. texto es qué hay que recordar, corto y como lo \
dirías (por ejemplo «sacar la ropa del lavarropas»); dejalo vacío para un temporizador sin \
motivo.";

const LIST_DESCRIPTION: &str = "Lista los avisos pendientes (temporizadores y \
recordatorios): su número, cuándo suenan y qué dicen.";

const CANCEL_DESCRIPTION: &str = "Cancela un aviso pendiente por su número, el que dan \
avisame y recordatorios. Si no sabés el número, llamá antes a recordatorios.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reminder {
    pub id: u64,
    /// Seconds since the epoch.
    pub due: i64,
    pub created: i64,
    /// What to remind; `None` for a bare timer.
    pub text: Option<String>,
    /// For a timer (`en_segundos`), how long it was set for.
    pub timer_s: Option<i64>,
}

impl Reminder {
    /// What it is about, for lists: `sacar la ropa`, `el temporizador de 10
    /// minutos`.
    pub fn what(&self) -> String {
        match (&self.text, self.timer_s) {
            (Some(text), _) => text.trim_end_matches(['.', '!']).to_owned(),
            (None, Some(seconds)) => format!("el temporizador de {}", clock::duration(seconds)),
            (None, None) => "el aviso que me pediste".into(),
        }
    }

    /// What Wawi says when it is due, at `now`.
    pub fn spoken(&self, now: i64, clock: &dyn WallClock) -> String {
        if now - self.due > LATE_S {
            let when = clock::when(&clock.local(self.due), &clock.local(now));
            return format!("Se me pasó la hora, era {when}: {}.", self.what());
        }
        match (&self.text, self.timer_s) {
            (Some(_), _) => format!("Te aviso: {}.", self.what()),
            (None, Some(seconds)) => {
                format!("Terminó el temporizador de {}.", clock::duration(seconds))
            }
            (None, None) => "Es la hora que me pediste.".into(),
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "due": self.due,
            "created": self.created,
            "text": self.text,
            "timer_s": self.timer_s,
        })
    }

    fn from_json(value: &Value) -> Option<Self> {
        let text = match value.get("text") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) => Some(text.clone()),
            Some(_) => return None,
        };
        let timer_s = match value.get("timer_s") {
            None | Some(Value::Null) => None,
            Some(seconds) => Some(seconds.as_i64()?),
        };
        Some(Self {
            id: value.get("id")?.as_u64()?,
            due: value.get("due")?.as_i64()?,
            created: value.get("created")?.as_i64()?,
            text,
            timer_s,
        })
    }
}

/// The pending reminders, soonest first, and the file that keeps them.
#[derive(Debug, Default)]
pub struct Reminders {
    path: Option<PathBuf>,
    next_id: u64,
    pending: Vec<Reminder>,
}

impl Reminders {
    /// Only in memory: nothing survives a restart.
    pub fn in_memory() -> Self {
        Self {
            path: None,
            next_id: 1,
            pending: Vec::new(),
        }
    }

    /// Loads `path`, or starts empty if it does not exist. A file that does
    /// not parse is moved aside to `reminders.json.bad` (never deleted) and
    /// the list starts empty; the second value says so.
    pub fn open(path: PathBuf) -> (Self, Option<String>) {
        let mut reminders = Self {
            path: Some(path.clone()),
            next_id: 1,
            pending: Vec::new(),
        };
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return (reminders, None);
            }
            Err(error) => {
                return (
                    reminders,
                    Some(format!("no pude leer {}: {error}", path.display())),
                );
            }
        };
        match parse(&text) {
            Some((next_id, pending)) => {
                reminders.next_id = next_id;
                reminders.pending = pending;
                reminders.sort();
                (reminders, None)
            }
            None => {
                let aside = path.with_extension("json.bad");
                let moved = fs::rename(&path, &aside).is_ok();
                let note = if moved {
                    format!(
                        "{} no se entendía: lo dejé en {} y empiezo sin avisos",
                        path.display(),
                        aside.display()
                    )
                } else {
                    format!("{} no se entendía; empiezo sin avisos", path.display())
                };
                (reminders, Some(note))
            }
        }
    }

    pub fn pending(&self) -> &[Reminder] {
        &self.pending
    }

    /// Adds one and saves. If the file cannot be written, nothing is added.
    pub fn add(
        &mut self,
        due: i64,
        created: i64,
        text: Option<String>,
        timer_s: Option<i64>,
    ) -> Result<Reminder, String> {
        if self.pending.len() >= MAX_PENDING {
            return Err(format!(
                "ya hay {MAX_PENDING} avisos pendientes; cancelá alguno antes"
            ));
        }
        let reminder = Reminder {
            id: self.next_id,
            due,
            created,
            text,
            timer_s,
        };
        self.next_id += 1;
        self.pending.push(reminder.clone());
        self.sort();
        if let Err(error) = self.save() {
            self.pending.retain(|known| known.id != reminder.id);
            return Err(error);
        }
        Ok(reminder)
    }

    pub fn cancel(&mut self, id: u64) -> Result<Reminder, String> {
        let index = self
            .pending
            .iter()
            .position(|known| known.id == id)
            .ok_or_else(|| format!("no hay ningún aviso pendiente con el número {id}"))?;
        let removed = self.pending.remove(index);
        if let Err(error) = self.save() {
            self.pending.insert(index, removed);
            return Err(error);
        }
        Ok(removed)
    }

    /// Removes and returns every reminder due at `now`, soonest first. They
    /// leave the file at once: a reminder is said once, even if Wawi closes
    /// before its turn to speak comes.
    pub fn take_due(&mut self, now: i64) -> Vec<Reminder> {
        let split = self.pending.partition_point(|known| known.due <= now);
        if split == 0 {
            return Vec::new();
        }
        let due: Vec<Reminder> = self.pending.drain(..split).collect();
        // A failed save only means they may be said again after a restart.
        let _ = self.save();
        due
    }

    fn sort(&mut self) {
        self.pending.sort_by_key(|known| (known.due, known.id));
    }

    fn save(&self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let reminders: Vec<Value> = self.pending.iter().map(Reminder::to_json).collect();
        let doc = json!({ "version": 1, "next_id": self.next_id, "reminders": reminders });
        let text = serde_json::to_string_pretty(&doc).map_err(|error| error.to_string())?;
        write_atomic(path, &format!("{text}\n"))
            .map_err(|error| format!("no pude guardar {}: {error}", path.display()))
    }
}

fn parse(text: &str) -> Option<(u64, Vec<Reminder>)> {
    let doc: Value = serde_json::from_str(text).ok()?;
    let pending: Vec<Reminder> = doc
        .get("reminders")?
        .as_array()?
        .iter()
        .map(Reminder::from_json)
        .collect::<Option<_>>()?;
    let highest = pending.iter().map(|known| known.id).max().unwrap_or(0);
    let next_id = doc.get("next_id")?.as_u64()?.max(highest + 1);
    Some((next_id, pending))
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

/// One line per reminder for the brain: `3: hoy a las 15:30 (en 12
/// minutos), sacar la ropa`.
pub fn describe(reminder: &Reminder, now: i64, clock: &dyn WallClock) -> String {
    let when = clock::when(&clock.local(reminder.due), &clock.local(now));
    format!(
        "{}: {when} (en {}), {}",
        reminder.id,
        clock::duration(reminder.due - now),
        reminder.what()
    )
}

/// What Wawi says at startup about the reminders that came due while she
/// was closed, or `None` if there were none.
pub fn missed_notice(missed: &[Reminder], now: i64, clock: &dyn WallClock) -> Option<String> {
    const SHOWN: usize = 5;
    let today = clock.local(now);
    let item = |reminder: &Reminder| {
        format!(
            "{}, {}",
            clock::when(&clock.local(reminder.due), &today),
            reminder.what()
        )
    };
    match missed {
        [] => None,
        [one] => Some(format!(
            "Mientras no estaba se pasó un aviso: {}.",
            item(one)
        )),
        many => {
            let mut items: Vec<String> = many.iter().take(SHOWN).map(item).collect();
            if many.len() > SHOWN {
                items.push(format!("y {} más", many.len() - SHOWN));
            }
            Some(format!(
                "Mientras no estaba se pasaron {} avisos: {}.",
                many.len(),
                items.join("; ")
            ))
        }
    }
}

/// When a reminder is due, from the `avisame` arguments. Strict: exactly
/// one of `en_segundos` and `a_las`, in range and in the future.
pub fn due_from(
    in_seconds: Option<i64>,
    at: Option<&str>,
    clock: &dyn WallClock,
) -> Result<(i64, Option<i64>), String> {
    let now = clock.now();
    let now_local = clock.local(now);
    match (in_seconds, at) {
        (Some(_), Some(_)) => Err("pasá en_segundos o a_las, no los dos".into()),
        (None, None) => Err("falta cuándo: en_segundos o a_las".into()),
        (Some(seconds), None) => {
            if seconds < 1 {
                return Err("en_segundos tiene que ser 1 o más".into());
            }
            if seconds > MAX_AHEAD_S {
                return Err("es demasiado lejos: el máximo es un año".into());
            }
            Ok((now + seconds, Some(seconds)))
        }
        (None, Some(text)) => {
            let local = clock::parse_local(text).ok_or_else(|| {
                format!(
                    "a_las tiene que ser fecha y hora local, así: {}, sin zona ni palabras",
                    now_local.iso_minute()
                )
            })?;
            let due = clock
                .epoch_of(local)
                .filter(|&due| clock.local(due) == local)
                .ok_or("esa hora no existe ese día (el cambio de horario se la salta)")?;
            if due <= now {
                return Err(format!(
                    "esa hora ya pasó: ahora es {}; si era para otro día, mandá esa fecha",
                    clock::describe_now(&now_local)
                ));
            }
            if due - now > MAX_AHEAD_S {
                return Err("es demasiado lejos: el máximo es un año".into());
            }
            Ok((due, None))
        }
    }
}

fn clean_text(text: Option<&str>) -> Result<Option<String>, String> {
    let Some(text) = text else {
        return Ok(None);
    };
    if text.contains(['\n', '\r']) {
        return Err("texto tiene que ser una sola línea".into());
    }
    if text.chars().count() > MAX_TEXT {
        return Err(format!("texto es muy largo: hasta {MAX_TEXT} letras"));
    }
    if looks_like_secret(text) {
        return Err("no guardo contraseñas, claves ni tokens, tampoco en un aviso".into());
    }
    Ok(Some(text.to_owned()))
}

pub type Shared = Rc<RefCell<Reminders>>;

/// `avisame(en_segundos | a_las, texto)`.
pub fn set_tool(
    reminders: Shared,
    clock: Rc<dyn WallClock>,
    sink: Rc<dyn EventSink>,
) -> InstantTool {
    let schema = json!({
        "type": "object",
        "properties": {
            "en_segundos": {
                "type": "integer",
                "minimum": 1,
                "description": "Dentro de cuántos segundos (diez minutos = 600)."
            },
            "a_las": {
                "type": "string",
                "description": "Fecha y hora local, formato 2026-11-20T08:00."
            },
            "texto": {
                "type": "string",
                "description": "Qué hay que recordar, corto. Vacío para un temporizador."
            }
        },
        "additionalProperties": false
    });
    InstantTool::new(
        spec(SET, SET_DESCRIPTION, schema, PermissionLevel::Write),
        Box::new(move |args| {
            known_args(args, &["en_segundos", "a_las", "texto"])?;
            let in_seconds = optional_integer(args, "en_segundos")?;
            let at = optional_text(args, "a_las")?;
            let text = clean_text(optional_text(args, "texto")?)?;
            let (due, timer_s) = due_from(in_seconds, at, clock.as_ref())?;
            let now = clock.now();
            let reminder = reminders.borrow_mut().add(due, now, text, timer_s)?;
            Ok(format!(
                "Listo, aviso {} programado: {}.",
                reminder.id,
                describe(&reminder, now, clock.as_ref())
            ))
        }),
        sink,
    )
}

/// `recordatorios()`.
pub fn list_tool(
    reminders: Shared,
    clock: Rc<dyn WallClock>,
    sink: Rc<dyn EventSink>,
) -> InstantTool {
    let schema = json!({ "type": "object", "properties": {}, "additionalProperties": false });
    InstantTool::new(
        spec(LIST, LIST_DESCRIPTION, schema, PermissionLevel::Read),
        Box::new(move |args| {
            known_args(args, &[])?;
            let now = clock.now();
            let reminders = reminders.borrow();
            let lines: Vec<String> = reminders
                .pending()
                .iter()
                .map(|reminder| describe(reminder, now, clock.as_ref()))
                .collect();
            Ok(match lines.len() {
                0 => "No hay avisos pendientes.".into(),
                1 => format!("Hay 1 aviso pendiente. {}.", lines[0]),
                n => format!("Hay {n} avisos pendientes. {}.", lines.join(". ")),
            })
        }),
        sink,
    )
}

/// `cancelar_recordatorio(id)`.
pub fn cancel_tool(
    reminders: Shared,
    clock: Rc<dyn WallClock>,
    sink: Rc<dyn EventSink>,
) -> InstantTool {
    let schema = json!({
        "type": "object",
        "properties": {
            "id": { "type": "integer", "description": "El número del aviso." }
        },
        "required": ["id"],
        "additionalProperties": false
    });
    InstantTool::new(
        spec(CANCEL, CANCEL_DESCRIPTION, schema, PermissionLevel::Write),
        Box::new(move |args| {
            known_args(args, &["id"])?;
            let id = optional_integer(args, "id")?.ok_or("falta «id»")?;
            let id = u64::try_from(id).map_err(|_| "«id» tiene que ser positivo".to_owned())?;
            let now = clock.now();
            let removed = reminders.borrow_mut().cancel(id)?;
            Ok(format!(
                "Cancelado el aviso {}: {}.",
                removed.id,
                describe(&removed, now, clock.as_ref())
            ))
        }),
        sink,
    )
}
