//! One smoke test against the real `claude` CLI, on the owner's subscription.
//! Ignored by default (it costs a few cents); run it on purpose with:
//!
//! cargo test -p wawi-brain-claude --test real_cli -- --ignored --nocapture

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use wawi_brain_claude::{ClaudeBrain, ClaudeConfig};
use wawi_core::{
    Brain, BrainContext, Event, EventSink, PermissionLevel, ToolOutcome, ToolSpec, TurnId,
    TurnSource,
};

const WAIT: Duration = Duration::from_secs(90);

struct ChannelSink(Sender<Event>);

impl EventSink for ChannelSink {
    fn send(&self, event: Event) {
        let _ = self.0.send(event);
    }
}

fn next(events: &Receiver<Event>) -> Event {
    events
        .recv_timeout(WAIT)
        .expect("the real brain went quiet")
}

/// Runs one turn to its end. Returns the time to the first sentence and the
/// sentences.
fn turn(brain: &mut ClaudeBrain, events: &Receiver<Event>, id: u64, text: &str) -> (u128, String) {
    let context = context();
    let started = Instant::now();
    brain.start(TurnId(id), text, &context);
    let mut first = None;
    let mut said = Vec::new();
    loop {
        match next(events) {
            Event::BrainText { turn, text } if turn == TurnId(id) => {
                first.get_or_insert_with(|| started.elapsed().as_millis());
                said.push(text);
            }
            Event::BrainToolCall { turn, call } if turn == TurnId(id) => {
                assert_eq!(call.name, "eco", "{call:?}");
                brain.tool_result(
                    turn,
                    &call.id,
                    &ToolOutcome::Success("el eco dice: frutilla".into()),
                );
            }
            Event::BrainDone { turn } if turn == TurnId(id) => break,
            Event::BrainFailed { turn, reason } if turn == TurnId(id) => {
                panic!("turn {id} failed: {reason}")
            }
            _ => {}
        }
    }
    (first.expect("no text at all"), said.join(" "))
}

fn context() -> BrainContext {
    BrainContext {
        tools: vec![ToolSpec {
            name: "eco".into(),
            description: "Devuelve un eco. Usala solo si te la piden.".into(),
            params_schema_json:
                r#"{"type":"object","properties":{"texto":{"type":"string"}},"required":["texto"]}"#
                    .into(),
            permission: PermissionLevel::Read,
            needs_confirmation: false,
            ack: None,
        }],
        source: TurnSource::Owner,
        memory: "El dueño se llama Marcos.".into(),
        now: String::new(),
    }
}

fn brain(name: &str) -> (ClaudeBrain, Receiver<Event>) {
    let config = ClaudeConfig {
        workdir: std::env::temp_dir().join(format!("wawi2-real-smoke-{name}")),
        model: std::env::var("WAWI2_CLAUDE_MODEL").ok(),
        effort: std::env::var("WAWI2_CLAUDE_EFFORT").ok(),
        ..ClaudeConfig::default()
    };
    let (tx, rx) = mpsc::channel();
    (ClaudeBrain::new(config, Box::new(ChannelSink(tx))), rx)
}

#[test]
#[ignore = "calls the real claude CLI on the owner's subscription"]
fn the_real_cli_streams_uses_a_tool_and_can_be_interrupted() {
    let (mut cold, events) = brain("cold");
    let (cold_ms, said) = turn(&mut cold, &events, 1, "Decí hola en una frase corta.");
    eprintln!("cold start (spawn + first sentence): {cold_ms} ms: {said}");
    let (warm_ms, said) = turn(&mut cold, &events, 2, "Ahora decí chau en una frase corta.");
    eprintln!("warm process, first sentence: {warm_ms} ms: {said}");
    let (tool_ms, said) = turn(
        &mut cold,
        &events,
        3,
        "Usá la herramienta eco con el texto frutilla y decime qué devolvió.",
    );
    eprintln!("tool turn, first sentence: {tool_ms} ms: {said}");
    assert!(said.to_lowercase().contains("frutilla"), "{said}");

    // Interrupt mid-answer, then ask again right away.
    cold.start(
        TurnId(4),
        "Contá del uno al cuarenta, en palabras, una por frase.",
        &context(),
    );
    loop {
        if let Event::BrainText { turn, .. } = next(&events)
            && turn == TurnId(4)
        {
            break;
        }
    }
    cold.cancel(TurnId(4));
    let (after_ms, said) = turn(&mut cold, &events, 5, "Decí solamente: listo.");
    eprintln!("after an interrupt, first sentence: {after_ms} ms: {said}");
    drop(cold);

    // The same first turn, on a process started ahead of time.
    let (mut warm, events) = brain("prewarmed");
    let context = context();
    warm.prewarm(&context.memory, &context.tools);
    std::thread::sleep(Duration::from_secs(4));
    let (prewarmed_ms, said) = turn(&mut warm, &events, 1, "Decí hola en una frase corta.");
    eprintln!("prewarmed first turn, first sentence: {prewarmed_ms} ms: {said}");
}
