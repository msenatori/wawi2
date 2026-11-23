//! The long-running engine: one AVAudioEngine with voice processing on, the
//! mic behind the push-to-talk gate, and a player node for TTS audio.
//!
//! Capture and playback share the engine on purpose: the echo canceller
//! removes whatever this engine plays, so Wawi can be interrupted while she
//! speaks. Findings from the `aec-test` spike that shape this:
//! - the engine fails to start with -10875 unless mixer -> output uses the
//!   voice-processing input format;
//! - voice processing delivers 9 identical channels: channel 0 is the mic;
//! - a mono source reaches only channel 0 of the 9-channel mixer, so play
//!   stereo.
//!
//! Threads: the input tap (an AVFoundation queue) and the hotkey only post
//! messages; one worker thread owns the gate and calls the segment callback
//! and the sink, always in the order "hand audio, then `SpeechEnded`".
//!
//! Volume: nothing here touches the system or device volume. Voice processing
//! itself ducks other apps' audio while it runs; that is set to the minimum
//! level the API offers.

use std::ptr::NonNull;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObjectProtocol};
use objc2::{AnyThread, sel};
use objc2_avf_audio::{
    AVAudioEngine, AVAudioFormat, AVAudioInputNode, AVAudioPCMBuffer, AVAudioPlayerNode,
    AVAudioPlayerNodeCompletionCallbackType, AVAudioTime,
    AVAudioVoiceProcessingOtherAudioDuckingConfiguration,
    AVAudioVoiceProcessingOtherAudioDuckingLevel,
};
use wawi_core::{Event, EventSink};

use crate::dsp::first_channel;
use crate::gate::{GateConfig, GateEvent, PttGate};
use crate::permissions::ensure_microphone;
use crate::playback::{Done, Playback, Player};

const TAP_FRAMES: u32 = 1024;
const WORKER_TICK: Duration = Duration::from_millis(50);

/// `AudioOut` on the live engine.
pub type MacAudioOut = Playback<AvPlayer>;

#[derive(Debug)]
pub enum AudioError {
    /// The message says what to enable, in Spanish.
    Microphone(String),
    Engine(String),
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AudioError::Microphone(message) => f.write_str(message),
            AudioError::Engine(reason) => write!(f, "no pude arrancar el audio: {reason}"),
        }
    }
}

impl std::error::Error for AudioError {}

enum Msg {
    Samples(Vec<f32>),
    Key { down: bool, at: Instant },
    Shutdown,
}

/// Feeds the push-to-talk key into the gate. Cheap to clone and `Send`.
#[derive(Clone)]
pub struct PttHandle(Sender<Msg>);

impl PttHandle {
    /// Key down (`true`) or up (`false`). Repeats are ignored.
    pub fn set_held(&self, down: bool) {
        let _ = self.0.send(Msg::Key {
            down,
            at: Instant::now(),
        });
    }
}

/// The engine objects. AVFoundation classes are not marked thread-safe in
/// Rust; every call on them goes through `Playback`'s lock or happens at
/// setup and teardown, with the engine owned by one `Arc`.
struct Nodes {
    engine: Retained<AVAudioEngine>,
    input: Retained<AVAudioInputNode>,
    player: Retained<AVAudioPlayerNode>,
    /// Stereo, at the engine rate.
    play_format: Retained<AVAudioFormat>,
}

/// The player node, for [`Playback`].
pub struct AvPlayer {
    nodes: Nodes,
    rate: f64,
    play_lock: Mutex<()>,
}

// SAFETY: see `Nodes`; scheduling and stopping are serialized by `play_lock`
// (and by Playback's own lock), and AVAudioPlayerNode/AVAudioEngine accept
// calls from any thread when not made concurrently.
unsafe impl Send for AvPlayer {}
// SAFETY: as above.
unsafe impl Sync for AvPlayer {}

impl AvPlayer {
    pub fn engine_running(&self) -> bool {
        // SAFETY: plain property read.
        unsafe { self.nodes.engine.isRunning() }
    }

    fn ensure_running(&self) -> Result<(), String> {
        // SAFETY: restarting a prepared engine; a route change can stop it.
        unsafe {
            if !self.nodes.engine.isRunning() {
                self.nodes.engine.prepare();
                self.nodes
                    .engine
                    .startAndReturnError()
                    .map_err(|e| format!("rearrancar el motor: {e}"))?;
            }
        }
        Ok(())
    }
}

impl Player for AvPlayer {
    fn sample_rate(&self) -> f64 {
        self.rate
    }

    fn schedule(&self, samples: &[f32], done: Done) -> Result<(), String> {
        let _lock = self.play_lock.lock().unwrap_or_else(|e| e.into_inner());
        let frames = u32::try_from(samples.len()).map_err(|_| "audio demasiado largo")?;
        self.ensure_running()?;
        // SAFETY: the buffer is created with the player's connection format and
        // filled within its capacity; both channels get the same mono audio.
        // The completion block is copied by AVFoundation.
        unsafe {
            let buffer = AVAudioPCMBuffer::initWithPCMFormat_frameCapacity(
                AVAudioPCMBuffer::alloc(),
                &self.nodes.play_format,
                frames,
            )
            .ok_or("no se pudo crear el buffer")?;
            buffer.setFrameLength(frames);
            let channels = buffer.floatChannelData();
            if channels.is_null() {
                return Err("buffer sin datos".into());
            }
            for i in 0..2 {
                (*channels.add(i))
                    .as_ptr()
                    .copy_from_nonoverlapping(samples.as_ptr(), samples.len());
            }
            let slot = Mutex::new(Some(done));
            let completion = RcBlock::new(move |_: AVAudioPlayerNodeCompletionCallbackType| {
                let done = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
                if let Some(done) = done {
                    done();
                }
            });
            self.nodes
                .player
                .scheduleBuffer_completionCallbackType_completionHandler(
                    &buffer,
                    // Fires when the audio has actually left the speakers.
                    AVAudioPlayerNodeCompletionCallbackType::DataPlayedBack,
                    RcBlock::as_ptr(&completion),
                );
            if !self.nodes.player.isPlaying() {
                self.nodes.player.play();
            }
        }
        Ok(())
    }

    fn stop(&self) {
        let _lock = self.play_lock.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: stopping a player node is immediate and drops its queue;
        // pending completions then run, and find a newer generation.
        unsafe { self.nodes.player.stop() };
    }
}

impl Drop for AvPlayer {
    /// Leaves the system as found: tap removed, engine stopped, voice
    /// processing released.
    fn drop(&mut self) {
        // SAFETY: valid objects; these calls are legal on a stopped engine.
        unsafe {
            self.nodes.player.stop();
            self.nodes.engine.stop();
            self.nodes.input.removeTapOnBus(0);
            let _ = self.nodes.input.setVoiceProcessingEnabled_error(false);
        }
    }
}

/// Mic, push-to-talk and playback, live until dropped.
pub struct LiveAudio {
    player: Arc<AvPlayer>,
    output: MacAudioOut,
    messages: Sender<Msg>,
    worker: Option<JoinHandle<()>>,
    input_rate: u32,
}

impl LiveAudio {
    /// Starts the engine with echo cancellation on.
    ///
    /// The gate sends `SpeechStarted` to `sink` on key down. After key up and
    /// the release tail it calls `on_segment(samples, sample_rate)` with the
    /// mono mic audio, starting with the pre-roll from before the press (empty
    /// for a tap under 250 ms), and then sends `SpeechEnded`. Wire
    /// `on_segment` to `ParakeetStt::hand_audio`. Both run on the gate's
    /// worker thread. Playback reports `PlaybackFinished` to the same sink.
    ///
    /// Checks the mic permission first, showing the system prompt if nobody
    /// decided yet.
    pub fn start(
        sink: Arc<dyn EventSink + Send + Sync>,
        on_segment: impl FnMut(Vec<f32>, u32) + Send + 'static,
        gate: GateConfig,
    ) -> Result<Self, AudioError> {
        ensure_microphone().map_err(AudioError::Microphone)?;
        let (messages, inbox) = mpsc::channel::<Msg>();
        let (nodes, rate) = build_engine(messages.clone()).map_err(AudioError::Engine)?;
        let input_rate = rate.round() as u32;
        let player = Arc::new(AvPlayer {
            nodes,
            rate,
            play_lock: Mutex::new(()),
        });
        let output = Playback::new(player.clone(), sink.clone());
        let gate = PttGate::new(input_rate, gate);
        let worker = thread::Builder::new()
            .name("wawi-ptt".into())
            .spawn(move || run_gate(gate, inbox, sink, on_segment))
            .map_err(|e| AudioError::Engine(e.to_string()))?;
        Ok(Self {
            player,
            output,
            messages,
            worker: Some(worker),
            input_rate,
        })
    }

    /// The `AudioOut` for the core. Clones share the same queue.
    pub fn output(&self) -> MacAudioOut {
        self.output.clone()
    }

    /// Hand this to the hotkey.
    pub fn ptt(&self) -> PttHandle {
        PttHandle(self.messages.clone())
    }

    /// Rate of the audio `on_segment` receives.
    pub fn input_rate(&self) -> u32 {
        self.input_rate
    }

    pub fn is_running(&self) -> bool {
        self.player.engine_running()
    }
}

impl Drop for LiveAudio {
    fn drop(&mut self) {
        let _ = self.messages.send(Msg::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        // The engine stops when the last `AvPlayer` reference goes, which
        // includes any `MacAudioOut` clones still held.
    }
}

fn run_gate(
    mut gate: PttGate,
    inbox: Receiver<Msg>,
    sink: Arc<dyn EventSink + Send + Sync>,
    mut on_segment: impl FnMut(Vec<f32>, u32),
) {
    loop {
        let mut events = match inbox.recv_timeout(WORKER_TICK) {
            // Every buffer goes to the gate, key down or not: it keeps the
            // pre-roll from the stream.
            Ok(Msg::Samples(samples)) => gate.push(&samples, Instant::now()),
            Ok(Msg::Key { down: true, at }) => gate.press(at),
            Ok(Msg::Key { down: false, at }) => gate.release(at),
            Ok(Msg::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => Vec::new(),
        };
        events.extend(gate.tick(Instant::now()));
        for event in events {
            match event {
                GateEvent::Started => sink.send(Event::SpeechStarted),
                GateEvent::Ended(segment) => {
                    on_segment(segment.samples, segment.sample_rate);
                    sink.send(Event::SpeechEnded);
                }
            }
        }
    }
}

fn build_engine(messages: Sender<Msg>) -> Result<(Nodes, f64), String> {
    // SAFETY: AVAudioEngine setup in the documented order: voice processing is
    // toggled while stopped and before reading formats, since it changes them.
    // On any error the objects are released, and the engine never started.
    unsafe {
        let engine = AVAudioEngine::new();
        let input = engine.inputNode();
        input
            .setVoiceProcessingEnabled_error(true)
            .map_err(|e| format!("activar la cancelación de eco: {e}"))?;
        let release_vp = |input: &AVAudioInputNode| {
            let _ = input.setVoiceProcessingEnabled_error(false);
        };
        // macOS 14+: duck other apps as little as voice processing allows.
        if input.respondsToSelector(sel!(setVoiceProcessingOtherAudioDuckingConfiguration:)) {
            input.setVoiceProcessingOtherAudioDuckingConfiguration(
                AVAudioVoiceProcessingOtherAudioDuckingConfiguration {
                    enableAdvancedDucking: Bool::NO,
                    duckingLevel: AVAudioVoiceProcessingOtherAudioDuckingLevel::Min,
                },
            );
        }

        let tap_format = input.outputFormatForBus(0);
        let rate = tap_format.sampleRate();
        let channels = tap_format.channelCount() as usize;
        if rate <= 0.0 || channels == 0 {
            release_vp(&input);
            return Err(format!(
                "el micrófono no tiene un formato usable ({rate} Hz, {channels} canales)"
            ));
        }
        // Without this the engine fails to start with -10875 under voice processing.
        engine.connect_to_format(
            &engine.mainMixerNode(),
            &engine.outputNode(),
            Some(&tap_format),
        );

        let player = AVAudioPlayerNode::new();
        engine.attachNode(&player);
        let Some(play_format) = AVAudioFormat::initStandardFormatWithSampleRate_channels(
            AVAudioFormat::alloc(),
            rate,
            2,
        ) else {
            release_vp(&input);
            return Err("formato de reproducción inválido".into());
        };
        engine.connect_to_format(&player, &engine.mainMixerNode(), Some(&play_format));

        // The tap runs on an AVFoundation queue, not the realtime thread, so
        // allocating and sending here is fine.
        let tap = RcBlock::new(
            move |buffer: NonNull<AVAudioPCMBuffer>, _when: NonNull<AVAudioTime>| {
                let samples = mono_of(buffer.as_ref());
                if !samples.is_empty() {
                    let _ = messages.send(Msg::Samples(samples));
                }
            },
        );
        input.installTapOnBus_bufferSize_format_block(0, TAP_FRAMES, None, RcBlock::as_ptr(&tap));

        engine.prepare();
        if let Err(error) = engine.startAndReturnError() {
            input.removeTapOnBus(0);
            release_vp(&input);
            return Err(format!("arrancar el motor: {error}"));
        }
        Ok((
            Nodes {
                engine,
                input,
                player,
                play_format,
            },
            rate,
        ))
    }
}

/// Channel 0 of a tap buffer: with voice processing every channel carries the
/// same processed mic signal.
fn mono_of(buffer: &AVAudioPCMBuffer) -> Vec<f32> {
    // SAFETY: the buffer is valid for the duration of the tap; pointers and
    // lengths come from the buffer itself, and data is null-checked.
    unsafe {
        let frames = buffer.frameLength() as usize;
        let format = buffer.format();
        let count = format.channelCount() as usize;
        let data = buffer.floatChannelData();
        if data.is_null() || frames == 0 || count == 0 {
            return Vec::new();
        }
        let first = (*data).as_ptr();
        if format.isInterleaved() {
            first_channel(std::slice::from_raw_parts(first, frames * count), count)
        } else {
            std::slice::from_raw_parts(first, frames).to_vec()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct LogSink(Arc<Mutex<Vec<String>>>);

    impl EventSink for LogSink {
        fn send(&self, event: Event) {
            self.0.lock().unwrap().push(event.name().to_owned());
        }
    }

    fn run(messages: Vec<Msg>) -> Vec<String> {
        let log = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = mpsc::channel();
        for message in messages {
            tx.send(message).unwrap();
        }
        tx.send(Msg::Shutdown).unwrap();
        let segments = log.clone();
        run_gate(
            PttGate::new(1000, GateConfig::default()),
            rx,
            Arc::new(LogSink(log.clone())),
            move |samples, rate| {
                segments
                    .lock()
                    .unwrap()
                    .push(format!("audio {} @ {rate}", samples.len()));
            },
        );
        Arc::try_unwrap(log).unwrap().into_inner().unwrap()
    }

    #[test]
    fn audio_is_handed_over_before_speech_ended() {
        let t = Instant::now();
        let log = run(vec![
            Msg::Samples(vec![0.1; 1000]),
            Msg::Key { down: true, at: t },
            Msg::Samples(vec![0.1; 400]),
            Msg::Key {
                down: false,
                at: t + Duration::from_millis(400),
            },
            Msg::Samples(vec![0.1; 400]),
        ]);
        assert_eq!(
            log,
            ["speech_started", "audio 1100 @ 1000", "speech_ended"],
            "400 of pre-roll, 400 held and the 300 ms tail"
        );
    }

    #[test]
    fn a_tap_still_closes_its_segment_with_no_audio() {
        let t = Instant::now();
        let log = run(vec![
            Msg::Key { down: true, at: t },
            Msg::Samples(vec![0.1; 100]),
            Msg::Key {
                down: false,
                at: t + Duration::from_millis(100),
            },
        ]);
        assert_eq!(log, ["speech_started", "audio 0 @ 1000", "speech_ended"]);
    }
}
