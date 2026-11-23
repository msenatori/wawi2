//! Measures the real brain on a fixed set of short spoken turns, through
//! `ClaudeBrain` with the same isolation flags Wawi uses. It costs real money
//! on the owner's subscription (a few cents per turn), so it only runs by hand:
//!
//! cargo run --release -p wawi-brain-claude --example voice_bench -- LABEL [MODEL] [EFFORT] [ancla|sin-ancla]
//!
//! Without MODEL or EFFORT, Wawi's defaults apply (`DEFAULT_MODEL`,
//! `DEFAULT_EFFORT`); `-` leaves the flag out, for the CLI's own default.
//! The fourth argument turns the per-turn anchor on (the default) or off,
//! to measure what it costs. Every turn carries an `[Ahora: …]` line, as in
//! Wawi. Prints one JSON object
//! per turn: time to the first text delta, to the first spoken chunk (first
//! whole sentence, and first clause as released today), to the end, the
//! session's cost so far and what was said.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wawi_brain_claude::speech::SentenceBuffer;
use wawi_brain_claude::{ClaudeBrain, ClaudeConfig};
use wawi_core::{
    Brain, BrainContext, Event, EventSink, PermissionLevel, ToolOutcome, ToolSpec, TurnId,
    TurnSource,
};
use wawi_memory::Home;

const WAIT: Duration = Duration::from_secs(120);

/// (kind, prompt). The follow-up needs the turn before it; the weather one
/// needs the tool.
const PROMPTS: &[(&str, &str)] = &[
    ("saludo", "Hola Wawi, ¿cómo andás?"),
    ("dato", "¿Cuál es la capital de Australia?"),
    (
        "razonamiento",
        "Si tengo tres manzanas, me como una y después compro cinco, ¿cuántas me quedan?",
    ),
    ("opinión", "¿Qué opinás del mate amargo?"),
    ("seguimiento", "¿Y vos con qué lo acompañarías?"),
    ("memoria", "¿Te acordás cómo me llamo?"),
    ("herramienta", "¿Va a llover mañana en Buenos Aires?"),
    (
        "larga",
        "Contame la historia del tango, con bastante detalle.",
    ),
];

struct ChannelSink(Sender<(Instant, Event)>);

impl EventSink for ChannelSink {
    fn send(&self, event: Event) {
        let _ = self.0.send((Instant::now(), event));
    }
}

fn clima() -> ToolSpec {
    ToolSpec {
        name: "clima".into(),
        description: "El pronóstico del tiempo para una ciudad, hoy y mañana.".into(),
        params_schema_json:
            r#"{"type":"object","properties":{"ciudad":{"type":"string"}},"required":["ciudad"]}"#
                .into(),
        permission: PermissionLevel::Read,
        needs_confirmation: false,
        ack: None,
    }
}

fn memory(dir: &Path) -> String {
    let home = Home::open(dir).expect("bench home");
    let _ = home.add_fact("El dueño se llama Marcos.", "2026-11-20");
    let _ = home.add_fact(
        "Vive en Buenos Aires y toma mate todos los días.",
        "2026-11-20",
    );
    home.context().expect("bench context")
}

struct Turn {
    first_chunk_ms: Option<u128>,
    total_ms: u128,
    said: Vec<String>,
}

fn run_turn(
    brain: &mut ClaudeBrain,
    events: &Receiver<(Instant, Event)>,
    id: u64,
    text: &str,
    context: &BrainContext,
) -> Turn {
    let started = Instant::now();
    brain.start(TurnId(id), text, context);
    let mut first = None;
    let mut said = Vec::new();
    loop {
        let (at, event) = events.recv_timeout(WAIT).expect("the brain went quiet");
        match event {
            Event::BrainText { turn, text } if turn == TurnId(id) => {
                first.get_or_insert_with(|| at.duration_since(started).as_millis());
                said.push(text);
            }
            Event::BrainToolCall { turn, call } if turn == TurnId(id) => {
                let answer = if call.name == "clima" {
                    ToolOutcome::Success(
                        "Buenos Aires. Hoy: 24 grados, soleado. Mañana: 19 grados, \
                         lluvias por la tarde, 80 por ciento de probabilidad."
                            .into(),
                    )
                } else {
                    ToolOutcome::Failure("herramienta desconocida".into())
                };
                brain.tool_result(turn, &call.id, &answer);
            }
            Event::BrainDone { turn } if turn == TurnId(id) => break,
            Event::BrainFailed { turn, reason } if turn == TurnId(id) => {
                panic!("turn {id} failed: {reason}")
            }
            _ => {}
        }
    }
    Turn {
        first_chunk_ms: first,
        total_ms: started.elapsed().as_millis(),
        said,
    }
}

/// What the trace says about one turn: text deltas with their times, since
/// the turn started, and the result line.
#[derive(Default)]
struct Traced {
    deltas: Vec<(u128, String)>,
    result: Option<Value>,
}

fn read_trace(path: &Path) -> Vec<Traced> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut turns: Vec<Traced> = Vec::new();
    let mut start_ms = 0;
    for line in text.lines() {
        let line = line.trim_start();
        let Some((ms, rest)) = line.split_once(' ') else {
            continue;
        };
        let Ok(ms) = ms.parse::<u128>() else { continue };
        if rest.starts_with("-- start turn") {
            start_ms = ms;
            turns.push(Traced::default());
            continue;
        }
        let Some(json) = rest.strip_prefix("<< ") else {
            continue;
        };
        let Some(turn) = turns.last_mut() else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(json) else {
            continue;
        };
        if value["type"] == "stream_event"
            && value["event"]["delta"]["type"] == "text_delta"
            && value["parent_tool_use_id"].is_null()
        {
            let delta = value["event"]["delta"]["text"].as_str().unwrap_or("");
            turn.deltas.push((ms - start_ms, delta.to_owned()));
        } else if value["type"] == "result" {
            turn.result = Some(value);
        }
    }
    turns
}

/// When `buffer` would have released its first chunk, replaying the deltas.
fn first_release(mut buffer: SentenceBuffer, deltas: &[(u128, String)]) -> Option<(u128, String)> {
    for (ms, delta) in deltas {
        if let Some(chunk) = buffer.push(delta).into_iter().next() {
            return Some((*ms, chunk));
        }
    }
    let last = deltas.last()?.0;
    buffer.flush().into_iter().next().map(|chunk| (last, chunk))
}

/// Prints, per turn of each trace, when the first chunk would go out with
/// whole sentences only and with the buffer Wawi uses.
fn replay(traces: &[String]) {
    for path in traces {
        for (index, traced) in read_trace(Path::new(path)).iter().enumerate() {
            let sentence = first_release(SentenceBuffer::sentences_only(), &traced.deltas);
            let chunk = first_release(SentenceBuffer::default(), &traced.deltas);
            println!(
                "{}",
                json!({
                    "trace": path,
                    "turn": index,
                    "first_delta_ms": traced.deltas.first().map(|(ms, _)| *ms),
                    "first_sentence_ms": sentence.as_ref().map(|(ms, _)| *ms),
                    "first_chunk_ms": chunk.as_ref().map(|(ms, _)| *ms),
                    "first_chunk": chunk.map(|(_, text)| text),
                })
            );
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "replay") {
        return replay(&args[1..]);
    }
    let label = args.first().cloned().unwrap_or_else(|| "bench".into());
    let pick = |i: usize| {
        args.get(i)
            .map(|v| if v == "-" { String::new() } else { v.clone() })
    };
    let dir = std::env::temp_dir().join(format!("wawi2-voice-bench-{label}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("bench dir");
    let trace: PathBuf = dir.join("trace.log");
    let anchor = args.get(3).is_none_or(|v| v != "sin-ancla");
    let config = ClaudeConfig {
        workdir: dir.join("work"),
        model: pick(1),
        effort: pick(2),
        trace: Some(trace.clone()),
        anchor,
        ..ClaudeConfig::default()
    };
    let context = BrainContext {
        tools: vec![clima()],
        source: TurnSource::Owner,
        memory: memory(&dir.join("home")),
        now: "sábado 21 de noviembre de 2026, 11:30 (2026-11-21T11:30, hora local)".into(),
    };
    eprintln!("[{label}] anchor: {:?}", config.anchor(&context.memory));
    let (tx, events) = mpsc::channel();
    let mut brain = ClaudeBrain::new(config, Box::new(ChannelSink(tx)));
    brain.prewarm(&context.memory, &context.tools);
    std::thread::sleep(Duration::from_secs(5));
    // Warms the prompt cache, like any turn after the first in a session.
    let warm = run_turn(&mut brain, &events, 1, "Decí solamente: listo.", &context);
    eprintln!("[{label}] warm-up: {} ms", warm.total_ms);
    let mut turns = Vec::new();
    for (index, (kind, prompt)) in PROMPTS.iter().enumerate() {
        let turn = run_turn(&mut brain, &events, index as u64 + 2, prompt, &context);
        eprintln!("[{label}] {kind}: {} ms", turn.total_ms);
        turns.push(turn);
    }
    drop(brain);
    let traced = read_trace(&trace);
    let mut cost = 0.0;
    for (index, traced) in traced.iter().enumerate() {
        // The CLI reports the session's cost so far, not the turn's.
        cost = traced
            .result
            .as_ref()
            .and_then(|result| result["total_cost_usd"].as_f64())
            .unwrap_or(cost);
        // Index 0 is the warm-up.
        let Some(turn) = index.checked_sub(1).and_then(|i| turns.get(i)) else {
            continue;
        };
        let (kind, prompt) = PROMPTS[index - 1];
        let sentence = first_release(SentenceBuffer::sentences_only(), &traced.deltas);
        let clause = first_release(SentenceBuffer::default(), &traced.deltas);
        let usage = traced
            .result
            .as_ref()
            .map(|result| result["usage"].clone())
            .unwrap_or(Value::Null);
        println!(
            "{}",
            json!({
                "label": label,
                "anchor": anchor,
                "kind": kind,
                "prompt": prompt,
                "first_delta_ms": traced.deltas.first().map(|(ms, _)| *ms),
                "first_sentence_ms": sentence.as_ref().map(|(ms, _)| *ms),
                "first_sentence": sentence.map(|(_, text)| text),
                "first_clause_ms": clause.as_ref().map(|(ms, _)| *ms),
                "first_clause": clause.map(|(_, text)| text),
                "first_chunk_event_ms": turn.first_chunk_ms,
                "total_ms": turn.total_ms,
                "said": turn.said.join(" "),
                "session_cost_usd": cost,
                "input_tokens": usage["input_tokens"],
                "cache_read": usage["cache_read_input_tokens"],
                "cache_write": usage["cache_creation_input_tokens"],
                "output_tokens": usage["output_tokens"],
            })
        );
    }
    eprintln!("[{label}] cost: ${cost:.4} (trace in {})", trace.display());
}
