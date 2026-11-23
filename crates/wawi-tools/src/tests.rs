//! The tools through `Toolbox`, with a fake clock, and reminders reaching the
//! real dialogue core: idle, while Wawi speaks and while the owner holds the
//! key.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use wawi_core::{
    Config, Dialogue, Effect, Event, EventSink, SegmentId, SpeechId, SpeechKind, Tool, ToolCall,
    ToolOutcome, TurnId,
};
use wawi_memory::Home;

use super::*;

#[derive(Default)]
struct Collect(RefCell<Vec<Event>>);

impl EventSink for Collect {
    fn send(&self, event: Event) {
        self.0.borrow_mut().push(event);
    }
}

fn dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wawi2-tools-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Friday 2026-11-20 15:00 in Buenos Aires (UTC-3).
fn clock() -> Rc<FakeClock> {
    FakeClock::at(Civil::date(2026, 11, 20).at(15, 0, 0), -3)
}

struct Kit {
    toolbox: Toolbox,
    tools: Vec<Box<dyn Tool>>,
    sink: Rc<Collect>,
}

impl Kit {
    fn new(home: Option<Home>, clock: Rc<FakeClock>) -> Self {
        let toolbox = Toolbox::open(home, clock);
        let sink = Rc::new(Collect::default());
        let tools = toolbox.tools(sink.clone());
        Self {
            toolbox,
            tools,
            sink,
        }
    }

    fn call(&mut self, name: &str, args: &str) -> ToolOutcome {
        let tool = self
            .tools
            .iter_mut()
            .find(|tool| tool.spec().name == name)
            .unwrap_or_else(|| panic!("no tool {name}"));
        tool.start(
            TurnId(1),
            &ToolCall {
                id: "c".into(),
                name: name.into(),
                args_json: args.into(),
            },
        );
        match self.sink.0.borrow_mut().pop() {
            Some(Event::ToolFinished { outcome, .. }) => outcome,
            other => panic!("expected ToolFinished, got {other:?}"),
        }
    }

    fn ok(&mut self, name: &str, args: &str) -> String {
        match self.call(name, args) {
            ToolOutcome::Success(text) => text,
            ToolOutcome::Failure(reason) => panic!("{name} {args} failed: {reason}"),
        }
    }

    fn fails(&mut self, name: &str, args: &str) -> String {
        match self.call(name, args) {
            ToolOutcome::Failure(reason) => reason,
            ToolOutcome::Success(text) => panic!("{name} {args} should fail, got: {text}"),
        }
    }
}

fn floor_text(event: &Event) -> &str {
    match event {
        Event::RequestFloor {
            text,
            source,
            priority,
        } => {
            assert_eq!(source, FLOOR_SOURCE);
            assert_eq!(*priority, wawi_core::Priority::High);
            text
        }
        other => panic!("expected a floor request, got {other:?}"),
    }
}

#[test]
fn the_model_sees_these_tools_and_no_others() {
    let home = Home::open(dir("names")).unwrap();
    let kit = Kit::new(Some(home), clock());
    let names: Vec<(String, wawi_core::PermissionLevel, bool)> = kit
        .tools
        .iter()
        .map(|tool| {
            let spec = tool.spec();
            (spec.name.clone(), spec.permission, spec.needs_confirmation)
        })
        .collect();
    use wawi_core::PermissionLevel::{Read, Write};
    assert_eq!(
        names,
        [
            ("recordar".to_owned(), Write, false),
            ("olvidar".to_owned(), Write, false),
            ("avisame".to_owned(), Write, false),
            ("recordatorios".to_owned(), Read, false),
            ("cancelar_recordatorio".to_owned(), Write, false),
        ]
    );
    for tool in &kit.tools {
        let schema: serde_json::Value =
            serde_json::from_str(&tool.spec().params_schema_json).unwrap();
        assert_eq!(schema["type"], "object", "{}", tool.spec().name);
    }
    // Without a home there is no memory to write.
    let bare = Kit::new(None, clock());
    assert_eq!(bare.tools.len(), 3);
}

#[test]
fn the_brain_gets_the_local_date_and_time() {
    let kit = Kit::new(None, clock());
    assert_eq!(
        kit.toolbox.now_line(),
        "viernes 20 de noviembre de 2026, 15:00 (2026-11-20T15:00, hora local)"
    );
    assert_eq!(kit.toolbox.today(), "2026-11-20");
}

#[test]
fn remember_dates_facts_with_the_local_day() {
    let home = Home::open(dir("local-day")).unwrap();
    // 23:30 on the 20th in Buenos Aires is already the 21st in UTC.
    let late = FakeClock::at(Civil::date(2026, 11, 20).at(23, 30, 0), -3);
    let mut kit = Kit::new(Some(home.clone()), late);
    kit.ok("recordar", r#"{"dato":"Riega las plantas los viernes"}"#);
    let facts = home.memory().unwrap().facts;
    assert_eq!(facts[0].date, "2026-11-20");
    assert_eq!(
        kit.toolbox.remember("Riega las plantas los viernes"),
        Ok("Ya lo tenía guardado: «Riega las plantas los viernes».".into())
    );
    assert!(kit.toolbox.forget("riega").is_err(), "not exact: kept");
    assert!(kit.toolbox.forget("Riega las plantas los viernes").is_ok());
    assert!(home.memory().unwrap().facts.is_empty());
}

#[test]
fn a_timer_fires_after_its_seconds() {
    let clock = clock();
    let mut kit = Kit::new(None, clock.clone());
    let reply = kit.ok("avisame", r#"{"en_segundos": 600}"#);
    assert_eq!(
        reply,
        "Listo, aviso 1 programado: 1: hoy a las 15:10 (en 10 minutos), el temporizador de 10 minutos."
    );
    clock.advance(599);
    assert!(kit.toolbox.due().is_empty());
    clock.advance(1);
    let due = kit.toolbox.due();
    assert_eq!(due.len(), 1);
    assert_eq!(
        floor_text(&due[0]),
        "Terminó el temporizador de 10 minutos."
    );
    assert!(kit.toolbox.due().is_empty(), "said once");
    assert!(kit.toolbox.pending().is_empty());
}

#[test]
fn a_reminder_at_a_time_says_its_text() {
    let clock = clock();
    let mut kit = Kit::new(None, clock.clone());
    let reply = kit.ok(
        "avisame",
        r#"{"a_las":"2026-11-21T08:00","texto":"llamar a mamá"}"#,
    );
    assert!(
        reply.contains("mañana a las 8:00 (en 17 horas), llamar a mamá"),
        "{reply}"
    );
    clock.set(
        clock
            .epoch_of(Civil::date(2026, 11, 21).at(8, 0, 0))
            .unwrap(),
    );
    let due = kit.toolbox.due();
    assert_eq!(floor_text(&due[0]), "Te aviso: llamar a mamá.");
}

#[test]
fn a_late_reminder_owns_up_to_it() {
    let clock = clock();
    let mut kit = Kit::new(None, clock.clone());
    kit.ok("avisame", r#"{"en_segundos":60,"texto":"sacar la ropa"}"#);
    // The Mac slept through it.
    clock.advance(60 * 30);
    let due = kit.toolbox.due();
    assert_eq!(
        floor_text(&due[0]),
        "Se me pasó la hora, era hoy a las 15:01: sacar la ropa."
    );
}

#[test]
fn avisame_validates_strictly() {
    let mut kit = Kit::new(None, clock());
    for (args, expected) in [
        (r#"{}"#, "falta cuándo"),
        (
            r#"{"en_segundos":60,"a_las":"2026-11-20T16:00"}"#,
            "no los dos",
        ),
        (r#"{"en_segundos":0}"#, "1 o más"),
        (r#"{"en_segundos":-5}"#, "1 o más"),
        (r#"{"en_segundos":1.5}"#, "número entero"),
        (r#"{"en_segundos":"60"}"#, "número entero"),
        (r#"{"en_segundos":99999999}"#, "demasiado lejos"),
        (r#"{"en_minutos":5}"#, "no conozco «en_minutos»"),
        (r#"{"a_las":"mañana a las 8"}"#, "así: 2026-11-20T15:00"),
        (r#"{"a_las":"2026-11-20T15:30Z"}"#, "sin zona"),
        (r#"{"a_las":"2026-11-20T14:59"}"#, "ya pasó"),
        (r#"{"a_las":"2026-11-20T15:00"}"#, "ya pasó"),
        (r#"{"a_las":"2026-02-30T10:00"}"#, "así:"),
        (r#"{"a_las":"2028-01-01T10:00"}"#, "demasiado lejos"),
        (
            r#"{"en_segundos":60,"texto":"una\nlínea"}"#,
            "una sola línea",
        ),
        (
            r#"{"en_segundos":60,"texto":"la contraseña del wifi"}"#,
            "contraseñas",
        ),
    ] {
        let reason = kit.fails("avisame", args);
        assert!(reason.contains(expected), "{args}: {reason}");
    }
    assert!(kit.toolbox.pending().is_empty());
}

#[test]
fn reminders_list_and_cancel() {
    let mut kit = Kit::new(None, clock());
    assert_eq!(kit.ok("recordatorios", "{}"), "No hay avisos pendientes.");
    kit.ok(
        "avisame",
        r#"{"a_las":"2026-11-20T18:00","texto":"regar las plantas"}"#,
    );
    kit.ok("avisame", r#"{"en_segundos":300}"#);
    assert_eq!(
        kit.ok("recordatorios", "{}"),
        "Hay 2 avisos pendientes. 2: hoy a las 15:05 (en 5 minutos), el temporizador de 5 \
         minutos. 1: hoy a las 18:00 (en 3 horas), regar las plantas."
    );
    assert!(
        kit.fails("cancelar_recordatorio", r#"{"id":7}"#)
            .contains("número 7"),
        "unknown id"
    );
    assert!(
        kit.fails("cancelar_recordatorio", "{}")
            .contains("falta «id»")
    );
    let reply = kit.ok("cancelar_recordatorio", r#"{"id":1}"#);
    assert!(
        reply.starts_with("Cancelado el aviso 1:") && reply.contains("regar"),
        "{reply}"
    );
    assert_eq!(kit.toolbox.pending().len(), 1);
}

#[test]
fn reminders_survive_a_restart() {
    let home = Home::open(dir("persist")).unwrap();
    let clock = clock();
    let mut kit = Kit::new(Some(home.clone()), clock.clone());
    kit.ok(
        "avisame",
        r#"{"en_segundos":3600,"texto":"sacar la pizza"}"#,
    );
    kit.ok("avisame", r#"{"en_segundos":7200}"#);
    kit.ok("cancelar_recordatorio", r#"{"id":2}"#);
    drop(kit);

    let file = std::fs::read_to_string(home.root().join("reminders.json")).unwrap();
    assert!(file.contains("sacar la pizza"), "{file}");

    let mut again = Kit::new(Some(home.clone()), clock.clone());
    let pending = again.toolbox.pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].text.as_deref(), Some("sacar la pizza"));
    assert_eq!(again.toolbox.missed_notice(), None, "nothing was missed");
    // Ids are never reused, even after a restart.
    let reply = again.ok("avisame", r#"{"en_segundos":60}"#);
    assert!(reply.starts_with("Listo, aviso 3 programado"), "{reply}");
}

#[test]
fn reminders_due_while_closed_are_announced_once_at_startup() {
    let home = Home::open(dir("missed")).unwrap();
    let clock = clock();
    let mut kit = Kit::new(Some(home.clone()), clock.clone());
    kit.ok("avisame", r#"{"en_segundos":600,"texto":"sacar la ropa"}"#);
    kit.ok("avisame", r#"{"a_las":"2026-11-20T15:30"}"#);
    kit.ok(
        "avisame",
        r#"{"a_las":"2026-11-21T09:00","texto":"pagar la luz"}"#,
    );
    drop(kit);

    // Closed until the next morning at 8.
    clock.set(
        clock
            .epoch_of(Civil::date(2026, 11, 21).at(8, 0, 0))
            .unwrap(),
    );
    let mut kit = Kit::new(Some(home.clone()), clock.clone());
    let notice = kit.toolbox.missed_notice().expect("a notice");
    assert_eq!(
        floor_text(&notice),
        "Mientras no estaba se pasaron 2 avisos: ayer a las 15:10, sacar la ropa; ayer a \
         las 15:30, el aviso que me pediste."
    );
    assert_eq!(kit.toolbox.missed_notice(), None, "only once");
    assert!(
        kit.toolbox.due().is_empty(),
        "missed ones are not said again"
    );
    assert_eq!(kit.toolbox.pending().len(), 1, "the future one stays");
    drop(kit);
    let kit = Kit::new(Some(home), clock);
    assert_eq!(kit.toolbox.pending().len(), 1, "and they left the file");
}

#[test]
fn a_broken_file_is_kept_aside_not_lost() {
    let root = dir("broken");
    let home = Home::open(&root).unwrap();
    std::fs::write(root.join("reminders.json"), "{ esto no es json").unwrap();
    let mut kit = Kit::new(Some(home), clock());
    assert!(kit.toolbox.warning().unwrap().contains("no se entendía"));
    assert!(root.join("reminders.json.bad").exists());
    kit.ok("avisame", r#"{"en_segundos":60}"#);
}

/// The real core, with reminders arriving as the host would deliver them.
struct Core {
    dialogue: Dialogue,
    now_ms: u64,
}

impl Core {
    fn new() -> Self {
        Self {
            dialogue: Dialogue::new(Config::default()),
            now_ms: 0,
        }
    }

    fn handle(&mut self, event: Event) -> Vec<Effect> {
        self.dialogue.handle(event)
    }

    fn tick(&mut self, ms: u64) -> Vec<Effect> {
        self.now_ms += ms;
        self.handle(Event::Tick {
            now_ms: self.now_ms,
        })
    }
}

fn floor_speech(effects: &[Effect]) -> Option<(SpeechId, String)> {
    effects.iter().find_map(|effect| match effect {
        Effect::Speak {
            speech,
            text,
            kind: SpeechKind::Floor(source),
            ..
        } if source == FLOOR_SOURCE => Some((*speech, text.clone())),
        _ => None,
    })
}

fn timer_due_now() -> (Kit, Rc<FakeClock>) {
    let clock = clock();
    let mut kit = Kit::new(None, clock.clone());
    kit.ok("avisame", r#"{"en_segundos":60,"texto":"sacar la ropa"}"#);
    clock.advance(60);
    (kit, clock)
}

#[test]
fn a_reminder_while_idle_is_said_at_once() {
    let (kit, _) = timer_due_now();
    let mut core = Core::new();
    let due = kit.toolbox.due();
    let effects = core.handle(due[0].clone());
    let (_, text) = floor_speech(&effects).expect("spoken now");
    assert_eq!(text, "Te aviso: sacar la ropa.");
}

#[test]
fn a_reminder_while_wawi_speaks_waits_for_her_to_finish() {
    let (kit, _) = timer_due_now();
    let mut core = Core::new();
    core.handle(Event::SpeechStarted);
    core.handle(Event::SpeechEnded);
    core.handle(Event::FinalTranscript {
        segment: SegmentId(1),
        text: "contame un cuento".into(),
    });
    let effects = core.handle(Event::BrainText {
        turn: TurnId(1),
        text: "Había una vez.".into(),
    });
    let answer = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Speak { speech, .. } => Some(*speech),
            _ => None,
        })
        .expect("she is speaking");
    let effects = core.handle(kit.toolbox.due()[0].clone());
    assert!(floor_speech(&effects).is_none(), "not over her own answer");
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::FloorQueued { .. })),
        "{effects:?}"
    );
    core.handle(Event::BrainDone { turn: TurnId(1) });
    let effects = core.handle(Event::PlaybackFinished { up_to: answer });
    let (_, text) = floor_speech(&effects).expect("said right after");
    assert_eq!(text, "Te aviso: sacar la ropa.");
}

#[test]
fn a_reminder_while_the_owner_holds_the_key_waits_for_the_turn() {
    let (kit, _) = timer_due_now();
    let mut core = Core::new();
    core.handle(Event::SpeechStarted);
    let effects = core.handle(kit.toolbox.due()[0].clone());
    assert!(floor_speech(&effects).is_none(), "never over the owner");
    assert!(floor_speech(&core.tick(5_000)).is_none(), "not while held");
    core.handle(Event::SpeechEnded);
    let effects = core.handle(Event::FinalTranscript {
        segment: SegmentId(1),
        text: "hola".into(),
    });
    assert!(
        floor_speech(&effects).is_none(),
        "the owner's turn comes first"
    );
    let effects = core.handle(Event::BrainText {
        turn: TurnId(1),
        text: "Hola.".into(),
    });
    assert!(floor_speech(&effects).is_none());
    core.handle(Event::BrainDone { turn: TurnId(1) });
    let effects = core.handle(Event::PlaybackFinished { up_to: SpeechId(1) });
    let (_, text) = floor_speech(&effects).expect("said after the owner's turn");
    assert_eq!(text, "Te aviso: sacar la ropa.");
}
