//! Command line, test sequence, and files.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::thread;
use std::time::Duration;

use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};

use crate::analysis::{is_all_zero, rms_db};
use crate::devices::{Device, default_input, default_output};
use crate::engine::{Options, PhaseError, PhaseResult, PhaseSpec, Source, run_phase};
use crate::report;

const HARD_TIMEOUT: Duration = Duration::from_secs(60);
const SENTENCE: &str =
    "Hola, soy Wawi. Estoy probando si puedo seguir escuchándote mientras hablo por los parlantes.";
const PREFERRED_VOICES: [&str; 2] = ["Paulina", "Mónica"];
const MAX_SOURCE_SECONDS: f64 = 7.0;

const EXIT_ERROR: u8 = 1;
const EXIT_USAGE: u8 = 2;
const EXIT_NO_MIC: u8 = 3;
const EXIT_TIMEOUT: u8 = 4;

const PHASE_A: PhaseSpec = PhaseSpec {
    key: "A",
    label: "voice processing apagado",
    voice_processing: false,
    bypass: false,
};
const PHASE_B: PhaseSpec = PhaseSpec {
    key: "B",
    label: "voice processing prendido",
    voice_processing: true,
    bypass: false,
};
// Same voice-processing engine with its DSP bypassed. Separates "the echo was
// removed" from "voice processing mode made the speakers or the mic quieter".
const PHASE_C: PhaseSpec = PhaseSpec {
    key: "C",
    label: "control: voice processing en bypass",
    voice_processing: true,
    bypass: true,
};

const NO_MIC_HELP: &str = "No llega audio del micrófono (ceros o nada). Lo más probable es que \
falte el permiso: Ajustes del Sistema > Privacidad y seguridad > Micrófono, y habilitá la app \
desde la que corrés esto (la terminal, no aec-test). Después cerrala y volvé a abrirla.";

struct Args {
    out: Option<PathBuf>,
    talk_over: bool,
    mute: bool,
}

const USAGE: &str = "uso: aec-test [--out DIR] [--talk-over] [--mute]
  --out DIR     dónde guardar los WAV (por defecto, una carpeta temporal)
  --talk-over   solo fase B: te pide que hables encima de la frase, para escuchar el WAV
  --mute        hace todo el recorrido a -80 dB, sin que se oiga; sirve para probar, no mide eco";

fn parse_args() -> Result<Option<Args>, String> {
    let mut args = Args {
        out: None,
        talk_over: false,
        mute: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--out" => {
                let dir = it.next().ok_or("--out necesita una carpeta")?;
                args.out = Some(PathBuf::from(dir));
            }
            "--talk-over" => args.talk_over = true,
            "--mute" => args.mute = true,
            "--help" | "-h" => return Ok(None),
            other => return Err(format!("argumento desconocido: {other}")),
        }
    }
    Ok(Some(args))
}

fn spawn_watchdog() {
    thread::spawn(|| {
        thread::sleep(HARD_TIMEOUT);
        eprintln!(
            "aec-test: pasaron {} s sin terminar, salgo. Al morir el proceso, el sistema libera el audio.",
            HARD_TIMEOUT.as_secs()
        );
        std::process::exit(i32::from(EXIT_TIMEOUT));
    });
}

fn mic_permission() -> AVAuthorizationStatus {
    // SAFETY: AVMediaTypeAudio is an immutable framework constant, and audio is an
    // accepted media type for this call.
    unsafe {
        match AVMediaTypeAudio {
            Some(media) => AVCaptureDevice::authorizationStatusForMediaType(media),
            None => AVAuthorizationStatus::NotDetermined,
        }
    }
}

fn spanish_voice() -> Option<String> {
    let listing = Command::new("say").args(["-v", "?"]).output().ok()?;
    let listing = String::from_utf8_lossy(&listing.stdout).into_owned();
    // Each line is "<name, may contain spaces>  <locale>  # <sample>".
    let names: Vec<&str> = listing
        .lines()
        .filter_map(|line| {
            let (left, _) = line.split_once('#')?;
            let left = left.trim_end();
            let (name, locale) = left.rsplit_once(' ')?;
            locale.starts_with("es_").then_some(name.trim_end())
        })
        .collect();
    PREFERRED_VOICES
        .iter()
        .find(|preferred| names.contains(preferred))
        .map(|name| (*name).to_owned())
        .or_else(|| names.first().map(|name| (*name).to_owned()))
}

fn make_source(dir: &Path) -> Result<Source, String> {
    let voice = spanish_voice();
    match &voice {
        Some(name) => println!("Voz: {name}"),
        None => println!("No hay voz en español, uso la voz por defecto."),
    }
    let path = dir.join("frase.wav");
    let mut say = Command::new("say");
    if let Some(name) = &voice {
        say.args(["-v", name]);
    }
    let status = say
        .args(["--file-format=WAVE", "--data-format=LEF32@48000", "-o"])
        .arg(&path)
        .arg(SENTENCE)
        .status()
        .map_err(|e| format!("no se pudo ejecutar say: {e}"))?;
    if !status.success() {
        return Err(format!("say falló: {status}"));
    }
    let mut reader =
        hound::WavReader::open(&path).map_err(|e| format!("no se pudo leer la frase: {e}"))?;
    let spec = reader.spec();
    if spec.channels != 1 || spec.sample_format != hound::SampleFormat::Float {
        return Err(format!("say generó un formato inesperado: {spec:?}"));
    }
    let mut samples = reader
        .samples::<f32>()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("no se pudo leer la frase: {e}"))?;
    let sample_rate = f64::from(spec.sample_rate);
    samples.truncate((MAX_SOURCE_SECONDS * sample_rate) as usize);
    let source = Source {
        samples,
        sample_rate,
    };
    println!("Frase: {:.1} s, {:.0} Hz", source.seconds(), sample_rate);
    Ok(source)
}

fn write_wav(path: &Path, samples: &[f32], sample_rate: f64) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: sample_rate as u32,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let fail = |e: hound::Error| format!("no se pudo escribir {}: {e}", path.display());
    let mut writer = hound::WavWriter::create(path, spec).map_err(fail)?;
    for &sample in samples {
        writer.write_sample(sample).map_err(fail)?;
    }
    writer.finalize().map_err(fail)
}

fn save_phase(dir: &Path, name: &str, phase: &PhaseResult) -> Result<PathBuf, String> {
    let path = dir.join(name);
    write_wav(
        &path,
        &phase.channels[report::ANALYSIS_CHANNEL],
        phase.sample_rate,
    )?;
    Ok(path)
}

fn phase_file(phase: &PhaseResult) -> String {
    format!("fase_{}.wav", phase.spec.key.to_lowercase())
}

/// Any device or volume difference means the numbers cross a route change, or that
/// this program failed to leave things as it found them.
fn print_state_change(before: (&Option<Device>, &Option<Device>)) {
    let after = (default_output(), default_input());
    if *before.0 == after.0 && *before.1 == after.1 {
        println!("Dispositivos y volumen: igual que antes de empezar.");
    } else {
        println!("OJO: los dispositivos o el volumen cambiaron durante la prueba. Ahora:");
        report::print_devices(after.0.as_ref(), after.1.as_ref());
    }
}

fn run_talk_over(args: &Args, dir: &Path, source: &Source) -> Result<u8, String> {
    println!(
        "Modo talk-over: va a sonar la frase. Cuando aparezca \"hablá ahora\", hablá con voz normal \
         y seguí unos segundos después de que termine."
    );
    let options = Options {
        mute: args.mute,
        talk_over: true,
        abort_on_silent_mic: false,
    };
    let phase = match run_phase(PHASE_B, source, options) {
        Ok(phase) => phase,
        Err(PhaseError::NoAudio) | Err(PhaseError::SilentMic) => {
            eprintln!("{NO_MIC_HELP}");
            return Ok(EXIT_NO_MIC);
        }
        Err(PhaseError::Failed(e)) => return Err(e),
    };
    report::print_phase_info(&phase);
    let path = save_phase(dir, "talk_over.wav", &phase)?;
    let channel = &phase.channels[report::ANALYSIS_CHANNEL];
    if is_all_zero(channel) {
        eprintln!("{NO_MIC_HELP}");
        return Ok(EXIT_NO_MIC);
    }
    let start = phase.playback.start;
    let end = phase.playback.end;
    let mid = phase.talk_mark.unwrap_or(end);
    println!(
        "RMS dBFS: solo eco {:.1} | eco y voz {:.1} | solo voz {:.1}",
        rms_db(report::samples_of(&phase, &(start..mid))),
        rms_db(report::samples_of(&phase, &(mid..end))),
        rms_db(report::samples_of(&phase, &(end..channel.len()))),
    );
    println!(
        "Escuchá {}: tu voz tiene que oírse igual de clara con la frase sonando y sin ella.",
        path.display()
    );
    Ok(0)
}

fn run_measurement(
    args: &Args,
    dir: &Path,
    source: &Source,
    output: Option<&Device>,
) -> Result<u8, String> {
    let mut phases = Vec::new();
    let read_volume = || default_output().and_then(|d| d.volume);
    let mut volumes = vec![read_volume()];
    for (i, spec) in [PHASE_A, PHASE_B, PHASE_C].into_iter().enumerate() {
        println!("Fase {} ({})...", spec.key, spec.label);
        let options = Options {
            mute: args.mute,
            talk_over: false,
            // Only phase A: voice processing may legitimately gate silence to zero.
            abort_on_silent_mic: i == 0,
        };
        match run_phase(spec, source, options) {
            Ok(phase) => {
                phases.push(phase);
                volumes.push(read_volume());
            }
            Err(PhaseError::SilentMic) => {
                eprintln!("{NO_MIC_HELP}\nNo llegó a sonar nada.");
                return Ok(EXIT_NO_MIC);
            }
            Err(PhaseError::NoAudio) => {
                eprintln!("{NO_MIC_HELP}");
                return Ok(EXIT_NO_MIC);
            }
            Err(PhaseError::Failed(e)) => return Err(format!("fase {}: {e}", spec.key)),
        }
    }

    println!();
    for phase in &phases {
        report::print_phase_info(phase);
        save_phase(dir, &phase_file(phase), phase)?;
    }
    report::print_table(&phases, source);
    report::print_reductions(&phases);

    let mut exit = 0;
    for phase in &phases {
        if is_all_zero(&phase.channels[report::ANALYSIS_CHANNEL]) {
            eprintln!(
                "La fase {} grabó solo ceros en toda la pasada. Eso no es cancelación: no hay señal.",
                phase.spec.key
            );
            exit = EXIT_NO_MIC;
        }
    }
    let problems = report::validity_problems(output, &phases, source, &volumes, args.mute);
    println!();
    if problems.is_empty() {
        println!("Validez: sin problemas detectados.");
    } else {
        println!("Validez: estos números NO alcanzan para concluir:");
        for problem in problems {
            println!("  - {problem}");
        }
    }
    Ok(exit)
}

pub fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(Some(args)) => args,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    spawn_watchdog();
    match run(&args) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("aec-test: {e}");
            ExitCode::from(EXIT_ERROR)
        }
    }
}

fn run(args: &Args) -> Result<u8, String> {
    let dir = args
        .out
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join(format!("wawi-aec-{}", std::process::id())));
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("no se pudo crear {}: {e}", dir.display()))?;

    let permission = mic_permission();
    println!(
        "Permiso de micrófono: {}",
        match permission {
            AVAuthorizationStatus::Authorized => "autorizado",
            AVAuthorizationStatus::Denied => "denegado",
            AVAuthorizationStatus::Restricted => "restringido",
            _ => "sin decidir (el sistema puede preguntar ahora)",
        }
    );
    if permission == AVAuthorizationStatus::Denied
        || permission == AVAuthorizationStatus::Restricted
    {
        eprintln!("{NO_MIC_HELP}");
        return Ok(EXIT_NO_MIC);
    }

    let before = (default_output(), default_input());
    report::print_devices(before.0.as_ref(), before.1.as_ref());

    let source = make_source(&dir)?;
    let code = if args.talk_over {
        run_talk_over(args, &dir, &source)?
    } else {
        run_measurement(args, &dir, &source, before.0.as_ref())?
    };
    print_state_change((&before.0, &before.1));
    println!("WAV en {}", dir.display());
    Ok(code)
}
