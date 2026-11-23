//! Connection points for real components. Calls start work and return nothing:
//! results come back through an `EventSink` the component was built with.

use crate::event::{Event, SegmentId, SpeechId, ToolCall, ToolOutcome, TurnId};
use crate::policy::TurnSource;
use crate::registry::ToolSpec;

pub trait EventSink {
    fn send(&self, event: Event);
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BrainContext {
    /// Tools this turn may use, as of when it started: every enabled tool for
    /// the owner, only what the policy allows plus `suggest` for initiative.
    pub tools: Vec<ToolSpec>,
    pub source: TurnSource,
    /// Soul and memory text. The core never reads files and leaves it empty;
    /// the host fills it in before starting the brain.
    pub memory: String,
    /// The local date and time as the brain should read it, for example
    /// `viernes 20 de noviembre de 2026, 16:23 (2026-11-20T16:23)`. The core
    /// has no wall clock and leaves it empty; the host fills it in.
    pub now: String,
}

pub trait Brain {
    fn start(&mut self, turn: TurnId, text: &str, context: &BrainContext);
    fn tool_result(&mut self, turn: TurnId, call_id: &str, outcome: &ToolOutcome);
    fn cancel(&mut self, turn: TurnId);
    /// Gets ready for turns with this memory and these tools, off the turn
    /// path: a brain that needs a new process starts it in the background.
    /// Hosts call it when the tool registry changes. Optional.
    fn prewarm(&mut self, _memory: &str, _tools: &[ToolSpec]) {}
}

pub trait Tool {
    fn spec(&self) -> &ToolSpec;
    fn start(&mut self, turn: TurnId, call: &ToolCall);
    fn cancel(&mut self, turn: TurnId, call_id: &str);
}

pub trait Stt {
    fn transcribe(&mut self, segment: SegmentId);
}

pub trait Tts {
    fn synthesize(&mut self, speech: SpeechId, text: &str);
}

pub trait AudioOut {
    fn play(&mut self, speech: SpeechId, samples: &[f32]);
    fn stop(&mut self);
}
