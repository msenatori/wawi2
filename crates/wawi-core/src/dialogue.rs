use std::collections::VecDeque;
use std::fmt;
use std::mem;

use crate::event::{
    DiagnosticReason as Reason, Effect, Event, FailureKind, InstinctOutcome, Priority, SegmentId,
    SpeechId, SpeechKind, ToolCall, ToolOutcome, TurnId,
};
use crate::instinct::{GateConfig, InstinctCandidate, InstinctGate};
use crate::plugin::BrainContext;
use crate::policy::{self, TurnSource};
use crate::registry::{Registry, ToolSpec};
use crate::router::Router;
use crate::suggestions::{SUGGEST_TOOL, SuggestionId, Suggestions, suggest_spec};
use crate::text::{Confirmation, is_hard_stop, parse_confirmation};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Phrases {
    pub watchdog_failure: String,
    pub brain_failure: String,
    pub confirmation_reask: String,
    pub confirmation_cancelled: String,
    /// Said when all the owner said was an `UnclearTranscript`.
    pub unclear_transcript: String,
}

impl Default for Phrases {
    fn default() -> Self {
        Self {
            watchdog_failure: "Se me colgó, probá de nuevo.".into(),
            brain_failure: "Algo salió mal, probá de nuevo.".into(),
            confirmation_reask: "No te entendí. ¿Sí o no?".into(),
            confirmation_cancelled: "Listo, no lo hago.".into(),
            unclear_transcript: "No te entendí, ¿me lo repetís?".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// How long the brain may stay silent while nothing is playing.
    pub think_timeout_ms: u64,
    /// Added to the thinking deadline by each tool start.
    pub tool_extension_ms: u64,
    pub confirm_timeout_ms: u64,
    /// Guards against a stuck VAD or an STT that never answers.
    pub listen_timeout_ms: u64,
    /// Guards against an audio output that never reports the end.
    pub playback_timeout_ms: u64,
    /// Quiet time before queued floor items resume after the owner cancels, so
    /// pressing "stop" is not answered by more talking.
    pub floor_resume_after_cancel_ms: u64,
    /// Hard cap on a whole brain turn, from its start, however many tools it
    /// chains or sentences it says. Checked in every state a brain runs in.
    pub max_turn_ms: u64,
    /// High-priority floor items that may wait; beyond it the oldest is dropped.
    pub floor_queue_cap: usize,
    /// Suggestions that may wait for the owner at once.
    pub max_open_suggestions: usize,
    /// The rules that decide whether a plugin's instinct becomes speech.
    pub instinct: GateConfig,
    pub phrases: Phrases,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            think_timeout_ms: 45_000,
            tool_extension_ms: 60_000,
            confirm_timeout_ms: 30_000,
            listen_timeout_ms: 60_000,
            playback_timeout_ms: 120_000,
            floor_resume_after_cancel_ms: 2_000,
            max_turn_ms: 180_000,
            floor_queue_cap: 8,
            max_open_suggestions: 5,
            instinct: GateConfig::default(),
            phrases: Phrases::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CapturedSegment {
    id: SegmentId,
    text: Option<String>,
}

/// User speech being collected. The utterance only closes when no segment is
/// open and every segment has its transcript.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Capture {
    held: Vec<String>,
    segments: Vec<CapturedSegment>,
    active: Option<SegmentId>,
    /// Some segment came back as an `UnclearTranscript`.
    unclear: bool,
}

impl Capture {
    fn holding(held: Vec<String>) -> Self {
        Self {
            held,
            ..Self::default()
        }
    }

    /// Whether a segment is open: the owner is talking right now.
    pub fn is_talking(&self) -> bool {
        self.active.is_some()
    }

    fn begin(&mut self, id: SegmentId) {
        self.segments.push(CapturedSegment { id, text: None });
        self.active = Some(id);
    }

    /// `None` marks the segment unclear: it adds no words.
    fn resolve(&mut self, id: SegmentId, text: Option<String>) -> Result<(), Reason> {
        if self.active == Some(id) {
            return Err(Reason::SegmentStillOpen);
        }
        let segment = self
            .segments
            .iter_mut()
            .find(|segment| segment.id == id && segment.text.is_none())
            .ok_or(Reason::StaleSegment)?;
        self.unclear |= text.is_none();
        segment.text = Some(text.unwrap_or_default());
        Ok(())
    }

    fn is_complete(&self) -> bool {
        self.active.is_none() && self.segments.iter().all(|s| s.text.is_some())
    }

    fn new_texts(&self) -> impl DoubleEndedIterator<Item = &str> {
        self.segments
            .iter()
            .filter_map(|s| s.text.as_deref())
            .map(str::trim)
            .filter(|text| !text.is_empty())
    }

    /// Held speech first, then segments in spoken order, whatever order their
    /// transcripts arrived in.
    fn merged_text(&self) -> String {
        let held = self.held.iter().map(String::as_str);
        held.chain(self.new_texts()).collect::<Vec<_>>().join(" ")
    }

    /// Judged on the newest words alone: "pará" after a held sentence stops,
    /// it does not get appended to that sentence.
    fn ends_in_hard_stop(&self) -> bool {
        self.new_texts().next_back().is_some_and(is_hard_stop)
    }
}

/// A brain that is running for the current turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrainWork {
    pub transcript: String,
    pub deadline_ms: u64,
    /// Something reached the owner or the world (speech, a started tool), so
    /// new speech is a new request and can't be merged by restarting the turn.
    pub committed: bool,
    pub running_tools: Vec<ToolCall>,
    pub source: TurnSource,
    /// The hard cap: `Config::max_turn_ms` after the turn started. Unlike
    /// `deadline_ms`, nothing extends it.
    pub turn_deadline_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    Listening {
        capture: Capture,
        deadline_ms: u64,
    },
    Thinking {
        turn: TurnId,
        work: BrainWork,
    },
    /// Audio is queued. `brain` is `None` for replies with no brain behind them
    /// or once the brain is done.
    Speaking {
        turn: TurnId,
        brain: Option<BrainWork>,
        last_speech: SpeechId,
        deadline_ms: u64,
    },
    AwaitingConfirmation {
        turn: TurnId,
        work: BrainWork,
        call: ToolCall,
        capture: Capture,
        /// Whether the single allowed re-ask was already used.
        reasked: bool,
        playback: Option<SpeechId>,
        deadline_ms: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StateKind {
    Idle,
    Listening,
    Thinking,
    Speaking,
    AwaitingConfirmation,
}

impl State {
    pub fn kind(&self) -> StateKind {
        match self {
            State::Idle => StateKind::Idle,
            State::Listening { .. } => StateKind::Listening,
            State::Thinking { .. } => StateKind::Thinking,
            State::Speaking { .. } => StateKind::Speaking,
            State::AwaitingConfirmation { .. } => StateKind::AwaitingConfirmation,
        }
    }

    pub fn turn(&self) -> Option<TurnId> {
        match self {
            State::Idle | State::Listening { .. } => None,
            State::Thinking { turn, .. }
            | State::Speaking { turn, .. }
            | State::AwaitingConfirmation { turn, .. } => Some(*turn),
        }
    }

    /// The turn whose brain is running, the only turn brain events may affect.
    pub fn brain_turn(&self) -> Option<TurnId> {
        match self {
            State::Thinking { turn, .. }
            | State::AwaitingConfirmation { turn, .. }
            | State::Speaking {
                turn,
                brain: Some(_),
                ..
            } => Some(*turn),
            _ => None,
        }
    }

    pub fn work(&self) -> Option<&BrainWork> {
        match self {
            State::Thinking { work, .. }
            | State::AwaitingConfirmation { work, .. }
            | State::Speaking {
                brain: Some(work), ..
            } => Some(work),
            _ => None,
        }
    }

    fn work_mut(&mut self) -> Option<&mut BrainWork> {
        match self {
            State::Thinking { work, .. }
            | State::AwaitingConfirmation { work, .. }
            | State::Speaking {
                brain: Some(work), ..
            } => Some(work),
            _ => None,
        }
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            State::Idle => write!(f, "idle"),
            State::Listening { .. } => write!(f, "listening"),
            State::Thinking { turn, .. } => write!(f, "thinking(turn {})", turn.0),
            State::Speaking { turn, .. } => write!(f, "speaking(turn {})", turn.0),
            State::AwaitingConfirmation { turn, call, .. } => {
                write!(f, "awaiting_confirmation(turn {}, {})", turn.0, call.name)
            }
        }
    }
}

#[derive(Clone, Debug)]
struct FloorItem {
    text: String,
    source: String,
}

/// The single owner of dialogue state. Everything goes through `handle`.
pub struct Dialogue {
    config: Config,
    registry: Registry,
    router: Router,
    state: State,
    now_ms: u64,
    last_turn: u64,
    last_segment: u64,
    last_speech: u64,
    floor_queue: VecDeque<FloorItem>,
    floor_hold_until_ms: u64,
    suggestions: Suggestions,
    instincts: InstinctGate,
}

impl Dialogue {
    pub fn new(config: Config) -> Self {
        Self {
            suggestions: Suggestions::new(config.max_open_suggestions),
            instincts: InstinctGate::new(config.instinct.clone()),
            config,
            registry: Registry::default(),
            router: Router::default(),
            state: State::Idle,
            now_ms: 0,
            last_turn: 0,
            last_segment: 0,
            last_speech: 0,
            floor_queue: VecDeque::new(),
            floor_hold_until_ms: 0,
        }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn now_ms(&self) -> u64 {
        self.now_ms
    }

    pub fn floor_queue_len(&self) -> usize {
        self.floor_queue.len()
    }

    pub fn suggestions(&self) -> &Suggestions {
        &self.suggestions
    }

    /// For the host to restore dismissed titles from an earlier run.
    pub fn suggestions_mut(&mut self) -> &mut Suggestions {
        &mut self.suggestions
    }

    pub fn registry_mut(&mut self) -> &mut Registry {
        &mut self.registry
    }

    pub fn router_mut(&mut self) -> &mut Router {
        &mut self.router
    }

    pub fn handle(&mut self, event: Event) -> Vec<Effect> {
        let mut fx = Vec::new();
        let state = mem::replace(&mut self.state, State::Idle);
        self.state = self.step(state, event, &mut fx);
        // The only place queued floor items are released, so reaching Idle by
        // any path releases them.
        self.release_floor(&mut fx);
        // Instincts wait behind the floor queue, so a reminder goes first.
        self.release_instinct(&mut fx);
        fx
    }

    /// Queued instincts that still wait for the floor.
    pub fn instincts_waiting(&self) -> usize {
        self.instincts.waiting_len()
    }

    fn step(&mut self, state: State, event: Event, fx: &mut Vec<Effect>) -> State {
        let name = event.name();
        match event {
            Event::Tick { now_ms } => self.on_tick(state, now_ms, fx),
            Event::SpeechStarted => self.on_speech_started(state, fx),
            Event::SpeechEnded => self.on_speech_ended(state, fx),
            Event::FinalTranscript { segment, text } => {
                self.on_transcript(state, segment, Some(text), fx)
            }
            Event::UnclearTranscript { segment, .. } => {
                self.on_transcript(state, segment, None, fx)
            }
            Event::PlaybackFinished { up_to } => self.on_playback_finished(state, up_to, fx),
            Event::Cancel => self.on_cancel(state, fx),
            Event::RequestFloor {
                priority,
                text,
                source,
            } => self.on_floor_request(state, priority, text, source, fx),
            Event::Initiative { text } => self.on_initiative(state, text, fx),
            Event::Instinct { candidate } => self.on_instinct(state, candidate, fx),
            Event::AcceptSuggestion { id } => self.on_accept(state, id, fx),
            Event::DismissSuggestion { id } => {
                if self.suggestions.dismiss(id).is_none() {
                    diagnostic(fx, name, Reason::UnknownSuggestion);
                }
                state
            }
            Event::BrainText { turn, text } if state.brain_turn() == Some(turn) => {
                self.on_brain_text(state, text, fx)
            }
            Event::BrainToolCall { turn, call } if state.brain_turn() == Some(turn) => {
                self.on_tool_call(state, turn, call, fx)
            }
            Event::BrainDone { turn } if state.brain_turn() == Some(turn) => {
                self.on_brain_done(state, turn, fx)
            }
            Event::BrainFailed { turn, reason } if state.brain_turn() == Some(turn) => {
                self.on_brain_failed(state, turn, reason, fx)
            }
            Event::ToolFinished {
                turn,
                call_id,
                outcome,
            } if state.brain_turn() == Some(turn) => {
                self.on_tool_finished(state, turn, call_id, outcome, fx)
            }
            // Every brain or tool event reaching here names a turn that is not
            // running: this drop is what makes cancelling safe.
            Event::BrainText { .. }
            | Event::BrainToolCall { .. }
            | Event::BrainDone { .. }
            | Event::BrainFailed { .. }
            | Event::ToolFinished { .. } => {
                diagnostic(fx, name, Reason::StaleTurn);
                state
            }
        }
    }

    fn on_tick(&mut self, state: State, now_ms: u64, fx: &mut Vec<Effect>) -> State {
        if now_ms < self.now_ms {
            diagnostic(fx, "tick", Reason::ClockWentBack);
            return state;
        }
        self.now_ms = now_ms;
        if state
            .work()
            .is_some_and(|work| now_ms >= work.turn_deadline_ms)
        {
            diagnostic(fx, "tick", Reason::TurnLimit);
            return self.cut_turn(state, fx);
        }
        match state {
            State::Listening {
                capture,
                deadline_ms,
            } if now_ms >= deadline_ms => {
                diagnostic(fx, "tick", Reason::ListenTimeout);
                self.close_listening(capture, fx)
            }
            State::Thinking { turn, work } if now_ms >= work.deadline_ms => {
                cancel_work(turn, &work, fx);
                self.fail(work.source, FailureKind::Watchdog, "tick", fx)
            }
            State::Speaking {
                turn,
                brain,
                deadline_ms,
                ..
            } if now_ms >= deadline_ms => {
                fx.push(Effect::StopPlayback);
                diagnostic(fx, "tick", Reason::PlaybackTimeout);
                match brain {
                    Some(work) => self.back_to_thinking(turn, work),
                    None => State::Idle,
                }
            }
            State::AwaitingConfirmation {
                turn,
                work,
                playback,
                deadline_ms,
                ..
            } if now_ms >= deadline_ms => self.abort_confirmation(turn, &work, playback, fx),
            other => other,
        }
    }

    fn on_speech_started(&mut self, state: State, fx: &mut Vec<Effect>) -> State {
        let deadline_ms = self.now_ms + self.config.listen_timeout_ms;
        match state {
            State::Idle => self.listen(Capture::default(), deadline_ms),
            State::Listening { mut capture, .. } if capture.active.is_none() => {
                capture.begin(self.next_segment());
                State::Listening {
                    capture,
                    deadline_ms,
                }
            }
            State::Thinking { turn, work } => {
                cancel_work(turn, &work, fx);
                // Initiative text is an instruction, not something the owner
                // said, so it is never merged into the owner's words.
                let held = if work.committed || work.source == TurnSource::Initiative {
                    Vec::new()
                } else {
                    vec![work.transcript]
                };
                self.listen(Capture::holding(held), deadline_ms)
            }
            State::Speaking { turn, brain, .. } => {
                fx.push(Effect::StopPlayback);
                if let Some(work) = &brain {
                    cancel_work(turn, work, fx);
                }
                self.listen(Capture::default(), deadline_ms)
            }
            State::AwaitingConfirmation {
                turn,
                work,
                call,
                mut capture,
                reasked,
                playback,
                ..
            } if capture.active.is_none() => {
                if playback.is_some() {
                    fx.push(Effect::StopPlayback);
                }
                capture.begin(self.next_segment());
                State::AwaitingConfirmation {
                    turn,
                    work,
                    call,
                    capture,
                    reasked,
                    playback: None,
                    deadline_ms: self.now_ms + self.config.confirm_timeout_ms,
                }
            }
            other => {
                diagnostic(fx, "speech_started", Reason::SpeechAlreadyStarted);
                other
            }
        }
    }

    fn on_speech_ended(&mut self, mut state: State, fx: &mut Vec<Effect>) -> State {
        let (capture, deadline_ms, extension) = match &mut state {
            State::Listening {
                capture,
                deadline_ms,
            } => (capture, deadline_ms, self.config.listen_timeout_ms),
            State::AwaitingConfirmation {
                capture,
                deadline_ms,
                ..
            } => (capture, deadline_ms, self.config.confirm_timeout_ms),
            _ => {
                diagnostic(fx, "speech_ended", Reason::SpeechNotStarted);
                return state;
            }
        };
        match capture.active.take() {
            Some(segment) => {
                *deadline_ms = self.now_ms + extension;
                fx.push(Effect::Transcribe { segment });
            }
            None => diagnostic(fx, "speech_ended", Reason::SpeechNotStarted),
        }
        state
    }

    /// `text` is `None` for an `UnclearTranscript`.
    fn on_transcript(
        &mut self,
        state: State,
        segment: SegmentId,
        text: Option<String>,
        fx: &mut Vec<Effect>,
    ) -> State {
        match state {
            State::Listening {
                mut capture,
                deadline_ms,
            } => {
                if let Err(reason) = capture.resolve(segment, text) {
                    diagnostic(fx, "final_transcript", reason);
                    return State::Listening {
                        capture,
                        deadline_ms,
                    };
                }
                if capture.is_complete() {
                    return self.close_listening(capture, fx);
                }
                State::Listening {
                    capture,
                    deadline_ms: self.now_ms + self.config.listen_timeout_ms,
                }
            }
            State::AwaitingConfirmation {
                turn,
                work,
                call,
                mut capture,
                reasked,
                playback,
                deadline_ms,
            } => {
                let resolved = capture.resolve(segment, text);
                if let Err(reason) = resolved {
                    diagnostic(fx, "final_transcript", reason);
                } else if capture.is_complete() {
                    return self.decide_confirmation(
                        turn,
                        work,
                        call,
                        capture,
                        reasked,
                        playback,
                        deadline_ms,
                        fx,
                    );
                }
                State::AwaitingConfirmation {
                    turn,
                    work,
                    call,
                    capture,
                    reasked,
                    playback,
                    deadline_ms,
                }
            }
            other => {
                diagnostic(fx, "final_transcript", Reason::StaleSegment);
                other
            }
        }
    }

    fn close_listening(&mut self, capture: Capture, fx: &mut Vec<Effect>) -> State {
        if capture.ends_in_hard_stop() {
            self.hold_floor();
            return State::Idle;
        }
        let text = capture.merged_text();
        if text.is_empty() {
            if capture.unclear {
                let text = self.config.phrases.unclear_transcript.clone();
                return self.speak_alone(text, SpeechKind::Notice, fx);
            }
            diagnostic(fx, "final_transcript", Reason::EmptyTranscript);
            return State::Idle;
        }
        self.start_turn(text, fx)
    }

    /// An owner turn: routes first, then the brain.
    fn start_turn(&mut self, text: String, fx: &mut Vec<Effect>) -> State {
        let turn = self.next_turn();
        if let Some((route, reply)) = self.router.route(&text) {
            if reply.trim().is_empty() {
                return State::Idle;
            }
            return self.speak_turn(turn, reply, SpeechKind::Route(route), fx);
        }
        self.start_brain(turn, text, TurnSource::Owner, fx)
    }

    fn start_brain(
        &mut self,
        turn: TurnId,
        text: String,
        source: TurnSource,
        fx: &mut Vec<Effect>,
    ) -> State {
        let tools = match source {
            TurnSource::Owner => self.registry.enabled_specs(),
            TurnSource::Initiative => {
                let mut tools: Vec<ToolSpec> = self
                    .registry
                    .enabled_specs()
                    .into_iter()
                    .filter(|spec| spec.name != SUGGEST_TOOL && policy::offered(source, spec))
                    .collect();
                tools.push(suggest_spec());
                tools
            }
        };
        fx.push(Effect::StartBrain {
            turn,
            text: text.clone(),
            context: BrainContext {
                tools,
                source,
                memory: String::new(),
                now: String::new(),
            },
        });
        State::Thinking {
            turn,
            work: BrainWork {
                transcript: text,
                deadline_ms: self.now_ms + self.config.think_timeout_ms,
                committed: false,
                running_tools: Vec::new(),
                source,
                turn_deadline_ms: self.now_ms + self.config.max_turn_ms,
            },
        }
    }

    fn on_initiative(&mut self, state: State, text: String, fx: &mut Vec<Effect>) -> State {
        let busy = !matches!(state, State::Idle)
            || !self.floor_queue.is_empty()
            || self.now_ms < self.floor_hold_until_ms;
        if busy {
            diagnostic(fx, "initiative", Reason::InitiativeBusy);
            return state;
        }
        if text.trim().is_empty() {
            diagnostic(fx, "initiative", Reason::EmptyText);
            return state;
        }
        let turn = self.next_turn();
        self.start_brain(turn, text, TurnSource::Initiative, fx)
    }

    /// Queues the candidate if the gate lets it through; `handle` starts it
    /// as soon as the floor is free (maybe right away).
    fn on_instinct(
        &mut self,
        state: State,
        candidate: InstinctCandidate,
        fx: &mut Vec<Effect>,
    ) -> State {
        let floor_free = self.floor_is_free(&state);
        let id = candidate.id.clone();
        let mut dropped = Vec::new();
        let offered = self
            .instincts
            .offer(candidate, self.now_ms, floor_free, &mut dropped);
        if let Err(outcome) = offered {
            dropped.push((id, outcome));
        }
        instinct_dropped(dropped, fx);
        state
    }

    fn floor_is_free(&self, state: &State) -> bool {
        matches!(state, State::Idle)
            && self.floor_queue.is_empty()
            && self.now_ms >= self.floor_hold_until_ms
    }

    /// Starts the next instinct the gate lets through, as an initiative turn.
    fn release_instinct(&mut self, fx: &mut Vec<Effect>) {
        if !self.floor_is_free(&self.state) {
            return;
        }
        let mut dropped = Vec::new();
        let next = self.instincts.next(self.now_ms, &mut dropped);
        instinct_dropped(dropped, fx);
        if let Some(candidate) = next {
            let turn = self.next_turn();
            fx.push(Effect::InstinctDecided {
                id: candidate.id.clone(),
                outcome: InstinctOutcome::Started(turn),
            });
            self.state =
                self.start_brain(turn, candidate.instruction(), TurnSource::Initiative, fx);
        }
    }

    /// Accepting is the owner asking for it, so it interrupts like a new request.
    fn on_accept(&mut self, state: State, id: SuggestionId, fx: &mut Vec<Effect>) -> State {
        match self.suggestions.accept(id) {
            Some(suggestion) => {
                stop_everything(state, fx);
                self.start_turn(suggestion.ask, fx)
            }
            None => {
                diagnostic(fx, "accept_suggestion", Reason::UnknownSuggestion);
                state
            }
        }
    }

    /// Whether `source` may run the call now, and the tool's spec if so. The
    /// error carries the failure for the brain and, for a policy refusal, the
    /// diagnostic to report.
    fn admit(
        &self,
        source: TurnSource,
        call: &ToolCall,
    ) -> Result<ToolSpec, (String, Option<Reason>)> {
        let spec = self
            .registry
            .available(&call.name)
            .map_err(|failure| (failure, None))?;
        policy::check_tool(source, spec)
            .map_err(|refusal| (refusal.to_owned(), Some(Reason::ToolNotAllowed)))?;
        policy::parse_args(&call.args_json)
            .map_err(|failure| (failure, Some(Reason::InvalidToolArgs)))?;
        Ok(spec.clone())
    }

    #[allow(clippy::too_many_arguments)]
    fn decide_confirmation(
        &mut self,
        turn: TurnId,
        work: BrainWork,
        call: ToolCall,
        capture: Capture,
        reasked: bool,
        playback: Option<SpeechId>,
        deadline_ms: u64,
        fx: &mut Vec<Effect>,
    ) -> State {
        let answer = capture.merged_text();
        if answer.is_empty() && !capture.unclear {
            // A cough is not an answer and must not use up the re-ask.
            diagnostic(fx, "final_transcript", Reason::EmptyTranscript);
            return State::AwaitingConfirmation {
                turn,
                work,
                call,
                capture: Capture::default(),
                reasked,
                playback,
                deadline_ms,
            };
        }
        if is_hard_stop(&answer) {
            cancel_work(turn, &work, fx);
            self.hold_floor();
            return State::Idle;
        }
        // Words the STT could not make out are no answer, but they were said:
        // they get the re-ask.
        let confirmation = if answer.is_empty() {
            Confirmation::Unclear
        } else {
            parse_confirmation(&answer)
        };
        match confirmation {
            Confirmation::Yes => self.approve(turn, work, call, fx),
            Confirmation::No => self.abort_confirmation(turn, &work, None, fx),
            Confirmation::Unclear if !reasked => {
                let text = self.config.phrases.confirmation_reask.clone();
                let speech = self.say(turn, text, SpeechKind::Question, fx);
                State::AwaitingConfirmation {
                    turn,
                    work,
                    call,
                    capture: Capture::default(),
                    reasked: true,
                    playback: Some(speech),
                    deadline_ms: self.now_ms + self.config.confirm_timeout_ms,
                }
            }
            Confirmation::Unclear => self.abort_confirmation(turn, &work, None, fx),
        }
    }

    fn approve(
        &mut self,
        turn: TurnId,
        mut work: BrainWork,
        call: ToolCall,
        fx: &mut Vec<Effect>,
    ) -> State {
        // The time spent waiting for the owner is not the brain's fault.
        work.deadline_ms = self.now_ms + self.config.think_timeout_ms;
        // Checked again: the tool may have gone away or changed while waiting.
        match self.admit(work.source, &call) {
            Ok(spec) => self.start_tool(turn, work, call, spec.ack, None, fx),
            Err((failure, reason)) => {
                if let Some(reason) = reason {
                    diagnostic(fx, "final_transcript", reason);
                }
                fx.push(Effect::ToolResultToBrain {
                    turn,
                    call_id: call.id,
                    outcome: ToolOutcome::Failure(failure),
                });
                State::Thinking { turn, work }
            }
        }
    }

    fn abort_confirmation(
        &mut self,
        turn: TurnId,
        work: &BrainWork,
        playback: Option<SpeechId>,
        fx: &mut Vec<Effect>,
    ) -> State {
        if playback.is_some() {
            fx.push(Effect::StopPlayback);
        }
        cancel_work(turn, work, fx);
        let text = self.config.phrases.confirmation_cancelled.clone();
        self.speak_alone(text, SpeechKind::Notice, fx)
    }

    fn on_playback_finished(
        &mut self,
        state: State,
        up_to: SpeechId,
        fx: &mut Vec<Effect>,
    ) -> State {
        const EVENT: &str = "playback_finished";
        match state {
            State::Speaking {
                turn,
                brain,
                last_speech,
                deadline_ms,
            } => {
                if up_to == last_speech {
                    return match brain {
                        Some(work) => self.back_to_thinking(turn, work),
                        None => State::Idle,
                    };
                }
                let reason = if up_to < last_speech {
                    Reason::PlaybackStillQueued
                } else {
                    Reason::StalePlayback
                };
                diagnostic(fx, EVENT, reason);
                State::Speaking {
                    turn,
                    brain,
                    last_speech,
                    deadline_ms,
                }
            }
            State::AwaitingConfirmation {
                turn,
                work,
                call,
                capture,
                reasked,
                playback,
                deadline_ms,
            } => {
                let playback = match playback {
                    Some(last) if up_to == last => None,
                    Some(last) if up_to < last => {
                        diagnostic(fx, EVENT, Reason::PlaybackStillQueued);
                        Some(last)
                    }
                    other => {
                        diagnostic(fx, EVENT, Reason::StalePlayback);
                        other
                    }
                };
                State::AwaitingConfirmation {
                    turn,
                    work,
                    call,
                    capture,
                    reasked,
                    playback,
                    deadline_ms,
                }
            }
            other => {
                diagnostic(fx, EVENT, Reason::StalePlayback);
                other
            }
        }
    }

    fn back_to_thinking(&self, turn: TurnId, mut work: BrainWork) -> State {
        // Keeps any tool extension still pending. Time spent playing audio does
        // not count against the brain.
        work.deadline_ms = work
            .deadline_ms
            .max(self.now_ms + self.config.think_timeout_ms);
        State::Thinking { turn, work }
    }

    fn on_cancel(&mut self, state: State, fx: &mut Vec<Effect>) -> State {
        if matches!(state, State::Idle) {
            diagnostic(fx, "cancel", Reason::NothingToCancel);
            return State::Idle;
        }
        stop_everything(state, fx);
        self.hold_floor();
        State::Idle
    }

    fn on_floor_request(
        &mut self,
        state: State,
        priority: Priority,
        text: String,
        source: String,
        fx: &mut Vec<Effect>,
    ) -> State {
        if text.trim().is_empty() {
            diagnostic(fx, "request_floor", Reason::EmptyText);
            return state;
        }
        if matches!(state, State::Idle) && self.floor_queue.is_empty() {
            return self.speak_alone(text, SpeechKind::Floor(source), fx);
        }
        match priority {
            Priority::High => {
                while self.floor_queue.len() >= self.config.floor_queue_cap.max(1) {
                    self.floor_queue.pop_front();
                    diagnostic(fx, "request_floor", Reason::FloorQueueFull);
                }
                fx.push(Effect::FloorQueued {
                    source: source.clone(),
                });
                self.floor_queue.push_back(FloorItem { text, source });
            }
            Priority::Low => diagnostic(fx, "request_floor", Reason::FloorDropped),
        }
        state
    }

    fn release_floor(&mut self, fx: &mut Vec<Effect>) {
        if !matches!(self.state, State::Idle) || self.now_ms < self.floor_hold_until_ms {
            return;
        }
        if let Some(item) = self.floor_queue.pop_front() {
            self.state = self.speak_alone(item.text, SpeechKind::Floor(item.source), fx);
        }
    }

    fn hold_floor(&mut self) {
        self.floor_hold_until_ms = self.now_ms + self.config.floor_resume_after_cancel_ms;
    }

    fn on_brain_text(&mut self, state: State, text: String, fx: &mut Vec<Effect>) -> State {
        const EVENT: &str = "brain_text";
        if matches!(state, State::AwaitingConfirmation { .. }) {
            // Whatever the model says here, it can't answer the question.
            diagnostic(fx, EVENT, Reason::BrainPaused);
            return state;
        }
        if text.trim().is_empty() {
            diagnostic(fx, EVENT, Reason::EmptyText);
            return state;
        }
        match state {
            State::Thinking { turn, mut work } => {
                work.committed = true;
                let last_speech = self.say(turn, text, SpeechKind::Answer, fx);
                self.speaking(turn, Some(work), last_speech)
            }
            State::Speaking { turn, brain, .. } => {
                let last_speech = self.say(turn, text, SpeechKind::Answer, fx);
                self.speaking(turn, brain, last_speech)
            }
            other => other,
        }
    }

    fn on_tool_call(
        &mut self,
        state: State,
        turn: TurnId,
        call: ToolCall,
        fx: &mut Vec<Effect>,
    ) -> State {
        let (work, playback) = match state {
            State::Thinking { work, .. } => (work, None),
            State::Speaking {
                brain: Some(work),
                last_speech,
                ..
            } => (work, Some(last_speech)),
            other => {
                fx.push(Effect::ToolResultToBrain {
                    turn,
                    call_id: call.id,
                    outcome: ToolOutcome::Failure("a confirmation is pending".into()),
                });
                return other;
            }
        };
        if work.source == TurnSource::Initiative && call.name == SUGGEST_TOOL {
            return self.suggest(turn, work, call, playback, fx);
        }
        match self.admit(work.source, &call) {
            Err((failure, reason)) => {
                if let Some(reason) = reason {
                    diagnostic(fx, "brain_tool_call", reason);
                }
                fx.push(Effect::ToolResultToBrain {
                    turn,
                    call_id: call.id,
                    outcome: ToolOutcome::Failure(failure),
                });
                self.resume(turn, work, playback)
            }
            Ok(spec) if spec.needs_confirmation => {
                let question = format!(
                    "Voy a usar {}: {}. ¿Confirmás? Decime sí o no.",
                    spec.name, spec.description
                );
                let speech = self.say(turn, question, SpeechKind::Question, fx);
                State::AwaitingConfirmation {
                    turn,
                    work,
                    call,
                    capture: Capture::default(),
                    reasked: false,
                    playback: Some(speech),
                    deadline_ms: self.now_ms + self.config.confirm_timeout_ms,
                }
            }
            Ok(spec) => self.start_tool(turn, work, call, spec.ack, playback, fx),
        }
    }

    /// The built-in `suggest` tool: stores the proposal, runs nothing.
    fn suggest(
        &mut self,
        turn: TurnId,
        work: BrainWork,
        call: ToolCall,
        playback: Option<SpeechId>,
        fx: &mut Vec<Effect>,
    ) -> State {
        let outcome = match policy::parse_args(&call.args_json) {
            Err(failure) => {
                diagnostic(fx, "brain_tool_call", Reason::InvalidToolArgs);
                ToolOutcome::Failure(failure)
            }
            Ok(args) => match self.suggestions.add_from_args(&args) {
                Ok(suggestion) => {
                    let result = format!("sugerencia guardada: {}", suggestion.title);
                    fx.push(Effect::SuggestionAdded { suggestion });
                    ToolOutcome::Success(result)
                }
                Err(error) => ToolOutcome::Failure(error.to_string()),
            },
        };
        fx.push(Effect::ToolResultToBrain {
            turn,
            call_id: call.id,
            outcome,
        });
        self.resume(turn, work, playback)
    }

    fn start_tool(
        &mut self,
        turn: TurnId,
        mut work: BrainWork,
        call: ToolCall,
        ack: Option<String>,
        playback: Option<SpeechId>,
        fx: &mut Vec<Effect>,
    ) -> State {
        work.committed = true;
        work.deadline_ms += self.config.tool_extension_ms;
        work.running_tools.push(call.clone());
        let playback = match ack {
            Some(ack) => Some(self.say(turn, ack, SpeechKind::Ack, fx)),
            None => playback,
        };
        fx.push(Effect::RunTool { turn, call });
        self.resume(turn, work, playback)
    }

    fn on_brain_done(&mut self, state: State, turn: TurnId, fx: &mut Vec<Effect>) -> State {
        match state {
            State::Thinking { work, .. } => {
                cancel_tools(turn, &work, fx);
                State::Idle
            }
            State::Speaking {
                brain: Some(work),
                last_speech,
                deadline_ms,
                ..
            } => {
                cancel_tools(turn, &work, fx);
                State::Speaking {
                    turn,
                    brain: None,
                    last_speech,
                    deadline_ms,
                }
            }
            other => {
                diagnostic(fx, "brain_done", Reason::BrainPaused);
                other
            }
        }
    }

    fn on_brain_failed(
        &mut self,
        state: State,
        turn: TurnId,
        reason: String,
        fx: &mut Vec<Effect>,
    ) -> State {
        let (work, playing) = match state {
            State::Thinking { work, .. } => (work, false),
            State::Speaking {
                brain: Some(work), ..
            } => (work, true),
            State::AwaitingConfirmation { work, playback, .. } => (work, playback.is_some()),
            other => return other,
        };
        if playing {
            fx.push(Effect::StopPlayback);
        }
        cancel_tools(turn, &work, fx);
        self.fail(work.source, FailureKind::Brain(reason), "brain_failed", fx)
    }

    /// Owner turns hear what went wrong. Initiative turns end quietly: the owner
    /// did not ask for anything, so there is nothing to apologize for.
    fn fail(
        &mut self,
        source: TurnSource,
        kind: FailureKind,
        event: &'static str,
        fx: &mut Vec<Effect>,
    ) -> State {
        match source {
            TurnSource::Owner => {
                let text = match kind {
                    FailureKind::Watchdog => self.config.phrases.watchdog_failure.clone(),
                    FailureKind::Brain(_) => self.config.phrases.brain_failure.clone(),
                };
                self.speak_alone(text, SpeechKind::Failure(kind), fx)
            }
            TurnSource::Initiative => {
                diagnostic(fx, event, Reason::InitiativeFailed);
                State::Idle
            }
        }
    }

    /// The hard per-turn cap: whatever the brain is doing, the turn ends.
    fn cut_turn(&mut self, state: State, fx: &mut Vec<Effect>) -> State {
        let source = state.work().map_or(TurnSource::Owner, |work| work.source);
        stop_everything(state, fx);
        self.fail(source, FailureKind::Watchdog, "tick", fx)
    }

    fn on_tool_finished(
        &mut self,
        mut state: State,
        turn: TurnId,
        call_id: String,
        outcome: ToolOutcome,
        fx: &mut Vec<Effect>,
    ) -> State {
        let running = state.work_mut().and_then(|work| {
            let index = work.running_tools.iter().position(|c| c.id == call_id)?;
            work.running_tools.remove(index);
            Some(())
        });
        match running {
            Some(()) => fx.push(Effect::ToolResultToBrain {
                turn,
                call_id,
                outcome,
            }),
            None => diagnostic(fx, "tool_finished", Reason::UnknownToolCall),
        }
        state
    }

    fn listen(&mut self, mut capture: Capture, deadline_ms: u64) -> State {
        capture.begin(self.next_segment());
        State::Listening {
            capture,
            deadline_ms,
        }
    }

    fn resume(&self, turn: TurnId, work: BrainWork, playback: Option<SpeechId>) -> State {
        match playback {
            Some(last_speech) => self.speaking(turn, Some(work), last_speech),
            None => State::Thinking { turn, work },
        }
    }

    fn speaking(&self, turn: TurnId, brain: Option<BrainWork>, last_speech: SpeechId) -> State {
        State::Speaking {
            turn,
            brain,
            last_speech,
            deadline_ms: self.now_ms + self.config.playback_timeout_ms,
        }
    }

    /// Speech that isn't an answer to a running brain, as its own short turn:
    /// that keeps it interruptible, and nothing is ever said for a cancelled turn.
    fn speak_alone(&mut self, text: String, kind: SpeechKind, fx: &mut Vec<Effect>) -> State {
        let turn = self.next_turn();
        self.speak_turn(turn, text, kind, fx)
    }

    fn speak_turn(
        &mut self,
        turn: TurnId,
        text: String,
        kind: SpeechKind,
        fx: &mut Vec<Effect>,
    ) -> State {
        let last_speech = self.say(turn, text, kind, fx);
        self.speaking(turn, None, last_speech)
    }

    fn say(
        &mut self,
        turn: TurnId,
        text: String,
        kind: SpeechKind,
        fx: &mut Vec<Effect>,
    ) -> SpeechId {
        self.last_speech += 1;
        let speech = SpeechId(self.last_speech);
        fx.push(Effect::Speak {
            speech,
            turn,
            text,
            kind,
        });
        speech
    }

    fn next_turn(&mut self) -> TurnId {
        self.last_turn += 1;
        TurnId(self.last_turn)
    }

    fn next_segment(&mut self) -> SegmentId {
        self.last_segment += 1;
        SegmentId(self.last_segment)
    }
}

fn instinct_dropped(dropped: Vec<(String, InstinctOutcome)>, fx: &mut Vec<Effect>) {
    for (id, outcome) in dropped {
        if let Some(reason) = outcome.reason() {
            diagnostic(fx, "instinct", reason);
        }
        fx.push(Effect::InstinctDecided { id, outcome });
    }
}

fn diagnostic(fx: &mut Vec<Effect>, event: &'static str, reason: Reason) {
    fx.push(Effect::Diagnostic { event, reason });
}

/// Stops whatever the state was doing: audio, brain and tools.
fn stop_everything(state: State, fx: &mut Vec<Effect>) {
    match state {
        State::Idle | State::Listening { .. } => {}
        State::Thinking { turn, work } => cancel_work(turn, &work, fx),
        State::Speaking { turn, brain, .. } => {
            fx.push(Effect::StopPlayback);
            if let Some(work) = &brain {
                cancel_work(turn, work, fx);
            }
        }
        State::AwaitingConfirmation {
            turn,
            work,
            playback,
            ..
        } => {
            if playback.is_some() {
                fx.push(Effect::StopPlayback);
            }
            cancel_work(turn, &work, fx);
        }
    }
}

fn cancel_work(turn: TurnId, work: &BrainWork, fx: &mut Vec<Effect>) {
    fx.push(Effect::CancelBrain { turn });
    cancel_tools(turn, work, fx);
}

fn cancel_tools(turn: TurnId, work: &BrainWork, fx: &mut Vec<Effect>) {
    for call in &work.running_tools {
        fx.push(Effect::CancelTool {
            turn,
            call_id: call.id.clone(),
            name: call.name.clone(),
        });
    }
}
