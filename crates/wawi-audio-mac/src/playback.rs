//! `AudioOut` over any [`Player`]: the queue per `SpeechId`, the cut, and the
//! `PlaybackFinished` reports. Pure logic; the AVFoundation player lives in
//! `live`, and the tests use a fake.
//!
//! Every buffer is scheduled with a completion that fires when the player has
//! actually played it out (or was stopped). Completions arrive in schedule
//! order. When the last buffer of a speech completes, `PlaybackFinished {
//! up_to: speech }` goes to the sink.
//!
//! A speech may arrive in several parts (the TTS sends one per sentence).
//! [`Playback::play_part`] says which part is the last one: a speech whose
//! parts have all played but whose last part has not arrived yet is not
//! finished, so a TTS that falls behind between two sentences never ends a
//! speech early. `AudioOut::play` is a whole speech in one call. `stop()` bumps a generation first, so the
//! completions the cut itself triggers report nothing: after a cut the core has
//! already moved on, and a late report would only be a stale diagnostic.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use wawi_core::{AudioOut, Event, EventSink, SpeechId};

use crate::dsp::resample;

/// The rate `AudioOut::play` expects: what the TTS produces (mono f32).
pub const TTS_SAMPLE_RATE: u32 = 24_000;

pub type Done = Box<dyn FnOnce() + Send>;

/// The device side of playback.
pub trait Player: Send + Sync + 'static {
    /// Rate the scheduled samples must be at.
    fn sample_rate(&self) -> f64;
    /// Queues mono samples after whatever is queued, and makes sure they play.
    /// On `Ok`, `done` runs exactly once, on any thread, when the samples have
    /// played out or the player was stopped. On `Err` it never runs.
    fn schedule(&self, samples: &[f32], done: Done) -> Result<(), String>;
    /// Silences at once and drops everything queued. It may run pending
    /// `done`s, on this thread or another.
    fn stop(&self);
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    speech: SpeechId,
    /// False for something with nothing to play (empty audio, or a buffer the
    /// player refused): it finishes as soon as everything before it has.
    scheduled: bool,
    /// The speech's last part: once it has played, the speech is finished.
    end: bool,
}

#[derive(Default)]
struct Queue {
    generation: u64,
    entries: VecDeque<Entry>,
}

impl Queue {
    /// Drops the unscheduled entries at the front and says which speech, if
    /// any, has now fully finished. `completed` is the entry just played.
    fn settle(&mut self, completed: Option<Entry>) -> Option<SpeechId> {
        let mut finished = completed.filter(|e| e.end).map(|e| e.speech);
        while let Some(entry) = self.entries.front().copied() {
            if entry.scheduled {
                break;
            }
            self.entries.pop_front();
            if entry.end {
                finished = Some(entry.speech);
            }
        }
        let speech = finished?;
        // More buffers of the same speech still to play: not finished yet.
        match self.entries.front() {
            Some(next) if next.scheduled && next.speech == speech => None,
            _ => Some(speech),
        }
    }
}

struct Shared {
    sink: Arc<dyn EventSink + Send + Sync>,
    queue: Mutex<Queue>,
    /// Serializes `play` and `stop`, including the player calls, so a buffer is
    /// never scheduled between a cut's bookkeeping and the cut itself.
    /// Completions never take it.
    ops: Mutex<()>,
}

impl Shared {
    fn queue(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn report(&self, speech: Option<SpeechId>) {
        if let Some(up_to) = speech {
            self.sink.send(Event::PlaybackFinished { up_to });
        }
    }

    fn completed(&self, generation: u64) {
        let finished = {
            let mut queue = self.queue();
            if queue.generation != generation {
                return;
            }
            let Some(entry) = queue.entries.pop_front() else {
                return;
            };
            queue.settle(Some(entry))
        };
        self.report(finished);
    }
}

/// `AudioOut` for the core. Cheap to clone; clones share one queue.
pub struct Playback<P: Player> {
    player: Arc<P>,
    shared: Arc<Shared>,
    source_rate: f64,
}

impl<P: Player> Clone for Playback<P> {
    fn clone(&self) -> Self {
        Self {
            player: self.player.clone(),
            shared: self.shared.clone(),
            source_rate: self.source_rate,
        }
    }
}

impl<P: Player> Playback<P> {
    /// `play` takes audio at [`TTS_SAMPLE_RATE`].
    pub fn new(player: Arc<P>, sink: Arc<dyn EventSink + Send + Sync>) -> Self {
        Self::with_source_rate(player, sink, TTS_SAMPLE_RATE)
    }

    pub fn with_source_rate(
        player: Arc<P>,
        sink: Arc<dyn EventSink + Send + Sync>,
        source_rate: u32,
    ) -> Self {
        Self {
            player,
            shared: Arc::new(Shared {
                sink,
                queue: Mutex::new(Queue::default()),
                ops: Mutex::new(()),
            }),
            source_rate: f64::from(source_rate),
        }
    }

    /// Something is queued or playing.
    pub fn is_playing(&self) -> bool {
        !self.shared.queue().entries.is_empty()
    }

    pub fn player(&self) -> &P {
        &self.player
    }
}

impl<P: Player> Playback<P> {
    /// Queues one part of a speech. The speech finishes (`PlaybackFinished`)
    /// only after its part with `last` has played, however long the gap
    /// between parts. An empty `last` part just closes the speech.
    pub fn play_part(&mut self, speech: SpeechId, samples: &[f32], last: bool) {
        let samples = resample(samples, self.source_rate, self.player.sample_rate());
        let _ops = self.shared.ops.lock().unwrap_or_else(|e| e.into_inner());
        let generation = {
            let mut queue = self.shared.queue();
            queue.entries.push_back(Entry {
                speech,
                scheduled: !samples.is_empty(),
                end: last,
            });
            if samples.is_empty() {
                let finished = queue.settle(None);
                drop(queue);
                self.shared.report(finished);
                return;
            }
            queue.generation
        };
        let shared = self.shared.clone();
        let done: Done = Box::new(move || shared.completed(generation));
        if let Err(error) = self.player.schedule(&samples, done) {
            eprintln!(
                "wawi-audio: could not schedule speech {}: {error}",
                speech.0
            );
            // Finish it in order instead of leaving the core waiting.
            let finished = {
                let mut queue = self.shared.queue();
                if let Some(entry) = queue.entries.back_mut() {
                    entry.scheduled = false;
                }
                queue.settle(None)
            };
            self.shared.report(finished);
        }
    }

    /// Silences at once and drops everything queued; nothing queued before
    /// reports `PlaybackFinished`.
    pub fn cut(&mut self) {
        let _ops = self.shared.ops.lock().unwrap_or_else(|e| e.into_inner());
        {
            let mut queue = self.shared.queue();
            queue.generation += 1;
            queue.entries.clear();
        }
        // Outside the queue lock: stopping may run completions right here.
        self.player.stop();
    }
}

impl<P: Player> AudioOut for Playback<P> {
    /// A whole speech in one call. Calls for the same speech that queue
    /// back to back still finish together, with the last of them.
    fn play(&mut self, speech: SpeechId, samples: &[f32]) {
        self.play_part(speech, samples, true);
    }

    fn stop(&mut self) {
        self.cut();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{self, Receiver, Sender};

    struct FakePlayer {
        rate: f64,
        pending: Mutex<VecDeque<(usize, Done)>>,
        fail_next: Mutex<bool>,
        stops: Mutex<usize>,
    }

    impl FakePlayer {
        fn new(rate: f64) -> Arc<Self> {
            Arc::new(Self {
                rate,
                pending: Mutex::new(VecDeque::new()),
                fail_next: Mutex::new(false),
                stops: Mutex::new(0),
            })
        }

        /// The device finished the oldest buffer.
        fn finish_one(&self) -> usize {
            let (len, done) = self.pending.lock().unwrap().pop_front().expect("queued");
            done();
            len
        }

        fn queued(&self) -> usize {
            self.pending.lock().unwrap().len()
        }
    }

    impl Player for FakePlayer {
        fn sample_rate(&self) -> f64 {
            self.rate
        }

        fn schedule(&self, samples: &[f32], done: Done) -> Result<(), String> {
            if std::mem::take(&mut *self.fail_next.lock().unwrap()) {
                return Err("refused".into());
            }
            self.pending
                .lock()
                .unwrap()
                .push_back((samples.len(), done));
            Ok(())
        }

        /// Like AVAudioPlayerNode: stopping runs every pending completion,
        /// synchronously.
        fn stop(&self) {
            *self.stops.lock().unwrap() += 1;
            let drained: Vec<_> = self.pending.lock().unwrap().drain(..).collect();
            for (_, done) in drained {
                done();
            }
        }
    }

    struct ChannelSink(Mutex<Sender<Event>>);

    impl EventSink for ChannelSink {
        fn send(&self, event: Event) {
            let _ = self.0.lock().unwrap().send(event);
        }
    }

    fn setup(rate: f64) -> (Playback<FakePlayer>, Arc<FakePlayer>, Receiver<Event>) {
        let (tx, rx) = mpsc::channel();
        let player = FakePlayer::new(rate);
        let playback = Playback::new(player.clone(), Arc::new(ChannelSink(Mutex::new(tx))));
        (playback, player, rx)
    }

    fn finished(rx: &Receiver<Event>) -> Vec<u64> {
        rx.try_iter()
            .map(|event| match event {
                Event::PlaybackFinished { up_to } => up_to.0,
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    #[test]
    fn reports_each_speech_when_its_audio_has_played() {
        let (mut out, player, rx) = setup(48_000.0);
        out.play(SpeechId(1), &[0.1; 2400]);
        out.play(SpeechId(2), &[0.1; 1200]);
        assert!(finished(&rx).is_empty(), "nothing has played yet");
        assert!(out.is_playing());
        // Resampled from 24 kHz to the player's 48 kHz.
        assert_eq!(player.finish_one(), 4800);
        assert_eq!(finished(&rx), vec![1]);
        assert_eq!(player.finish_one(), 2400);
        assert_eq!(finished(&rx), vec![2]);
        assert!(!out.is_playing());
    }

    #[test]
    fn a_speech_in_several_chunks_finishes_with_its_last_chunk() {
        let (mut out, player, rx) = setup(24_000.0);
        out.play(SpeechId(1), &[0.1; 100]);
        out.play(SpeechId(1), &[0.1; 100]);
        out.play(SpeechId(2), &[0.1; 100]);
        player.finish_one();
        assert!(finished(&rx).is_empty());
        player.finish_one();
        assert_eq!(finished(&rx), vec![1]);
        player.finish_one();
        assert_eq!(finished(&rx), vec![2]);
    }

    #[test]
    fn a_speech_in_parts_waits_for_its_last_part_even_after_draining() {
        let (mut out, player, rx) = setup(24_000.0);
        out.play_part(SpeechId(1), &[0.1; 100], false);
        player.finish_one();
        assert!(
            finished(&rx).is_empty(),
            "the first sentence played, but the TTS is still making the next"
        );
        out.play_part(SpeechId(1), &[0.1; 100], false);
        player.finish_one();
        assert!(finished(&rx).is_empty());
        out.play_part(SpeechId(1), &[0.1; 100], true);
        player.finish_one();
        assert_eq!(finished(&rx), vec![1]);
    }

    #[test]
    fn an_empty_last_part_closes_the_speech_in_order() {
        let (mut out, player, rx) = setup(24_000.0);
        out.play_part(SpeechId(1), &[0.1; 100], false);
        out.play_part(SpeechId(1), &[], true);
        assert!(finished(&rx).is_empty(), "its audio is still playing");
        player.finish_one();
        assert_eq!(finished(&rx), vec![1]);
        // Already drained: the empty last part finishes it on arrival.
        out.play_part(SpeechId(2), &[0.1; 100], false);
        player.finish_one();
        assert!(finished(&rx).is_empty());
        out.play_part(SpeechId(2), &[], true);
        assert_eq!(finished(&rx), vec![2]);
    }

    #[test]
    fn a_cut_drops_a_speech_that_was_still_missing_its_last_part() {
        let (mut out, player, rx) = setup(24_000.0);
        out.play_part(SpeechId(1), &[0.1; 100], false);
        out.stop();
        out.play_part(SpeechId(2), &[0.1; 100], true);
        player.finish_one();
        assert_eq!(finished(&rx), vec![2]);
    }

    #[test]
    fn stop_cuts_everything_and_reports_nothing() {
        let (mut out, player, rx) = setup(24_000.0);
        out.play(SpeechId(1), &[0.1; 100]);
        out.play(SpeechId(2), &[0.1; 100]);
        out.stop();
        assert_eq!(*player.stops.lock().unwrap(), 1);
        assert_eq!(player.queued(), 0);
        assert!(finished(&rx).is_empty());
        assert!(!out.is_playing());
        // Playback works again after a cut.
        out.play(SpeechId(3), &[0.1; 100]);
        player.finish_one();
        assert_eq!(finished(&rx), vec![3]);
    }

    #[test]
    fn a_completion_from_before_the_cut_is_ignored() {
        let (mut out, player, rx) = setup(24_000.0);
        out.play(SpeechId(1), &[0.1; 100]);
        // Grab the old completion as if the device thread were slow to run it.
        let (_, late) = player.pending.lock().unwrap().pop_front().unwrap();
        out.stop();
        out.play(SpeechId(2), &[0.1; 100]);
        late();
        assert!(finished(&rx).is_empty(), "the old buffer says nothing");
        player.finish_one();
        assert_eq!(finished(&rx), vec![2]);
    }

    #[test]
    fn empty_audio_finishes_in_order() {
        let (mut out, player, rx) = setup(24_000.0);
        out.play(SpeechId(1), &[]);
        assert_eq!(finished(&rx), vec![1], "nothing queued: right away");
        out.play(SpeechId(2), &[0.1; 100]);
        out.play(SpeechId(3), &[]);
        assert!(finished(&rx).is_empty(), "waits for speech 2");
        player.finish_one();
        assert_eq!(finished(&rx), vec![3], "3 covers 2");
    }

    #[test]
    fn a_refused_buffer_does_not_leave_the_core_waiting() {
        let (mut out, player, rx) = setup(24_000.0);
        out.play(SpeechId(1), &[0.1; 100]);
        *player.fail_next.lock().unwrap() = true;
        out.play(SpeechId(2), &[0.1; 100]);
        assert!(finished(&rx).is_empty());
        player.finish_one();
        assert_eq!(finished(&rx), vec![2]);
        *player.fail_next.lock().unwrap() = true;
        out.play(SpeechId(3), &[0.1; 100]);
        assert_eq!(finished(&rx), vec![3]);
    }

    #[test]
    fn completions_on_another_thread() {
        let (mut out, player, rx) = setup(24_000.0);
        for id in 1..=3 {
            out.play(SpeechId(id), &[0.1; 100]);
        }
        let device = player.clone();
        std::thread::spawn(move || {
            while device.queued() > 0 {
                device.finish_one();
            }
        })
        .join()
        .unwrap();
        assert_eq!(finished(&rx), vec![1, 2, 3]);
    }
}
