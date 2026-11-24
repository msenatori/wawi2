use crate::instinct::InstinctCandidate;
use crate::plugin::BrainContext;
use crate::suggestions::{Suggestion, SuggestionId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TurnId(pub u64);

/// One stretch of user speech, from VAD start to VAD end. Transcripts refer to
/// it, so a transcript that arrives after a cancel can be told apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SegmentId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpeechId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority {
    Low,
    High,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args_json: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolOutcome {
    Success(String),
    Failure(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Tick {
        now_ms: u64,
    },
    SpeechStarted,
    SpeechEnded,
    FinalTranscript {
        segment: SegmentId,
        text: String,
    },
    /// The STT heard speech for the segment but judged it unusable (for
    /// example, not Spanish). It closes the segment like an empty transcript;
    /// if nothing else was said, Wawi asks the owner to repeat instead of
    /// starting a turn. `text` is what the STT heard, for logs only: it
    /// never reaches the brain.
    UnclearTranscript {
        segment: SegmentId,
        text: String,
    },
    /// Everything up to and including `up_to` has finished playing.
    PlaybackFinished {
        up_to: SpeechId,
    },
    Cancel,
    BrainText {
        turn: TurnId,
        text: String,
    },
    /// Carries no confirmation flag on purpose: only the registry decides
    /// whether a tool needs one, and only the owner's transcript answers it.
    BrainToolCall {
        turn: TurnId,
        call: ToolCall,
    },
    BrainDone {
        turn: TurnId,
    },
    BrainFailed {
        turn: TurnId,
        reason: String,
    },
    /// The owner's subscription refused the brain for its usage limit. It
    /// ends `turn` (if that turn is still running) like a failure, and starts
    /// the limit (`limit.rs`). `resets_in_ms` is when the CLI said it
    /// resets, if it did; `until` is that time in words for the owner
    /// ("las 18:00"), filled in by the host, or empty.
    UsageLimited {
        turn: Option<TurnId>,
        resets_in_ms: Option<u64>,
        until: String,
    },
    ToolFinished {
        turn: TurnId,
        call_id: String,
        outcome: ToolOutcome,
    },
    RequestFloor {
        priority: Priority,
        text: String,
        source: String,
    },
    /// Wawi's own proactive run (scheduled or triggered). Starts an initiative
    /// turn only when nothing else is going on; it never interrupts the owner.
    /// `text` is the instruction for the brain, not something the owner said.
    Initiative {
        text: String,
    },
    /// Something a plugin noticed in the background (a watch result, a
    /// routine, a push). The instinct gate decides whether and when it
    /// becomes an initiative turn; while the floor is busy it waits.
    Instinct {
        candidate: InstinctCandidate,
    },
    /// The owner accepts an open suggestion: its `ask` becomes an owner turn,
    /// interrupting whatever was going on, like a new request would.
    AcceptSuggestion {
        id: SuggestionId,
    },
    /// The owner dismisses a suggestion; its title is never proposed again.
    DismissSuggestion {
        id: SuggestionId,
    },
}

impl Event {
    pub fn name(&self) -> &'static str {
        match self {
            Event::Tick { .. } => "tick",
            Event::SpeechStarted => "speech_started",
            Event::SpeechEnded => "speech_ended",
            Event::FinalTranscript { .. } => "final_transcript",
            Event::UnclearTranscript { .. } => "unclear_transcript",
            Event::PlaybackFinished { .. } => "playback_finished",
            Event::Cancel => "cancel",
            Event::BrainText { .. } => "brain_text",
            Event::BrainToolCall { .. } => "brain_tool_call",
            Event::BrainDone { .. } => "brain_done",
            Event::BrainFailed { .. } => "brain_failed",
            Event::UsageLimited { .. } => "usage_limited",
            Event::ToolFinished { .. } => "tool_finished",
            Event::RequestFloor { .. } => "request_floor",
            Event::Initiative { .. } => "initiative",
            Event::Instinct { .. } => "instinct",
            Event::AcceptSuggestion { .. } => "accept_suggestion",
            Event::DismissSuggestion { .. } => "dismiss_suggestion",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FailureKind {
    Watchdog,
    Brain(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpeechKind {
    Answer,
    Ack,
    Question,
    Notice,
    Route(String),
    Failure(FailureKind),
    Floor(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticReason {
    StaleTurn,
    StaleSegment,
    SegmentStillOpen,
    StalePlayback,
    PlaybackStillQueued,
    SpeechNotStarted,
    SpeechAlreadyStarted,
    NothingToCancel,
    BrainPaused,
    UnknownToolCall,
    EmptyTranscript,
    EmptyText,
    ClockWentBack,
    FloorDropped,
    ListenTimeout,
    PlaybackTimeout,
    /// The floor queue was full: its oldest item was dropped.
    FloorQueueFull,
    /// The turn reached `Config::max_turn_ms` and was cut.
    TurnLimit,
    /// An initiative arrived while the owner or another turn had the floor.
    InitiativeBusy,
    /// An initiative turn failed or timed out; it ends without speaking.
    InitiativeFailed,
    /// The turn's source may not use that tool; the brain got a failure.
    ToolNotAllowed,
    /// Tool arguments were not a JSON object; the brain got a failure.
    InvalidToolArgs,
    UnknownSuggestion,
    /// An instinct arrived during quiet hours, or waited into them.
    InstinctQuiet,
    /// The same instinct spoke too recently.
    InstinctCooldown,
    /// The same instinct already said this result recently.
    InstinctDuplicate,
    /// A queued instinct waited too long for the floor.
    InstinctExpired,
    /// The instinct queue was full: its oldest item was dropped.
    InstinctQueueFull,
    /// The instinct's own deadline (`expires_in_ms`) passed.
    InstinctPastDue,
    /// The subscription's usage limit: background work waits for the reset.
    UsageLimited,
}

/// What the instinct gate did with a candidate, for the host that has to
/// acknowledge it (the inbox moves the item to `cur/`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstinctOutcome {
    /// It became this initiative turn.
    Started(TurnId),
    /// Not now, only for timing (quiet hours, cooldown, pacing, a busy
    /// floor): the same candidate may be offered again after `retry_in_ms`.
    /// A host with durable items (the inbox) keeps them and retries.
    Postponed {
        reason: DiagnosticReason,
        retry_in_ms: u64,
    },
    /// It will never be said, for this reason (a repeat, past due, empty).
    Dropped(DiagnosticReason),
}

impl InstinctOutcome {
    /// The diagnostic behind a postponement or a drop.
    pub fn reason(&self) -> Option<DiagnosticReason> {
        match self {
            InstinctOutcome::Started(_) => None,
            InstinctOutcome::Postponed { reason, .. } | InstinctOutcome::Dropped(reason) => {
                Some(*reason)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Speak {
        speech: SpeechId,
        turn: TurnId,
        text: String,
        kind: SpeechKind,
    },
    StopPlayback,
    StartBrain {
        turn: TurnId,
        text: String,
        context: BrainContext,
    },
    CancelBrain {
        turn: TurnId,
    },
    RunTool {
        turn: TurnId,
        call: ToolCall,
    },
    CancelTool {
        turn: TurnId,
        call_id: String,
        name: String,
    },
    ToolResultToBrain {
        turn: TurnId,
        call_id: String,
        outcome: ToolOutcome,
    },
    /// The segment closed: the STT must answer with a `FinalTranscript` for it.
    Transcribe {
        segment: SegmentId,
    },
    FloorQueued {
        source: String,
    },
    /// The initiative proposed something; the host may show it or persist it.
    SuggestionAdded {
        suggestion: Suggestion,
    },
    /// The instinct gate decided about the candidate with this `id`.
    InstinctDecided {
        id: String,
        outcome: InstinctOutcome,
    },
    /// The usage limit started (`limited`, trying again in `retry_in_ms`)
    /// or ended. The host lets background services know (`<home>/cupo.json`)
    /// and, when it ended and `told_owner`, has Wawi say she is back.
    UsageLimit {
        limited: bool,
        retry_in_ms: u64,
        told_owner: bool,
    },
    /// Something was dropped or gave up, on purpose. Never affects behaviour;
    /// it exists so that nothing disappears silently.
    Diagnostic {
        event: &'static str,
        reason: DiagnosticReason,
    },
}
