#![allow(dead_code)]

use wawi_core::{
    Clock, Config, Dialogue, Effect, Event, PermissionLevel, Priority, SegmentId, SpeechId,
    StateKind, Tool, ToolCall, ToolOutcome, ToolSpec, TurnId,
};

pub struct FakeTool {
    spec: ToolSpec,
}

impl FakeTool {
    pub fn plain(name: &str) -> Box<dyn Tool> {
        Box::new(Self {
            spec: spec(name, false, None),
        })
    }

    pub fn with_permission(
        name: &str,
        permission: PermissionLevel,
        needs_confirmation: bool,
    ) -> Box<dyn Tool> {
        let mut spec = spec(name, needs_confirmation, None);
        spec.permission = permission;
        Box::new(Self { spec })
    }

    pub fn with_ack(name: &str, ack: &str) -> Box<dyn Tool> {
        Box::new(Self {
            spec: spec(name, false, Some(ack)),
        })
    }

    pub fn confirmed(name: &str) -> Box<dyn Tool> {
        Box::new(Self {
            spec: spec(name, true, None),
        })
    }
}

fn spec(name: &str, needs_confirmation: bool, ack: Option<&str>) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: format!("fake {name}"),
        params_schema_json: "{}".into(),
        permission: PermissionLevel::Read,
        needs_confirmation,
        ack: ack.map(Into::into),
    }
}

impl Tool for FakeTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    fn start(&mut self, _turn: TurnId, _call: &ToolCall) {}
    fn cancel(&mut self, _turn: TurnId, _call_id: &str) {}
}

pub struct FixedClock(pub u8, pub u8);

impl Clock for FixedClock {
    fn now_hm(&self) -> (u8, u8) {
        (self.0, self.1)
    }
}

pub fn call(id: &str, name: &str) -> ToolCall {
    call_with_args(id, name, "{}")
}

pub fn call_with_args(id: &str, name: &str, args_json: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        args_json: args_json.into(),
    }
}

pub fn suggest_args(title: &str, ask: &str) -> String {
    format!(r#"{{"title":"{title}","why":"porque sí","ask":"{ask}"}}"#)
}

/// Drives a `Dialogue` one event at a time, keeping the effects of the last
/// step and the ids the owner of a real STT or audio output would see.
pub struct Scenario {
    pub dialogue: Dialogue,
    pub fx: Vec<Effect>,
    pub open_segments: Vec<SegmentId>,
    pub last_speech: SpeechId,
    pub last_turn: TurnId,
}

impl Scenario {
    pub fn new() -> Self {
        Self::with_config(Config::default())
    }

    pub fn with_config(config: Config) -> Self {
        Self {
            dialogue: Dialogue::new(config),
            fx: Vec::new(),
            open_segments: Vec::new(),
            last_speech: SpeechId(0),
            last_turn: TurnId(0),
        }
    }

    pub fn send(&mut self, event: Event) -> &mut Self {
        self.fx = self.dialogue.handle(event);
        for effect in &self.fx {
            match effect {
                Effect::Transcribe { segment } => self.open_segments.push(*segment),
                Effect::Speak { speech, turn, .. } => {
                    self.last_speech = *speech;
                    self.last_turn = *turn;
                }
                Effect::StartBrain { turn, .. } => self.last_turn = *turn,
                _ => {}
            }
        }
        self
    }

    pub fn speech_start(&mut self) -> &mut Self {
        self.send(Event::SpeechStarted)
    }

    pub fn speech_end(&mut self) -> &mut Self {
        self.send(Event::SpeechEnded)
    }

    /// Final transcript for the oldest segment still waiting for one.
    pub fn transcript(&mut self, text: &str) -> &mut Self {
        let segment = self.open_segments.remove(0);
        self.transcript_for(segment, text)
    }

    pub fn transcript_for(&mut self, segment: SegmentId, text: &str) -> &mut Self {
        self.send(Event::FinalTranscript {
            segment,
            text: text.into(),
        })
    }

    /// The STT could not make out the oldest segment still waiting.
    pub fn unclear(&mut self) -> &mut Self {
        let segment = self.open_segments.remove(0);
        self.send(Event::UnclearTranscript {
            segment,
            text: "Alors oui".into(),
        })
    }

    /// A whole utterance: speech starts, ends and gets its transcript.
    pub fn say(&mut self, text: &str) -> &mut Self {
        self.speech_start().speech_end().transcript(text)
    }

    pub fn tick(&mut self, now_ms: u64) -> &mut Self {
        self.send(Event::Tick { now_ms })
    }

    pub fn cancel(&mut self) -> &mut Self {
        self.send(Event::Cancel)
    }

    pub fn playback_finished(&mut self) -> &mut Self {
        let up_to = self.last_speech;
        self.send(Event::PlaybackFinished { up_to })
    }

    pub fn brain_text(&mut self, text: &str) -> &mut Self {
        let turn = self.last_turn;
        self.send(Event::BrainText {
            turn,
            text: text.into(),
        })
    }

    pub fn brain_done(&mut self) -> &mut Self {
        let turn = self.last_turn;
        self.send(Event::BrainDone { turn })
    }

    pub fn brain_tool(&mut self, id: &str, name: &str) -> &mut Self {
        self.brain_tool_args(id, name, "{}")
    }

    pub fn brain_tool_args(&mut self, id: &str, name: &str, args_json: &str) -> &mut Self {
        let turn = self.last_turn;
        self.send(Event::BrainToolCall {
            turn,
            call: call_with_args(id, name, args_json),
        })
    }

    pub fn initiative(&mut self, text: &str) -> &mut Self {
        self.send(Event::Initiative { text: text.into() })
    }

    /// Accepts or dismisses the open suggestion at `index`, oldest first.
    pub fn accept(&mut self, index: usize) -> &mut Self {
        let id = self.dialogue.suggestions().open()[index].id;
        self.send(Event::AcceptSuggestion { id })
    }

    pub fn dismiss(&mut self, index: usize) -> &mut Self {
        let id = self.dialogue.suggestions().open()[index].id;
        self.send(Event::DismissSuggestion { id })
    }

    /// The outcome of the last `ToolResultToBrain` in the last step.
    pub fn tool_result(&self) -> Option<&ToolOutcome> {
        self.fx.iter().rev().find_map(|effect| match effect {
            Effect::ToolResultToBrain { outcome, .. } => Some(outcome),
            _ => None,
        })
    }

    pub fn tool_finished(&mut self, id: &str) -> &mut Self {
        let turn = self.last_turn;
        self.send(Event::ToolFinished {
            turn,
            call_id: id.into(),
            outcome: ToolOutcome::Success("ok".into()),
        })
    }

    pub fn floor(&mut self, priority: Priority, text: &str) -> &mut Self {
        self.send(Event::RequestFloor {
            priority,
            text: text.into(),
            source: "test".into(),
        })
    }

    pub fn kind(&self) -> StateKind {
        self.dialogue.state().kind()
    }

    pub fn assert_state(&mut self, expected: StateKind) -> &mut Self {
        assert_eq!(self.kind(), expected, "effects: {:#?}", self.fx);
        self
    }

    /// Text of every `Speak` in the last step.
    pub fn spoken(&self) -> Vec<&str> {
        self.fx
            .iter()
            .filter_map(|effect| match effect {
                Effect::Speak { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Text given to the brain in the last step.
    pub fn brain_started_with(&self) -> Option<&str> {
        self.fx.iter().find_map(|effect| match effect {
            Effect::StartBrain { text, .. } => Some(text.as_str()),
            _ => None,
        })
    }
}

/// Asserts the last step produced an effect matching the pattern.
#[macro_export]
macro_rules! assert_fx {
    ($scenario:expr, $($pattern:tt)+) => {
        assert!(
            $scenario.fx.iter().any(|effect| matches!(effect, $($pattern)+)),
            "expected {} in {:#?}",
            stringify!($($pattern)+),
            $scenario.fx
        )
    };
}

/// Asserts the last step produced no effect matching the pattern.
#[macro_export]
macro_rules! assert_no_fx {
    ($scenario:expr, $($pattern:tt)+) => {
        assert!(
            !$scenario.fx.iter().any(|effect| matches!(effect, $($pattern)+)),
            "unexpected {} in {:#?}",
            stringify!($($pattern)+),
            $scenario.fx
        )
    };
}
