//! The host around the dialogue core: one channel of [`Input`]s from every
//! piece (mic gate, hotkey, recognizer, brain, TTS, playback), a real clock,
//! and the core's effects carried out on the pieces. Everything runs on one
//! thread; the pieces only post messages.
//!
//! The pieces are traits so the tests can run the whole loop with fakes:
//! [`Listener`] (speech recognition), [`Voice`] (TTS), [`Speaker`] (playback)
//! and the core's `Brain`.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use wawi_audio_mac::{Playback, Player};
use wawi_core::{
    Brain, Config, DiagnosticReason, Dialogue, Effect, Event, EventSink, SpeechId, SpeechKind,
    State, Stt, TimeRoute, Tool, ToolOutcome, Tts,
};
use wawi_memory::Home;
use wawi_stt_parakeet::ParakeetStt;
use wawi_tts_kokoro::{KokoroTts, SpeechAudio};

use wawi_body::Flash;
use wawi_plugins::{PluginEvent, PluginHost, PluginSink};
use wawi_tools::{RouteClock, Toolbox, WallClock, clock::describe_now};

use super::body::Reporter;
use super::clock::{SystemClock, local_now};
use super::incidents::Incidents;
use super::turns::{TurnReport, TurnTracker};

/// How often the real clock reaches the core, so its watchdogs work.
pub const TICK: Duration = Duration::from_millis(100);

/// Silence added after the last sentence of every speech. Bluetooth
/// headphones swallow the end of a sound that stops dead (Wawi 1 did this).
pub const TRAILING_SILENCE_S: f32 = 0.3;

/// Everything that reaches the host, on one channel.
pub enum Input {
    /// For the core: from the gate, the recognizer, the brain, playback, tools.
    Core(Event),
    /// A synthesized chunk from the TTS.
    Audio(SpeechAudio),
    /// The audio of the segment about to close. Always sent before its
    /// `SpeechEnded`, by the same thread, so it arrives first.
    Segment { samples: Vec<f32>, sample_rate: u32 },
    /// The push-to-talk key went up (for the latency log).
    KeyReleased(Instant),
    /// From the plugin host's workers.
    Plugin(PluginEvent),
    /// Enter: end the session.
    Quit,
}

/// Core events into the host's channel.
pub struct InputSink(pub Sender<Input>);

impl EventSink for InputSink {
    fn send(&self, event: Event) {
        let _ = self.0.send(Input::Core(event));
    }
}

impl PluginSink for InputSink {
    fn core(&self, event: Event) {
        let _ = self.0.send(Input::Core(event));
    }

    fn plugin(&self, event: PluginEvent) {
        let _ = self.0.send(Input::Plugin(event));
    }
}

/// Speech recognition: `Stt` plus the audio hand-off and the discard.
pub trait Listener: Stt {
    fn hand_audio(&mut self, samples: Vec<f32>, sample_rate: u32);
    fn discard_pending(&self);
}

impl Listener for ParakeetStt {
    fn hand_audio(&mut self, samples: Vec<f32>, sample_rate: u32) {
        ParakeetStt::hand_audio(self, samples, sample_rate);
    }

    fn discard_pending(&self) {
        ParakeetStt::discard_pending(self);
    }
}

/// Text to speech: `Tts` plus the discard after a cut.
pub trait Voice: Tts {
    fn discard_pending(&self);
}

impl Voice for KokoroTts {
    fn discard_pending(&self) {
        KokoroTts::discard_pending(self);
    }
}

/// Playback that knows which chunk ends a speech, so `PlaybackFinished`
/// waits for the last one even if the TTS falls behind between sentences.
pub trait Speaker {
    fn play_part(&mut self, speech: SpeechId, samples: &[f32], last: bool);
    fn stop(&mut self);
}

impl<P: Player> Speaker for Playback<P> {
    fn play_part(&mut self, speech: SpeechId, samples: &[f32], last: bool) {
        Playback::play_part(self, speech, samples, last);
    }

    fn stop(&mut self) {
        self.cut();
    }
}

/// The pieces the host drives.
pub struct Parts {
    pub brain: Box<dyn Brain>,
    pub stt: Box<dyn Listener>,
    pub tts: Box<dyn Voice>,
    pub out: Box<dyn Speaker>,
}

/// `logs/session.log` (what happened, for debugging: no texts, no audio) and
/// `logs/turns.log` (one line per turn, with its latencies).
#[derive(Default)]
pub struct Logs {
    session: Option<File>,
    turns: Option<File>,
}

impl Logs {
    pub fn open(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let append = |name: &str| {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join(name))
        };
        Ok(Self {
            session: Some(append("session.log")?),
            turns: Some(append("turns.log")?),
        })
    }

    pub fn session(&mut self, line: &str) {
        if let Some(file) = &mut self.session {
            let _ = writeln!(file, "{} {line}", local_now().stamp());
        }
    }

    fn turn(&mut self, line: &str) {
        if let Some(file) = &mut self.turns {
            let _ = writeln!(file, "{} {line}", local_now().stamp());
        }
    }
}

/// Why [`run`] returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    /// Enter.
    Quit,
    /// Ctrl+C (the flag was set).
    Signal,
    /// The requested number of turns finished and the dialogue is idle.
    TurnsDone,
    /// The time limit ran out.
    TimeLimit,
    /// Every sender is gone.
    Disconnected,
}

/// When [`run`] should stop on its own, besides Enter and Ctrl+C.
#[derive(Clone, Copy, Debug, Default)]
pub struct Until {
    pub turns: Option<usize>,
    pub limit: Option<Duration>,
}

pub struct Host {
    dialogue: Dialogue,
    parts: Option<Parts>,
    home: Option<Home>,
    console: Box<dyn Write>,
    logs: Logs,
    epoch: Instant,
    /// The newest speech handed to the TTS.
    last_speech: Option<SpeechId>,
    /// Audio for speeches up to this one was cut: late chunks are dropped.
    cut_up_to: Option<SpeechId>,
    /// Speeches that have played some audio, until their last chunk.
    sounding: HashSet<SpeechId>,
    turns: TurnTracker,
    finished_turns: usize,
    /// Wawi's body on screen, when there is one.
    body: Option<Reporter>,
    /// The owner's clock: the time route, the turn's date and time.
    clock: Rc<dyn WallClock>,
    /// Her tools and the reminders they keep, once installed.
    toolbox: Option<Toolbox>,
    /// The plugins, their watches and routines, and the inbox.
    plugins: Option<PluginHost>,
    /// The incident hook for autoarreglo, once installed (fire and forget).
    incidents: Option<Incidents>,
    /// Speeches with text and no audio yet: one that ends silent is an
    /// incident (only tracked with the hook installed).
    unvoiced: HashSet<SpeechId>,
}

impl Host {
    pub fn new(parts: Parts, home: Option<Home>, console: Box<dyn Write>, logs: Logs) -> Self {
        let clock: Rc<dyn WallClock> = Rc::new(SystemClock::new());
        let mut dialogue = Dialogue::new(Config::default());
        dialogue
            .router_mut()
            .add(Box::new(TimeRoute::new(RouteClock(clock.clone()))));
        Self {
            dialogue,
            parts: Some(parts),
            home,
            console,
            logs,
            epoch: Instant::now(),
            last_speech: None,
            cut_up_to: None,
            sounding: HashSet::new(),
            turns: TurnTracker::default(),
            finished_turns: 0,
            body: None,
            clock,
            toolbox: None,
            plugins: None,
            incidents: None,
            unvoiced: HashSet::new(),
        }
    }

    /// From now on plugin tools register as they connect, and watches,
    /// routines, pushes and the inbox reach the core's instinct gate.
    pub fn install_plugins(&mut self, plugins: PluginHost) {
        self.plugins = Some(plugins);
    }

    /// From now on what goes wrong (a failed turn, the brain dying, a
    /// plugin crash loop, transcripts that keep coming back empty, a slow
    /// turn, an audio failure) is written to `<home>/cola/incidentes` for
    /// autoarreglo, off the turn path.
    pub fn install_incidents(&mut self, incidents: Incidents) {
        self.incidents = Some(incidents);
    }

    /// Registers the tools (built from `toolbox`, in its order) and takes
    /// the toolbox's clock for the time route and the turns. From now on the
    /// due reminders are said on every tick.
    pub fn install_tools(&mut self, toolbox: Toolbox, tools: Vec<Box<dyn Tool>>) {
        self.clock = toolbox.clock().clone();
        self.dialogue
            .router_mut()
            .add(Box::new(TimeRoute::new(RouteClock(self.clock.clone()))));
        for tool in tools {
            self.dialogue.registry_mut().register(tool);
        }
        self.toolbox = Some(toolbox);
    }

    /// Readies the brain for the tools in the registry now, off the turn
    /// path: if its process has other tools, a replacement warms up in the
    /// background and takes over between turns. No turn waits for it.
    pub fn prewarm_brain(&mut self) {
        let memory = self
            .home
            .as_ref()
            .and_then(|home| home.context().ok())
            .unwrap_or_default();
        let specs = self.dialogue.registry_mut().enabled_specs();
        if let Some(parts) = &mut self.parts {
            parts.brain.prewarm(&memory, &specs);
        }
    }

    /// At startup: waits up to `limit` for the plugins to connect (or
    /// fail), handling whatever arrives meanwhile, then readies the brain.
    /// In the common case the first process already has every tool.
    pub fn warm_up(&mut self, rx: &Receiver<Input>, limit: Duration) {
        let deadline = Instant::now() + limit;
        while self.plugins.as_ref().is_some_and(|p| !p.settled()) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match rx.recv_timeout(left) {
                Ok(input) => self.handle(input),
                Err(_) => break,
            }
        }
        self.prewarm_brain();
    }

    /// At startup: what came due while Wawi was closed, said once.
    pub fn announce_missed(&mut self) {
        if let Some(event) = self.toolbox.as_mut().and_then(Toolbox::missed_notice) {
            self.logs.session("reminders missed while closed");
            self.core(event, Instant::now());
        }
    }

    /// Reports her look and words to the body from now on.
    pub fn set_body(&mut self, body: Reporter) {
        self.body = Some(body);
    }

    pub fn dialogue(&self) -> &Dialogue {
        &self.dialogue
    }

    pub fn finished_turns(&self) -> usize {
        self.finished_turns
    }

    pub fn log(&mut self, line: &str) {
        self.logs.session(line);
    }

    fn say(&mut self, line: &str) {
        let _ = writeln!(self.console, "{line}");
        let _ = self.console.flush();
    }

    pub fn handle(&mut self, input: Input) {
        let now = Instant::now();
        match input {
            Input::Core(event) => self.core(event, now),
            Input::Audio(chunk) => self.audio(chunk, now),
            Input::Segment {
                samples,
                sample_rate,
            } => {
                let seconds = samples.len() as f64 / f64::from(sample_rate.max(1));
                self.logs
                    .session(&format!("segment {seconds:.2} s @ {sample_rate} Hz"));
                if let Some(incidents) = &mut self.incidents {
                    incidents.detector.segment(seconds);
                }
                if let Some(parts) = &mut self.parts {
                    parts.stt.hand_audio(samples, sample_rate);
                }
            }
            Input::Plugin(event) => {
                if let (Some(incidents), PluginEvent::Down { plugin, reason }) =
                    (&mut self.incidents, &event)
                {
                    let found = incidents.detector.plugin_down(plugin, reason, now);
                    incidents.report(found, now);
                }
                let tools_changed = matches!(event, PluginEvent::Ready { .. });
                let events = match &mut self.plugins {
                    Some(plugins) => plugins.on_event(event, self.dialogue.registry_mut()),
                    None => Vec::new(),
                };
                if tools_changed {
                    self.prewarm_brain();
                }
                for event in events {
                    self.core(event, now);
                }
            }
            Input::KeyReleased(at) => {
                self.logs.session("key up");
                self.turns.released(at);
            }
            Input::Quit => {}
        }
    }

    /// Gives the core the real time, and the floor to the reminders that
    /// are due (the core decides when they are said).
    pub fn tick(&mut self) {
        let now_ms = u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX);
        if now_ms > self.dialogue.now_ms() {
            self.core(Event::Tick { now_ms }, Instant::now());
        }
        let due = self.toolbox.as_ref().map(Toolbox::due).unwrap_or_default();
        for event in due {
            self.logs.session("reminder due");
            self.core(event, Instant::now());
        }
        let instincts = self
            .plugins
            .as_mut()
            .map(PluginHost::tick)
            .unwrap_or_default();
        for event in instincts {
            self.core(event, Instant::now());
        }
    }

    fn core(&mut self, event: Event, now: Instant) {
        match &event {
            Event::Tick { .. } => {}
            Event::SpeechStarted => {
                self.logs.session("event speech_started");
                if let Some(plugins) = &mut self.plugins
                    && matches!(
                        self.dialogue.state(),
                        State::Speaking { .. } | State::Thinking { .. }
                    )
                {
                    plugins.interrupted();
                }
                if let Some(report) = self.turns.speech_started() {
                    self.finish(report);
                }
            }
            Event::SpeechEnded => {
                self.logs.session("event speech_ended");
                self.turns.speech_ended(now);
            }
            Event::FinalTranscript { segment, text } => {
                self.logs.session(&format!(
                    "event final_transcript segment {} ({} chars)",
                    segment.0,
                    text.chars().count()
                ));
                if let Some(plugins) = &mut self.plugins {
                    plugins.heard(text);
                }
                if !text.trim().is_empty() {
                    let line = format!("vos:  «{}»", text.trim());
                    self.say(&line);
                    if let Some(body) = &self.body {
                        body.heard(text);
                    }
                }
                self.turns.transcript(text, now);
                if !text.trim().is_empty()
                    && let Some(incidents) = &mut self.incidents
                {
                    incidents.detector.heard();
                }
            }
            Event::UnclearTranscript { segment, text } => {
                self.logs.session(&format!(
                    "event unclear_transcript segment {} ({} chars)",
                    segment.0,
                    text.chars().count()
                ));
                if !text.trim().is_empty() {
                    let line = format!("vos (no entendido):  «{}»", text.trim());
                    self.say(&line);
                    if let Some(body) = &self.body {
                        body.heard(text);
                    }
                }
                self.turns.unclear(text, now);
                if let Some(incidents) = &mut self.incidents {
                    let found = incidents.detector.unclear(now);
                    incidents.report(found, now);
                }
            }
            Event::BrainFailed { reason, .. } => {
                self.logs
                    .session(&format!("event {}", describe_event(&event)));
                if let Some(incidents) = &mut self.incidents {
                    let found = incidents.detector.brain_failed(reason);
                    incidents.report(Some(found), now);
                }
            }
            other => self
                .logs
                .session(&format!("event {}", describe_event(other))),
        }
        let before = self.dialogue.state().kind();
        let effects = self.dialogue.handle(event);
        let after = self.dialogue.state().kind();
        if before != after {
            self.logs.session(&format!("state {before:?} -> {after:?}"));
        }
        for effect in effects {
            self.execute(effect, now);
        }
        if let Some(body) = &mut self.body {
            body.state(self.dialogue.state());
        }
        if matches!(self.dialogue.state(), State::Idle)
            && let Some(plugins) = &mut self.plugins
        {
            plugins.idle();
        }
        if matches!(self.dialogue.state(), State::Idle)
            && let Some(report) = self.turns.idle()
        {
            self.finish(report);
        }
    }

    fn finish(&mut self, report: TurnReport) {
        self.finished_turns += 1;
        let line = report.console();
        self.say(&line);
        self.logs.turn(&report.log_line());
        if let Some(incidents) = &mut self.incidents {
            let found = incidents.detector.turn(&report);
            incidents.report(found, Instant::now());
        }
    }

    fn audio(&mut self, chunk: SpeechAudio, now: Instant) {
        let SpeechAudio {
            speech,
            mut samples,
            sample_rate,
            last,
        } = chunk;
        if self.cut_up_to.is_some_and(|cut| speech <= cut) {
            self.logs
                .session(&format!("audio #{} dropped: it was cut", speech.0));
            return;
        }
        if !samples.is_empty() {
            self.sounding.insert(speech);
            self.turns.audio(speech, now);
            self.unvoiced.remove(&speech);
        } else if last
            && self.unvoiced.remove(&speech)
            && let Some(incidents) = &mut self.incidents
        {
            let found = incidents
                .detector
                .audio("la voz no dio audio para una frase");
            incidents.report(Some(found), now);
        }
        if last && self.sounding.remove(&speech) {
            let silence = (TRAILING_SILENCE_S * sample_rate as f32) as usize;
            samples.extend(std::iter::repeat_n(0.0, silence));
        }
        self.logs.session(&format!(
            "audio #{} {:.2} s{}",
            speech.0,
            samples.len() as f64 / f64::from(sample_rate.max(1)),
            if last { " last" } else { "" }
        ));
        if let Some(parts) = &mut self.parts {
            parts.out.play_part(speech, &samples, last);
        }
    }

    fn execute(&mut self, effect: Effect, now: Instant) {
        self.logs
            .session(&format!("effect {}", describe_effect(&effect)));
        if let Some(plugins) = &mut self.plugins {
            match &effect {
                Effect::Speak {
                    turn, text, kind, ..
                } => {
                    let initiative = self
                        .dialogue
                        .state()
                        .work()
                        .is_some_and(|work| work.source == wawi_core::TurnSource::Initiative);
                    plugins.said(*turn, text, kind, initiative);
                }
                Effect::InstinctDecided { id, outcome } => plugins.instinct_decided(id, outcome),
                _ => {}
            }
        }
        let Some(parts) = self.parts.as_mut() else {
            return;
        };
        match effect {
            Effect::Speak {
                speech, text, kind, ..
            } => {
                self.last_speech = Some(speech);
                self.turns.speak(speech, &text, &kind, now);
                if let Some(incidents) = &mut self.incidents {
                    if !text.trim().is_empty() {
                        if self.unvoiced.len() > 64 {
                            self.unvoiced.clear();
                        }
                        self.unvoiced.insert(speech);
                    }
                    if matches!(kind, SpeechKind::Failure(wawi_core::FailureKind::Watchdog)) {
                        let found = incidents
                            .detector
                            .turn_failed("el turno se colgó y lo cortó el watchdog");
                        incidents.report(Some(found), now);
                    }
                }
                parts.tts.synthesize(speech, &text);
                let label = match kind {
                    SpeechKind::Question => "wawi (pregunta)",
                    SpeechKind::Failure(_) => "wawi (falla)",
                    SpeechKind::Floor(_) => "wawi (aviso)",
                    _ => "wawi",
                };
                let line = format!("{label}: «{text}»");
                self.say(&line);
                if let Some(body) = &self.body {
                    if matches!(kind, SpeechKind::Failure(_)) {
                        body.flash(Flash::Error);
                    }
                    body.said(&text);
                }
            }
            Effect::StopPlayback => {
                self.unvoiced.clear();
                parts.out.stop();
                parts.tts.discard_pending();
                if self.cut_up_to < self.last_speech {
                    self.cut_up_to = self.last_speech;
                }
                let was_sounding = !self.sounding.is_empty();
                self.sounding.clear();
                if was_sounding {
                    self.say("  (cortada)");
                    if let Some(body) = &self.body {
                        body.flash(Flash::Interrupted);
                    }
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
                        Err(error) => self.logs.session(&format!("memory error: {error}")),
                    }
                }
                context.now = describe_now(&self.clock.local_now());
                self.turns.brain_started();
                parts.brain.start(turn, &text, &context);
            }
            Effect::CancelBrain { turn } => parts.brain.cancel(turn),
            Effect::ToolResultToBrain {
                turn,
                call_id,
                outcome,
            } => parts.brain.tool_result(turn, &call_id, &outcome),
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
            Effect::Transcribe { segment } => parts.stt.transcribe(segment),
            Effect::SuggestionAdded { suggestion } => {
                let line = format!(
                    "  sugerencia: {} (decí «{}»)",
                    suggestion.title, suggestion.ask
                );
                self.say(&line);
            }
            Effect::FloorQueued { .. } | Effect::InstinctDecided { .. } => {}
            Effect::Diagnostic { reason, .. } => {
                if let Some(incidents) = &mut self.incidents {
                    let found = match reason {
                        DiagnosticReason::EmptyTranscript => incidents.detector.empty(now),
                        DiagnosticReason::ListenTimeout => Some(
                            incidents
                                .detector
                                .audio("el reconocedor de voz no contestó a tiempo"),
                        ),
                        DiagnosticReason::PlaybackTimeout => Some(
                            incidents
                                .detector
                                .audio("la reproducción nunca avisó que terminó"),
                        ),
                        DiagnosticReason::TurnLimit => Some(
                            incidents
                                .detector
                                .turn_failed("el turno llegó a su tope de tiempo"),
                        ),
                        _ => None,
                    };
                    incidents.report(found, now);
                }
                let note = match reason {
                    DiagnosticReason::EmptyTranscript => Some("  (no escuché nada)"),
                    DiagnosticReason::ListenTimeout => {
                        Some("  (aviso: el reconocedor no contestó a tiempo)")
                    }
                    DiagnosticReason::PlaybackTimeout => {
                        Some("  (aviso: la reproducción nunca avisó que terminó)")
                    }
                    DiagnosticReason::TurnLimit => Some("  (aviso: el turno llegó a su tope)"),
                    _ => None,
                };
                if let Some(note) = note {
                    self.say(note);
                }
                if let Some(body) = &self.body
                    && matches!(
                        reason,
                        DiagnosticReason::ListenTimeout
                            | DiagnosticReason::PlaybackTimeout
                            | DiagnosticReason::TurnLimit
                    )
                {
                    body.flash(Flash::Error);
                }
            }
        }
    }

    /// Cuts the audio and drops every piece: the brain's process ends, the
    /// TTS and recognizer workers stop. Idempotent.
    pub fn shutdown(&mut self) {
        if let Some(mut plugins) = self.plugins.take() {
            plugins.shutdown();
        }
        if let Some(mut parts) = self.parts.take() {
            self.logs.session("shutdown");
            parts.out.stop();
            parts.tts.discard_pending();
            parts.stt.discard_pending();
            drop(parts);
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The main loop: one input at a time, a tick at least every [`TICK`].
pub fn run(host: &mut Host, rx: &Receiver<Input>, quit: &AtomicBool, until: Until) -> Ended {
    let started = Instant::now();
    loop {
        if quit.load(Ordering::SeqCst) {
            return Ended::Signal;
        }
        match rx.recv_timeout(TICK) {
            Ok(Input::Quit) => return Ended::Quit,
            Ok(input) => host.handle(input),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ended::Disconnected,
        }
        host.tick();
        if let Some(turns) = until.turns
            && host.finished_turns() >= turns
            && matches!(host.dialogue().state(), State::Idle)
        {
            return Ended::TurnsDone;
        }
        if until.limit.is_some_and(|limit| started.elapsed() > limit) {
            return Ended::TimeLimit;
        }
    }
}

/// For the session log: never the texts.
fn describe_event(event: &Event) -> String {
    match event {
        Event::PlaybackFinished { up_to } => format!("playback_finished up_to #{}", up_to.0),
        Event::BrainText { turn, text } => {
            format!(
                "brain_text turn {} ({} chars)",
                turn.0,
                text.chars().count()
            )
        }
        Event::BrainToolCall { turn, call } => {
            format!("brain_tool_call turn {} {}", turn.0, call.name)
        }
        Event::BrainDone { turn } => format!("brain_done turn {}", turn.0),
        Event::BrainFailed { turn, .. } => format!("brain_failed turn {}", turn.0),
        Event::ToolFinished { turn, call_id, .. } => {
            format!("tool_finished turn {} {call_id}", turn.0)
        }
        other => other.name().to_owned(),
    }
}

/// For the session log: ids and kinds, never the texts.
fn describe_effect(effect: &Effect) -> String {
    match effect {
        Effect::Speak {
            speech,
            turn,
            text,
            kind,
        } => format!(
            "speak #{} turn {} {} ({} chars)",
            speech.0,
            turn.0,
            match kind {
                SpeechKind::Answer => "answer".to_owned(),
                SpeechKind::Ack => "ack".to_owned(),
                SpeechKind::Question => "question".to_owned(),
                SpeechKind::Notice => "notice".to_owned(),
                SpeechKind::Route(name) => format!("route {name}"),
                SpeechKind::Failure(_) => "failure".to_owned(),
                SpeechKind::Floor(source) => format!("floor {source}"),
            },
            text.chars().count()
        ),
        Effect::StopPlayback => "stop_playback".into(),
        Effect::StartBrain { turn, context, .. } => format!(
            "start_brain turn {} ({} tools, {:?})",
            turn.0,
            context.tools.len(),
            context.source
        ),
        Effect::CancelBrain { turn } => format!("cancel_brain turn {}", turn.0),
        Effect::RunTool { turn, call } => format!("run_tool turn {} {}", turn.0, call.name),
        Effect::CancelTool { turn, name, .. } => format!("cancel_tool turn {} {name}", turn.0),
        Effect::ToolResultToBrain { turn, outcome, .. } => format!(
            "tool_result_to_brain turn {} {}",
            turn.0,
            match outcome {
                ToolOutcome::Success(_) => "ok",
                ToolOutcome::Failure(_) => "failed",
            }
        ),
        Effect::Transcribe { segment } => format!("transcribe segment {}", segment.0),
        Effect::FloorQueued { source } => format!("floor_queued {source}"),
        Effect::SuggestionAdded { .. } => "suggestion_added".into(),
        Effect::InstinctDecided { id, outcome } => format!("instinct_decided {id} {outcome:?}"),
        Effect::Diagnostic { event, reason } => format!("diagnostic {event}: {reason:?}"),
    }
}
