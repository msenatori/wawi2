//! Dialogue core for Wawi 2: a pure state machine plus the traits that plug
//! components into it. No I/O, threads or async live here.

pub mod dialogue;
pub mod event;
pub mod instinct;
pub mod json;
pub mod plugin;
pub mod policy;
pub mod registry;
pub mod router;
pub mod suggestions;
pub mod text;

pub use dialogue::{BrainWork, Capture, Config, Dialogue, Phrases, State, StateKind};
pub use event::{
    DiagnosticReason, Effect, Event, FailureKind, InstinctOutcome, Priority, SegmentId, SpeechId,
    SpeechKind, ToolCall, ToolOutcome, TurnId,
};
pub use instinct::{GateConfig, InstinctCandidate, InstinctGate};
pub use plugin::{AudioOut, Brain, BrainContext, EventSink, Stt, Tool, Tts};
pub use policy::{INITIATIVE_NO_CONFIRMATION, INITIATIVE_READ_ONLY, TurnSource};
pub use registry::{PermissionLevel, Registry, ToolSpec};
pub use router::{Clock, Route, RouteDecision, Router, TimeRoute};
pub use suggestions::{
    SUGGEST_TOOL, SuggestError, Suggestion, SuggestionId, Suggestions, suggest_spec,
};
pub use text::{Confirmation, is_hard_stop, normalize, parse_confirmation};
