//! `wawi2`: a stdin simulator of the dialogue core. Tools, speech recognition
//! and playback are fakes. The brain is a fake too, unless `--brain claude`
//! puts the real one (Claude Code in headless mode) behind it. With
//! `--stt parakeet`, `/hear FILE.wav` runs the real speech recognizer on a
//! file; typed lines still go through the fake one.
//!
//! `--voice` is the real program instead: mic, key, Parakeet, Claude Code and
//! Kokoro (see `voice`).

mod fakes;
mod launch;
mod setup;
mod voice;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use fakes::{EventQueue, FakeBrain, FakeStt, FakeTool, SimClock, Sink};
use wawi_brain_claude::{ClaudeBrain, ClaudeConfig};
use wawi_core::{
    Brain, Config, Dialogue, Effect, Event, EventSink, PermissionLevel, SpeechId, SpeechKind,
    State, Stt, TimeRoute, ToolOutcome, TurnId, TurnSource,
};
use wawi_memory::{Home, MemoryError};
use wawi_stt_parakeet::model::{self as stt_model, PARAKEET_TDT_V3_INT8};
use wawi_stt_parakeet::{Parakeet, ParakeetStt, audio};
use wawi_tools::{RouteClock, Toolbox, WallClock};

const HELP: &str = "\
comandos: texto = lo que dijo el dueño (transcripción final) | /talk = empieza a hablar, interrumpe
          /talk TEXTO = le habla encima y dice TEXTO | /stop = cancelar | /done = terminó la reproducción | /brain = el cerebro falso avanza un paso
          /tick MS = avanza el reloj simulado | /hear ARCHIVO.wav = Parakeet lo escucha (con --stt parakeet) | /help | /quit
iniciativa: /initiative TEXTO = Wawi piensa por su cuenta y propone | /suggestions = lista
          /accept N = la sugerencia N se vuelve un pedido tuyo | /dismiss N = no la propone nunca más
memoria: /memory = lo que ve el cerebro | /remember DATO | /forget DATO (si no es exacto, muestra los parecidos)
avisos: /remind SEGUNDOS [TEXTO] | /remind AAAA-MM-DDTHH:MM [TEXTO] | /reminders | /cancel N
          (los dice Wawi sola cuando llegan: adelantá el reloj con /tick) | /now = la fecha y hora que ve el cerebro
palabras del cerebro falso: \"borrar\" (escribe, pide confirmación), \"clima\" (lee, con aviso),
          \"rompe\" (falla), \"lento\" (no avanza solo: usá /brain, /tick o seguí hablando)";

/// The brain behind the simulator: the scripted fake, or the real one.
enum SimBrain {
    Fake(FakeBrain),
    Claude(ClaudeBrain),
}

impl SimBrain {
    fn brain(&mut self) -> &mut dyn Brain {
        match self {
            SimBrain::Fake(brain) => brain,
            SimBrain::Claude(brain) => brain,
        }
    }

    /// Only the fake brain moves in steps; the real one streams on its own.
    fn step(&mut self) {
        if let SimBrain::Fake(brain) = self {
            brain.step();
        }
    }

    fn is_manual(&self) -> bool {
        match self {
            SimBrain::Fake(brain) => brain.is_manual(),
            SimBrain::Claude(_) => true,
        }
    }
}

/// Speech recognition in the simulator. Typed lines always go through the
/// fake; `/hear FILE.wav` goes through Parakeet when `--stt parakeet` loaded it.
struct SimStt {
    fake: FakeStt,
    parakeet: Option<RealStt>,
    /// The next `Transcribe` is for audio handed to Parakeet.
    audio_next: bool,
    /// Segments sent to Parakeet whose transcript has not arrived yet.
    waiting: usize,
    /// When the last `/hear` closed its segment, to show how long it took.
    heard_at: Option<Instant>,
}

struct RealStt {
    stt: ParakeetStt,
    /// With the fake brain nothing else delivers transcripts, so `/hear`
    /// waits for them here. `None` when they arrive through the main loop.
    inbox: Option<Receiver<Event>>,
}

impl Stt for SimStt {
    fn transcribe(&mut self, segment: wawi_core::SegmentId) {
        match self.parakeet.as_mut() {
            Some(real) if self.audio_next => {
                self.audio_next = false;
                self.waiting += 1;
                real.stt.transcribe(segment);
            }
            _ => self.fake.transcribe(segment),
        }
    }
}

/// How long `/hear` waits for a transcript with the fake brain.
const HEAR_LIMIT: Duration = Duration::from_secs(60);

struct Sim<W> {
    dialogue: Dialogue,
    brain: SimBrain,
    stt: SimStt,
    queue: EventQueue,
    last_speech: Option<SpeechId>,
    /// Soul and memory files; `None` runs without them.
    home: Option<Home>,
    out: W,
    /// Every utterance "finishes playing" right away. With a real brain there
    /// is too much speech to end each one with `/done`.
    auto_playback: bool,
    /// When the running brain turn started, until its first sentence.
    waiting_since: Option<(TurnId, Instant)>,
    /// The real tools (memory and reminders), on the simulated clock.
    toolbox: Toolbox,
    clock: Rc<SimClock>,
}

impl<W: Write> Sim<W> {
    fn new(out: W, home: Option<Home>) -> Self {
        let queue: EventQueue = Rc::new(RefCell::new(VecDeque::new()));
        let brain = SimBrain::Fake(FakeBrain::new(queue.clone()));
        Self::with_brain(out, home, queue, brain)
    }

    fn with_brain(out: W, home: Option<Home>, queue: EventQueue, brain: SimBrain) -> Self {
        let mut dialogue = Dialogue::new(Config::default());
        let registry = dialogue.registry_mut();
        registry.register(FakeTool::boxed(
            "borrar",
            "borra un archivo de mentira",
            PermissionLevel::Write,
            true,
            None,
            queue.clone(),
        ));
        registry.register(FakeTool::boxed(
            "clima",
            "consulta un clima de mentira",
            PermissionLevel::Read,
            false,
            Some("a ver, me fijo"),
            queue.clone(),
        ));
        let clock = SimClock::new();
        let toolbox = Toolbox::open(home.clone(), clock.clone());
        for tool in toolbox.tools(Rc::new(Sink(queue.clone()))) {
            registry.register(tool);
        }
        let route_clock: Rc<dyn WallClock> = clock.clone();
        dialogue
            .router_mut()
            .add(Box::new(TimeRoute::new(RouteClock(route_clock))));
        Self {
            dialogue,
            brain,
            stt: SimStt {
                fake: FakeStt::new(queue.clone()),
                parakeet: None,
                audio_next: false,
                waiting: 0,
                heard_at: None,
            },
            queue,
            last_speech: None,
            home,
            out,
            auto_playback: false,
            waiting_since: None,
            toolbox,
            clock,
        }
    }

    /// At startup: the reminders that came due while the simulator was closed.
    fn announce_missed(&mut self) -> io::Result<()> {
        if let Some(warning) = self.toolbox.warning() {
            writeln!(self.out, "avisos: {warning}")?;
        }
        match self.toolbox.missed_notice() {
            Some(event) => self.send(event),
            None => Ok(()),
        }
    }

    /// Runs a real tool as the model would and prints its answer.
    fn tool(&mut self, name: &str, args: serde_json::Value) -> io::Result<()> {
        match self.toolbox.run(name, &args.to_string()) {
            Some(Ok(reply)) => writeln!(self.out, "{name}: {reply}"),
            Some(Err(reason)) => writeln!(self.out, "{name} falló: {reason}"),
            None => writeln!(self.out, "no hay herramienta {name}"),
        }
    }

    /// `/remind 600 sacar la ropa` or `/remind 2026-11-21T08:00 llamar a mamá`.
    fn remind(&mut self, rest: &str) -> io::Result<()> {
        let (when, text) = rest.split_once(' ').unwrap_or((rest, ""));
        let mut args = serde_json::Map::new();
        match when.parse::<i64>() {
            Ok(seconds) => args.insert("en_segundos".into(), seconds.into()),
            Err(_) if !when.is_empty() => args.insert("a_las".into(), when.into()),
            Err(_) => {
                return writeln!(
                    self.out,
                    "uso: /remind SEGUNDOS [TEXTO] o /remind AAAA-MM-DDTHH:MM [TEXTO]"
                );
            }
        };
        if !text.trim().is_empty() {
            args.insert("texto".into(), text.trim().into());
        }
        self.tool(wawi_tools::reminders::SET, args.into())
    }

    /// Returns false when the session should end.
    fn command(&mut self, line: &str) -> io::Result<bool> {
        match line {
            "" => writeln!(
                self.out,
                "estado: {}. Escribí lo que le dirías, o /help.",
                self.dialogue.state()
            )?,
            "/quit" => return Ok(false),
            "/help" => writeln!(self.out, "{HELP}")?,
            "/talk" => self.send(Event::SpeechStarted)?,
            "/stop" => self.send(Event::Cancel)?,
            "/done" => {
                let up_to = self.last_speech.unwrap_or(SpeechId(0));
                self.send(Event::PlaybackFinished { up_to })?;
                self.step_brain_unless_manual();
                self.pump()?;
            }
            "/brain" => {
                self.brain.step();
                self.pump()?;
            }
            "/suggestions" => self.list_suggestions()?,
            "/memory" => self.memory(Home::context)?,
            _ if line.starts_with("/tick") => self.tick(line)?,
            _ if line.starts_with("/hear") => self.hear(line.trim_start_matches("/hear").trim())?,
            _ if line.starts_with("/initiative ") => {
                let text = line.trim_start_matches("/initiative").trim().to_owned();
                self.send(Event::Initiative { text })?
            }
            _ if line.starts_with("/accept ") => self.on_suggestion(line, true)?,
            _ if line.starts_with("/dismiss ") => self.on_suggestion(line, false)?,
            // The same code as the model's `recordar` and `olvidar`.
            _ if line.starts_with("/remember ") => {
                let reply = self
                    .toolbox
                    .remember(line.trim_start_matches("/remember").trim());
                self.print_reply(reply)?
            }
            _ if line.starts_with("/forget ") => {
                let reply = self
                    .toolbox
                    .forget(line.trim_start_matches("/forget").trim());
                self.print_reply(reply)?
            }
            _ if line == "/remind" || line.starts_with("/remind ") => {
                self.remind(line.trim_start_matches("/remind").trim())?
            }
            "/reminders" => self.tool(wawi_tools::reminders::LIST, serde_json::json!({}))?,
            _ if line.starts_with("/cancel ") => {
                match line.trim_start_matches("/cancel").trim().parse::<i64>() {
                    Ok(id) => self.tool(
                        wawi_tools::reminders::CANCEL,
                        serde_json::json!({ "id": id }),
                    )?,
                    Err(_) => writeln!(self.out, "uso: /cancel N (el número de /reminders)")?,
                }
            }
            "/now" => {
                let line = self.toolbox.now_line();
                writeln!(self.out, "ahora: {line}")?
            }
            _ if line.starts_with("/talk ") => {
                self.user_said(line.trim_start_matches("/talk").trim())?
            }
            _ if line.starts_with('/') => {
                writeln!(self.out, "comando desconocido: {line}\n{HELP}")?
            }
            text => self.user_said(text)?,
        }
        Ok(true)
    }

    fn list_suggestions(&mut self) -> io::Result<()> {
        let open = self.dialogue.suggestions().open();
        if open.is_empty() {
            return writeln!(self.out, "no hay sugerencias");
        }
        for (index, suggestion) in open.iter().enumerate() {
            writeln!(
                self.out,
                "{}. {} — {} (decí: «{}»)",
                index + 1,
                suggestion.title,
                suggestion.why,
                suggestion.ask
            )?;
        }
        Ok(())
    }

    /// `/accept N` or `/dismiss N`, N as numbered by `/suggestions`.
    fn on_suggestion(&mut self, line: &str, accept: bool) -> io::Result<()> {
        let number = line
            .split_whitespace()
            .nth(1)
            .and_then(|n| n.parse::<usize>().ok());
        let open = self.dialogue.suggestions().open();
        let Some(id) = number
            .and_then(|n| n.checked_sub(1))
            .and_then(|index| open.get(index))
            .map(|s| s.id)
        else {
            return writeln!(
                self.out,
                "no hay sugerencia con ese número; mirá /suggestions"
            );
        };
        if accept {
            self.send(Event::AcceptSuggestion { id })
        } else {
            self.send(Event::DismissSuggestion { id })?;
            writeln!(self.out, "descartada: no la vuelvo a proponer")
        }
    }

    fn print_reply(&mut self, reply: Result<String, String>) -> io::Result<()> {
        match reply {
            Ok(reply) | Err(reply) => writeln!(self.out, "{reply}"),
        }
    }

    /// Runs a memory action and prints its reply or its error.
    fn memory(
        &mut self,
        action: impl FnOnce(&Home) -> Result<String, MemoryError>,
    ) -> io::Result<()> {
        match self.home.as_ref().map(action) {
            None => writeln!(self.out, "sin memoria en esta sesión"),
            Some(Ok(reply)) => writeln!(self.out, "{}", reply.trim_end()),
            Some(Err(error)) => writeln!(self.out, "memoria: {error}"),
        }
    }

    fn tick(&mut self, line: &str) -> io::Result<()> {
        match line.trim_start_matches("/tick").trim().parse::<u64>() {
            Ok(ms) => {
                let now_ms = self.dialogue.now_ms().saturating_add(ms);
                writeln!(self.out, "clock: {now_ms} ms")?;
                self.send(Event::Tick { now_ms })
            }
            Err(_) => writeln!(self.out, "uso: /tick MS"),
        }
    }

    /// Typed text takes the same path speech will: the segment closes and the
    /// fake STT answers for it. Without an earlier `/talk` it opens one first.
    fn user_said(&mut self, text: &str) -> io::Result<()> {
        if !self.is_talking() {
            self.send(Event::SpeechStarted)?;
        }
        self.stt.fake.will_hear(text);
        self.send(Event::SpeechEnded)
    }

    /// `/hear FILE.wav`: the file is what the owner said, recognized by
    /// Parakeet. The segment opens (unless one is open), the audio goes to the
    /// recognizer and the segment closes.
    fn hear(&mut self, path: &str) -> io::Result<()> {
        if self.stt.parakeet.is_none() {
            return writeln!(
                self.out,
                "/hear necesita el reconocedor real: wawi2 --stt parakeet"
            );
        }
        if path.is_empty() {
            return writeln!(self.out, "uso: /hear ARCHIVO.wav");
        }
        let clip = match audio::read_wav(Path::new(path)) {
            Ok(clip) => clip,
            Err(error) => return writeln!(self.out, "no puedo leer el audio: {error}"),
        };
        writeln!(self.out, "escuchando {path} ({:.1} s)", clip.seconds())?;
        if !self.is_talking() {
            self.send(Event::SpeechStarted)?;
        }
        if let Some(real) = self.stt.parakeet.as_mut() {
            real.stt.hand_audio(clip.samples, clip.sample_rate);
        }
        self.stt.audio_next = true;
        self.stt.heard_at = Some(Instant::now());
        self.send(Event::SpeechEnded)?;
        // No `Transcribe` came out (no open segment): the audio is unused.
        self.stt.audio_next = false;
        while self.stt.waiting > 0 {
            let Some(inbox) = self.stt.parakeet.as_ref().and_then(|r| r.inbox.as_ref()) else {
                break;
            };
            match inbox.recv_timeout(HEAR_LIMIT) {
                Ok(event) => self.send(event)?,
                Err(_) => {
                    self.stt.waiting = 0;
                    writeln!(self.out, "el reconocedor no contestó")?;
                }
            }
        }
        Ok(())
    }

    fn is_talking(&self) -> bool {
        match self.dialogue.state() {
            State::Listening { capture, .. } | State::AwaitingConfirmation { capture, .. } => {
                capture.is_talking()
            }
            _ => false,
        }
    }

    fn step_brain_unless_manual(&mut self) {
        if matches!(self.dialogue.state(), State::Thinking { .. }) && !self.brain.is_manual() {
            self.brain.step();
        }
    }

    fn send(&mut self, event: Event) -> io::Result<()> {
        if let Event::FinalTranscript { segment, text } | Event::UnclearTranscript { segment, text } =
            &event
            && self.stt.parakeet.is_some()
        {
            // Either answer ends the wait for `/hear`.
            self.stt.waiting = self.stt.waiting.saturating_sub(1);
            let ms = self.stt.heard_at.map(|t| t.elapsed().as_millis());
            let unclear = if matches!(event, Event::UnclearTranscript { .. }) {
                " (no entendido)"
            } else {
                ""
            };
            writeln!(
                self.out,
                "oído{unclear}, segmento {} ({} ms): «{text}»",
                segment.0,
                ms.unwrap_or_default()
            )?;
        }
        self.queue.borrow_mut().push_back(event);
        self.pump()
    }

    fn pump(&mut self) -> io::Result<()> {
        loop {
            let event = self.queue.borrow_mut().pop_front();
            let Some(event) = event else {
                // Nothing left: the reminders that are due by now ask for
                // the floor, as the voice host does on every tick.
                self.clock.follow(self.dialogue.now_ms());
                let due = self.toolbox.due();
                if due.is_empty() {
                    return Ok(());
                }
                self.queue.borrow_mut().extend(due);
                continue;
            };
            let before = self.dialogue.state().to_string();
            let effects = self.dialogue.handle(event);
            let after = self.dialogue.state().to_string();
            if before != after {
                writeln!(self.out, "state: {before} -> {after}")?;
            }
            for effect in effects {
                writeln!(self.out, "effect: {}", describe(&effect))?;
                self.execute(effect)?;
            }
        }
    }

    fn execute(&mut self, effect: Effect) -> io::Result<()> {
        match effect {
            Effect::Speak {
                speech, turn, kind, ..
            } => {
                self.last_speech = Some(speech);
                if kind == SpeechKind::Answer
                    && let Some((waiting, since)) = self.waiting_since
                    && waiting == turn
                {
                    self.waiting_since = None;
                    let ms = since.elapsed().as_millis();
                    writeln!(self.out, "(primera frase en {ms} ms)")?;
                }
                if self.auto_playback {
                    self.queue
                        .borrow_mut()
                        .push_back(Event::PlaybackFinished { up_to: speech });
                }
            }
            Effect::StartBrain {
                turn,
                text,
                mut context,
            } => {
                // Read on every turn, so edits to the files count right away.
                if let Some(home) = &self.home {
                    match home.context() {
                        Ok(memory) => context.memory = memory,
                        Err(error) => writeln!(self.out, "memoria: {error}")?,
                    }
                }
                context.now = self.toolbox.now_line();
                self.waiting_since = Some((turn, Instant::now()));
                self.brain.brain().start(turn, &text, &context)
            }
            Effect::CancelBrain { turn } => self.brain.brain().cancel(turn),
            Effect::ToolResultToBrain {
                turn,
                call_id,
                outcome,
            } => self.brain.brain().tool_result(turn, &call_id, &outcome),
            Effect::RunTool { turn, call } => {
                if let Some(tool) = self.dialogue.registry_mut().tool_mut(&call.name) {
                    tool.start(turn, &call);
                }
            }
            Effect::CancelTool {
                turn,
                call_id,
                name,
            } => {
                if let Some(tool) = self.dialogue.registry_mut().tool_mut(&name) {
                    tool.cancel(turn, &call_id);
                }
            }
            Effect::Transcribe { segment } => self.stt.transcribe(segment),
            Effect::StopPlayback
            | Effect::FloorQueued { .. }
            | Effect::SuggestionAdded { .. }
            | Effect::InstinctDecided { .. }
            | Effect::Diagnostic { .. } => {}
        }
        Ok(())
    }
}

fn describe(effect: &Effect) -> String {
    match effect {
        Effect::Speak {
            speech,
            turn,
            text,
            kind,
        } => format!(
            "speak #{} turn {} [{}] \"{text}\"",
            speech.0,
            turn.0,
            speech_kind(kind)
        ),
        Effect::StopPlayback => "stop_playback".into(),
        Effect::StartBrain {
            turn,
            text,
            context,
        } => format!(
            "start_brain turn {} \"{text}\" ({} tools{})",
            turn.0,
            context.tools.len(),
            match context.source {
                TurnSource::Owner => "",
                TurnSource::Initiative => ", initiative",
            }
        ),
        Effect::CancelBrain { turn } => format!("cancel_brain turn {}", turn.0),
        Effect::RunTool { turn, call } => {
            format!("run_tool turn {} {} {}", turn.0, call.name, call.args_json)
        }
        Effect::CancelTool {
            turn,
            call_id,
            name,
        } => format!("cancel_tool turn {} {name} {call_id}", turn.0),
        Effect::ToolResultToBrain {
            turn,
            call_id,
            outcome,
        } => {
            let outcome = match outcome {
                ToolOutcome::Success(result) => format!("ok: {result}"),
                ToolOutcome::Failure(reason) => format!("failed: {reason}"),
            };
            format!("tool_result_to_brain turn {} {call_id} {outcome}", turn.0)
        }
        Effect::Transcribe { segment } => format!("transcribe segment {}", segment.0),
        Effect::FloorQueued { source } => format!("floor_queued {source}"),
        Effect::SuggestionAdded { suggestion } => {
            format!("suggestion_added \"{}\"", suggestion.title)
        }
        Effect::InstinctDecided { id, outcome } => format!("instinct_decided {id}: {outcome:?}"),
        Effect::Diagnostic { event, reason } => format!("diagnostic {event}: {reason:?}"),
    }
}

fn speech_kind(kind: &SpeechKind) -> String {
    match kind {
        SpeechKind::Answer => "answer".into(),
        SpeechKind::Ack => "ack".into(),
        SpeechKind::Question => "question".into(),
        SpeechKind::Notice => "notice".into(),
        SpeechKind::Route(name) => format!("route {name}"),
        SpeechKind::Failure(failure) => format!("failure {failure:?}"),
        SpeechKind::Floor(source) => format!("floor {source}"),
    }
}

fn run(
    input: impl BufRead,
    out: impl Write,
    home: Option<Home>,
    parakeet: Option<Parakeet>,
    echo: bool,
    prompt: bool,
) -> io::Result<()> {
    let mut sim = Sim::new(out, home);
    if let Some(parakeet) = parakeet {
        let (tx, rx) = mpsc::channel();
        let stt = ParakeetStt::spawn(parakeet, Box::new(DirectSink(tx)));
        stt.set_language_guard(true);
        sim.stt.parakeet = Some(RealStt {
            stt,
            inbox: Some(rx),
        });
    }
    writeln!(
        sim.out,
        "wawi2: simulador del diálogo, sin audio ni IA real.\n\
         Probá: escribí «hola», después /done para que termine cada frase, o /talk TEXTO para hablarle encima. /help para ver todo."
    )?;
    writeln!(sim.out, "state: {}", sim.dialogue.state())?;
    sim.announce_missed()?;
    let mut lines = input.lines();
    loop {
        if prompt {
            write!(sim.out, "> ")?;
            sim.out.flush()?;
        }
        let Some(line) = lines.next() else {
            return Ok(());
        };
        let line = line?;
        let line = line.trim();
        if echo {
            writeln!(sim.out, "> {line}")?;
        }
        if !sim.command(line)? {
            return Ok(());
        }
    }
}

/// `$WAWI2_HOME`, or `~/.wawi2`.
fn home_dir() -> Option<PathBuf> {
    match std::env::var_os("WAWI2_HOME") {
        Some(dir) => Some(dir.into()),
        None => std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".wawi2")),
    }
}

/// What reaches the simulator with a real brain: typed lines and brain events,
/// from their own threads, on one channel.
enum Input {
    Line(String),
    Closed,
    Brain(Event),
}

/// Brain and recognizer events, into the main loop's channel.
struct ForwardSink(Sender<Input>);

impl EventSink for ForwardSink {
    fn send(&self, event: Event) {
        let _ = self.0.send(Input::Brain(event));
    }
}

/// Recognizer events, for `/hear` to wait on.
struct DirectSink(Sender<Event>);

impl EventSink for DirectSink {
    fn send(&self, event: Event) {
        let _ = self.0.send(event);
    }
}

/// How often the real clock reaches the core, so its watchdogs work.
const TICK: Duration = Duration::from_millis(250);
/// With piped input, how long to wait for the last answer after the input ends.
const DRAIN_LIMIT: Duration = Duration::from_secs(120);

/// The brain works from `~/.wawi2/claude` (or `$WAWI2_HOME/claude`): a folder
/// with no project and no CLAUDE.md. `WAWI2_CLAUDE_MODEL` and
/// `WAWI2_CLAUDE_EFFORT` pick the model and effort.
fn claude_config() -> ClaudeConfig {
    voice::claude_config(home_dir().as_deref())
}

/// The simulator with Claude Code as the brain. Answers arrive on their own,
/// so input is read on a thread and the core gets the real time.
fn run_claude(
    out: impl Write,
    home: Option<Home>,
    parakeet: Option<Parakeet>,
    echo: bool,
    prompt: bool,
) -> io::Result<()> {
    let (tx, rx) = mpsc::channel();
    let config = claude_config();
    let brain = ClaudeBrain::new(config, Box::new(ForwardSink(tx.clone())));
    let queue: EventQueue = Rc::new(RefCell::new(VecDeque::new()));
    let mut sim = Sim::with_brain(out, home, queue, SimBrain::Claude(brain));
    if let Some(parakeet) = parakeet {
        let stt = ParakeetStt::spawn(parakeet, Box::new(ForwardSink(tx.clone())));
        stt.set_language_guard(true);
        sim.stt.parakeet = Some(RealStt { stt, inbox: None });
    }
    sim.auto_playback = true;
    let memory = match &sim.home {
        Some(home) => home.context().unwrap_or_default(),
        None => String::new(),
    };
    let tools = sim.dialogue.registry_mut().enabled_specs();
    if let SimBrain::Claude(brain) = &sim.brain {
        brain.prewarm(&memory, &tools);
    }
    writeln!(
        sim.out,
        "wawi2: simulador del diálogo con Claude Code como cerebro (sin audio).\n\
         Escribí lo que le dirías; las respuestas llegan solas. /stop corta, /help ayuda, /quit sale."
    )?;
    writeln!(sim.out, "state: {}", sim.dialogue.state())?;
    sim.announce_missed()?;
    thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if tx.send(Input::Line(line)).is_err() {
                return;
            }
        }
        let _ = tx.send(Input::Closed);
    });
    let clock = Instant::now();
    let mut draining_since: Option<Instant> = None;
    loop {
        if prompt && draining_since.is_none() {
            write!(sim.out, "> ")?;
            sim.out.flush()?;
        }
        let input = match rx.recv_timeout(TICK) {
            Ok(input) => Some(input),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        match input {
            Some(Input::Line(line)) => {
                let line = line.trim();
                if echo {
                    writeln!(sim.out, "> {line}")?;
                }
                if !sim.command(line)? {
                    break;
                }
            }
            Some(Input::Brain(event)) => sim.send(event)?,
            Some(Input::Closed) => draining_since = Some(Instant::now()),
            None => {}
        }
        let now_ms = u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX);
        let now_ms = now_ms.max(sim.dialogue.now_ms());
        if now_ms > sim.dialogue.now_ms() {
            sim.send(Event::Tick { now_ms })?;
        }
        if let Some(since) = draining_since
            && (matches!(sim.dialogue.state(), State::Idle) || since.elapsed() > DRAIN_LIMIT)
        {
            break;
        }
    }
    // Dropping the simulator ends the claude process.
    drop(sim);
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BrainChoice {
    Fake,
    Claude,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SttChoice {
    Fake,
    Parakeet,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Choices {
    brain: BrainChoice,
    stt: SttChoice,
    /// `--voice` (or `--input-wav`/`--mute`/`--no-ui`): the real voice program.
    voice: Option<voice::VoiceOptions>,
    /// `--ui-demo[=LOOK]`: Wawi's body alone, no audio and no brain.
    ui_demo: Option<Option<voice::DemoLook>>,
    /// `--mascota DIR`: the demo shows this pet pack.
    mascota: Option<PathBuf>,
}

fn choices(args: impl Iterator<Item = String>) -> Result<Choices, String> {
    let mut choices = Choices {
        brain: BrainChoice::Fake,
        stt: SttChoice::Fake,
        voice: None,
        ui_demo: None,
        mascota: None,
    };
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--voice" => {
                choices.voice.get_or_insert_default();
                continue;
            }
            "--mute" => {
                choices.voice.get_or_insert_default().mute = true;
                continue;
            }
            "--no-ui" => {
                choices.voice.get_or_insert_default().no_ui = true;
                continue;
            }
            "--ui-demo" => {
                choices.ui_demo = Some(None);
                continue;
            }
            _ => {}
        }
        let (flag, value) = match arg.split_once('=') {
            Some((flag, value)) => (flag.to_owned(), value.to_owned()),
            None => (arg.clone(), args.next().unwrap_or_default()),
        };
        match (flag.as_str(), value.as_str()) {
            ("--brain", "fake") => choices.brain = BrainChoice::Fake,
            ("--brain", "claude") => choices.brain = BrainChoice::Claude,
            ("--brain", other) => return Err(format!("--brain fake|claude, no «{other}»")),
            ("--stt", "fake") => choices.stt = SttChoice::Fake,
            ("--stt", "parakeet") => choices.stt = SttChoice::Parakeet,
            ("--stt", other) => return Err(format!("--stt fake|parakeet, no «{other}»")),
            ("--ui-demo", look) => match voice::DemoLook::parse(look) {
                Some(look) => choices.ui_demo = Some(Some(look)),
                None => {
                    let looks: Vec<&str> = voice::DemoLook::ALL.iter().map(|l| l.name()).collect();
                    return Err(format!("--ui-demo={}, no «{look}»", looks.join("|")));
                }
            },
            ("--mascota", "") => return Err("--mascota necesita la carpeta de una mascota".into()),
            ("--mascota", dir) => choices.mascota = Some(PathBuf::from(dir)),
            ("--input-wav", "") => return Err("--input-wav necesita un archivo .wav".into()),
            ("--input-wav", path) => {
                choices.voice.get_or_insert_default().input_wav = Some(PathBuf::from(path));
            }
            _ => return Err(format!("argumento desconocido: {arg}")),
        }
    }
    if choices.mascota.is_some() && choices.ui_demo.is_none() {
        return Err("--mascota va con --ui-demo (o usá WAWI2_MASCOTA=CARPETA)".into());
    }
    Ok(choices)
}

/// Gets the model on disk (downloading it the first time) and loads it.
fn load_parakeet() -> Result<Parakeet, String> {
    let dir = stt_model::model_dir(&PARAKEET_TDT_V3_INT8).map_err(|e| e.to_string())?;
    stt_model::ensure(&PARAKEET_TDT_V3_INT8, &dir, &mut |line| eprintln!("{line}"))
        .map_err(|e| e.to_string())?;
    let (parakeet, took) = Parakeet::load_timed(&dir)?;
    eprintln!("Parakeet cargado en {} ms", took.as_millis());
    Ok(parakeet)
}

const USAGE: &str = "\
uso: wawi2 setup [--sin-abrir]                             la primera vez: modelos, Claude Code y permisos
     wawi2 [--brain fake|claude] [--stt fake|parakeet]     el simulador
     wawi2 --voice [--mute] [--input-wav ARCHIVO.wav] [--no-ui]
                                                         la voz de verdad (con Wawi en pantalla)
     wawi2 --ui-demo[=idle|listening|transcribing|thinking|speaking|interrupted|error|muted|sleeping]
           [--mascota CARPETA]                           su cuerpo solo, sin audio ni cerebro";

fn main() -> ExitCode {
    let home = home_dir();
    // First, before any thread: an app opened from the Finder has no shell
    // PATH, and `claude` and plugin commands must still be found.
    launch::extend_path(home.as_deref());
    let args = launch::real_args(std::env::args().skip(1));
    if args.first().is_some_and(|arg| arg == "setup") {
        return match &args[1..] {
            [] => setup::main(home, true),
            [flag] if flag == "--sin-abrir" => setup::main(home, false),
            _ => {
                eprintln!("setup solo acepta --sin-abrir\n{USAGE}");
                ExitCode::from(2)
            }
        };
    }
    if launch::from_bundle() && !io::stdout().is_terminal() {
        // Wawi.app: nobody reads the output, so it goes to the logs.
        if let Some(dir) = &home {
            launch::log_output_to(&dir.join("logs"));
        }
        if args.is_empty() {
            // Opened from the Finder: the voice program, as `--voice`.
            return voice::main(voice::VoiceOptions::default(), home);
        }
    }
    let choice = match choices(args.into_iter()) {
        Ok(choice) => choice,
        Err(error) => {
            eprintln!("{error}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if let Some(hold) = choice.ui_demo {
        return voice::ui_demo(hold, home, choice.mascota.clone());
    }
    if let Some(options) = choice.voice {
        return voice::main(options, home);
    }
    match simulate(&choice) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn simulate(choice: &Choices) -> io::Result<()> {
    let parakeet = match choice.stt {
        SttChoice::Fake => None,
        SttChoice::Parakeet => match load_parakeet() {
            Ok(parakeet) => Some(parakeet),
            Err(error) => {
                eprintln!("no pude cargar Parakeet: {error}");
                std::process::exit(1);
            }
        },
    };
    let home = match home_dir().map(Home::open) {
        Some(Ok(home)) => Some(home),
        Some(Err(error)) => {
            eprintln!("sin memoria: {error}");
            None
        }
        None => {
            eprintln!("sin memoria: no encuentro la carpeta personal");
            None
        }
    };
    let stdin = io::stdin();
    let interactive = stdin.is_terminal();
    if choice.brain == BrainChoice::Claude {
        return run_claude(
            io::stdout().lock(),
            home,
            parakeet,
            !interactive,
            interactive,
        );
    }
    run(
        stdin.lock(),
        io::stdout().lock(),
        home,
        parakeet,
        !interactive,
        interactive,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(script: &str) -> String {
        let mut out = Vec::new();
        run(script.as_bytes(), &mut out, None, None, false, false).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn args(list: &[&str]) -> Result<Choices, String> {
        choices(list.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn the_brain_is_fake_unless_claude_is_asked_for() {
        let brain = |list: &[&str]| args(list).map(|c| c.brain);
        assert_eq!(brain(&[]), Ok(BrainChoice::Fake));
        assert_eq!(brain(&["--brain", "claude"]), Ok(BrainChoice::Claude));
        assert_eq!(brain(&["--brain=fake"]), Ok(BrainChoice::Fake));
        assert!(args(&["--brain", "gpt"]).is_err());
        assert!(args(&["--loud"]).is_err());
    }

    #[test]
    fn speech_recognition_is_fake_unless_parakeet_is_asked_for() {
        let stt = |list: &[&str]| args(list).map(|c| c.stt);
        assert_eq!(stt(&[]), Ok(SttChoice::Fake));
        assert_eq!(stt(&["--stt", "parakeet"]), Ok(SttChoice::Parakeet));
        assert_eq!(
            args(&["--stt=parakeet", "--brain", "claude"]),
            Ok(Choices {
                brain: BrainChoice::Claude,
                stt: SttChoice::Parakeet,
                voice: None,
                ui_demo: None,
                mascota: None,
            })
        );
        assert!(args(&["--stt", "whisper"]).is_err());
    }

    #[test]
    fn the_ui_demo_cycles_or_holds_one_look() {
        assert_eq!(args(&["--ui-demo"]).map(|c| c.ui_demo), Ok(Some(None)));
        assert_eq!(
            args(&["--ui-demo=speaking"]).map(|c| c.ui_demo),
            Ok(Some(Some(voice::DemoLook::Speaking)))
        );
        assert!(args(&["--ui-demo=dancing"]).is_err());
        assert_eq!(args(&["--ui-demo"]).map(|c| c.voice), Ok(None), "no audio");
        assert_eq!(args(&[]).map(|c| c.ui_demo), Ok(None));
        assert_eq!(
            args(&["--ui-demo", "--mascota", "/x/kitty"]).map(|c| c.mascota),
            Ok(Some(PathBuf::from("/x/kitty")))
        );
        assert!(
            args(&["--mascota", "/x/kitty"]).is_err(),
            "only with the demo"
        );
    }

    #[test]
    fn voice_is_its_own_mode_and_the_simulator_stays_the_default() {
        assert_eq!(args(&[]).map(|c| c.voice), Ok(None));
        assert_eq!(
            args(&["--voice"]).map(|c| c.voice),
            Ok(Some(voice::VoiceOptions::default()))
        );
        assert_eq!(
            args(&["--voice", "--mute", "--input-wav", "x.wav"]).map(|c| c.voice),
            Ok(Some(voice::VoiceOptions {
                input_wav: Some(PathBuf::from("x.wav")),
                mute: true,
                no_ui: false,
            }))
        );
        assert_eq!(
            args(&["--voice", "--no-ui"]).map(|c| c.voice.map(|v| v.no_ui)),
            Ok(Some(true)),
            "the window comes up unless --no-ui"
        );
        assert_eq!(
            args(&["--input-wav=x.wav"]).map(|c| c.voice.map(|v| v.mute)),
            Ok(Some(false)),
            "a WAV alone means the voice program"
        );
        assert!(args(&["--input-wav"]).is_err());
    }

    #[test]
    fn hear_needs_the_real_recognizer() {
        let out = session("/hear algo.wav\nhola\n");
        assert!(out.contains("/hear necesita el reconocedor real"), "{out}");
        assert!(out.contains("start_brain turn 1 \"hola\""), "{out}");
    }

    #[test]
    fn talk_with_text_interrupts_and_says_it() {
        let out = session("hola\n/talk probando probando\n");
        assert!(out.contains("effect: stop_playback"), "{out}");
        assert!(
            out.contains("start_brain turn 2 \"probando probando\""),
            "{out}"
        );
    }

    #[test]
    fn a_normal_turn_returns_to_idle() {
        let out = session("hola\n/done\n/done\n");
        assert!(out.contains("start_brain turn 1 \"hola\""), "{out}");
        assert!(
            out.trim_end().ends_with("state: speaking(turn 1) -> idle"),
            "{out}"
        );
    }

    #[test]
    fn talking_over_an_answer_cancels_and_drops_its_late_text() {
        let out = session("hola\n/talk\notra cosa\n");
        assert!(
            out.contains("effect: stop_playback\neffect: cancel_brain turn 1"),
            "{out}"
        );
        assert!(out.contains("diagnostic brain_text: StaleTurn"), "{out}");
        assert!(out.contains("start_brain turn 2 \"otra cosa\""), "{out}");
    }

    #[test]
    fn a_tool_runs_only_after_the_typed_yes() {
        let out = session("borrar las fotos\nsí\n");
        let asked = out.find("[question]").unwrap();
        let ran = out.find("run_tool").unwrap();
        assert!(asked < ran, "{out}");
    }

    #[test]
    fn an_initiative_cannot_write_and_proposes_instead() {
        let out = session("/initiative borrar fotos viejas\n/done\n/suggestions\n");
        assert!(
            out.contains("failed: la iniciativa solo puede leer; proponelo como sugerencia"),
            "{out}"
        );
        assert!(!out.contains("run_tool"), "{out}");
        assert!(
            out.contains("suggestion_added \"borrar fotos viejas\""),
            "{out}"
        );
        assert!(out.contains("1. borrar fotos viejas"), "{out}");
    }

    #[test]
    fn an_accepted_suggestion_is_the_owners_request() {
        let out = session("/initiative borrar fotos viejas\n/done\n/accept 1\nsí\n");
        assert!(
            out.contains("start_brain turn 2 \"borrar fotos viejas\" (5 tools)"),
            "{out}"
        );
        assert!(out.contains("run_tool turn 2 borrar"), "{out}");
    }

    #[test]
    fn a_dismissed_suggestion_is_not_proposed_again() {
        let out = session(
            "/initiative regar\n/done\n/dismiss 1\n/initiative Regar\n/done\n/suggestions\n/accept 1\n",
        );
        assert!(out.contains("failed: el dueño ya descartó esto"), "{out}");
        assert!(out.contains("no hay sugerencias"), "{out}");
        assert!(out.contains("no hay sugerencia con ese número"), "{out}");
    }

    #[test]
    fn memory_commands_need_a_home() {
        let out = session("/memory\n/remember algo\n");
        assert!(out.contains("sin memoria en esta sesión"), "{out}");
    }

    #[test]
    fn a_reminder_set_from_the_simulator_fires_when_the_clock_gets_there() {
        let out =
            session("/remind 60 sacar la ropa\n/reminders\n/tick 59000\n/tick 1000\n/reminders\n");
        assert!(
            out.contains("avisame: Listo, aviso 1 programado: 1: hoy a las "),
            "{out}"
        );
        assert!(out.contains("(en 1 minuto), sacar la ropa"), "{out}");
        let fired = out
            .find("[floor recordatorio] \"Te aviso: sacar la ropa.\"")
            .expect("said");
        assert!(fired > out.find("clock: 60000 ms").unwrap(), "{out}");
        assert!(
            out.trim_end()
                .ends_with("recordatorios: No hay avisos pendientes."),
            "{out}"
        );
    }

    #[test]
    fn reminder_commands_check_their_arguments() {
        let out = session("/remind\n/remind mañana sacar la ropa\n/cancel 3\n/cancel x\n");
        assert!(out.contains("uso: /remind SEGUNDOS"), "{out}");
        assert!(out.contains("avisame falló: a_las tiene que ser"), "{out}");
        assert!(
            out.contains(
                "cancelar_recordatorio falló: no hay ningún aviso pendiente con el número 3"
            ),
            "{out}"
        );
        assert!(out.contains("uso: /cancel N"), "{out}");
    }

    #[test]
    fn the_brain_and_now_see_the_local_date() {
        let out = session("/now\nhola\n");
        assert!(
            out.contains("ahora: ") && out.contains("hora local)"),
            "{out}"
        );
    }

    #[test]
    fn talking_before_the_brain_said_anything_merges() {
        let out = session("lento poné música\nde los Beatles\n");
        assert!(
            out.contains("\"lento poné música de los Beatles\""),
            "{out}"
        );
    }
}
