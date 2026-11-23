//! One capture-and-playback pass through a single AVAudioEngine. Playback and
//! capture share the engine because the canceller's reference signal is whatever
//! the engine itself plays.

use std::ops::Range;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::{AnyThread, runtime::ProtocolObject};
use objc2_avf_audio::{
    AVAudioEngine, AVAudioFormat, AVAudioInputNode, AVAudioMixerNode, AVAudioMixing,
    AVAudioPCMBuffer, AVAudioPlayerNode, AVAudioPlayerNodeCompletionCallbackType, AVAudioTime,
};

const SETTLE: Duration = Duration::from_millis(500);
const SILENCE: Duration = Duration::from_millis(1500);
const TAIL: Duration = Duration::from_millis(400);
const TALK_OVER_TAIL: Duration = Duration::from_secs(4);
/// Skips output latency at the start of the playback window. The end marker comes
/// from the "played back" callback, so it needs no margin.
const PLAYBACK_LEAD_IN: Duration = Duration::from_millis(250);
const NO_AUDIO_TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(5);
/// -80 dB: inaudible, but still lets the output tap show which channels carry signal.
const MUTE_GAIN: f32 = 1e-4;

#[derive(Clone, Copy, Debug)]
pub struct PhaseSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub voice_processing: bool,
    pub bypass: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub mute: bool,
    pub talk_over: bool,
    /// Stop before making any sound if the mic delivers exact zeros.
    pub abort_on_silent_mic: bool,
}

pub struct Source {
    pub samples: Vec<f32>,
    pub sample_rate: f64,
}

impl Source {
    pub fn seconds(&self) -> f64 {
        self.samples.len() as f64 / self.sample_rate
    }
}

#[derive(Debug)]
pub enum PhaseError {
    SilentMic,
    NoAudio,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct FormatInfo {
    pub sample_rate: f64,
    pub channels: u32,
    pub interleaved: bool,
}

impl std::fmt::Display for FormatInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:.0} Hz, {} can{}",
            self.sample_rate,
            self.channels,
            if self.interleaved {
                ", intercalado"
            } else {
                ""
            }
        )
    }
}

#[derive(Debug)]
pub struct PhaseResult {
    pub spec: PhaseSpec,
    pub vp_reported: bool,
    pub bypass_reported: bool,
    pub agc_reported: bool,
    pub ducking_level: isize,
    pub mic_hw_format: FormatInfo,
    pub tap_format: FormatInfo,
    pub output_format: FormatInfo,
    pub input_latency_ms: f64,
    pub output_latency_ms: f64,
    pub engine_running_at_end: bool,
    pub wall_seconds: f64,
    /// One Vec per input channel.
    pub channels: Vec<Vec<f32>>,
    /// What the engine sent toward the speakers, per channel, whole phase.
    pub output_channels: Vec<Vec<f32>>,
    pub sample_rate: f64,
    pub silence: Range<usize>,
    pub playback: Range<usize>,
    /// Capture frame when the player was started, before output latency.
    pub play_mark: usize,
    /// Talk-over only: frame where the owner was told to speak.
    pub talk_mark: Option<usize>,
}

struct Capture {
    channels: Mutex<Vec<Vec<f32>>>,
    frames: AtomicUsize,
}

fn format_info(format: &AVAudioFormat) -> FormatInfo {
    // SAFETY: plain property reads on a valid format.
    unsafe {
        FormatInfo {
            sample_rate: format.sampleRate(),
            channels: format.channelCount(),
            interleaved: format.isInterleaved(),
        }
    }
}

/// Copies the tap's buffer. The tap runs on an AVFoundation queue, not the realtime
/// thread, so taking a lock and allocating here is fine.
fn append(capture: &Capture, buffer: &AVAudioPCMBuffer) {
    // SAFETY: the buffer is valid for the duration of the tap; channel pointers and
    // lengths come from the buffer itself, and data is null-checked.
    unsafe {
        let frames = buffer.frameLength() as usize;
        let format = buffer.format();
        let count = format.channelCount() as usize;
        let data = buffer.floatChannelData();
        if data.is_null() || frames == 0 {
            return;
        }
        let mut out = capture.channels.lock().expect("capture lock");
        out.resize_with(count, Vec::new);
        if format.isInterleaved() {
            let base = (*data).as_ptr();
            let all = std::slice::from_raw_parts(base, frames * count);
            for frame in all.chunks_exact(count) {
                for (channel, sample) in out.iter_mut().zip(frame) {
                    channel.push(*sample);
                }
            }
        } else {
            for (i, channel) in out.iter_mut().enumerate() {
                let samples = (*data.add(i)).as_ptr();
                channel.extend_from_slice(std::slice::from_raw_parts(samples, frames));
            }
        }
        capture.frames.fetch_add(frames, Ordering::SeqCst);
    }
}

/// Leaves the system as found on every exit path: tap removed, engine stopped,
/// voice processing released.
struct EngineGuard {
    engine: Retained<AVAudioEngine>,
    input: Retained<AVAudioInputNode>,
    mixer: Retained<AVAudioMixerNode>,
    tap_installed: bool,
    voice_processing: bool,
}

impl Drop for EngineGuard {
    fn drop(&mut self) {
        // SAFETY: valid objects; these calls are legal on a stopped engine too.
        unsafe {
            self.engine.stop();
            if self.tap_installed {
                self.input.removeTapOnBus(0);
                self.mixer.removeTapOnBus(0);
            }
            if self.voice_processing {
                let _ = self.input.setVoiceProcessingEnabled_error(false);
            }
        }
    }
}

fn wait_frames(capture: &Capture, target: usize, timeout: Duration) -> Result<(), PhaseError> {
    let start = Instant::now();
    while capture.frames.load(Ordering::SeqCst) < target {
        if start.elapsed() > timeout {
            return Err(PhaseError::NoAudio);
        }
        sleep(POLL);
    }
    Ok(())
}

pub fn run_phase(
    spec: PhaseSpec,
    source: &Source,
    options: Options,
) -> Result<PhaseResult, PhaseError> {
    let started = Instant::now();
    // SAFETY: AVAudioEngine setup follows the documented order: voice processing is
    // toggled while stopped and before reading formats, since it changes them. Every
    // object stays alive until the guard drops, after the engine has stopped.
    unsafe {
        let engine = AVAudioEngine::new();
        let input = engine.inputNode();
        let mut guard = EngineGuard {
            engine: engine.clone(),
            input: input.clone(),
            mixer: engine.mainMixerNode(),
            tap_installed: false,
            voice_processing: false,
        };

        if spec.voice_processing {
            input
                .setVoiceProcessingEnabled_error(true)
                .map_err(|e| PhaseError::Failed(format!("activar voice processing: {e}")))?;
            guard.voice_processing = true;
            if spec.bypass {
                input.setVoiceProcessingBypassed(true);
            }
        }

        let tap_format = input.outputFormatForBus(0);
        let tap_info = format_info(&tap_format);
        if spec.voice_processing {
            // Voice processing requires the input node's output format to equal the
            // output node's input format, or the engine fails to start with -10875.
            engine.connect_to_format(
                &engine.mainMixerNode(),
                &engine.outputNode(),
                Some(&tap_format),
            );
        }

        let player = AVAudioPlayerNode::new();
        engine.attachNode(&player);
        // Stereo on purpose: under voice processing the mixer is 9-channel, and a mono
        // source would land on channel 0 only, unlike the voice-processing-off phase.
        let play_format = AVAudioFormat::initStandardFormatWithSampleRate_channels(
            AVAudioFormat::alloc(),
            source.sample_rate,
            2,
        )
        .ok_or_else(|| PhaseError::Failed("formato de reproducción inválido".to_owned()))?;
        engine.connect_to_format(&player, &engine.mainMixerNode(), Some(&play_format));
        if options.mute {
            let mixing: &ProtocolObject<dyn AVAudioMixing> = ProtocolObject::from_ref(&*player);
            mixing.setVolume(MUTE_GAIN);
        }

        let buffer = AVAudioPCMBuffer::initWithPCMFormat_frameCapacity(
            AVAudioPCMBuffer::alloc(),
            &play_format,
            source.samples.len() as u32,
        )
        .ok_or_else(|| PhaseError::Failed("buffer de reproducción inválido".to_owned()))?;
        buffer.setFrameLength(source.samples.len() as u32);
        let play_channels = buffer.floatChannelData();
        for i in 0..2 {
            (*play_channels.add(i))
                .as_ptr()
                .copy_from_nonoverlapping(source.samples.as_ptr(), source.samples.len());
        }

        if tap_info.sample_rate <= 0.0 || tap_info.channels == 0 {
            return Err(PhaseError::Failed(format!(
                "el micrófono no tiene un formato usable ({tap_info})"
            )));
        }
        let capture = Arc::new(Capture {
            channels: Mutex::new(Vec::new()),
            frames: AtomicUsize::new(0),
        });
        let tap_capture = capture.clone();
        let tap = RcBlock::new(
            move |buf: NonNull<AVAudioPCMBuffer>, _when: NonNull<AVAudioTime>| {
                append(&tap_capture, buf.as_ref());
            },
        );
        input.installTapOnBus_bufferSize_format_block(0, 1024, None, RcBlock::as_ptr(&tap));
        let output_capture = Arc::new(Capture {
            channels: Mutex::new(Vec::new()),
            frames: AtomicUsize::new(0),
        });
        let output_tap_capture = output_capture.clone();
        let output_tap = RcBlock::new(
            move |buf: NonNull<AVAudioPCMBuffer>, _when: NonNull<AVAudioTime>| {
                append(&output_tap_capture, buf.as_ref());
            },
        );
        engine
            .mainMixerNode()
            .installTapOnBus_bufferSize_format_block(0, 1024, None, RcBlock::as_ptr(&output_tap));
        guard.tap_installed = true;

        engine.prepare();
        engine
            .startAndReturnError()
            .map_err(|e| PhaseError::Failed(format!("arrancar el motor (tap: {tap_info}): {e}")))?;

        let rate = tap_info.sample_rate;
        let to_frames = |d: Duration| (d.as_secs_f64() * rate) as usize;

        // Room noise, before any sound from this phase.
        let silence_start = to_frames(SETTLE);
        wait_frames(&capture, silence_start, NO_AUDIO_TIMEOUT)?;
        let silence_end = silence_start + to_frames(SILENCE);
        wait_frames(&capture, silence_end, NO_AUDIO_TIMEOUT)?;
        if options.abort_on_silent_mic {
            let channels = capture.channels.lock().expect("capture lock");
            if channels.iter().all(|c| c.iter().all(|&s| s == 0.0)) {
                return Err(PhaseError::SilentMic);
            }
        }

        let done = Arc::new(AtomicBool::new(false));
        let done_block = done.clone();
        let completion = RcBlock::new(move |_: AVAudioPlayerNodeCompletionCallbackType| {
            done_block.store(true, Ordering::SeqCst);
        });
        player.scheduleBuffer_completionCallbackType_completionHandler(
            &buffer,
            AVAudioPlayerNodeCompletionCallbackType::DataPlayedBack,
            RcBlock::as_ptr(&completion),
        );
        let play_mark = capture.frames.load(Ordering::SeqCst);
        player.play();

        let limit = Duration::from_secs_f64(source.seconds()) + NO_AUDIO_TIMEOUT;
        let play_started = Instant::now();
        let mut talk_mark = None;
        while !done.load(Ordering::SeqCst) {
            if play_started.elapsed() > limit {
                return Err(PhaseError::Failed(
                    "la reproducción nunca terminó".to_owned(),
                ));
            }
            if options.talk_over
                && talk_mark.is_none()
                && play_started.elapsed().as_secs_f64() >= source.seconds() / 2.0
            {
                talk_mark = Some(capture.frames.load(Ordering::SeqCst));
                println!("\n>>> hablá ahora <<<\n");
            }
            sleep(POLL);
        }
        let play_end = capture.frames.load(Ordering::SeqCst);

        let tail = if options.talk_over {
            TALK_OVER_TAIL
        } else {
            TAIL
        };
        wait_frames(&capture, play_end + to_frames(tail), NO_AUDIO_TIMEOUT)?;
        if options.talk_over {
            println!("listo, ya podés dejar de hablar.");
        }

        let running = engine.isRunning();
        let result = PhaseResult {
            spec,
            vp_reported: input.isVoiceProcessingEnabled(),
            bypass_reported: input.isVoiceProcessingBypassed(),
            agc_reported: input.isVoiceProcessingAGCEnabled(),
            ducking_level: input
                .voiceProcessingOtherAudioDuckingConfiguration()
                .duckingLevel
                .0,
            mic_hw_format: format_info(&input.inputFormatForBus(0)),
            tap_format: tap_info,
            output_format: format_info(&engine.outputNode().outputFormatForBus(0)),
            input_latency_ms: input.presentationLatency() * 1000.0,
            output_latency_ms: engine.outputNode().presentationLatency() * 1000.0,
            engine_running_at_end: running,
            wall_seconds: started.elapsed().as_secs_f64(),
            channels: Vec::new(),
            output_channels: Vec::new(),
            sample_rate: rate,
            silence: silence_start..silence_end,
            playback: (play_mark + to_frames(PLAYBACK_LEAD_IN)).min(play_end)..play_end,
            play_mark,
            talk_mark,
        };
        drop(guard);
        let channels = std::mem::take(&mut *capture.channels.lock().expect("capture lock"));
        let output_channels =
            std::mem::take(&mut *output_capture.channels.lock().expect("capture lock"));
        Ok(PhaseResult {
            channels,
            output_channels,
            ..result
        })
    }
}
