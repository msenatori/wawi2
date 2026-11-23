//! Thousands of arbitrary events, including impossible orders and stale ids.
//! Failures print the seed and step so they can be replayed.

mod common;

use std::collections::HashSet;

use common::{FakeTool, FixedClock, call_with_args, suggest_args};
use wawi_core::{
    Config, DiagnosticReason, Dialogue, Effect, Event, PermissionLevel, Priority, SUGGEST_TOOL,
    SegmentId, SpeechId, SpeechKind, State, StateKind, SuggestionId, TimeRoute, ToolCall,
    ToolOutcome, TurnId, TurnSource, json,
};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // SplitMix64.
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len() as u64) as usize]
    }
}

const TRANSCRIPTS: &[&str] = &[
    "hola",
    "contame algo",
    "qué hora es",
    "sí",
    "yes",
    "dale",
    "no",
    "no sé",
    "pará",
    "stop",
    "",
    "   ",
    "sí, pero no",
];
const BRAIN_TEXTS: &[&str] = &["Una frase.", "sí", "yes", "", " ", "stop"];
const TOOLS: &[&str] = &[
    "plain",
    "acked",
    "confirmed",
    "writer",
    "missing",
    SUGGEST_TOOL,
];
const TITLES: &[&str] = &[
    "Regar", "regar!", "Mate", "Backup", "Fotos", "Música", "musica",
];

fn any_call(rng: &mut Rng) -> ToolCall {
    let name = rng.pick(TOOLS);
    let args = match rng.below(10) {
        0 => "{bad".to_owned(),
        1 => "[]".to_owned(),
        _ if name == SUGGEST_TOOL => suggest_args(rng.pick(TITLES), "hacelo"),
        _ => "{}".to_owned(),
    };
    call_with_args(&format!("c{}", rng.below(4)), name, &args)
}

/// What the test has seen, kept apart from the dialogue so it can check it.
#[derive(Default)]
struct Observer {
    brain: Option<TurnId>,
    source: Option<TurnSource>,
    cancelled: HashSet<TurnId>,
    highest_turn: u64,
    highest_speech: u64,
    highest_segment: u64,
    playing: Option<SpeechId>,
    pending_segments: Vec<SegmentId>,
    speaks: u64,
    tool_runs: u64,
    confirmed_runs: u64,
    cancels: u64,
    initiatives: u64,
    refusals: u64,
    suggestions: u64,
    accepts: u64,
}

impl Observer {
    /// Half the steps follow a plausible flow, or chaos would cancel every turn
    /// long before it reached the deeper states; the other half is chaos.
    fn generate(
        &self,
        rng: &mut Rng,
        now: &mut u64,
        state: &State,
        open: &[SuggestionId],
    ) -> Event {
        if rng.below(2) == 0 {
            self.guided(rng, state, open)
        } else {
            self.chaos(rng, now, open)
        }
    }

    fn guided(&self, rng: &mut Rng, state: &State, open: &[SuggestionId]) -> Event {
        let answer = |rng: &mut Rng, texts: &[&str]| match self.pending_segments.first() {
            Some(segment) => Event::FinalTranscript {
                segment: *segment,
                text: rng.pick(texts).into(),
            },
            None => Event::SpeechStarted,
        };
        match state {
            State::Idle => match rng.below(6) {
                0 => Event::Initiative {
                    text: "mirá a ver".into(),
                },
                1 if !open.is_empty() => Event::AcceptSuggestion {
                    id: open[rng.below(open.len() as u64) as usize],
                },
                _ => Event::SpeechStarted,
            },
            State::Listening { capture, .. } if capture.is_talking() => Event::SpeechEnded,
            State::Listening { .. } => answer(rng, TRANSCRIPTS),
            State::AwaitingConfirmation { capture, .. } if capture.is_talking() => {
                Event::SpeechEnded
            }
            State::AwaitingConfirmation { .. } => answer(rng, TRANSCRIPTS),
            State::Thinking { turn, .. } => match rng.below(4) {
                0 => Event::BrainText {
                    turn: *turn,
                    text: "Una frase.".into(),
                },
                1 => Event::BrainDone { turn: *turn },
                _ => Event::BrainToolCall {
                    turn: *turn,
                    call: any_call(rng),
                },
            },
            State::Speaking {
                turn,
                brain: Some(_),
                ..
            } if rng.below(2) == 0 => Event::BrainDone { turn: *turn },
            State::Speaking { last_speech, .. } => Event::PlaybackFinished {
                up_to: *last_speech,
            },
        }
    }

    fn chaos(&self, rng: &mut Rng, now: &mut u64, open: &[SuggestionId]) -> Event {
        let turn = self.any_turn(rng);
        // Mostly the segment a real STT would answer for next, sometimes any.
        let segment = match (self.pending_segments.first(), rng.below(10)) {
            (Some(pending), 0..=7) => *pending,
            _ => SegmentId(self.highest_segment.saturating_sub(rng.below(3))),
        };
        let up_to = match (self.playing, rng.below(10)) {
            (Some(playing), 0..=7) => playing,
            _ => SpeechId(self.highest_speech.saturating_sub(rng.below(3)) + rng.below(2)),
        };
        // Mostly an open suggestion, sometimes one that is gone or never was.
        let suggestion = match (open.is_empty(), rng.below(10)) {
            (false, 0..=7) => open[rng.below(open.len() as u64) as usize],
            _ => SuggestionId(rng.below(20)),
        };
        match rng.below(110) {
            0..=14 => Event::SpeechStarted,
            15..=27 => Event::SpeechEnded,
            28..=40 => Event::FinalTranscript {
                segment,
                text: rng.pick(TRANSCRIPTS).into(),
            },
            41..=42 => Event::UnclearTranscript {
                segment,
                text: "I could just".into(),
            },
            43..=52 => Event::PlaybackFinished { up_to },
            53..=56 => Event::Cancel,
            57..=66 => Event::BrainText {
                turn,
                text: rng.pick(BRAIN_TEXTS).into(),
            },
            67..=74 => Event::BrainToolCall {
                turn,
                call: any_call(rng),
            },
            75..=78 => Event::BrainDone { turn },
            79..=80 => Event::BrainFailed {
                turn,
                reason: "fake".into(),
            },
            81..=85 => Event::ToolFinished {
                turn,
                call_id: format!("c{}", rng.below(4)),
                outcome: ToolOutcome::Success("sí".into()),
            },
            86..=89 => Event::RequestFloor {
                priority: if rng.below(2) == 0 {
                    Priority::High
                } else {
                    Priority::Low
                },
                text: "aviso".into(),
                source: "fuzz".into(),
            },
            90..=94 => Event::Initiative {
                text: "fijate algo".into(),
            },
            95..=97 => Event::AcceptSuggestion { id: suggestion },
            98..=99 => Event::DismissSuggestion { id: suggestion },
            _ => {
                // Mostly small steps, sometimes past every deadline, rarely back.
                match rng.below(20) {
                    0 => *now += 200_000,
                    1 => {
                        return Event::Tick {
                            now_ms: now.saturating_sub(1_000),
                        };
                    }
                    _ => *now += rng.below(5_000),
                }
                Event::Tick { now_ms: *now }
            }
        }
    }

    /// Biased to the running brain, but also old and never-issued turns.
    fn any_turn(&self, rng: &mut Rng) -> TurnId {
        match (self.brain, rng.below(10)) {
            (Some(turn), 0..=6) => turn,
            _ => TurnId((self.highest_turn + 1).saturating_sub(rng.below(4))),
        }
    }

    fn check(&mut self, before: &State, event: &Event, fx: &[Effect], after: &State) {
        let before_source = before.work().map(|work| work.source);
        let brain_event_turn = match event {
            Event::BrainText { turn, .. }
            | Event::BrainToolCall { turn, .. }
            | Event::BrainDone { turn }
            | Event::BrainFailed { turn, .. }
            | Event::ToolFinished { turn, .. } => Some(*turn),
            _ => None,
        };
        if let Some(turn) = brain_event_turn
            && before.brain_turn() != Some(turn)
        {
            assert!(
                fx.iter().all(|e| matches!(e, Effect::Diagnostic { .. })),
                "stale brain event had real effects: {fx:?}"
            );
            assert_eq!(before, after, "stale brain event changed the state");
        }

        for effect in fx {
            match effect {
                Effect::StartBrain { turn, context, .. } => {
                    assert!(
                        self.brain.is_none(),
                        "second active brain: {:?}",
                        self.brain
                    );
                    assert!(turn.0 > self.highest_turn, "turn id did not grow");
                    self.highest_turn = turn.0;
                    self.brain = Some(*turn);
                    self.source = Some(context.source);
                    if context.source == TurnSource::Initiative {
                        assert!(
                            matches!(before, State::Idle)
                                && matches!(event, Event::Initiative { .. }),
                            "initiative started over something: {event:?} from {before:?}"
                        );
                        self.initiatives += 1;
                    }
                    if matches!(event, Event::AcceptSuggestion { .. }) {
                        assert_eq!(context.source, TurnSource::Owner);
                        self.accepts += 1;
                    }
                }
                Effect::CancelBrain { turn } => {
                    assert_eq!(self.brain, Some(*turn), "cancelled a brain not running");
                    self.brain = None;
                    self.cancelled.insert(*turn);
                    self.cancels += 1;
                }
                Effect::Speak {
                    speech, turn, kind, ..
                } => {
                    assert!(
                        !(matches!(kind, SpeechKind::Failure(_))
                            && before_source == Some(TurnSource::Initiative)),
                        "an initiative failure was spoken"
                    );
                    assert!(
                        !self.cancelled.contains(turn),
                        "spoke for cancelled turn {turn:?}"
                    );
                    assert!(speech.0 > self.highest_speech, "speech id did not grow");
                    assert!(turn.0 >= self.highest_turn, "spoke for an old turn");
                    self.highest_speech = speech.0;
                    self.highest_turn = turn.0;
                    self.playing = Some(*speech);
                    self.speaks += 1;
                }
                Effect::StopPlayback => self.playing = None,
                Effect::SuggestionAdded { .. } => {
                    assert_eq!(before_source, Some(TurnSource::Initiative));
                    self.suggestions += 1;
                }
                Effect::Diagnostic {
                    reason: DiagnosticReason::ToolNotAllowed,
                    ..
                } => self.refusals += 1,
                Effect::Transcribe { segment } => {
                    self.highest_segment = self.highest_segment.max(segment.0);
                    self.pending_segments.push(*segment);
                }
                Effect::RunTool { call, turn } => {
                    assert_eq!(after.brain_turn(), Some(*turn), "tool ran with no brain");
                    assert!(!self.cancelled.contains(turn), "tool for cancelled turn");
                    assert!(
                        matches!(json::parse(&call.args_json), Ok(json::Json::Object(_))),
                        "tool ran with bad arguments: {call:?}"
                    );
                    assert_ne!(call.name, SUGGEST_TOOL, "suggest is never run outside");
                    if self.source == Some(TurnSource::Initiative) {
                        assert!(
                            call.name == "plain" || call.name == "acked",
                            "initiative ran {}",
                            call.name
                        );
                    }
                    self.tool_runs += 1;
                    if call.name == "confirmed" {
                        assert!(
                            matches!(before, State::AwaitingConfirmation { .. })
                                && matches!(event, Event::FinalTranscript { .. }),
                            "confirmed tool ran without the owner: {event:?} from {before:?}"
                        );
                        self.confirmed_runs += 1;
                    }
                }
                _ => {}
            }
        }

        if let Event::FinalTranscript { segment, .. } = event {
            self.pending_segments.retain(|pending| pending != segment);
        }
        // An STT answers for what it was asked lately; older ones were lost.
        if self.pending_segments.len() > 4 {
            self.pending_segments.remove(0);
        }

        if let Event::PlaybackFinished { up_to } = event
            && self.playing == Some(*up_to)
        {
            self.playing = None;
        }
        // A brain that finished or failed leaves on its own, without a cancel.
        if after.brain_turn().is_none() {
            self.brain = None;
        }

        assert_eq!(after.brain_turn(), self.brain, "brain tracking diverged");
        if let Some(turn) = after.turn() {
            assert_eq!(turn.0, self.highest_turn, "the state holds an old turn");
        }
        if let State::Thinking { .. } | State::Idle | State::Listening { .. } = after {
            assert_eq!(self.playing, None, "{after} while audio is playing");
        } else if let State::Speaking { last_speech, .. } = after {
            assert_eq!(self.playing, Some(*last_speech), "wrong tail of the audio");
        }
    }
}

fn run(seed: u64, steps: usize, seen: &mut HashSet<StateKind>, total: &mut Observer) {
    let mut rng = Rng(seed);
    let mut dialogue = Dialogue::new(Config::default());
    dialogue.registry_mut().register(FakeTool::plain("plain"));
    dialogue
        .registry_mut()
        .register(FakeTool::with_ack("acked", "a ver"));
    dialogue
        .registry_mut()
        .register(FakeTool::confirmed("confirmed"));
    dialogue.registry_mut().register(FakeTool::with_permission(
        "writer",
        PermissionLevel::Write,
        false,
    ));
    dialogue
        .router_mut()
        .add(Box::new(TimeRoute::new(FixedClock(9, 5))));

    let mut observer = Observer::default();
    let mut now = 0;
    for step in 0..steps {
        // Plug-ins come and go while dialogue runs.
        if rng.below(200) == 0 {
            let enabled = rng.below(2) == 0;
            dialogue.registry_mut().set_enabled("confirmed", enabled);
            dialogue.registry_mut().set_enabled("plain", enabled);
        }
        let before = dialogue.state().clone();
        let open: Vec<SuggestionId> = dialogue.suggestions().open().iter().map(|s| s.id).collect();
        let event = observer.generate(&mut rng, &mut now, &before, &open);
        let fx = dialogue.handle(event.clone());
        let after = dialogue.state().clone();
        seen.insert(after.kind());
        let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            observer.check(&before, &event, &fx, &after);
            let store = dialogue.suggestions();
            let max_open = Config::default().max_open_suggestions;
            assert!(store.open().len() <= max_open, "too many open suggestions");
            for suggestion in store.open() {
                assert!(
                    !store.is_dismissed(&suggestion.title),
                    "a dismissed title is open again: {}",
                    suggestion.title
                );
            }
        }));
        if let Err(cause) = checked {
            eprintln!("seed {seed} step {step}: {event:?}\n  before {before:?}\n  fx {fx:?}");
            std::panic::resume_unwind(cause);
        }
    }

    // Whatever mess it ended in, time alone must bring it back to rest.
    let mut settled = false;
    for _ in 0..(dialogue.floor_queue_len() * 2 + 10) {
        now += 1_000_000;
        dialogue.handle(Event::Tick { now_ms: now });
        if matches!(dialogue.state(), State::Idle) && dialogue.floor_queue_len() == 0 {
            settled = true;
            break;
        }
    }
    assert!(settled, "seed {seed} never settled: {:?}", dialogue.state());

    total.speaks += observer.speaks;
    total.tool_runs += observer.tool_runs;
    total.confirmed_runs += observer.confirmed_runs;
    total.cancels += observer.cancels;
    total.initiatives += observer.initiatives;
    total.refusals += observer.refusals;
    total.suggestions += observer.suggestions;
    total.accepts += observer.accepts;
}

#[test]
fn arbitrary_event_sequences_keep_the_invariants() {
    let mut seen = HashSet::new();
    let mut total = Observer::default();
    for seed in 0..50 {
        run(seed, 5_000, &mut seen, &mut total);
    }

    // Guards the test itself: a generator that never reaches these proves nothing.
    assert_eq!(seen.len(), 5, "states reached: {seen:?}");
    assert!(total.speaks > 1_000, "only {} speaks", total.speaks);
    assert!(total.cancels > 1_000, "only {} cancels", total.cancels);
    assert!(total.tool_runs > 100, "only {} tool runs", total.tool_runs);
    assert!(
        total.confirmed_runs > 10,
        "only {} confirmed tool runs",
        total.confirmed_runs
    );
    assert!(
        total.initiatives > 1_000,
        "only {} initiatives",
        total.initiatives
    );
    assert!(total.refusals > 50, "only {} refusals", total.refusals);
    assert!(
        total.suggestions > 50,
        "only {} suggestions",
        total.suggestions
    );
    assert!(total.accepts > 50, "only {} accepts", total.accepts);
}
