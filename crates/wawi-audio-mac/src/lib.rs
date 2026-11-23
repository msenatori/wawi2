//! Live audio for Wawi on macOS: one long-running AVAudioEngine with voice
//! processing (echo cancellation) that captures the mic behind a push-to-talk
//! gate and plays TTS audio, plus the right-Command hotkey.
//!
//! The pure parts ([`gate`], [`dsp`], [`playback`]) build on every platform and
//! are what the tests cover; the AVFoundation and CoreGraphics glue is macOS only.
//!
//! Nothing here reads or writes the system or device volume, except
//! [`devices`], which only reads it for diagnostics.

pub mod analysis;
pub mod dsp;
pub mod gate;
pub mod playback;

#[cfg(target_os = "macos")]
pub mod devices;
#[cfg(target_os = "macos")]
pub mod hotkey;
#[cfg(target_os = "macos")]
pub mod live;
#[cfg(target_os = "macos")]
pub mod permissions;

// The `aec-test` spike. Kept as is; not part of the live API.
#[cfg(target_os = "macos")]
#[doc(hidden)]
pub mod engine;
#[cfg(target_os = "macos")]
#[doc(hidden)]
pub mod report;
#[cfg(target_os = "macos")]
#[doc(hidden)]
pub mod run;

pub use gate::{EndReason, GateConfig, GateEvent, PttGate, Segment};
pub use playback::{Playback, Player, TTS_SAMPLE_RATE};

#[cfg(target_os = "macos")]
pub use hotkey::{HotkeyError, RightCommandKey};
#[cfg(target_os = "macos")]
pub use live::{AudioError, LiveAudio, MacAudioOut, PttHandle};
#[cfg(target_os = "macos")]
pub use permissions::{MicPermission, ensure_microphone, microphone_mode, show_microphone_modes};
