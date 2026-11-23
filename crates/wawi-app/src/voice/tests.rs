//! The host loop end to end with fakes: the real core and the real playback
//! queue, with a recognizer, brain, TTS and device under the test's control.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{self, Write};
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use wawi_audio_mac::Player;
use wawi_audio_mac::playback::{Done, Playback};
use wawi_core::{
    Brain, BrainContext, Event, SegmentId, SpeechId, State, Stt, ToolCall, ToolOutcome, Tts, TurnId,
};
use wawi_memory::Home;
use wawi_tools::{Civil, FakeClock, Toolbox};
use wawi_tts_kokoro::{SAMPLE_RATE, SpeechAudio};

use super::host::{Ended, Host, Input, InputSink, Listener, Logs, Parts, Until, Voice, run};

type Calls = Rc<RefCell<Vec<String>>>;

/// Records the calls, and the date and time each turn carried.
struct FakeBrain(Calls, Rc<RefCell<Vec<String>>>);

impl Brain for FakeBrain {
    fn start(&mut self, turn: TurnId, text: &str, context: &BrainContext) {
        self.1.borrow_mut().push(context.now.clone());
        self.0
            .borrow_mut()
            .push(format!("brain start {} {text}", turn.0));
    }
    fn tool_result(&mut self, turn: TurnId, call_id: &str, _outcome: &ToolOutcome) {
        self.0
            .borrow_mut()
            .push(format!("brain tool_result {} {call_id}", turn.0));
    }
    fn cancel(&mut self, turn: TurnId) {
        self.0.borrow_mut().push(format!("brain cancel {}", turn.0));
    }
}

impl Drop for FakeBrain {
    fn drop(&mut self) {
        self.0.borrow_mut().push("brain dropped".into());
    }
}

struct FakeStt(Calls);

impl Stt for FakeStt {
    fn transcribe(&mut self, segment: SegmentId) {
        self.0
            .borrow_mut()
            .push(format!("stt transcribe {}", segment.0));
    }
}

impl Listener for FakeStt {
    fn hand_audio(&mut self, samples: Vec<f32>, sample_rate: u32) {
        self.0
            .borrow_mut()
            .push(format!("stt audio {} @ {sample_rate}", samples.len()));
    }
    fn discard_pending(&self) {
        self.0.borrow_mut().push("stt discard".into());
    }
}

impl Drop for FakeStt {
    fn drop(&mut self) {
        self.0.borrow_mut().push("stt dropped".into());
    }
}

struct FakeTts(Calls);

impl Tts for FakeTts {
    fn synthesize(&mut self, speech: SpeechId, text: &str) {
        self.0
            .borrow_mut()
            .push(format!("tts say {} {text}", speech.0));
    }
}

impl Voice for FakeTts {
    fn discard_pending(&self) {
        self.0.borrow_mut().push("tts discard".into());
    }
}

impl Drop for FakeTts {
    fn drop(&mut self) {
        self.0.borrow_mut().push("tts dropped".into());
    }
}

/// The device: buffers wait until the test plays them out.
#[derive(Default)]
struct Device {
    pending: Mutex<VecDeque<(usize, Done)>>,
    stops: Mutex<usize>,
}

impl Device {
    fn play_one(&self) -> usize {
        let (len, done) = self.pending.lock().unwrap().pop_front().expect("queued");
        done();
        len
    }

    fn queued(&self) -> Vec<usize> {
        self.pending
            .lock()
            .unwrap()
            .iter()
            .map(|(n, _)| *n)
            .collect()
    }

    fn stops(&self) -> usize {
        *self.stops.lock().unwrap()
    }
}

impl Player for Device {
    fn sample_rate(&self) -> f64 {
        f64::from(SAMPLE_RATE)
    }

    fn schedule(&self, samples: &[f32], done: Done) -> Result<(), String> {
        self.pending
            .lock()
            .unwrap()
            .push_back((samples.len(), done));
        Ok(())
    }

    fn stop(&self) {
        *self.stops.lock().unwrap() += 1;
        let drained: Vec<_> = self.pending.lock().unwrap().drain(..).collect();
        for (_, done) in drained {
            done();
        }
    }
}

#[derive(Clone, Default)]
struct Console(Rc<RefCell<Vec<u8>>>);

impl Write for Console {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Console {
    fn text(&self) -> String {
        String::from_utf8(self.0.borrow().clone()).unwrap()
    }
}

struct Rig {
    host: Host,
    calls: Calls,
    device: Arc<Device>,
    /// What playback reports (`PlaybackFinished`), for the host.
    rx: Receiver<Input>,
    /// The host's channel, for the tools to answer on.
    tx: Sender<Input>,
    console: Console,
    /// `BrainContext::now` of every turn the brain started.
    nows: Rc<RefCell<Vec<String>>>,
}

impl Rig {
    fn new(logs: Logs) -> Self {
        let calls: Calls = Rc::default();
        let (tx, rx) = mpsc::channel();
        let device = Arc::new(Device::default());
        let out = Playback::new(device.clone(), Arc::new(InputSink(tx.clone())));
        let nows: Rc<RefCell<Vec<String>>> = Rc::default();
        let console = Console::default();
        let host = Host::new(
            Parts {
                brain: Box::new(FakeBrain(calls.clone(), nows.clone())),
                stt: Box::new(FakeStt(calls.clone())),
                tts: Box::new(FakeTts(calls.clone())),
                out: Box::new(out),
            },
            None,
            Box::new(console.clone()),
            logs,
        );
        Self {
            host,
            calls,
            device,
            rx,
            tx,
            console,
            nows,
        }
    }

    /// Her real tools, on a fake clock: Friday 2026-11-20 15:00, UTC-3.
    fn with_tools(logs: Logs, home: Option<Home>) -> (Self, Rc<FakeClock>) {
        let clock = FakeClock::at(Civil::date(2026, 11, 20).at(15, 0, 0), -3);
        let rig = Self::new(logs);
        (rig.tools_on(home, clock.clone()), clock)
    }

    fn tools_on(mut self, home: Option<Home>, clock: Rc<FakeClock>) -> Self {
        let toolbox = Toolbox::open(home, clock);
        let tools = toolbox.tools(Rc::new(InputSink(self.tx.clone())));
        self.host.install_tools(toolbox, tools);
        self
    }

    fn tick(&mut self) {
        self.host.tick();
        self.pump();
    }

    fn said(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|call| call.starts_with("tts say"))
            .collect()
    }

    fn core(&mut self, event: Event) {
        self.host.handle(Input::Core(event));
        self.pump();
    }

    fn input(&mut self, input: Input) {
        self.host.handle(input);
        self.pump();
    }

    /// Delivers what playback reported.
    fn pump(&mut self) {
        while let Ok(input) = self.rx.try_recv() {
            self.host.handle(input);
        }
    }

    fn play_one(&mut self) -> usize {
        let len = self.device.play_one();
        self.pump();
        len
    }

    fn chunk(&mut self, speech: u64, samples: usize, last: bool) {
        self.input(Input::Audio(SpeechAudio {
            speech: SpeechId(speech),
            samples: vec![0.1; samples],
            sample_rate: SAMPLE_RATE,
            last,
        }));
    }

    /// The owner holds the key, says something and lets go; Parakeet hears
    /// `text`.
    fn owner_says(&mut self, segment: u64, text: &str) {
        self.core(Event::SpeechStarted);
        self.input(Input::Segment {
            samples: vec![0.2; 16_000],
            sample_rate: 16_000,
        });
        self.input(Input::KeyReleased(std::time::Instant::now()));
        self.core(Event::SpeechEnded);
        self.core(Event::FinalTranscript {
            segment: SegmentId(segment),
            text: text.into(),
        });
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }

    fn state(&self) -> String {
        self.host.dialogue().state().to_string()
    }
}

const SILENCE: usize = (0.3 * SAMPLE_RATE as f32) as usize;

#[test]
fn a_turn_flows_from_the_key_to_the_last_sound() {
    let dir = std::env::temp_dir().join(format!("wawi2-host-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut rig = Rig::new(Logs::open(&dir).unwrap());
    rig.owner_says(1, "contame algo");
    assert_eq!(
        rig.calls(),
        [
            "stt audio 16000 @ 16000",
            "stt transcribe 1",
            "brain start 1 contame algo"
        ],
        "the audio reaches the recognizer before its segment closes"
    );

    rig.core(Event::BrainText {
        turn: TurnId(1),
        text: "Había una vez un perro.".into(),
    });
    rig.core(Event::BrainDone { turn: TurnId(1) });
    assert!(
        rig.calls()
            .contains(&"tts say 1 Había una vez un perro.".to_owned())
    );

    rig.chunk(1, 2400, true);
    assert_eq!(
        rig.device.queued(),
        [2400 + SILENCE],
        "0.3 s of silence closes the speech"
    );
    assert_eq!(rig.play_one(), 2400 + SILENCE);
    assert_eq!(rig.state(), "idle");

    let console = rig.console.text();
    assert!(console.contains("vos:  «contame algo»"), "{console}");
    assert!(
        console.contains("wawi: «Había una vez un perro.»"),
        "{console}"
    );
    assert!(console.contains("total sentido"), "{console}");

    drop(rig);
    let turns = std::fs::read_to_string(dir.join("turns.log")).unwrap();
    assert!(turns.contains("felt_ms="), "{turns}");
    assert!(turns.contains("heard=\"contame algo\""), "{turns}");
    let session = std::fs::read_to_string(dir.join("session.log")).unwrap();
    assert!(session.contains("effect start_brain turn 1"), "{session}");
    assert!(
        !session.contains("contame algo"),
        "the session log has no texts: {session}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_speech_finishes_only_after_its_last_chunk_played() {
    let mut rig = Rig::new(Logs::default());
    rig.owner_says(1, "contame algo");
    rig.core(Event::BrainText {
        turn: TurnId(1),
        text: "Primera frase. Segunda frase.".into(),
    });
    rig.core(Event::BrainDone { turn: TurnId(1) });

    rig.chunk(1, 1000, false);
    assert_eq!(rig.play_one(), 1000, "no silence in the middle");
    assert_eq!(
        rig.state(),
        "speaking(turn 1)",
        "the queue drained, but the TTS is still making sentence two"
    );

    rig.chunk(1, 800, false);
    rig.chunk(1, 0, true);
    assert_eq!(
        rig.device.queued(),
        [800, SILENCE],
        "an empty last chunk still brings the silence"
    );
    rig.play_one();
    assert_eq!(rig.state(), "speaking(turn 1)");
    rig.play_one();
    assert_eq!(rig.state(), "idle");
}

#[test]
fn pressing_the_key_while_she_speaks_cuts_audio_and_the_brain() {
    let mut rig = Rig::new(Logs::default());
    rig.owner_says(1, "contame un cuento");
    rig.core(Event::BrainText {
        turn: TurnId(1),
        text: "Había una vez.".into(),
    });
    rig.chunk(1, 2400, false);
    assert_eq!(rig.device.queued(), [2400]);

    rig.core(Event::SpeechStarted);
    assert_eq!(rig.device.stops(), 1, "the audio stops at once");
    assert!(rig.device.queued().is_empty());
    let calls = rig.calls();
    assert!(calls.contains(&"tts discard".to_owned()), "{calls:?}");
    assert!(calls.contains(&"brain cancel 1".to_owned()), "{calls:?}");
    assert!(rig.console.text().contains("(cortada)"));
    assert!(rig.console.text().contains("(la cortaste)"));

    // What the TTS still had on its way for the old speech is dropped.
    rig.chunk(1, 2400, false);
    rig.chunk(1, 0, true);
    assert!(rig.device.queued().is_empty());

    rig.input(Input::Segment {
        samples: vec![0.2; 8000],
        sample_rate: 16_000,
    });
    rig.core(Event::SpeechEnded);
    rig.core(Event::FinalTranscript {
        segment: SegmentId(2),
        text: "mejor un chiste".into(),
    });
    assert!(
        rig.calls()
            .contains(&"brain start 2 mejor un chiste".to_owned()),
        "{:?}",
        rig.calls()
    );
    rig.core(Event::BrainText {
        turn: TurnId(2),
        text: "Ahí va.".into(),
    });
    rig.chunk(2, 1200, true);
    assert_eq!(rig.device.queued(), [1200 + SILENCE], "new speech plays");
}

#[test]
fn the_time_question_is_answered_without_the_brain() {
    let mut rig = Rig::new(Logs::default());
    rig.owner_says(1, "¿Qué hora es?");
    let calls = rig.calls();
    assert!(
        !calls.iter().any(|c| c.starts_with("brain start")),
        "{calls:?}"
    );
    let said = calls
        .iter()
        .find(|c| c.starts_with("tts say 1 Son las "))
        .unwrap_or_else(|| panic!("{calls:?}"));
    assert!(said.ends_with('.'), "{said}");
    rig.chunk(1, 2400, true);
    rig.play_one();
    assert_eq!(rig.state(), "idle");
    assert!(rig.console.text().contains("total sentido"));
}

#[test]
fn a_tap_is_heard_as_nothing() {
    let mut rig = Rig::new(Logs::default());
    rig.core(Event::SpeechStarted);
    rig.input(Input::Segment {
        samples: Vec::new(),
        sample_rate: 16_000,
    });
    rig.core(Event::SpeechEnded);
    rig.core(Event::FinalTranscript {
        segment: SegmentId(1),
        text: String::new(),
    });
    assert!(matches!(rig.host.dialogue().state(), State::Idle));
    assert!(rig.console.text().contains("(no escuché nada)"));
    assert!(!rig.console.text().contains("tiempos"));
}

#[test]
fn shutdown_cuts_the_audio_and_drops_every_piece() {
    let mut rig = Rig::new(Logs::default());
    rig.owner_says(1, "contame algo");
    rig.core(Event::BrainText {
        turn: TurnId(1),
        text: "Había una vez.".into(),
    });
    rig.chunk(1, 2400, false);
    rig.host.shutdown();
    assert_eq!(rig.device.stops(), 1);
    assert!(rig.device.queued().is_empty());
    let calls = rig.calls();
    for piece in ["brain dropped", "stt dropped", "tts dropped"] {
        assert!(calls.contains(&piece.to_owned()), "{piece}: {calls:?}");
    }
    // Late input after shutdown is harmless.
    rig.chunk(1, 100, true);
    rig.core(Event::BrainDone { turn: TurnId(1) });
    rig.host.shutdown();
    assert_eq!(
        rig.calls().iter().filter(|c| *c == "brain dropped").count(),
        1
    );
}

#[test]
fn the_loop_ends_on_enter_on_ctrl_c_and_after_the_turns_asked_for() {
    let quit = AtomicBool::new(false);
    let mut rig = Rig::new(Logs::default());

    let (tx, rx) = mpsc::channel();
    tx.send(Input::Quit).unwrap();
    assert_eq!(
        run(&mut rig.host, &rx, &quit, Until::default()),
        Ended::Quit
    );

    let ctrl_c = AtomicBool::new(true);
    assert_eq!(
        run(&mut rig.host, &rx, &ctrl_c, Until::default()),
        Ended::Signal
    );

    // A whole turn through the channel, as the real pieces deliver it.
    let (tx, rx) = mpsc::channel();
    for input in [
        Input::Core(Event::SpeechStarted),
        Input::Segment {
            samples: vec![0.2; 1600],
            sample_rate: 16_000,
        },
        Input::Core(Event::SpeechEnded),
        Input::Core(Event::FinalTranscript {
            segment: SegmentId(1),
            text: "hola".into(),
        }),
        Input::Core(Event::BrainText {
            turn: TurnId(1),
            text: "Hola.".into(),
        }),
        Input::Core(Event::BrainDone { turn: TurnId(1) }),
        // The TTS had nothing to say: an empty last chunk.
        Input::Audio(SpeechAudio {
            speech: SpeechId(1),
            samples: Vec::new(),
            sample_rate: SAMPLE_RATE,
            last: true,
        }),
    ] {
        tx.send(input).unwrap();
    }
    // Playback reports on `rig.rx`; forward it so the loop sees it.
    let forward = tx.clone();
    let reports = std::mem::replace(&mut rig.rx, mpsc::channel().1);
    std::thread::spawn(move || {
        while let Ok(input) = reports.recv_timeout(Duration::from_secs(2)) {
            let _ = forward.send(input);
        }
    });
    let until = Until {
        turns: Some(1),
        limit: Some(Duration::from_secs(5)),
    };
    assert_eq!(run(&mut rig.host, &rx, &quit, until), Ended::TurnsDone);
    assert_eq!(rig.host.finished_turns(), 1);
}

#[test]
fn the_body_follows_the_turn_and_hears_what_was_said() {
    use wawi_body::{BodyLink, Flash, Mood, Update};

    let (link, body) = BodyLink::detached();
    let mut rig = Rig::new(Logs::default());
    rig.host.set_body(super::body::Reporter::new(link));

    rig.owner_says(1, "contame un cuento");
    rig.core(Event::BrainText {
        turn: TurnId(1),
        text: "Había una vez.".into(),
    });
    rig.chunk(1, 2400, false);
    // Cut off: the key goes down while she talks.
    rig.core(Event::SpeechStarted);

    let updates: Vec<Update> = body.try_iter().collect();
    assert_eq!(
        updates,
        [
            Update::Mood(Mood::Listening),
            Update::Mood(Mood::Transcribing),
            Update::Heard("contame un cuento".into()),
            Update::Mood(Mood::Thinking),
            Update::Said("Había una vez.".into()),
            Update::Mood(Mood::Speaking),
            Update::Flash(Flash::Interrupted),
            Update::Mood(Mood::Listening),
        ]
    );
}

#[test]
fn an_unclear_transcript_is_logged_and_never_reaches_the_brain() {
    let dir = std::env::temp_dir().join(format!("wawi2-unclear-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut rig = Rig::new(Logs::open(&dir).unwrap());
    rig.core(Event::SpeechStarted);
    rig.input(Input::Segment {
        samples: vec![0.2; 16_000],
        sample_rate: 16_000,
    });
    rig.input(Input::KeyReleased(std::time::Instant::now()));
    rig.core(Event::SpeechEnded);
    rig.core(Event::UnclearTranscript {
        segment: SegmentId(1),
        text: "what time is it".into(),
    });
    let calls = rig.calls();
    assert!(
        !calls.iter().any(|c| c.starts_with("brain start")),
        "{calls:?}"
    );
    assert!(
        calls.iter().any(|c| c.starts_with("tts say")),
        "she asks to repeat: {calls:?}"
    );
    let console = rig.console.text();
    assert!(console.contains("no entendido"), "{console}");

    let speech = rig
        .calls()
        .iter()
        .find_map(|c| c.strip_prefix("tts say ")?.split(' ').next()?.parse().ok())
        .unwrap();
    rig.chunk(speech, 1200, true);
    rig.play_one();
    assert_eq!(rig.state(), "idle");
    drop(rig);
    let turns = std::fs::read_to_string(dir.join("turns.log")).unwrap();
    assert!(turns.contains("via=\"no entendido\""), "{turns}");
    assert!(turns.contains("heard=\"what time is it\""), "{turns}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unset_brain_choices_keep_the_brains_defaults() {
    use wawi_brain_claude::{ClaudeConfig, DEFAULT_EFFORT, DEFAULT_MODEL};

    let mut config = ClaudeConfig::default();
    super::apply_choices(&mut config, None, None);
    assert_eq!(config.effective_model(), Some(DEFAULT_MODEL));
    assert_eq!(config.effective_effort(), Some(DEFAULT_EFFORT));

    super::apply_choices(
        &mut config,
        Some("claude-opus-5-5".into()),
        Some(String::new()),
    );
    assert_eq!(config.effective_model(), Some("claude-opus-5-5"));
    assert_eq!(config.effective_effort(), None, "empty: the CLI decides");
    assert_eq!(
        super::describe_choice(config.effective_effort()),
        "el predeterminado del CLI"
    );
}

/// The owner asks, the model calls `avisame`, and a minute later Wawi says
/// it on her own: in the console, the voice and the bubble.
#[test]
fn a_reminder_set_by_the_model_is_said_on_its_own_when_due() {
    use wawi_body::{BodyLink, Update};

    let (link, body) = BodyLink::detached();
    let (mut rig, clock) = Rig::with_tools(Logs::default(), None);
    rig.host.set_body(super::body::Reporter::new(link));
    rig.owner_says(1, "avisame en un minuto que saque la ropa");
    assert_eq!(
        *rig.nows.borrow(),
        ["viernes 20 de noviembre de 2026, 15:00 (2026-11-20T15:00, hora local)"],
        "every turn carries the local date and time"
    );
    rig.core(Event::BrainToolCall {
        turn: TurnId(1),
        call: ToolCall {
            id: "c1".into(),
            name: "avisame".into(),
            args_json: r#"{"en_segundos":60,"texto":"sacar la ropa"}"#.into(),
        },
    });
    rig.pump();
    assert!(
        rig.calls().contains(&"brain tool_result 1 c1".to_owned()),
        "{:?}",
        rig.calls()
    );
    rig.core(Event::BrainText {
        turn: TurnId(1),
        text: "Dale, en un minuto te aviso.".into(),
    });
    rig.core(Event::BrainDone { turn: TurnId(1) });
    rig.chunk(1, 2400, true);
    rig.play_one();
    assert_eq!(rig.state(), "idle");

    clock.advance(59);
    rig.tick();
    assert_eq!(rig.said().len(), 1, "not yet");
    clock.advance(1);
    rig.tick();
    assert_eq!(
        rig.said().last().unwrap(),
        "tts say 2 Te aviso: sacar la ropa."
    );
    assert!(
        rig.console
            .text()
            .contains("wawi (aviso): «Te aviso: sacar la ropa.»")
    );
    let updates: Vec<Update> = body.try_iter().collect();
    assert!(
        updates.contains(&Update::Said("Te aviso: sacar la ropa.".into())),
        "{updates:?}"
    );
    rig.tick();
    assert_eq!(rig.said().len(), 2, "said once");
}

/// A home with "sacar la ropa" set for a minute from now, and a rig whose
/// tools use it. The minute has not passed yet.
fn rig_with_a_reminder(name: &str) -> (Rig, Rc<FakeClock>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("wawi2-host-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let home = Home::open(&dir).unwrap();
    let clock = FakeClock::at(Civil::date(2026, 11, 20).at(15, 0, 0), -3);
    let earlier = Toolbox::open(Some(home.clone()), clock.clone());
    let set = earlier.run("avisame", r#"{"en_segundos":60,"texto":"sacar la ropa"}"#);
    assert!(matches!(set, Some(Ok(_))), "{set:?}");
    drop(earlier);
    let rig = Rig::new(Logs::default()).tools_on(Some(home), clock.clone());
    (rig, clock, dir)
}

#[test]
fn a_reminder_waits_while_the_owner_holds_the_key() {
    let (mut rig, clock, dir) = rig_with_a_reminder("key");
    rig.core(Event::SpeechStarted);
    clock.advance(60);
    rig.tick();
    assert!(
        rig.said().is_empty(),
        "never over the owner: {:?}",
        rig.calls()
    );
    rig.input(Input::Segment {
        samples: vec![0.2; 16_000],
        sample_rate: 16_000,
    });
    rig.core(Event::SpeechEnded);
    rig.core(Event::FinalTranscript {
        segment: SegmentId(1),
        text: "hola".into(),
    });
    rig.tick();
    assert!(rig.said().is_empty(), "the owner's turn comes first");
    rig.core(Event::BrainText {
        turn: TurnId(1),
        text: "Hola.".into(),
    });
    rig.core(Event::BrainDone { turn: TurnId(1) });
    rig.chunk(1, 2400, true);
    rig.play_one();
    assert_eq!(
        rig.said(),
        ["tts say 1 Hola.", "tts say 2 Te aviso: sacar la ropa."]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_reminder_waits_while_she_speaks() {
    let (mut rig, clock, dir) = rig_with_a_reminder("speaking");
    rig.owner_says(1, "contame un cuento");
    rig.core(Event::BrainText {
        turn: TurnId(1),
        text: "Había una vez.".into(),
    });
    rig.chunk(1, 2400, false);
    clock.advance(60);
    rig.tick();
    assert_eq!(rig.said(), ["tts say 1 Había una vez."], "not over herself");
    rig.core(Event::BrainDone { turn: TurnId(1) });
    rig.chunk(1, 0, true);
    rig.play_one();
    rig.play_one();
    assert_eq!(
        rig.said().last().unwrap(),
        "tts say 2 Te aviso: sacar la ropa."
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reminders_missed_while_closed_are_announced_at_startup() {
    let (rig, clock, dir) = rig_with_a_reminder("missed");
    drop(rig);
    // Wawi was closed for an hour.
    clock.advance(3_600);
    let mut rig = Rig::new(Logs::default()).tools_on(Some(Home::open(&dir).unwrap()), clock);
    rig.host.announce_missed();
    rig.pump();
    assert_eq!(
        rig.said(),
        ["tts say 1 Mientras no estaba se pasó un aviso: hoy a las 15:01, sacar la ropa."]
    );
    rig.tick();
    assert_eq!(rig.said().len(), 1, "and not again");
    let file = std::fs::read_to_string(dir.join("reminders.json")).unwrap();
    assert!(!file.contains("sacar la ropa"), "{file}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_brain_failure_writes_an_incident_off_the_turn_path() {
    let home = std::env::temp_dir().join(format!("wawi2-incident-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let mut rig = Rig::new(Logs::default());
    rig.host
        .install_incidents(super::incidents::Incidents::open(&home));
    rig.owner_says(1, "contame algo privado");
    let started = std::time::Instant::now();
    rig.core(Event::BrainFailed {
        turn: TurnId(1),
        reason: "claude se cerró: boom".into(),
    });
    assert!(
        started.elapsed() < Duration::from_millis(50),
        "the turn never waits on the disk"
    );
    assert!(!rig.said().is_empty(), "the failure is still said");
    drop(rig);
    let files: Vec<_> = std::fs::read_dir(home.join("cola/incidentes/new"))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert_eq!(files.len(), 1);
    let text = std::fs::read_to_string(files[0].path()).unwrap();
    assert!(text.contains("\"tipo\":\"cerebro_caido\""), "{text}");
    assert!(
        !text.contains("privado"),
        "never what the owner said: {text}"
    );
    let _ = std::fs::remove_dir_all(&home);
}
