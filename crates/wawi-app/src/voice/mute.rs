//! A silent [`Player`] that takes as long as the audio would: `--mute`.
//!
//! Behind the same [`Playback`](wawi_audio_mac::Playback) queue as the real
//! engine, so `PlaybackFinished` comes at the same moments it would with
//! sound, and a cut works the same way. Nothing reaches the speakers.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use wawi_audio_mac::Player;
use wawi_audio_mac::playback::Done;

#[derive(Default)]
struct State {
    /// Buffers still to "play", each with its length in time.
    queue: VecDeque<(Duration, Done)>,
    /// When the front buffer started playing.
    front_since: Option<Instant>,
    shutdown: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub struct NullPlayer {
    rate: f64,
    shared: Arc<Shared>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl NullPlayer {
    pub fn new(rate: f64) -> Arc<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
        });
        let clock = shared.clone();
        let worker = thread::Builder::new()
            .name("wawi-mute".into())
            .spawn(move || run(&clock))
            .expect("spawn the silent player");
        Arc::new(Self {
            rate,
            shared,
            worker: Mutex::new(Some(worker)),
        })
    }
}

fn run(shared: &Shared) {
    let mut state = shared.lock();
    loop {
        if state.shutdown {
            return;
        }
        let Some(length) = state.queue.front().map(|(length, _)| *length) else {
            state = shared.wake.wait(state).unwrap_or_else(|e| e.into_inner());
            continue;
        };
        let now = Instant::now();
        let since = *state.front_since.get_or_insert(now);
        let ends = since + length;
        if now < ends {
            state = shared
                .wake
                .wait_timeout(state, ends - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
            continue;
        }
        let (_, done) = state.queue.pop_front().expect("front checked");
        // The next buffer starts right where this one ended.
        state.front_since = (!state.queue.is_empty()).then_some(ends);
        drop(state);
        done();
        state = shared.lock();
    }
}

impl Player for NullPlayer {
    fn sample_rate(&self) -> f64 {
        self.rate
    }

    fn schedule(&self, samples: &[f32], done: Done) -> Result<(), String> {
        let length = Duration::from_secs_f64(samples.len() as f64 / self.rate);
        self.shared.lock().queue.push_back((length, done));
        self.shared.wake.notify_all();
        Ok(())
    }

    fn stop(&self) {
        let dropped: Vec<_> = {
            let mut state = self.shared.lock();
            state.front_since = None;
            state.queue.drain(..).collect()
        };
        self.shared.wake.notify_all();
        // Like AVAudioPlayerNode: a cut completes what was queued.
        for (_, done) in dropped {
            done();
        }
    }
}

impl Drop for NullPlayer {
    fn drop(&mut self) {
        self.shared.lock().shutdown = true;
        self.shared.wake.notify_all();
        let worker = self.worker.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn buffers_finish_after_their_real_length_in_order() {
        let player = NullPlayer::new(1000.0);
        let (tx, rx) = mpsc::channel();
        let started = Instant::now();
        for id in 1..=2 {
            let tx = tx.clone();
            player
                .schedule(&[0.0; 60], Box::new(move || tx.send(id).unwrap()))
                .unwrap();
        }
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(1));
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(2));
        let took = started.elapsed();
        assert!(took >= Duration::from_millis(120), "{took:?}");
    }

    #[test]
    fn stop_completes_everything_at_once() {
        let player = NullPlayer::new(1000.0);
        let (tx, rx) = mpsc::channel();
        player
            .schedule(&[0.0; 10_000], Box::new(move || tx.send(()).unwrap()))
            .unwrap();
        let cut = Instant::now();
        player.stop();
        assert_eq!(rx.recv_timeout(Duration::from_millis(100)), Ok(()));
        assert!(cut.elapsed() < Duration::from_secs(1));
    }
}
