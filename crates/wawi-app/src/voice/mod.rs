//! `wawi2 --voice`: Wawi with the real pieces. Hold right Command and talk;
//! Parakeet hears it, the core decides, Claude Code answers, Kokoro speaks.
//!
//! `--input-wav FILE` feeds a WAV as if it had been said (no mic, no key) and
//! ends after that turn; `--mute` plays through a silent player that takes
//! as long as the real audio would. Together they run one real turn without
//! opening the mic or making a sound.
//!
//! Wawi's body (the mascot and the menu bar item, `wawi-body`) comes up with
//! it unless `--no-ui`; `--ui-demo` shows the body alone, cycling through
//! her looks with no audio and no brain.

mod body;
pub mod clock;
#[cfg(target_os = "macos")]
mod demo;
mod host;
mod incidents;
#[cfg(target_os = "macos")]
mod mac;
mod mute;
mod turns;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use wawi_brain_claude::ClaudeConfig;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VoiceOptions {
    pub input_wav: Option<PathBuf>,
    pub mute: bool,
    /// `--no-ui`: terminal only, no mascot and no menu bar item.
    pub no_ui: bool,
}

/// Exit codes, the same as `ptt-test` where they overlap.
pub mod exit {
    pub const ERROR: u8 = 1;
    pub const NO_MIC: u8 = 3;
    pub const NO_MODELS: u8 = 4;
    pub const NO_KEY: u8 = 5;
}

/// The brain works from `<home>/claude`: a folder with no project and no
/// CLAUDE.md. `WAWI2_CLAUDE_MODEL` and `WAWI2_CLAUDE_EFFORT` pick the model
/// and effort; unset, the brain's defaults apply (`DEFAULT_MODEL`,
/// `DEFAULT_EFFORT`); set but empty, the flag is left out and the CLI decides.
pub fn claude_config(home: Option<&Path>) -> ClaudeConfig {
    let mut config = ClaudeConfig::default();
    if let Some(dir) = home {
        config.workdir = dir.join("claude");
    }
    apply_choices(
        &mut config,
        std::env::var("WAWI2_CLAUDE_MODEL").ok(),
        std::env::var("WAWI2_CLAUDE_EFFORT").ok(),
    );
    config
}

/// Only what the owner set overrides the brain's defaults.
fn apply_choices(config: &mut ClaudeConfig, model: Option<String>, effort: Option<String>) {
    if let Some(model) = model {
        config.model = Some(model);
    }
    if let Some(effort) = effort {
        config.effort = Some(effort);
    }
}

/// For the startup line: the model or effort in use, or the CLI's own.
pub fn describe_choice(choice: Option<&str>) -> &str {
    choice.unwrap_or("el predeterminado del CLI")
}

#[cfg(target_os = "macos")]
pub fn main(options: VoiceOptions, home: Option<PathBuf>) -> ExitCode {
    mac::main(options, home)
}

#[cfg(not(target_os = "macos"))]
pub fn main(_options: VoiceOptions, _home: Option<PathBuf>) -> ExitCode {
    eprintln!("La voz de Wawi 2 por ahora solo funciona en macOS.");
    ExitCode::from(exit::ERROR)
}

/// `--ui-demo[=LOOK]`: the body alone. Without a look it cycles through all
/// of them; with one it holds it (for screenshots).
#[cfg(target_os = "macos")]
pub fn ui_demo(hold: Option<DemoLook>, home: Option<PathBuf>, pet: Option<PathBuf>) -> ExitCode {
    demo::main(hold, home, pet)
}

#[cfg(not(target_os = "macos"))]
pub fn ui_demo(_hold: Option<DemoLook>, _home: Option<PathBuf>, _pet: Option<PathBuf>) -> ExitCode {
    eprintln!("El cuerpo de Wawi 2 por ahora solo existe en macOS.");
    ExitCode::from(exit::ERROR)
}

/// The looks `--ui-demo=LOOK` can hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DemoLook {
    Idle,
    Listening,
    Transcribing,
    Thinking,
    Speaking,
    Interrupted,
    Error,
    Muted,
    Sleeping,
}

impl DemoLook {
    pub const ALL: [DemoLook; 9] = [
        DemoLook::Idle,
        DemoLook::Listening,
        DemoLook::Transcribing,
        DemoLook::Thinking,
        DemoLook::Speaking,
        DemoLook::Interrupted,
        DemoLook::Error,
        DemoLook::Muted,
        DemoLook::Sleeping,
    ];

    pub fn name(self) -> &'static str {
        match self {
            DemoLook::Idle => "idle",
            DemoLook::Listening => "listening",
            DemoLook::Transcribing => "transcribing",
            DemoLook::Thinking => "thinking",
            DemoLook::Speaking => "speaking",
            DemoLook::Interrupted => "interrupted",
            DemoLook::Error => "error",
            DemoLook::Muted => "muted",
            DemoLook::Sleeping => "sleeping",
        }
    }

    pub fn parse(name: &str) -> Option<DemoLook> {
        DemoLook::ALL.into_iter().find(|look| look.name() == name)
    }
}
