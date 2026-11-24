mod common;

use common::{FakeTool, FixedClock, Scenario, suggest_args};
use wawi_core::{
    Config, DiagnosticReason as Reason, Effect, Event, FailureKind, INITIATIVE_NO_CONFIRMATION,
    INITIATIVE_READ_ONLY, PermissionLevel, Priority, Route, RouteDecision, SUGGEST_TOOL, SegmentId,
    SpeechId, SpeechKind, StateKind as K, SuggestionId, TimeRoute, ToolOutcome, TurnId, TurnSource,
};

fn thinking(text: &str) -> Scenario {
    let mut s = Scenario::new();
    s.say(text).assert_state(K::Thinking);
    s
}

fn speaking_mid_answer() -> Scenario {
    let mut s = thinking("contame algo");
    s.brain_text("Había una vez.").assert_state(K::Speaking);
    s
}

fn awaiting_confirmation() -> Scenario {
    let mut s = Scenario::new();
    s.dialogue
        .registry_mut()
        .register(FakeTool::confirmed("borrar"));
    s.say("borrá las fotos").assert_state(K::Thinking);
    s.brain_tool("c1", "borrar")
        .assert_state(K::AwaitingConfirmation);
    s
}

// --- Normal turn ---

#[test]
fn normal_turn_with_playback_ending_after_the_brain() {
    let mut s = Scenario::new();
    s.speech_start().assert_state(K::Listening);
    s.speech_end().assert_state(K::Listening);
    assert_fx!(s, Effect::Transcribe { .. });

    s.transcript("hola").assert_state(K::Thinking);
    assert_eq!(s.brain_started_with(), Some("hola"));
    assert_no_fx!(s, Effect::Speak { .. });

    s.brain_text("Hola.").assert_state(K::Speaking);
    assert_eq!(s.spoken(), ["Hola."]);
    s.brain_text("¿Cómo andás?").assert_state(K::Speaking);
    assert_eq!(s.spoken(), ["¿Cómo andás?"]);

    s.brain_done().assert_state(K::Speaking);
    s.playback_finished().assert_state(K::Idle);
}

#[test]
fn normal_turn_with_the_brain_ending_after_playback() {
    let mut s = thinking("hola");
    s.brain_text("Hola.").playback_finished();
    s.assert_state(K::Thinking);
    s.brain_done().assert_state(K::Idle);
}

#[test]
fn playback_of_an_earlier_sentence_does_not_end_the_turn() {
    let mut s = thinking("hola");
    s.brain_text("Uno.");
    let first = s.last_speech;
    s.brain_text("Dos.").brain_done();
    s.send(Event::PlaybackFinished { up_to: first });
    s.assert_state(K::Speaking);
    assert_fx!(
        s,
        Effect::Diagnostic {
            reason: Reason::PlaybackStillQueued,
            ..
        }
    );
    s.playback_finished().assert_state(K::Idle);
}

// --- Barge-in ---

#[test]
fn barge_in_stops_voice_and_brain_at_once_and_the_new_turn_stands_alone() {
    let mut s = speaking_mid_answer();
    let old = s.last_turn;

    s.speech_start().assert_state(K::Listening);
    assert_eq!(s.fx[0], Effect::StopPlayback);
    assert_eq!(s.fx[1], Effect::CancelBrain { turn: old });

    s.speech_end()
        .transcript("otra cosa")
        .assert_state(K::Thinking);
    assert_eq!(s.brain_started_with(), Some("otra cosa"));
    assert!(s.last_turn > old);
}

#[test]
fn barge_in_on_a_reply_with_no_brain_behind_it_only_stops_the_voice() {
    let mut s = Scenario::new();
    s.floor(Priority::High, "aviso").assert_state(K::Speaking);
    s.speech_start().assert_state(K::Listening);
    assert_fx!(s, Effect::StopPlayback);
    assert_no_fx!(s, Effect::CancelBrain { .. });
}

// --- Merge while thinking ---

#[test]
fn speech_while_thinking_cancels_and_merges_into_one_turn() {
    let mut s = thinking("poné música");
    let old = s.last_turn;

    s.speech_start().assert_state(K::Listening);
    assert_fx!(s, Effect::CancelBrain { turn } if *turn == old);

    s.speech_end()
        .transcript("de los Beatles")
        .assert_state(K::Thinking);
    assert_eq!(s.brain_started_with(), Some("poné música de los Beatles"));
    assert!(s.last_turn > old);
}

#[test]
fn repeated_interruptions_keep_every_part_in_order() {
    let mut s = thinking("uno");
    s.say("dos").assert_state(K::Thinking);
    s.say("tres").assert_state(K::Thinking);
    assert_eq!(s.brain_started_with(), Some("uno dos tres"));
}

#[test]
fn noise_while_thinking_restarts_the_turn_with_the_held_text() {
    let mut s = thinking("qué onda");
    s.say("   ").assert_state(K::Thinking);
    assert_eq!(s.brain_started_with(), Some("qué onda"));
}

#[test]
fn speech_after_a_sentence_was_spoken_is_a_new_turn() {
    let mut s = thinking("contame");
    s.brain_text("Bueno.").playback_finished();
    s.assert_state(K::Thinking);
    s.say("otra cosa").assert_state(K::Thinking);
    assert_eq!(s.brain_started_with(), Some("otra cosa"));
}

#[test]
fn speech_after_a_tool_started_does_not_rerun_the_request() {
    let mut s = Scenario::new();
    s.dialogue.registry_mut().register(FakeTool::plain("luces"));
    s.say("prendé la luz").brain_tool("c1", "luces");
    s.assert_state(K::Thinking);
    s.say("y la tele").assert_state(K::Thinking);
    assert_eq!(s.brain_started_with(), Some("y la tele"));
}

#[test]
fn stop_word_while_thinking_drops_the_held_text() {
    let mut s = thinking("poné música");
    s.say("pará").assert_state(K::Idle);
    assert_no_fx!(s, Effect::StartBrain { .. });
}

// --- A utterance closes only when its words exist ---

#[test]
fn a_transcript_that_arrives_while_speech_continues_is_held() {
    let mut s = Scenario::new();
    s.speech_start().speech_end();
    s.speech_start();
    s.transcript("quiero saber").assert_state(K::Listening);
    assert_no_fx!(s, Effect::StartBrain { .. });
    s.speech_end().assert_state(K::Listening);
    s.transcript("el clima").assert_state(K::Thinking);
    assert_eq!(s.brain_started_with(), Some("quiero saber el clima"));
}

#[test]
fn transcripts_arriving_out_of_order_keep_the_spoken_order() {
    let mut s = Scenario::new();
    s.speech_start().speech_end().speech_start().speech_end();
    let (first, second) = (s.open_segments[0], s.open_segments[1]);
    s.transcript_for(second, "segundo")
        .assert_state(K::Listening);
    s.transcript_for(first, "primero").assert_state(K::Thinking);
    assert_eq!(s.brain_started_with(), Some("primero segundo"));
}

#[test]
fn an_stt_that_never_answers_keeps_what_was_held_and_does_not_hang() {
    let config = Config::default();
    let timeout = config.listen_timeout_ms;
    let mut s = thinking("qué onda");
    s.speech_start().speech_end().assert_state(K::Listening);
    s.tick(timeout - 1).assert_state(K::Listening);
    s.tick(timeout).assert_state(K::Thinking);
    assert_eq!(s.brain_started_with(), Some("qué onda"));

    let late = s.open_segments.remove(0);
    s.transcript_for(late, "tarde");
    assert_fx!(
        s,
        Effect::Diagnostic {
            reason: Reason::StaleSegment,
            ..
        }
    );
}

#[test]
fn stuck_speech_with_nothing_captured_returns_to_idle() {
    let mut s = Scenario::new();
    s.speech_start();
    s.tick(Config::default().listen_timeout_ms)
        .assert_state(K::Idle);
}

// --- Stale events ---

#[test]
fn stale_brain_events_after_a_cancel_are_ignored() {
    let mut s = speaking_mid_answer();
    let old = s.last_turn;
    s.cancel().assert_state(K::Idle);

    for event in [
        Event::BrainText {
            turn: old,
            text: "tarde".into(),
        },
        Event::BrainToolCall {
            turn: old,
            call: common::call("c1", "borrar"),
        },
        Event::BrainDone { turn: old },
        Event::BrainFailed {
            turn: old,
            reason: "x".into(),
        },
        Event::ToolFinished {
            turn: old,
            call_id: "c1".into(),
            outcome: ToolOutcome::Success("x".into()),
        },
    ] {
        s.send(event).assert_state(K::Idle);
        assert_eq!(s.fx.len(), 1, "{:?}", s.fx);
        assert_fx!(
            s,
            Effect::Diagnostic {
                reason: Reason::StaleTurn,
                ..
            }
        );
    }
}

#[test]
fn stale_brain_events_do_not_touch_the_turn_that_replaced_it() {
    let mut s = speaking_mid_answer();
    let old = s.last_turn;
    s.say("otra cosa").assert_state(K::Thinking);
    let current = s.last_turn;

    s.send(Event::BrainText {
        turn: old,
        text: "tarde".into(),
    });
    assert_no_fx!(s, Effect::Speak { .. });
    s.send(Event::BrainDone { turn: old })
        .assert_state(K::Thinking);
    assert_eq!(s.dialogue.state().turn(), Some(current));
}

#[test]
fn a_turn_id_from_the_future_is_ignored_too() {
    let mut s = thinking("hola");
    s.send(Event::BrainDone { turn: TurnId(999) });
    s.assert_state(K::Thinking);
}

#[test]
fn playback_end_of_stopped_speech_does_not_end_newer_speech() {
    let mut s = speaking_mid_answer();
    let stopped = s.last_speech;
    s.say("otra cosa");
    s.brain_text("Nueva respuesta.").brain_done();
    s.send(Event::PlaybackFinished { up_to: stopped });
    s.assert_state(K::Speaking);
}

#[test]
fn a_transcript_arriving_after_the_cancel_does_not_start_a_turn() {
    let mut s = Scenario::new();
    s.speech_start().speech_end();
    let segment = s.open_segments.remove(0);
    s.cancel().assert_state(K::Idle);
    s.transcript_for(segment, "esto ya no")
        .assert_state(K::Idle);
    assert_no_fx!(s, Effect::StartBrain { .. });
}

#[test]
fn a_transcript_for_a_cancelled_segment_does_not_mix_into_new_speech() {
    let mut s = Scenario::new();
    s.speech_start().speech_end();
    let old = s.open_segments.remove(0);
    s.cancel();
    s.speech_start().speech_end();
    s.transcript_for(old, "viejo").assert_state(K::Listening);
    s.transcript("nuevo").assert_state(K::Thinking);
    assert_eq!(s.brain_started_with(), Some("nuevo"));
}

// --- Explicit cancel ---

#[test]
fn cancel_from_listening_drops_what_was_held() {
    let mut s = thinking("poné música");
    s.speech_start().assert_state(K::Listening);
    s.cancel().assert_state(K::Idle);
    s.say("hola");
    assert_eq!(s.brain_started_with(), Some("hola"));
}

#[test]
fn cancel_from_thinking_cancels_the_brain() {
    let mut s = thinking("hola");
    let turn = s.last_turn;
    s.cancel().assert_state(K::Idle);
    assert_eq!(s.fx, [Effect::CancelBrain { turn }]);
}

#[test]
fn cancel_from_speaking_stops_voice_and_brain() {
    let mut s = speaking_mid_answer();
    let turn = s.last_turn;
    s.cancel().assert_state(K::Idle);
    assert_eq!(s.fx, [Effect::StopPlayback, Effect::CancelBrain { turn }]);
}

#[test]
fn cancel_from_confirmation_stops_the_question_and_the_brain() {
    let mut s = awaiting_confirmation();
    let turn = s.last_turn;
    s.cancel().assert_state(K::Idle);
    assert_eq!(s.fx, [Effect::StopPlayback, Effect::CancelBrain { turn }]);
}

#[test]
fn cancel_when_idle_is_reported_not_a_panic() {
    let mut s = Scenario::new();
    s.cancel().assert_state(K::Idle);
    assert_fx!(
        s,
        Effect::Diagnostic {
            reason: Reason::NothingToCancel,
            ..
        }
    );
}

#[test]
fn cancel_also_cancels_running_tools() {
    let mut s = Scenario::new();
    s.dialogue.registry_mut().register(FakeTool::plain("luces"));
    s.say("prendé la luz").brain_tool("c1", "luces");
    let turn = s.last_turn;
    s.cancel();
    assert_fx!(s, Effect::CancelBrain { turn: t } if *t == turn);
    assert_fx!(s, Effect::CancelTool { call_id, .. } if call_id == "c1");
}

// --- Watchdog ---

#[test]
fn watchdog_cancels_a_silent_turn_at_the_default_45_seconds() {
    let mut s = thinking("hola");
    let turn = s.last_turn;
    s.tick(44_999).assert_state(K::Thinking);
    assert!(s.fx.is_empty());

    s.tick(45_000).assert_state(K::Speaking);
    assert_fx!(s, Effect::CancelBrain { turn: t } if *t == turn);
    assert_fx!(
        s,
        Effect::Speak {
            kind: SpeechKind::Failure(FailureKind::Watchdog),
            ..
        }
    );
    // The failure is not spoken for the turn it just cancelled.
    assert!(s.last_turn > turn);

    s.playback_finished().assert_state(K::Idle);
    s.send(Event::BrainDone { turn });
    assert_fx!(
        s,
        Effect::Diagnostic {
            reason: Reason::StaleTurn,
            ..
        }
    );
}

#[test]
fn watchdog_deadline_is_configurable() {
    let mut s = Scenario::with_config(Config {
        think_timeout_ms: 1_000,
        ..Config::default()
    });
    s.say("hola").assert_state(K::Thinking);
    s.tick(1_000).assert_state(K::Speaking);
}

#[test]
fn each_tool_start_extends_the_watchdog() {
    let mut s = Scenario::with_config(Config {
        tool_extension_ms: 10_000,
        ..Config::default()
    });
    s.dialogue.registry_mut().register(FakeTool::plain("uno"));
    s.say("hola");
    s.tick(10_000).brain_tool("c1", "uno");
    s.tick(44_999).assert_state(K::Thinking);
    s.brain_tool("c2", "uno");

    // 45 s + 2 extensions of 10 s.
    s.tick(64_999).assert_state(K::Thinking);
    s.tick(65_000).assert_state(K::Speaking);
    assert_fx!(s, Effect::CancelTool { call_id, .. } if call_id == "c1");
    assert_fx!(s, Effect::CancelTool { call_id, .. } if call_id == "c2");
}

#[test]
fn watchdog_does_not_cut_a_long_answer_while_it_plays() {
    let mut s = speaking_mid_answer();
    s.tick(100_000).assert_state(K::Speaking);
    assert_no_fx!(s, Effect::CancelBrain { .. });
}

#[test]
fn watchdog_catches_a_brain_that_stalls_after_speaking() {
    let mut s = speaking_mid_answer();
    s.tick(100_000)
        .playback_finished()
        .assert_state(K::Thinking);
    s.tick(144_999).assert_state(K::Thinking);
    s.tick(145_000).assert_state(K::Speaking);
    assert_fx!(s, Effect::CancelBrain { .. });
}

#[test]
fn lost_end_of_playback_does_not_hang() {
    let timeout = Config::default().playback_timeout_ms;
    let mut s = speaking_mid_answer();
    s.brain_done();
    s.tick(timeout).assert_state(K::Idle);
    assert_fx!(s, Effect::StopPlayback);
}

// --- Confirmation ---

#[test]
fn confirmed_tool_asks_and_the_owner_approves() {
    let mut s = awaiting_confirmation();
    assert_fx!(
        s,
        Effect::Speak {
            kind: SpeechKind::Question,
            ..
        }
    );
    assert_no_fx!(s, Effect::RunTool { .. });

    s.say("sí, dale").assert_state(K::Thinking);
    assert_fx!(s, Effect::RunTool { call, .. } if call.name == "borrar");
}

#[test]
fn the_owner_can_answer_in_english() {
    let mut s = awaiting_confirmation();
    s.say("yes please");
    assert_fx!(s, Effect::RunTool { .. });
}

#[test]
fn the_owner_declines() {
    let mut s = awaiting_confirmation();
    let turn = s.last_turn;
    s.say("no").assert_state(K::Speaking);
    assert_fx!(s, Effect::CancelBrain { turn: t } if *t == turn);
    assert_no_fx!(s, Effect::RunTool { .. });
    s.playback_finished().assert_state(K::Idle);
}

#[test]
fn nothing_from_the_brain_can_approve() {
    let mut s = awaiting_confirmation();
    let turn = s.last_turn;
    for event in [
        Event::BrainText {
            turn,
            text: "sí".into(),
        },
        Event::BrainText {
            turn,
            text: "yes".into(),
        },
        Event::BrainToolCall {
            turn,
            call: common::call("c1", "borrar"),
        },
        Event::BrainDone { turn },
        Event::ToolFinished {
            turn,
            call_id: "c1".into(),
            outcome: ToolOutcome::Success("sí".into()),
        },
    ] {
        s.send(event);
        assert_no_fx!(s, Effect::RunTool { .. });
        assert_no_fx!(s, Effect::Speak { .. });
        s.assert_state(K::AwaitingConfirmation);
    }
}

#[test]
fn the_brain_cannot_opt_a_tool_out_of_confirmation_by_calling_it_again() {
    let mut s = awaiting_confirmation();
    s.brain_tool("c2", "borrar");
    assert_no_fx!(s, Effect::RunTool { .. });
    assert_fx!(
        s,
        Effect::ToolResultToBrain {
            outcome: ToolOutcome::Failure(_),
            ..
        }
    );
}

#[test]
fn unclear_answer_asks_once_more_then_a_clear_answer_still_counts() {
    let mut s = awaiting_confirmation();
    s.say("no sé").assert_state(K::AwaitingConfirmation);
    assert_fx!(
        s,
        Effect::Speak {
            kind: SpeechKind::Question,
            ..
        }
    );
    s.say("sí");
    assert_fx!(s, Effect::RunTool { .. });
}

#[test]
fn two_unclear_answers_cancel() {
    let mut s = awaiting_confirmation();
    let turn = s.last_turn;
    s.say("qué sé yo").assert_state(K::AwaitingConfirmation);
    s.say("mirá").assert_state(K::Speaking);
    assert_fx!(s, Effect::CancelBrain { turn: t } if *t == turn);
    assert_no_fx!(s, Effect::RunTool { .. });
}

#[test]
fn noise_does_not_use_up_the_reask() {
    let mut s = awaiting_confirmation();
    s.say("  ").assert_state(K::AwaitingConfirmation);
    assert_no_fx!(s, Effect::Speak { .. });
    s.say("qué sé yo").assert_state(K::AwaitingConfirmation);
    assert_fx!(
        s,
        Effect::Speak {
            kind: SpeechKind::Question,
            ..
        }
    );
}

#[test]
fn confirmation_times_out_and_cancels() {
    let mut s = awaiting_confirmation();
    let timeout = Config::default().confirm_timeout_ms;
    s.tick(timeout - 1).assert_state(K::AwaitingConfirmation);
    s.tick(timeout).assert_state(K::Speaking);
    assert_fx!(s, Effect::CancelBrain { .. });
    assert_no_fx!(s, Effect::RunTool { .. });
}

#[test]
fn waiting_for_the_owner_does_not_count_against_the_brain() {
    let mut s = awaiting_confirmation();
    s.tick(29_000).say("sí").assert_state(K::Thinking);
    s.tick(29_001).assert_state(K::Thinking);
    assert_no_fx!(s, Effect::CancelBrain { .. });
}

#[test]
fn stop_word_during_confirmation_cancels_without_asking_again() {
    let mut s = awaiting_confirmation();
    s.say("pará").assert_state(K::Idle);
    assert_fx!(s, Effect::CancelBrain { .. });
    assert_no_fx!(s, Effect::RunTool { .. });
    assert_no_fx!(s, Effect::Speak { .. });
}

#[test]
fn speaking_over_the_question_stops_it_but_keeps_waiting() {
    let mut s = awaiting_confirmation();
    s.speech_start().assert_state(K::AwaitingConfirmation);
    assert_eq!(s.fx, [Effect::StopPlayback]);
    s.speech_end().transcript("dale");
    assert_fx!(s, Effect::RunTool { .. });
}

#[test]
fn an_answer_in_two_segments_is_read_as_a_whole() {
    let mut s = awaiting_confirmation();
    s.speech_start().speech_end().speech_start();
    s.transcript("sí").assert_state(K::AwaitingConfirmation);
    assert_no_fx!(s, Effect::RunTool { .. });
    s.speech_end().transcript("no, pará");
    assert_no_fx!(s, Effect::RunTool { .. });
}

#[test]
fn routes_do_not_see_a_confirmation_answer() {
    struct Hungry;
    impl Route for Hungry {
        fn name(&self) -> &str {
            "hungry"
        }
        fn handle(&mut self, _: &str) -> RouteDecision {
            RouteDecision::Handled("me lo comí".into())
        }
    }
    let mut s = Scenario::new();
    s.dialogue
        .registry_mut()
        .register(FakeTool::confirmed("borrar"));
    s.say("hola").assert_state(K::Thinking);
    s.dialogue.router_mut().add(Box::new(Hungry));
    s.brain_tool("c1", "borrar");
    s.say("sí");
    assert_fx!(s, Effect::RunTool { .. });
    assert_no_fx!(
        s,
        Effect::Speak {
            kind: SpeechKind::Route(_),
            ..
        }
    );
}

#[test]
fn a_tool_removed_while_waiting_fails_for_the_brain_even_after_a_yes() {
    let mut s = awaiting_confirmation();
    s.dialogue.registry_mut().remove("borrar");
    s.say("sí").assert_state(K::Thinking);
    assert_no_fx!(s, Effect::RunTool { .. });
    assert_fx!(
        s,
        Effect::ToolResultToBrain {
            outcome: ToolOutcome::Failure(_),
            ..
        }
    );
}

#[test]
fn asking_while_a_sentence_plays_keeps_tracking_that_audio() {
    let mut s = Scenario::new();
    s.dialogue
        .registry_mut()
        .register(FakeTool::confirmed("borrar"));
    s.say("borrá")
        .brain_text("Dejame ver.")
        .brain_tool("c1", "borrar");
    s.assert_state(K::AwaitingConfirmation);
    s.speech_start();
    assert_fx!(s, Effect::StopPlayback);
}

// --- Tool ack ---

#[test]
fn tool_ack_is_spoken_immediately_before_the_tool_runs() {
    let mut s = Scenario::new();
    s.dialogue
        .registry_mut()
        .register(FakeTool::with_ack("clima", "a ver, me fijo"));
    s.say("qué tiempo hace").brain_tool("c1", "clima");
    s.assert_state(K::Speaking);
    assert!(matches!(
        &s.fx[0],
        Effect::Speak { kind: SpeechKind::Ack, text, .. } if text == "a ver, me fijo"
    ));
    assert!(matches!(&s.fx[1], Effect::RunTool { .. }));

    s.playback_finished().assert_state(K::Thinking);
    s.tool_finished("c1");
    assert_fx!(s, Effect::ToolResultToBrain { call_id, .. } if call_id == "c1");
    s.brain_text("Hace frío.").brain_done();
    s.playback_finished().assert_state(K::Idle);
}

#[test]
fn ack_is_also_spoken_when_the_tool_starts_after_a_confirmation() {
    let mut s = Scenario::new();
    let mut tool = FakeTool::confirmed("borrar").spec().clone();
    tool.ack = Some("voy".into());
    s.dialogue.registry_mut().register(Box::new(SpecTool(tool)));
    s.say("borrá").brain_tool("c1", "borrar");
    s.say("sí").assert_state(K::Speaking);
    assert_eq!(s.spoken(), ["voy"]);
    assert_fx!(s, Effect::RunTool { .. });
}

struct SpecTool(wawi_core::ToolSpec);

impl wawi_core::Tool for SpecTool {
    fn spec(&self) -> &wawi_core::ToolSpec {
        &self.0
    }
    fn start(&mut self, _: TurnId, _: &wawi_core::ToolCall) {}
    fn cancel(&mut self, _: TurnId, _: &str) {}
}

#[test]
fn a_late_result_of_an_unknown_call_is_reported() {
    let mut s = thinking("hola");
    s.tool_finished("nunca");
    assert_fx!(
        s,
        Effect::Diagnostic {
            reason: Reason::UnknownToolCall,
            ..
        }
    );
    assert_no_fx!(s, Effect::ToolResultToBrain { .. });
}

// --- Floor requests ---

#[test]
fn floor_request_speaks_immediately_when_idle() {
    let mut s = Scenario::new();
    s.floor(Priority::Low, "llegó un mail")
        .assert_state(K::Speaking);
    assert_eq!(s.spoken(), ["llegó un mail"]);
    s.playback_finished().assert_state(K::Idle);
}

#[test]
fn high_priority_waits_for_idle_and_low_is_dropped() {
    let mut s = speaking_mid_answer();
    s.floor(Priority::High, "importante");
    assert_fx!(s, Effect::FloorQueued { .. });
    assert_no_fx!(
        s,
        Effect::Speak {
            kind: SpeechKind::Floor(_),
            ..
        }
    );
    s.floor(Priority::Low, "menor");
    assert_fx!(
        s,
        Effect::Diagnostic {
            reason: Reason::FloorDropped,
            ..
        }
    );

    s.brain_done().playback_finished();
    assert_eq!(s.spoken(), ["importante"]);
    s.assert_state(K::Speaking);
    s.playback_finished().assert_state(K::Idle);
    assert_eq!(s.dialogue.floor_queue_len(), 0);
}

#[test]
fn queued_items_play_one_at_a_time_in_order() {
    let mut s = speaking_mid_answer();
    s.floor(Priority::High, "primero")
        .floor(Priority::High, "segundo");
    s.brain_done().playback_finished();
    assert_eq!(s.spoken(), ["primero"]);
    s.playback_finished();
    assert_eq!(s.spoken(), ["segundo"]);
    s.playback_finished().assert_state(K::Idle);
}

#[test]
fn cancelling_a_turn_keeps_queued_items_and_they_wait_a_moment() {
    let mut s = speaking_mid_answer();
    s.floor(Priority::High, "importante");
    s.tick(1_000).cancel().assert_state(K::Idle);
    assert_eq!(s.dialogue.floor_queue_len(), 1);
    assert_no_fx!(
        s,
        Effect::Speak {
            kind: SpeechKind::Floor(_),
            ..
        }
    );

    s.tick(2_999).assert_state(K::Idle);
    s.tick(3_000).assert_state(K::Speaking);
    assert_eq!(s.spoken(), ["importante"]);
}

#[test]
fn barge_in_keeps_queued_items_until_idle() {
    let mut s = speaking_mid_answer();
    s.floor(Priority::High, "importante");
    s.say("hora").assert_state(K::Thinking);
    assert_eq!(s.dialogue.floor_queue_len(), 1);
    s.brain_done();
    assert_eq!(s.spoken(), ["importante"]);
}

#[test]
fn an_idle_moment_behind_a_queue_does_not_jump_it() {
    let mut s = speaking_mid_answer();
    s.floor(Priority::High, "primero");
    s.cancel().assert_state(K::Idle);
    s.floor(Priority::High, "segundo").assert_state(K::Idle);
    assert_eq!(s.dialogue.floor_queue_len(), 2);
    s.floor(Priority::Low, "menor").assert_state(K::Idle);
    assert_eq!(s.dialogue.floor_queue_len(), 2);
}

#[test]
fn a_spoken_failure_is_not_talked_over_by_the_queue() {
    let mut s = thinking("hola");
    s.floor(Priority::High, "importante");
    s.tick(45_000);
    assert_eq!(s.spoken().len(), 1);
    assert_no_fx!(
        s,
        Effect::Speak {
            kind: SpeechKind::Floor(_),
            ..
        }
    );
    s.playback_finished();
    assert_eq!(s.spoken(), ["importante"]);
}

// --- Brain failure ---

#[test]
fn brain_failure_speaks_and_returns_to_idle() {
    let mut s = thinking("hola");
    let turn = s.last_turn;
    s.send(Event::BrainFailed {
        turn,
        reason: "sin red".into(),
    });
    assert_fx!(
        s,
        Effect::Speak {
            kind: SpeechKind::Failure(FailureKind::Brain(reason)),
            ..
        } if reason == "sin red"
    );
    // The brain already ended, there is nothing to cancel.
    assert_no_fx!(s, Effect::CancelBrain { .. });
    s.playback_finished().assert_state(K::Idle);
}

#[test]
fn brain_failure_mid_answer_replaces_the_answer() {
    let mut s = speaking_mid_answer();
    let turn = s.last_turn;
    s.send(Event::BrainFailed {
        turn,
        reason: "x".into(),
    });
    assert_eq!(s.fx[0], Effect::StopPlayback);
    assert_eq!(s.spoken().len(), 1);
    s.playback_finished().assert_state(K::Idle);
}

// --- Registry ---

#[test]
fn unknown_tool_fails_for_the_brain_and_the_turn_goes_on() {
    let mut s = thinking("hola");
    s.brain_tool("c1", "no_existe").assert_state(K::Thinking);
    assert_fx!(
        s,
        Effect::ToolResultToBrain {
            outcome: ToolOutcome::Failure(reason),
            ..
        } if reason.contains("no_existe")
    );
    assert_no_fx!(s, Effect::RunTool { .. });
}

#[test]
fn disabled_tool_fails_for_the_brain_and_can_be_enabled_again() {
    let mut s = Scenario::new();
    s.dialogue.registry_mut().register(FakeTool::plain("luces"));
    assert!(s.dialogue.registry_mut().set_enabled("luces", false));
    s.say("hola");
    assert_fx!(s, Effect::StartBrain { context, .. } if context.tools.is_empty());
    s.brain_tool("c1", "luces");
    assert_no_fx!(s, Effect::RunTool { .. });
    assert_fx!(
        s,
        Effect::ToolResultToBrain {
            outcome: ToolOutcome::Failure(_),
            ..
        }
    );

    assert!(s.dialogue.registry_mut().set_enabled("luces", true));
    s.brain_tool("c2", "luces");
    assert_fx!(s, Effect::RunTool { .. });
}

#[test]
fn tools_are_registered_and_removed_at_runtime_and_the_brain_sees_it() {
    let mut s = Scenario::new();
    s.dialogue.registry_mut().register(FakeTool::plain("luces"));
    s.say("hola");
    assert_fx!(s, Effect::StartBrain { context, .. } if context.tools[0].name == "luces");
    s.cancel();
    assert!(s.dialogue.registry_mut().remove("luces").is_some());
    assert!(s.dialogue.registry_mut().remove("luces").is_none());
    assert!(!s.dialogue.registry_mut().set_enabled("luces", true));
    s.say("hola");
    assert_fx!(s, Effect::StartBrain { context, .. } if context.tools.is_empty());
}

// --- Routing ---

struct Canned {
    name: &'static str,
    trigger: &'static str,
    reply: &'static str,
}

impl Route for Canned {
    fn name(&self) -> &str {
        self.name
    }
    fn handle(&mut self, transcript: &str) -> RouteDecision {
        if transcript.contains(self.trigger) {
            RouteDecision::Handled(self.reply.into())
        } else {
            RouteDecision::Pass
        }
    }
}

fn canned(name: &'static str, trigger: &'static str, reply: &'static str) -> Box<Canned> {
    Box::new(Canned {
        name,
        trigger,
        reply,
    })
}

#[test]
fn time_route_answers_with_the_injected_clock_without_the_brain() {
    let mut s = Scenario::new();
    s.dialogue
        .router_mut()
        .add(Box::new(TimeRoute::new(FixedClock(9, 5))));
    s.say("¿Qué hora es?").assert_state(K::Speaking);
    assert_eq!(s.spoken(), ["Son las 09:05."]);
    assert_no_fx!(s, Effect::StartBrain { .. });
    s.playback_finished().assert_state(K::Idle);
}

#[test]
fn a_longer_question_passes_through_the_time_route_to_the_brain() {
    let mut s = Scenario::new();
    s.dialogue
        .router_mut()
        .add(Box::new(TimeRoute::new(FixedClock(9, 5))));
    s.say("qué hora es en Tokio").assert_state(K::Thinking);
}

#[test]
fn first_matching_route_wins_and_order_can_change_at_runtime() {
    let mut s = Scenario::new();
    let router = s.dialogue.router_mut();
    router.add(canned("a", "hola", "respuesta a"));
    router.add(canned("b", "hola", "respuesta b"));
    assert_eq!(router.names(), ["a", "b"]);

    s.say("hola");
    assert_eq!(s.spoken(), ["respuesta a"]);
    s.cancel();

    // Where a future light brain would slot in: ahead of everything.
    s.dialogue
        .router_mut()
        .insert(0, canned("light", "hola", "liviano"));
    s.say("hola");
    assert_eq!(s.spoken(), ["liviano"]);
    s.cancel();

    assert!(s.dialogue.router_mut().remove("light").is_some());
    assert!(s.dialogue.router_mut().remove("a").is_some());
    s.say("hola");
    assert_eq!(s.spoken(), ["respuesta b"]);
    s.cancel();

    assert!(s.dialogue.router_mut().remove("b").is_some());
    s.say("hola").assert_state(K::Thinking);
    assert_eq!(s.brain_started_with(), Some("hola"));
}

#[test]
fn hard_stop_words_are_decided_before_any_route() {
    let mut s = Scenario::new();
    s.dialogue.router_mut().add(canned("all", "", "me lo comí"));
    s.say("pará").assert_state(K::Idle);
    assert!(s.spoken().is_empty());
    s.say("Stop!").assert_state(K::Idle);
    assert!(s.spoken().is_empty());
    s.say("para mañana").assert_state(K::Speaking);
}

#[test]
fn routes_see_the_merged_transcript() {
    let mut s = thinking("qué hora");
    s.dialogue
        .router_mut()
        .add(Box::new(TimeRoute::new(FixedClock(9, 5))));
    s.say("es").assert_state(K::Speaking);
    assert_eq!(s.spoken(), ["Son las 09:05."]);
}

#[test]
fn a_route_reply_can_be_interrupted_like_any_speech() {
    let mut s = Scenario::new();
    s.dialogue.router_mut().add(canned("c", "hola", "qué tal"));
    s.say("hola");
    s.say("otra").assert_state(K::Thinking);
    assert_eq!(s.brain_started_with(), Some("otra"));
}

// --- Illegal input never panics ---

#[test]
fn out_of_place_events_are_reported_and_change_nothing() {
    let mut s = Scenario::new();
    s.speech_end().assert_state(K::Idle);
    assert_fx!(
        s,
        Effect::Diagnostic {
            reason: Reason::SpeechNotStarted,
            ..
        }
    );
    s.transcript_for(SegmentId(42), "x").assert_state(K::Idle);
    assert_fx!(
        s,
        Effect::Diagnostic {
            reason: Reason::StaleSegment,
            ..
        }
    );
    s.send(Event::PlaybackFinished {
        up_to: SpeechId(42),
    });
    assert_fx!(
        s,
        Effect::Diagnostic {
            reason: Reason::StalePlayback,
            ..
        }
    );

    s.speech_start().speech_start().assert_state(K::Listening);
    assert_fx!(
        s,
        Effect::Diagnostic {
            reason: Reason::SpeechAlreadyStarted,
            ..
        }
    );
}

#[test]
fn a_transcript_for_a_segment_still_open_is_refused() {
    let mut s = Scenario::new();
    s.speech_start();
    s.transcript_for(SegmentId(1), "antes de tiempo");
    s.assert_state(K::Listening);
    assert_fx!(
        s,
        Effect::Diagnostic {
            reason: Reason::SegmentStillOpen,
            ..
        }
    );
}

#[test]
fn a_clock_going_back_is_reported_and_does_not_move_deadlines() {
    let mut s = thinking("hola");
    s.tick(10_000).tick(5_000).assert_state(K::Thinking);
    assert_fx!(
        s,
        Effect::Diagnostic {
            reason: Reason::ClockWentBack,
            ..
        }
    );
    assert_eq!(s.dialogue.now_ms(), 10_000);
}

#[test]
fn turn_ids_only_grow() {
    let mut s = Scenario::new();
    let mut last = TurnId(0);
    for _ in 0..5 {
        s.say("hola");
        assert!(s.last_turn > last);
        last = s.last_turn;
        s.cancel();
    }
}

// --- Initiative: permissions decided in code ---

/// Registers one tool per permission level, plus a read tool that needs a yes.
fn with_every_level(s: &mut Scenario) {
    let registry = s.dialogue.registry_mut();
    registry.register(FakeTool::with_permission(
        "leer",
        PermissionLevel::Read,
        false,
    ));
    registry.register(FakeTool::with_permission(
        "escribir",
        PermissionLevel::Write,
        false,
    ));
    registry.register(FakeTool::with_permission(
        "sistema",
        PermissionLevel::System,
        false,
    ));
    registry.register(FakeTool::with_permission(
        "leer_con_si",
        PermissionLevel::Read,
        true,
    ));
}

fn initiative_thinking(text: &str) -> Scenario {
    let mut s = Scenario::new();
    with_every_level(&mut s);
    s.initiative(text).assert_state(K::Thinking);
    s
}

#[test]
fn an_initiative_turn_is_offered_only_read_tools_and_suggest() {
    let s = initiative_thinking("mirá el calendario");
    let names: Vec<&str> =
        s.fx.iter()
            .find_map(|effect| match effect {
                Effect::StartBrain { context, .. } => {
                    assert_eq!(context.source, TurnSource::Initiative);
                    Some(context.tools.iter().map(|t| t.name.as_str()).collect())
                }
                _ => None,
            })
            .unwrap();
    assert_eq!(names, ["leer", SUGGEST_TOOL]);
}

#[test]
fn an_initiative_turn_is_refused_write_and_system_tools_before_they_run() {
    let mut s = initiative_thinking("ordená las fotos");
    for (id, name) in [("c1", "escribir"), ("c2", "sistema")] {
        s.brain_tool(id, name).assert_state(K::Thinking);
        assert_no_fx!(s, Effect::RunTool { .. });
        assert_eq!(
            s.tool_result(),
            Some(&ToolOutcome::Failure(INITIATIVE_READ_ONLY.into()))
        );
        assert_fx!(
            s,
            Effect::Diagnostic {
                reason: Reason::ToolNotAllowed,
                ..
            }
        );
    }
    // Reading is still fine.
    s.brain_tool("c3", "leer");
    assert_fx!(s, Effect::RunTool { call, .. } if call.name == "leer");
}

#[test]
fn an_initiative_turn_cannot_ask_the_owner_for_a_confirmation() {
    let mut s = initiative_thinking("revisá algo");
    s.brain_tool("c1", "leer_con_si").assert_state(K::Thinking);
    assert_no_fx!(s, Effect::Speak { .. });
    assert_eq!(
        s.tool_result(),
        Some(&ToolOutcome::Failure(INITIATIVE_NO_CONFIRMATION.into()))
    );
}

#[test]
fn owner_turns_keep_every_tool_and_still_confirm() {
    let mut s = Scenario::new();
    with_every_level(&mut s);
    s.say("escribí y reiniciá");
    assert_fx!(s, Effect::StartBrain { context, .. } if context.tools.len() == 4
        && context.source == TurnSource::Owner);
    s.brain_tool("c1", "escribir");
    assert_fx!(s, Effect::RunTool { call, .. } if call.name == "escribir");
    s.brain_tool("c2", "sistema");
    assert_fx!(s, Effect::RunTool { call, .. } if call.name == "sistema");
    s.brain_tool("c3", "leer_con_si")
        .assert_state(K::AwaitingConfirmation);
    s.say("sí");
    assert_fx!(s, Effect::RunTool { call, .. } if call.name == "leer_con_si");
}

#[test]
fn an_initiative_never_interrupts() {
    let mut s = thinking("hola");
    s.initiative("algo").assert_state(K::Thinking);
    assert_fx!(
        s,
        Effect::Diagnostic {
            reason: Reason::InitiativeBusy,
            ..
        }
    );
    assert_no_fx!(s, Effect::StartBrain { .. } | Effect::CancelBrain { .. });

    let mut s = Scenario::new();
    s.speech_start()
        .initiative("algo")
        .assert_state(K::Listening);
    assert_no_fx!(s, Effect::StartBrain { .. });

    // Nor right after the owner pressed stop.
    let mut s = thinking("hola");
    s.cancel().initiative("algo").assert_state(K::Idle);
    s.tick(2_000).initiative("algo").assert_state(K::Thinking);
}

#[test]
fn the_owner_talking_cancels_an_initiative_and_its_text_is_not_merged() {
    let mut s = initiative_thinking("pensá algo");
    let initiative = s.last_turn;
    s.speech_start().assert_state(K::Listening);
    assert_fx!(s, Effect::CancelBrain { turn } if *turn == initiative);
    s.speech_end()
        .transcript("qué hora es en Tokio")
        .assert_state(K::Thinking);
    assert_eq!(s.brain_started_with(), Some("qué hora es en Tokio"));
    assert_fx!(s, Effect::StartBrain { context, .. } if context.source == TurnSource::Owner);
}

#[test]
fn a_failing_initiative_ends_quietly() {
    let mut s = initiative_thinking("pensá algo");
    s.tick(45_000).assert_state(K::Idle);
    assert_fx!(s, Effect::CancelBrain { .. });
    assert_no_fx!(s, Effect::Speak { .. });
    assert_fx!(
        s,
        Effect::Diagnostic {
            reason: Reason::InitiativeFailed,
            ..
        }
    );

    let mut s = initiative_thinking("pensá algo");
    let turn = s.last_turn;
    s.send(Event::BrainFailed {
        turn,
        reason: "x".into(),
    });
    s.assert_state(K::Idle);
    assert_no_fx!(s, Effect::Speak { .. });
}

#[test]
fn an_initiative_may_speak_like_any_turn() {
    let mut s = initiative_thinking("pensá algo");
    s.brain_text("Che, te dejé una idea.")
        .assert_state(K::Speaking);
    s.brain_done().playback_finished().assert_state(K::Idle);
}

// --- Suggestions ---

#[test]
fn the_initiative_proposes_with_suggest_and_nothing_runs() {
    let mut s = initiative_thinking("mirá las plantas");
    s.brain_tool_args(
        "c1",
        SUGGEST_TOOL,
        &suggest_args("Regar", "regá las plantas"),
    )
    .assert_state(K::Thinking);
    assert_no_fx!(s, Effect::RunTool { .. });
    assert_fx!(s, Effect::SuggestionAdded { suggestion } if suggestion.title == "Regar");
    assert!(matches!(s.tool_result(), Some(ToolOutcome::Success(_))));
    let open = s.dialogue.suggestions().open();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].ask, "regá las plantas");
}

#[test]
fn a_dismissed_suggestion_never_returns() {
    let mut s = initiative_thinking("mirá las plantas");
    s.brain_tool_args(
        "c1",
        SUGGEST_TOOL,
        &suggest_args("Regar las plantas", "regá"),
    );
    s.brain_done().assert_state(K::Idle);
    s.dismiss(0).assert_state(K::Idle);
    assert!(s.dialogue.suggestions().open().is_empty());

    for (i, title) in ["Regar las plantas", "regar las PLANTÁS!"]
        .iter()
        .enumerate()
    {
        s.initiative("otra vuelta").assert_state(K::Thinking);
        s.brain_tool_args(&format!("d{i}"), SUGGEST_TOOL, &suggest_args(title, "regá"));
        assert_no_fx!(s, Effect::SuggestionAdded { .. });
        assert!(
            matches!(s.tool_result(), Some(ToolOutcome::Failure(reason)) if reason.contains("descartó"))
        );
        s.brain_done();
    }
    assert!(s.dialogue.suggestions().open().is_empty());
}

#[test]
fn accepting_a_suggestion_becomes_an_owner_turn() {
    let mut s = initiative_thinking("mirá");
    s.brain_tool_args(
        "c1",
        SUGGEST_TOOL,
        &suggest_args("Fotos", "escribí el resumen"),
    );
    s.brain_done();

    s.accept(0).assert_state(K::Thinking);
    assert_eq!(s.brain_started_with(), Some("escribí el resumen"));
    assert_fx!(s, Effect::StartBrain { context, .. } if context.source == TurnSource::Owner);
    assert!(s.dialogue.suggestions().open().is_empty());
    // As the owner's request, it may write.
    s.brain_tool("c2", "escribir");
    assert_fx!(s, Effect::RunTool { call, .. } if call.name == "escribir");
}

#[test]
fn accepting_interrupts_what_was_playing_and_unknown_ids_are_reported() {
    let mut s = initiative_thinking("mirá");
    s.brain_tool_args("c1", SUGGEST_TOOL, &suggest_args("Mate", "poné la pava"));
    s.brain_text("Te dejé una idea.").assert_state(K::Speaking);
    let initiative = s.last_turn;
    s.accept(0).assert_state(K::Thinking);
    assert_fx!(s, Effect::StopPlayback);
    assert_fx!(s, Effect::CancelBrain { turn } if *turn == initiative);
    assert_eq!(s.brain_started_with(), Some("poné la pava"));

    s.send(Event::AcceptSuggestion {
        id: SuggestionId(1),
    });
    s.assert_state(K::Thinking);
    assert_fx!(
        s,
        Effect::Diagnostic {
            reason: Reason::UnknownSuggestion,
            ..
        }
    );
    s.send(Event::DismissSuggestion {
        id: SuggestionId(7),
    });
    assert_fx!(
        s,
        Effect::Diagnostic {
            reason: Reason::UnknownSuggestion,
            ..
        }
    );
}

#[test]
fn open_suggestions_are_capped() {
    let mut s = Scenario::with_config(Config {
        max_open_suggestions: 2,
        ..Config::default()
    });
    s.initiative("ideas").assert_state(K::Thinking);
    for (i, title) in ["uno", "dos", "tres"].iter().enumerate() {
        s.brain_tool_args(&format!("c{i}"), SUGGEST_TOOL, &suggest_args(title, "x"));
    }
    assert!(matches!(s.tool_result(), Some(ToolOutcome::Failure(_))));
    assert_eq!(s.dialogue.suggestions().open().len(), 2);
}

#[test]
fn suggest_is_built_in_only_for_initiative_turns() {
    let mut s = thinking("hola");
    s.brain_tool_args("c1", SUGGEST_TOOL, &suggest_args("x", "y"));
    assert_no_fx!(s, Effect::SuggestionAdded { .. });
    assert!(
        matches!(s.tool_result(), Some(ToolOutcome::Failure(reason)) if reason.contains("unknown tool"))
    );
}

// --- Tool arguments ---

#[test]
fn arguments_that_are_not_a_json_object_fail_for_the_brain() {
    let mut s = Scenario::new();
    s.dialogue.registry_mut().register(FakeTool::plain("luces"));
    s.dialogue
        .registry_mut()
        .register(FakeTool::confirmed("borrar"));
    s.say("prendé la luz");
    for (id, args) in [("c1", "{\"on\": tru}"), ("c2", "[1]"), ("c3", "")] {
        s.brain_tool_args(id, "luces", args)
            .assert_state(K::Thinking);
        assert_no_fx!(s, Effect::RunTool { .. });
        assert!(
            matches!(s.tool_result(), Some(ToolOutcome::Failure(reason)) if reason.starts_with("invalid arguments")),
            "{args}: {:?}",
            s.fx
        );
        assert_fx!(
            s,
            Effect::Diagnostic {
                reason: Reason::InvalidToolArgs,
                ..
            }
        );
    }
    // Checked before asking: the owner is not asked about a call that can't run.
    s.brain_tool_args("c4", "borrar", "{oops")
        .assert_state(K::Thinking);
    assert_no_fx!(s, Effect::Speak { .. });

    s.brain_tool_args("c5", "luces", r#"{"on": true}"#);
    assert_fx!(s, Effect::RunTool { .. });
}

#[test]
fn invalid_suggest_arguments_fail_for_the_initiative() {
    let mut s = initiative_thinking("ideas");
    s.brain_tool_args("c1", SUGGEST_TOOL, "{\"title\": ");
    assert!(
        matches!(s.tool_result(), Some(ToolOutcome::Failure(reason)) if reason.starts_with("invalid arguments"))
    );
    s.brain_tool_args("c2", SUGGEST_TOOL, r#"{"title": "x"}"#);
    assert!(matches!(s.tool_result(), Some(ToolOutcome::Failure(_))));
    assert!(s.dialogue.suggestions().open().is_empty());
}

// --- Hard cap per turn ---

#[test]
fn chaining_tools_cannot_extend_a_turn_past_the_hard_cap() {
    let mut s = Scenario::with_config(Config {
        max_turn_ms: 100_000,
        ..Config::default()
    });
    s.dialogue.registry_mut().register(FakeTool::plain("uno"));
    s.say("hola");
    let turn = s.last_turn;
    // Every 20 s another tool: the watchdog alone would never fire.
    for i in 0..4 {
        s.tick(i * 20_000).brain_tool(&format!("c{i}"), "uno");
        s.tool_finished(&format!("c{i}")).assert_state(K::Thinking);
    }
    s.tick(99_999).assert_state(K::Thinking);
    s.tick(100_000).assert_state(K::Speaking);
    assert_fx!(s, Effect::CancelBrain { turn: t } if *t == turn);
    assert_fx!(
        s,
        Effect::Diagnostic {
            reason: Reason::TurnLimit,
            ..
        }
    );
    assert_fx!(
        s,
        Effect::Speak {
            kind: SpeechKind::Failure(FailureKind::Watchdog),
            ..
        }
    );
}

#[test]
fn the_hard_cap_also_cuts_a_brain_that_keeps_talking() {
    let mut s = Scenario::with_config(Config {
        max_turn_ms: 50_000,
        ..Config::default()
    });
    s.say("contame");
    for i in 0..5 {
        s.tick(i * 10_000).brain_text("Y otra cosa.");
    }
    s.assert_state(K::Speaking);
    s.tick(50_000).assert_state(K::Speaking);
    assert_eq!(s.fx[1], Effect::StopPlayback);
    assert_fx!(s, Effect::CancelBrain { .. });
    assert_fx!(
        s,
        Effect::Speak {
            kind: SpeechKind::Failure(FailureKind::Watchdog),
            ..
        }
    );
    s.playback_finished().assert_state(K::Idle);
}

#[test]
fn the_hard_cap_defaults_to_three_minutes() {
    assert_eq!(Config::default().max_turn_ms, 180_000);
}

// --- Floor queue cap ---

#[test]
fn a_full_floor_queue_drops_its_oldest_item() {
    let mut s = Scenario::with_config(Config {
        floor_queue_cap: 2,
        ..Config::default()
    });
    s.say("hola").brain_text("Hola.");
    s.floor(Priority::High, "uno").floor(Priority::High, "dos");
    assert_no_fx!(s, Effect::Diagnostic { .. });
    s.floor(Priority::High, "tres");
    assert_fx!(
        s,
        Effect::Diagnostic {
            reason: Reason::FloorQueueFull,
            ..
        }
    );
    assert_fx!(s, Effect::FloorQueued { .. });
    assert_eq!(s.dialogue.floor_queue_len(), 2);

    s.brain_done().playback_finished();
    assert_eq!(s.spoken(), ["dos"]);
    s.playback_finished();
    assert_eq!(s.spoken(), ["tres"]);
}

// --- Unclear transcripts ---

#[test]
fn an_unclear_utterance_asks_to_repeat_without_the_brain() {
    let mut s = Scenario::new();
    s.speech_start().speech_end().unclear();
    assert_no_fx!(s, Effect::StartBrain { .. });
    assert_fx!(
        s,
        Effect::Speak {
            kind: SpeechKind::Notice,
            ..
        }
    );
    assert_eq!(s.spoken(), ["No te entendí, ¿me lo repetís?"]);
    s.assert_state(K::Speaking);
    s.playback_finished().assert_state(K::Idle);
    // The owner repeats it and it goes as usual.
    s.say("hola").assert_state(K::Thinking);
    assert_eq!(s.brain_started_with(), Some("hola"));
}

#[test]
fn an_unclear_segment_next_to_clear_words_is_just_dropped() {
    let mut s = Scenario::new();
    s.speech_start().speech_end().speech_start().speech_end();
    s.unclear().assert_state(K::Listening);
    s.transcript("poné música").assert_state(K::Thinking);
    assert_eq!(s.brain_started_with(), Some("poné música"));
    assert!(s.spoken().is_empty());
}

#[test]
fn an_unclear_answer_to_a_confirmation_gets_the_reask() {
    let mut s = awaiting_confirmation();
    s.speech_start().speech_end().unclear();
    s.assert_state(K::AwaitingConfirmation);
    assert_eq!(s.spoken(), ["No te entendí. ¿Sí o no?"]);
    assert_no_fx!(s, Effect::RunTool { .. });
}

#[test]
fn an_unclear_transcript_for_no_open_segment_is_stale() {
    let mut s = Scenario::new();
    s.send(Event::UnclearTranscript {
        segment: SegmentId(42),
        text: "vous".into(),
    });
    assert_fx!(
        s,
        Effect::Diagnostic {
            reason: Reason::StaleSegment,
            ..
        }
    );
    s.assert_state(K::Idle);
}

// The instinct gate, inside the dialogue.

fn instinct(id: &str, key: &str, context: &str) -> Event {
    Event::Instinct {
        candidate: wawi_core::InstinctCandidate {
            id: id.into(),
            key: key.into(),
            say: "Contale al dueño.".into(),
            context: context.into(),
            priority: Priority::High,
            cooldown_ms: None,
            local_minute: 9 * 60,
            urgent: false,
            expires_in_ms: None,
        },
    }
}

#[test]
fn an_instinct_becomes_an_initiative_turn_that_may_only_read() {
    let mut s = Scenario::new();
    with_every_level(&mut s);
    s.send(instinct("w1", "linear/hoy", "Lunes: A y B"))
        .assert_state(K::Thinking);
    assert_fx!(s, Effect::InstinctDecided { id, outcome: wawi_core::InstinctOutcome::Started(_) } if id == "w1");
    let (text, tools) =
        s.fx.iter()
            .find_map(|effect| match effect {
                Effect::StartBrain { text, context, .. } => {
                    assert_eq!(context.source, TurnSource::Initiative);
                    Some((
                        text.clone(),
                        context
                            .tools
                            .iter()
                            .map(|t| t.name.clone())
                            .collect::<Vec<_>>(),
                    ))
                }
                _ => None,
            })
            .unwrap();
    assert!(text.contains("Lunes: A y B") && text.ends_with("Contale al dueño."));
    assert_eq!(tools, ["leer", SUGGEST_TOOL]);
    s.brain_tool("c1", "escribir");
    assert_no_fx!(s, Effect::RunTool { .. });
    assert_eq!(
        s.tool_result(),
        Some(&ToolOutcome::Failure(INITIATIVE_READ_ONLY.into()))
    );
}

#[test]
fn an_instinct_waits_while_the_owner_talks_and_behind_the_floor() {
    let mut s = Scenario::new();
    s.speech_start().assert_state(K::Listening);
    s.send(instinct("w1", "a/b", "x"));
    assert_no_fx!(s, Effect::StartBrain { .. });
    assert_eq!(s.dialogue.instincts_waiting(), 1);
    s.speech_end().transcript("hola").assert_state(K::Thinking);
    // A reminder queued meanwhile goes first.
    s.floor(Priority::High, "Acordate del mate.");
    s.brain_text("Hola.").playback_finished().brain_done();
    assert_eq!(s.spoken(), ["Acordate del mate."]);
    assert_no_fx!(s, Effect::StartBrain { .. });
    s.playback_finished();
    assert!(s.brain_started_with().unwrap().contains("[Instinto: a/b"));
    assert_eq!(s.dialogue.instincts_waiting(), 0);
}

#[test]
fn the_gate_reports_what_it_drops() {
    let mut s = Scenario::new();
    s.send(instinct("w1", "a/b", "x")).assert_state(K::Thinking);
    s.brain_done().assert_state(K::Idle);
    s.send(instinct("w2", "a/b", "y"));
    assert_fx!(s, Effect::InstinctDecided { id, outcome: wawi_core::InstinctOutcome::Postponed { reason: Reason::InstinctCooldown, .. } } if id == "w2");
    let mut quiet = instinct("w3", "c/d", "z");
    if let Event::Instinct { candidate } = &mut quiet {
        candidate.local_minute = 23 * 60 + 30;
    }
    s.send(quiet);
    assert_fx!(
        s,
        Effect::InstinctDecided {
            outcome: wawi_core::InstinctOutcome::Postponed {
                reason: Reason::InstinctQuiet,
                ..
            },
            ..
        }
    );
}

// A curiosity turn (Wawi looks at the screen on her own) must never make the
// owner wait: whatever stage it is in, the owner's voice preempts it at once.

fn curiosity(stage: &str) -> Scenario {
    let mut s = Scenario::new();
    with_every_level(&mut s);
    let mut look = instinct("c1", "curiosidad/mirar", "Buen momento para mirar (15:20).");
    if let Event::Instinct { candidate } = &mut look {
        candidate.priority = Priority::Low;
    }
    s.send(look).assert_state(K::Thinking);
    match stage {
        "pensando" => {}
        "mirando" => {
            // The read tool that takes the screenshot is running.
            s.brain_tool("t1", "leer").assert_state(K::Thinking);
            assert_fx!(s, Effect::RunTool { call, .. } if call.name == "leer");
        }
        "hablando" => {
            s.brain_text("¡Qué lindo ese gráfico!")
                .assert_state(K::Speaking);
        }
        other => panic!("{other}"),
    }
    s
}

#[test]
fn the_owner_preempts_a_curiosity_turn_at_once_at_every_stage() {
    for stage in ["pensando", "mirando", "hablando"] {
        let mut s = curiosity(stage);
        let look = s.last_turn;
        s.speech_start().assert_state(K::Listening);
        assert_fx!(s, Effect::CancelBrain { turn } if *turn == look);
        if stage == "mirando" {
            assert_fx!(s, Effect::CancelTool { call_id, .. } if call_id == "t1");
        }
        if stage == "hablando" {
            assert_fx!(s, Effect::StopPlayback);
        }
        // Her late output belongs to a dead turn and is dropped.
        s.send(Event::BrainText {
            turn: look,
            text: "Y además...".into(),
        });
        assert_no_fx!(s, Effect::Speak { .. });
        s.speech_end()
            .transcript("¿qué hora es en Tokio?")
            .assert_state(K::Thinking);
        assert_fx!(s, Effect::StartBrain { context, text, .. }
            if context.source == TurnSource::Owner && text == "¿qué hora es en Tokio?");
    }
}

#[test]
fn a_curiosity_look_is_now_or_never() {
    // Low priority: while the owner talks it is dropped, never queued.
    let mut s = Scenario::new();
    s.speech_start().assert_state(K::Listening);
    let mut look = instinct("c1", "curiosidad/mirar", "Buen momento para mirar (15:20).");
    if let Event::Instinct { candidate } = &mut look {
        candidate.priority = Priority::Low;
    }
    s.send(look);
    assert_no_fx!(s, Effect::StartBrain { .. });
    assert_eq!(s.dialogue.instincts_waiting(), 0);
}

// The usage limit ("se quedó sin cupo").

const MIN: u64 = 60_000;

fn limited<'a>(s: &'a mut Scenario, resets_in_ms: Option<u64>, until: &str) -> &'a mut Scenario {
    let turn = s.last_turn;
    s.send(Event::UsageLimited {
        turn: Some(turn),
        resets_in_ms,
        until: until.into(),
    })
}

#[test]
fn an_owner_turn_refused_by_the_limit_hears_it_once_locally() {
    let mut s = thinking("¿qué tengo hoy?");
    limited(&mut s, Some(90 * MIN), "las 18:00").assert_state(K::Speaking);
    assert_eq!(
        s.spoken(),
        ["Uy, me quedé sin cupo hasta las 18:00. Te aviso cuando vuelva."]
    );
    assert_fx!(s, Effect::UsageLimit { limited: true, retry_in_ms, told_owner: false }
        if *retry_in_ms == 91 * MIN);
    assert_no_fx!(
        s,
        Effect::Speak {
            kind: SpeechKind::Failure(_),
            ..
        }
    );
    s.playback_finished().assert_state(K::Idle);
    // The next owner turn does not reach the brain: a short line, at once.
    s.tick(MIN).say("¿y el clima?").assert_state(K::Speaking);
    assert_no_fx!(s, Effect::StartBrain { .. });
    assert_eq!(s.spoken(), ["Sigo sin cupo hasta las 18:00, aguantame."]);
    s.playback_finished().assert_state(K::Idle);
    // Routes still answer.
    s.dialogue
        .router_mut()
        .add(Box::new(TimeRoute::new(FixedClock(16, 20))));
    s.say("qué hora es");
    assert!(s.spoken()[0].contains("16:20"), "{:?}", s.spoken());
}

#[test]
fn background_work_waits_out_the_limit_and_she_comes_back_through_the_gate() {
    let mut s = thinking("hola");
    limited(&mut s, Some(30 * MIN), "las 18:00");
    s.playback_finished().assert_state(K::Idle);
    // Initiative is dropped; an instinct is postponed until the reset.
    s.initiative("mirá algo");
    assert_no_fx!(s, Effect::StartBrain { .. });
    assert_fx!(
        s,
        Effect::Diagnostic {
            reason: Reason::UsageLimited,
            ..
        }
    );
    s.tick(10 * MIN)
        .send(instinct("cola:x", "cola/x/y", "Aprobaron el PR"));
    assert_no_fx!(s, Effect::StartBrain { .. });
    assert_fx!(s, Effect::InstinctDecided {
        id,
        outcome: wawi_core::InstinctOutcome::Postponed { reason: Reason::UsageLimited, retry_in_ms },
    } if id == "cola:x" && *retry_in_ms == 21 * MIN);
    // A reminder still speaks: it needs no brain.
    s.floor(Priority::High, "Acordate del mate.");
    assert_eq!(s.spoken(), ["Acordate del mate."]);
    s.playback_finished();
    // At the reset (plus the margin) the limit ends, and since the owner
    // heard of it, the host is asked to say she is back.
    s.tick(30 * MIN);
    assert_no_fx!(s, Effect::UsageLimit { .. });
    s.tick(31 * MIN);
    assert_fx!(
        s,
        Effect::UsageLimit {
            limited: false,
            told_owner: true,
            ..
        }
    );
    assert!(!s.dialogue.usage_limited());
    s.say("¿y ahora?").assert_state(K::Thinking);
}

#[test]
fn a_limit_found_by_initiative_stays_silent_and_postpones_its_instinct() {
    let mut s = Scenario::new();
    s.send(instinct("cola:x", "cola/x/y", "Aprobaron el PR"))
        .assert_state(K::Thinking);
    limited(&mut s, Some(60 * MIN), "las 18:00").assert_state(K::Idle);
    assert_no_fx!(s, Effect::Speak { .. });
    assert_fx!(s, Effect::InstinctDecided {
        id,
        outcome: wawi_core::InstinctOutcome::Postponed { reason: Reason::UsageLimited, .. },
    } if id == "cola:x");
    // Nobody was told, so its end says nothing; the owner hears it whole
    // the first time they ask.
    s.say("hola").assert_state(K::Speaking);
    assert_eq!(
        s.spoken(),
        ["Uy, me quedé sin cupo hasta las 18:00. Te aviso cuando vuelva."]
    );
}

#[test]
fn with_no_known_reset_she_backs_off_and_the_owner_may_try_now_and_then() {
    let mut s = thinking("hola");
    limited(&mut s, None, "").assert_state(K::Speaking);
    assert_eq!(
        s.spoken(),
        ["Uy, me quedé sin cupo por un rato. Te aviso cuando vuelva."]
    );
    assert_fx!(s, Effect::UsageLimit { limited: true, retry_in_ms, .. } if *retry_in_ms == 15 * MIN);
    s.playback_finished();
    s.tick(MIN).say("¿ya?").assert_state(K::Speaking);
    assert_eq!(s.spoken(), ["Sigo sin cupo, aguantame un rato."]);
    s.playback_finished();
    // Five minutes after the hit an owner turn gets a real try.
    s.tick(5 * MIN).say("¿y ahora?").assert_state(K::Thinking);
    limited(&mut s, None, "").assert_state(K::Speaking);
    assert_eq!(s.spoken(), ["Sigo sin cupo, aguantame un rato."]);
    assert_fx!(s, Effect::UsageLimit { limited: true, retry_in_ms, .. } if *retry_in_ms == 30 * MIN);
    s.playback_finished();
    s.tick(35 * MIN);
    assert_fx!(
        s,
        Effect::UsageLimit {
            limited: false,
            told_owner: true,
            ..
        }
    );
}

#[test]
fn a_stale_limit_report_still_starts_the_limit_but_touches_no_turn() {
    let mut s = thinking("hola");
    s.send(Event::UsageLimited {
        turn: Some(TurnId(99)),
        resets_in_ms: Some(MIN),
        until: "las 9:00".into(),
    })
    .assert_state(K::Thinking);
    assert!(s.dialogue.usage_limited());
    assert_no_fx!(s, Effect::Speak { .. });
}
