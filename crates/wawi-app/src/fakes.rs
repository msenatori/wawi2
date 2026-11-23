use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use wawi_core::{
    Brain, BrainContext, Event, EventSink, PermissionLevel, SUGGEST_TOOL, SegmentId, Stt, Tool,
    ToolCall, ToolOutcome, ToolSpec, TurnId, TurnSource,
};
use wawi_tools::{Civil, WallClock};

use crate::voice::clock::SystemClock;

pub type EventQueue = Rc<RefCell<VecDeque<Event>>>;

/// Events into the simulator's queue.
pub struct Sink(pub EventQueue);

impl EventSink for Sink {
    fn send(&self, event: Event) {
        self.0.borrow_mut().push_back(event);
    }
}

/// Answers in steps so an answer can be interrupted halfway, like a model that
/// is still streaming.
///
/// On an initiative turn it first tries any tool its text names (so a refusal
/// can be seen), then proposes the text as a suggestion.
pub struct FakeBrain {
    sink: Sink,
    turn: Option<TurnId>,
    steps: VecDeque<Vec<Event>>,
    manual: bool,
    next_call: u64,
    /// The initiative text, while an initiative turn is running.
    initiative: Option<String>,
    last_call: Option<String>,
}

impl FakeBrain {
    pub fn new(queue: EventQueue) -> Self {
        Self {
            sink: Sink(queue),
            turn: None,
            steps: VecDeque::new(),
            manual: false,
            next_call: 0,
            initiative: None,
            last_call: None,
        }
    }

    pub fn is_manual(&self) -> bool {
        self.manual
    }

    pub fn step(&mut self) {
        for event in self.steps.pop_front().unwrap_or_default() {
            self.sink.send(event);
        }
    }

    fn call(&mut self, turn: TurnId, name: &str, args_json: String) -> Vec<Event> {
        self.next_call += 1;
        self.last_call = Some(name.to_owned());
        vec![Event::BrainToolCall {
            turn,
            call: ToolCall {
                id: format!("call-{}", self.next_call),
                name: name.into(),
                args_json,
            },
        }]
    }

    fn suggestion(&mut self, turn: TurnId, text: &str) -> Vec<Event> {
        let args = format!(
            r#"{{"title":"{0}","why":"se me ocurrió solo","ask":"{0}"}}"#,
            text.replace(['"', '\\'], "")
        );
        self.call(turn, SUGGEST_TOOL, args)
    }

    fn answer(turn: TurnId, text: &str) -> Vec<Event> {
        vec![
            Event::BrainText {
                turn,
                text: text.into(),
            },
            Event::BrainDone { turn },
        ]
    }
}

impl Brain for FakeBrain {
    fn start(&mut self, turn: TurnId, text: &str, context: &BrainContext) {
        self.turn = Some(turn);
        self.manual = text.contains("lento");
        self.steps.clear();
        self.initiative = None;
        if context.source == TurnSource::Initiative {
            // Like a model that read about a tool elsewhere: it may name one it
            // was not offered, and the core has to refuse it.
            self.initiative = Some(text.to_owned());
            let step = match ["borrar", "clima"]
                .into_iter()
                .find(|name| text.contains(name))
            {
                Some(name) => self.call(turn, name, "{}".into()),
                None => self.suggestion(turn, text),
            };
            self.steps.push_back(step);
            if !self.manual {
                self.step();
            }
            return;
        }
        let requested = context.tools.iter().find(|tool| text.contains(&tool.name));
        if let Some(tool) = requested {
            let name = tool.name.clone();
            let step = self.call(turn, &name, "{}".into());
            self.steps.push_back(step);
        } else if text.contains("rompe") {
            self.steps.push_back(vec![Event::BrainFailed {
                turn,
                reason: "falla simulada".into(),
            }]);
        } else {
            self.steps.push_back(vec![Event::BrainText {
                turn,
                text: "Primera frase simulada.".into(),
            }]);
            self.steps
                .push_back(Self::answer(turn, "Segunda frase simulada."));
        }
        if !self.manual {
            self.step();
        }
    }

    fn tool_result(&mut self, turn: TurnId, _call_id: &str, outcome: &ToolOutcome) {
        if self.turn != Some(turn) {
            return;
        }
        if let Some(text) = self.initiative.clone() {
            let step = if self.last_call.as_deref() == Some(SUGGEST_TOOL) {
                self.initiative = None;
                let reply = match outcome {
                    ToolOutcome::Success(_) => "Te dejé una sugerencia.",
                    ToolOutcome::Failure(_) => "Tenía una idea, pero no la pude dejar.",
                };
                Self::answer(turn, reply)
            } else {
                self.suggestion(turn, &text)
            };
            self.steps.push_back(step);
            self.step();
            return;
        }
        let text = match outcome {
            ToolOutcome::Success(result) => format!("Listo: {result}."),
            ToolOutcome::Failure(_) => "No pude usar la herramienta.".to_owned(),
        };
        self.steps.push_back(Self::answer(turn, &text));
        self.step();
    }

    fn cancel(&mut self, turn: TurnId) {
        self.steps.clear();
        self.initiative = None;
        // A model doesn't stop on the spot: this is the output that was already
        // on its way. The dialogue has to drop it.
        self.sink.send(Event::BrainText {
            turn,
            text: "Texto que llegó tarde.".into(),
        });
    }
}

pub struct FakeStt {
    sink: Sink,
    next_text: Option<String>,
}

impl FakeStt {
    pub fn new(queue: EventQueue) -> Self {
        Self {
            sink: Sink(queue),
            next_text: None,
        }
    }

    pub fn will_hear(&mut self, text: &str) {
        self.next_text = Some(text.to_owned());
    }
}

impl Stt for FakeStt {
    fn transcribe(&mut self, segment: SegmentId) {
        if let Some(text) = self.next_text.take() {
            self.sink.send(Event::FinalTranscript { segment, text });
        }
    }
}

pub struct FakeTool {
    spec: ToolSpec,
    sink: Sink,
}

impl FakeTool {
    pub fn boxed(
        name: &str,
        description: &str,
        permission: PermissionLevel,
        needs_confirmation: bool,
        ack: Option<&str>,
        queue: EventQueue,
    ) -> Box<dyn Tool> {
        Box::new(Self {
            spec: ToolSpec {
                name: name.into(),
                description: description.into(),
                params_schema_json: r#"{"type":"object"}"#.into(),
                permission,
                needs_confirmation,
                ack: ack.map(Into::into),
            },
            sink: Sink(queue),
        })
    }
}

impl Tool for FakeTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn start(&mut self, turn: TurnId, call: &ToolCall) {
        self.sink.send(Event::ToolFinished {
            turn,
            call_id: call.id.clone(),
            outcome: ToolOutcome::Success(format!("{} simulado", self.spec.name)),
        });
    }

    fn cancel(&mut self, _turn: TurnId, _call_id: &str) {}
}

/// The simulator's wall clock: the real time and zone when it started, moved
/// along by the simulated milliseconds (`/tick`), so a reminder can be
/// fast-forwarded to.
pub struct SimClock {
    real: SystemClock,
    start: i64,
    elapsed_ms: Cell<u64>,
}

impl SimClock {
    pub fn new() -> Rc<Self> {
        let real = SystemClock::new();
        Rc::new(Self {
            start: real.now(),
            real,
            elapsed_ms: Cell::new(0),
        })
    }

    /// The dialogue's clock, in milliseconds since the simulator started.
    pub fn follow(&self, elapsed_ms: u64) {
        self.elapsed_ms.set(elapsed_ms);
    }
}

impl WallClock for SimClock {
    fn now(&self) -> i64 {
        self.start + (self.elapsed_ms.get() / 1000) as i64
    }

    fn local(&self, epoch: i64) -> Civil {
        self.real.local(epoch)
    }

    fn epoch_of(&self, local: Civil) -> Option<i64> {
        self.real.epoch_of(local)
    }
}
