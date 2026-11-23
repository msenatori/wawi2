//! The adapter against `fake-claude`, a test binary that speaks the same
//! stream-json protocol as `claude -p`. Nothing here calls the real CLI.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use wawi_brain_claude::{ClaudeBrain, ClaudeConfig};
use wawi_core::{
    Brain, BrainContext, Event, EventSink, PermissionLevel, ToolOutcome, ToolSpec, TurnId,
    TurnSource,
};

const WAIT: Duration = Duration::from_secs(10);

struct ChannelSink(Sender<Event>);

impl EventSink for ChannelSink {
    fn send(&self, event: Event) {
        let _ = self.0.send(event);
    }
}

struct Harness {
    brain: ClaudeBrain,
    events: Receiver<Event>,
    log: PathBuf,
    dir: PathBuf,
}

impl Harness {
    fn new(name: &str) -> Self {
        Self::with(name, |_| {})
    }

    fn with(name: &str, tweak: impl FnOnce(&mut ClaudeConfig)) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "wawi-brain-claude-tests-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("fake.log");
        let mut config = ClaudeConfig {
            program: env!("CARGO_BIN_EXE_fake-claude").into(),
            workdir: dir.join("work"),
            // Most tests read the user lines exactly; one checks the anchor.
            anchor: false,
            ..ClaudeConfig::default()
        };
        config
            .env
            .insert("FAKE_CLAUDE_LOG".into(), log.to_string_lossy().into_owned());
        config.env.insert(
            "FAKE_CLAUDE_SLOW_START".into(),
            dir.join("slow-start").to_string_lossy().into_owned(),
        );
        tweak(&mut config);
        let (tx, events) = mpsc::channel();
        let brain = ClaudeBrain::new(config, Box::new(ChannelSink(tx)));
        Self {
            brain,
            events,
            log,
            dir,
        }
    }

    /// Waits until a warmed-up replacement took over `n` times in all.
    fn wait_for_swaps(&self, n: u64) {
        let deadline = Instant::now() + WAIT;
        while self.brain.swaps() < n {
            assert!(Instant::now() < deadline, "no swap; {}", self.log());
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn start(&mut self, turn: u64, text: &str, context: &BrainContext) {
        self.brain.start(TurnId(turn), text, context);
    }

    /// Events until (and including) the first one `done` accepts.
    fn until(&self, done: impl Fn(&Event) -> bool) -> Vec<Event> {
        let deadline = Instant::now() + WAIT;
        let mut seen = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left) {
                Ok(event) => {
                    let stop = done(&event);
                    seen.push(event);
                    if stop {
                        return seen;
                    }
                }
                Err(_) => panic!("timed out; saw {seen:?}"),
            }
        }
    }

    fn until_done(&self, turn: u64) -> Vec<Event> {
        self.until(|event| matches!(event, Event::BrainDone { turn: t } if t.0 == turn))
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn spawns(&self) -> Vec<String> {
        self.log()
            .lines()
            .filter(|line| line.starts_with("spawn "))
            .map(str::to_owned)
            .collect()
    }
}

fn eco() -> ToolSpec {
    ToolSpec {
        name: "eco".into(),
        description: "devuelve lo que le digan".into(),
        params_schema_json: r#"{"type":"object","properties":{"x":{"type":"number"}}}"#.into(),
        permission: PermissionLevel::Read,
        needs_confirmation: false,
        ack: None,
    }
}

fn owner(tools: Vec<ToolSpec>) -> BrainContext {
    BrainContext {
        tools,
        source: TurnSource::Owner,
        memory: String::new(),
        now: String::new(),
    }
}

fn text(turn: u64, text: &str) -> Event {
    Event::BrainText {
        turn: TurnId(turn),
        text: text.into(),
    }
}

fn done(turn: u64) -> Event {
    Event::BrainDone { turn: TurnId(turn) }
}

#[test]
fn streams_sentences_and_keeps_one_process_across_turns() {
    let mut h = Harness::new("stream");
    h.brain.prewarm("", &[eco()]);
    h.start(1, "say Hola, che. ¿Todo bien?", &owner(vec![eco()]));
    assert_eq!(
        h.until_done(1),
        vec![text(1, "Hola, che."), text(1, "¿Todo bien?"), done(1)]
    );
    h.start(2, "say Otra vez.", &owner(vec![eco()]));
    assert_eq!(h.until_done(2), vec![text(2, "Otra vez."), done(2)]);
    assert_eq!(h.spawns().len(), 1, "{}", h.log());
    let spawn = &h.spawns()[0];
    for flag in [
        "\"--tools\",\"\"",
        "--strict-mcp-config",
        "--setting-sources=",
    ] {
        assert!(spawn.contains(flag), "{flag} in {spawn}");
    }
    assert!(h.log().contains("tools suggest,eco"), "{}", h.log());
}

#[test]
fn a_tool_call_waits_for_the_core_and_its_answer_goes_back() {
    let mut h = Harness::new("tool");
    h.start(1, r#"tool eco {"x":1}"#, &owner(vec![eco()]));
    let events = h.until(|event| matches!(event, Event::BrainToolCall { .. }));
    let Some(Event::BrainToolCall { turn, call }) = events.last() else {
        panic!("{events:?}");
    };
    assert_eq!(*turn, TurnId(1));
    assert_eq!(call.name, "eco");
    assert_eq!(call.args_json, r#"{"x":1}"#);
    // A result for another turn is not this call's answer.
    h.brain
        .tool_result(TurnId(9), &call.id, &ToolOutcome::Success("no".into()));
    std::thread::sleep(Duration::from_millis(100));
    h.brain
        .tool_result(TurnId(1), &call.id, &ToolOutcome::Success("hola".into()));
    assert_eq!(h.until_done(1), vec![text(1, "Resultado: hola."), done(1)]);

    // A refusal by the core goes back the same way, as a tool error.
    h.start(2, "tool eco {}", &owner(vec![eco()]));
    let events = h.until(|event| matches!(event, Event::BrainToolCall { .. }));
    let Some(Event::BrainToolCall { call, .. }) = events.last() else {
        panic!("{events:?}");
    };
    h.brain.tool_result(
        TurnId(2),
        &call.id,
        &ToolOutcome::Failure("a confirmation is pending".into()),
    );
    assert_eq!(
        h.until_done(2),
        vec![text(2, "Error: a confirmation is pending."), done(2)]
    );
}

#[test]
fn a_cancelled_turn_is_interrupted_and_its_late_output_keeps_its_old_id() {
    let mut h = Harness::new("interrupt");
    h.start(1, "slow", &owner(vec![]));
    assert_eq!(h.until(|_| true), vec![text(1, "Uno.")]);
    h.brain.cancel(TurnId(1));
    h.start(2, "say Listo.", &owner(vec![]));
    let events = h.until_done(2);
    assert_eq!(events, vec![text(1, "Tarde."), text(2, "Listo."), done(2)]);
    // The new message was written only after the old turn ended.
    assert!(!h.log().contains("overlap"), "{}", h.log());
    assert_eq!(h.spawns().len(), 1);
}

#[test]
fn cancelling_during_a_tool_call_drops_the_call() {
    let mut h = Harness::new("cancel-tool");
    h.start(1, "tool eco {}", &owner(vec![eco()]));
    let events = h.until(|event| matches!(event, Event::BrainToolCall { .. }));
    let Some(Event::BrainToolCall { call, .. }) = events.last() else {
        panic!("{events:?}");
    };
    let call_id = call.id.clone();
    h.brain.cancel(TurnId(1));
    h.start(2, "say Bueno.", &owner(vec![eco()]));
    // The late result of the cancelled call is ignored.
    h.brain
        .tool_result(TurnId(1), &call_id, &ToolOutcome::Success("tarde".into()));
    assert_eq!(h.until_done(2), vec![text(2, "Bueno."), done(2)]);
}

#[test]
fn a_dead_process_fails_its_turn_and_the_next_one_respawns_it() {
    let mut h = Harness::new("death");
    h.start(1, "die", &owner(vec![]));
    let events = h.until(|event| matches!(event, Event::BrainFailed { .. }));
    let Some(Event::BrainFailed { turn, reason }) = events.last() else {
        panic!("{events:?}");
    };
    assert_eq!(*turn, TurnId(1));
    assert!(reason.starts_with("claude se cerró"), "{reason}");
    h.start(2, "say Volví.", &owner(vec![]));
    assert_eq!(h.until_done(2), vec![text(2, "Volví."), done(2)]);
    let spawns = h.spawns();
    assert_eq!(spawns.len(), 2, "{}", h.log());
    assert!(
        spawns[1].contains("\"--resume\",\"fake-session\""),
        "{}",
        spawns[1]
    );
}

#[test]
fn a_turn_that_ignores_the_interrupt_gets_its_process_replaced() {
    let mut h = Harness::with("hang", |config| {
        config.interrupt_grace = Duration::from_millis(300);
    });
    h.start(1, "hang", &owner(vec![]));
    assert_eq!(h.until(|_| true), vec![text(1, "Uno.")]);
    h.brain.cancel(TurnId(1));
    h.start(2, "say Acá estoy.", &owner(vec![]));
    assert_eq!(h.until_done(2), vec![text(2, "Acá estoy."), done(2)]);
    assert_eq!(h.spawns().len(), 2, "{}", h.log());
}

#[test]
fn an_error_result_fails_the_turn() {
    let mut h = Harness::new("fail");
    h.start(1, "fail", &owner(vec![]));
    assert_eq!(
        h.until(|_| true),
        vec![Event::BrainFailed {
            turn: TurnId(1),
            reason: "fallo simulado".into()
        }]
    );
}

fn memory(soul: &str, facts: &[&str]) -> BrainContext {
    let mut context = owner(vec![]);
    let facts: Vec<String> = facts.iter().map(|fact| format!("- {fact}")).collect();
    context.memory = format!("{soul}\n\n# Lo que sabés\n\n{}", facts.join("\n"));
    context
}

#[test]
fn new_facts_ride_in_the_next_user_message_and_keep_the_process() {
    let mut h = Harness::new("memory");
    let first = memory("Sos Wawi.", &["dato uno"]);
    h.brain.prewarm(&first.memory, &[]);
    // The soul is in the system prompt, the facts are not.
    h.start(1, "prompt", &first);
    assert_eq!(h.until_done(1), vec![text(1, "Sos Wawi."), done(1)]);
    h.start(2, "say Hola.", &first);
    h.until_done(2);
    let second = memory("Sos Wawi.", &["dato dos", "dato uno"]);
    h.start(3, "say Anotado.", &second);
    h.until_done(3);
    h.start(4, "say Sigo.", &second);
    h.until_done(4);

    let spawns = h.spawns();
    assert_eq!(spawns.len(), 1, "{}", h.log());
    assert!(!spawns[0].contains("dato"), "{}", spawns[0]);
    let users: Vec<String> = h
        .log()
        .lines()
        .filter(|line| line.starts_with("user "))
        .map(str::to_owned)
        .collect();
    assert_eq!(users.len(), 4, "{users:?}");
    assert!(users[0].contains("- dato uno") && users[0].ends_with("\\nprompt"));
    // Unchanged facts are not sent again.
    assert_eq!(users[1], "user say Hola.");
    assert!(users[2].contains("- dato dos\\n- dato uno"), "{}", users[2]);
    assert_eq!(users[3], "user say Sigo.");
}

#[test]
fn every_turn_carries_the_anchor_and_the_prompt_never_changes() {
    let mut h = Harness::with("anchor", |config| config.anchor = true);
    let mut context = memory("# Wawi\n\n> Sos Wawi, juguetona.", &["dato"]);
    context.now = "sábado 21 de noviembre de 2026, 10:00".into();
    for turn in 1..=3 {
        h.start(turn, "say Hola.", &context);
        h.until_done(turn);
    }
    let users: Vec<String> = h
        .log()
        .lines()
        .filter(|line| line.starts_with("user "))
        .map(str::to_owned)
        .collect();
    assert_eq!(users.len(), 3, "{users:?}");
    for user in &users {
        assert!(
            user.contains("[Ahora: sábado 21 de noviembre de 2026, 10:00.]\\n[Sos Wawi, juguetona.]\\nsay Hola."),
            "{user}"
        );
    }
    // The anchor is per turn, never in the system prompt: one process.
    assert_eq!(h.spawns().len(), 1, "{}", h.log());
}

#[test]
fn after_a_compaction_the_facts_go_again() {
    let mut h = Harness::new("compact");
    let context = memory("Sos Wawi.", &["dato"]);
    h.start(1, "compact", &context);
    h.until_done(1);
    h.start(2, "say Hola.", &context);
    h.until_done(2);
    let log = h.log();
    let sent = log.lines().filter(|line| line.contains("- dato")).count();
    assert_eq!(sent, 2, "{log}");
    assert_eq!(h.spawns().len(), 1);
}

#[test]
fn a_new_soul_warms_a_replacement_that_takes_over_between_turns() {
    let mut h = Harness::new("soul");
    h.start(1, "say Hola.", &memory("Sos Wawi.", &["dato"]));
    h.until_done(1);
    let happier = memory("Sos Wawi, ahora más alegre.", &["dato"]);
    // This turn does not wait: it runs on the process there is.
    h.start(2, "prompt", &happier);
    assert_eq!(h.until_done(2), vec![text(2, "Sos Wawi."), done(2)]);
    h.wait_for_swaps(1);
    h.start(3, "prompt", &happier);
    assert_eq!(
        h.until_done(3),
        vec![text(3, "Sos Wawi, ahora más alegre."), done(3)]
    );
    let spawns = h.spawns();
    assert_eq!(spawns.len(), 2, "{}", h.log());
    assert!(spawns[1].contains("--resume"), "{}", spawns[1]);
    let log = h.log();
    let sent = log.lines().filter(|line| line.contains("- dato")).count();
    assert_eq!(sent, 2, "{log}");
}

#[test]
fn a_turn_never_waits_for_a_respawn() {
    let mut h = Harness::new("standby");
    let slow = h.dir.join("slow-start");
    h.brain.prewarm("", &[eco()]);
    h.start(1, "say Hola.", &owner(vec![eco()]));
    h.until_done(1);
    // New tools arrive (a plugin connected); the replacement starts slowly.
    let mut eco2 = eco();
    eco2.name = "eco2".into();
    std::fs::write(&slow, "1500").unwrap();
    h.brain.prewarm("", &[eco(), eco2.clone()]);
    let started = Instant::now();
    h.start(2, "say Rápido.", &owner(vec![eco(), eco2.clone()]));
    h.until_done(2);
    assert!(
        started.elapsed() < Duration::from_millis(1000),
        "the turn waited {:?} for the slow spawn",
        started.elapsed()
    );
    assert_eq!(h.brain.swaps(), 0);
    // Turn 2 ended after the slow one resumed the session, so a fresh
    // replacement warms up instead, and takes over once ready.
    std::fs::remove_file(&slow).unwrap();
    h.wait_for_swaps(1);
    let started = Instant::now();
    h.start(3, "tool eco2 {}", &owner(vec![eco(), eco2]));
    let events = h.until(|event| matches!(event, Event::BrainToolCall { .. }));
    assert!(started.elapsed() < Duration::from_millis(1000));
    assert!(matches!(&events[..], [Event::BrainToolCall { call, .. }] if call.name == "eco2"));
    let spawns = h.spawns();
    assert_eq!(spawns.len(), 3, "{}", h.log());
    assert!(spawns[2].contains("--resume"));
}

#[test]
fn initiative_turns_say_what_they_may_use() {
    let mut h = Harness::new("initiative");
    let context = BrainContext {
        tools: vec![wawi_core::suggest_spec()],
        source: TurnSource::Initiative,
        memory: String::new(),
        now: String::new(),
    };
    h.start(1, "say Hecho.", &context);
    h.until_done(1);
    let log = h.log();
    assert!(
        log.contains("user [Iniciativa de Wawi") && log.contains("permitidas: suggest"),
        "{log}"
    );
}

#[test]
fn dropping_the_brain_ends_the_process() {
    let mut h = Harness::new("drop");
    h.start(1, "say Hola.", &owner(vec![]));
    h.until_done(1);
    let Harness { brain, log, .. } = h;
    drop(brain);
    let log = std::fs::read_to_string(log).unwrap();
    assert!(log.trim_end().ends_with("exit"), "{log}");
}
