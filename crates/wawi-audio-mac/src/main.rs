//! `aec-test`: measures whether macOS voice processing removes Wawi's own voice
//! from the microphone. See the README for how to run it. macOS only.

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    wawi_audio_mac::run::main()
}

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!("aec-test solo funciona en macOS.");
    std::process::ExitCode::FAILURE
}
