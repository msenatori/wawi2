//! Turns captured phases into the table the spike exists to produce.

use std::ops::Range;

use crate::analysis::{EchoMatch, Trigger, echo_match, noise_floor_db, peak_db, rms_db, trigger};
use crate::devices::{Device, OutputKind};
use crate::engine::{PhaseResult, Source};

/// Measured in the first channel; other channels are listed separately.
pub const ANALYSIS_CHANNEL: usize = 0;
/// Below this margin over the room, the echo is too weak to test anything.
const MIN_ECHO_MARGIN_DB: f64 = 10.0;
const CLIP_DB: f64 = -0.1;
const EARLY_SECONDS: f64 = 1.5;
const MAX_ECHO_LAG_MS: f64 = 500.0;
/// In phase A the capture should plainly follow the sentence. Below this, room sound
/// competes with the echo. A judgment call: measured 0.02 with nothing audible.
const MIN_REFERENCE_ECHO_CORRELATION: f64 = 0.7;
/// A peak this far above the median means something other than steady noise happened.
const TRANSIENT_MARGIN_DB: f64 = 30.0;

pub struct Levels {
    pub floor: f64,
    pub silence_rms: f64,
    pub silence_peak: f64,
    pub playback_rms: f64,
    pub playback_peak: f64,
}

pub fn samples_of<'a>(phase: &'a PhaseResult, range: &Range<usize>) -> &'a [f32] {
    let channel = &phase.channels[ANALYSIS_CHANNEL];
    let end = range.end.min(channel.len());
    &channel[range.start.min(end)..end]
}

pub fn levels(phase: &PhaseResult) -> Levels {
    let silence = samples_of(phase, &phase.silence);
    let playback = samples_of(phase, &phase.playback);
    Levels {
        floor: noise_floor_db(silence, phase.sample_rate),
        silence_rms: rms_db(silence),
        silence_peak: peak_db(silence),
        playback_rms: rms_db(playback),
        playback_peak: peak_db(playback),
    }
}

fn trigger_of(phase: &PhaseResult, floor: f64) -> Trigger {
    trigger(
        samples_of(phase, &phase.playback),
        phase.sample_rate,
        floor + 10.0,
    )
}

fn echo_of(phase: &PhaseResult, source: &Source) -> EchoMatch {
    let channel = &phase.channels[ANALYSIS_CHANNEL];
    let start = phase.play_mark.min(channel.len());
    echo_match(
        &source.samples,
        &channel[start..],
        phase.sample_rate,
        MAX_ECHO_LAG_MS,
    )
}

pub fn print_devices(output: Option<&Device>, input: Option<&Device>) {
    match output {
        Some(d) => println!(
            "Salida:  {} [{}], {:.0} Hz, volumen {}, mute {}",
            d.name,
            d.output_kind.label(),
            d.sample_rate,
            d.volume
                .map_or("?".to_owned(), |v| format!("{:.0}%", v * 100.0)),
            d.muted.map_or("?".to_owned(), |m| m.to_string()),
        ),
        None => println!("Salida:  no se pudo leer"),
    }
    match input {
        Some(d) => println!(
            "Entrada: {}, {:.0} Hz, ganancia {}",
            d.name,
            d.sample_rate,
            d.volume
                .map_or("?".to_owned(), |v| format!("{:.0}%", v * 100.0)),
        ),
        None => println!("Entrada: no se pudo leer"),
    }
}

pub fn print_phase_info(phase: &PhaseResult) {
    println!("Fase {} ({}):", phase.spec.key, phase.spec.label);
    println!(
        "  voice processing reportado: {}, bypass: {}, AGC: {}, ducking de otras apps: nivel {}",
        phase.vp_reported, phase.bypass_reported, phase.agc_reported, phase.ducking_level
    );
    println!(
        "  mic hw: {} | tap: {} | salida: {}",
        phase.mic_hw_format, phase.tap_format, phase.output_format
    );
    println!(
        "  latencia informada: entrada {:.1} ms, salida {:.1} ms | motor corriendo al final: {} | duró {:.1} s",
        phase.input_latency_ms,
        phase.output_latency_ms,
        phase.engine_running_at_end,
        phase.wall_seconds
    );
    let output: Vec<String> = phase
        .output_channels
        .iter()
        .map(|c| format!("{:.1}", rms_db(c)))
        .collect();
    println!(
        "  señal enviada a la salida, RMS por canal, toda la fase (dBFS): {}",
        output.join(", ")
    );
    if phase.channels.len() > 1 {
        let identical = phase.channels.iter().all(|c| *c == phase.channels[0]);
        println!("  canales de entrada idénticos entre sí: {identical}");

        let per_channel: Vec<String> = phase
            .channels
            .iter()
            .map(|c| {
                let end = phase.playback.end.min(c.len());
                let start = phase.playback.start.min(end);
                format!("{:.1}", rms_db(&c[start..end]))
            })
            .collect();
        println!(
            "  RMS por canal durante reproducción (dBFS): {}",
            per_channel.join(", ")
        );
    }
}

pub fn print_table(phases: &[PhaseResult], source: &Source) {
    let Some(reference) = phases.first() else {
        return;
    };
    let reference_floor = levels(reference).floor;
    println!();
    println!(
        "{:<4} {:>7} {:>8} {:>8} {:>8} {:>8} {:>8} {:>9} {:>9} {:>8} {:>6} {:>7}",
        "fase",
        "piso",
        "sil RMS",
        "sil pico",
        "voz RMS",
        "voz pico",
        "voz-piso",
        "disp piso",
        "disp ref",
        "racha",
        "r eco",
        "ret ms",
    );
    for phase in phases {
        let l = levels(phase);
        let own = trigger_of(phase, l.floor);
        let common = trigger_of(phase, reference_floor);
        let echo = echo_of(phase, source);
        println!(
            "{:<4} {:>7.1} {:>8.1} {:>8.1} {:>8.1} {:>8.1} {:>8.1} {:>8.1}% {:>8.1}% {:>8.0} {:>6.2} {:>7.0}",
            phase.spec.key,
            l.floor,
            l.silence_rms,
            l.silence_peak,
            l.playback_rms,
            l.playback_peak,
            l.playback_rms - l.floor,
            own.fraction * 100.0,
            common.fraction * 100.0,
            common.longest_run_ms,
            echo.correlation,
            echo.lag_ms,
        );
    }
    println!(
        "dBFS. \"voz\": mientras suena la frase. \"sil\": silencio previo. \"piso\": mediana de cuadros de 30 ms del silencio."
    );
    println!(
        "\"disp piso\": % de cuadros sobre piso de esa fase + 10 dB. \"disp ref\" y \"racha\" (ms seguidos): con el umbral de la fase {} ({:.1} dBFS).",
        reference.spec.key,
        reference_floor + 10.0
    );
    println!(
        "\"r eco\": correlación de la envolvente grabada con la frase (cerca de 1 = se oye la frase; cerca de 0 = no). \"ret\": retardo de esa coincidencia."
    );
}

pub fn print_reductions(phases: &[PhaseResult]) {
    let find = |key: &str| phases.iter().find(|p| p.spec.key == key);
    let (Some(a), Some(b)) = (find("A"), find("B")) else {
        return;
    };
    let (la, lb) = (levels(a), levels(b));
    println!();
    println!(
        "Reducción A -> B: voz {:.1} dB RMS, {:.1} dB pico; silencio {:.1} dB RMS.",
        la.playback_rms - lb.playback_rms,
        la.playback_peak - lb.playback_peak,
        la.silence_rms - lb.silence_rms,
    );
    println!(
        "Eco sobre el piso: A {:.1} dB, B {:.1} dB. No depende de la ganancia de cada fase, que no es la misma.",
        la.playback_rms - la.floor,
        lb.playback_rms - lb.floor,
    );
    if let Some(c) = find("C") {
        let lc = levels(c);
        println!(
            "Control C (motor con voice processing, DSP en bypass) contra A: voz {:+.1} dB.",
            lc.playback_rms - la.playback_rms
        );
        println!(
            "Reducción C -> B: voz {:.1} dB RMS.",
            lc.playback_rms - lb.playback_rms
        );
    }
    // The canceller adapts, so the first moments can leak more than the average shows.
    let split = (EARLY_SECONDS * b.sample_rate) as usize;
    for phase in [a, b] {
        let start = phase.playback.start;
        let cut = (start + split).min(phase.playback.end);
        println!(
            "Fase {}: primeros {EARLY_SECONDS} s de voz {:.1} dBFS, resto {:.1} dBFS.",
            phase.spec.key,
            rms_db(samples_of(phase, &(start..cut))),
            rms_db(samples_of(phase, &(cut..phase.playback.end))),
        );
    }
}

/// Reasons this run's numbers can't answer the question, for the reader to see.
pub fn validity_problems(
    output: Option<&Device>,
    phases: &[PhaseResult],
    source: &Source,
    volumes: &[Option<f32>],
    mute: bool,
) -> Vec<String> {
    let mut problems = Vec::new();
    if mute {
        problems.push("--mute: la frase sonó a -80 dB, los números no miden eco.".to_owned());
    }
    match output.map(|d| d.output_kind) {
        Some(OutputKind::BuiltInSpeakers) => {}
        Some(kind) => problems.push(format!(
            "la salida es {}: el resultado solo vale por parlantes.",
            kind.label()
        )),
        None => problems.push("no se pudo identificar la salida.".to_owned()),
    }
    if output.and_then(|d| d.muted) == Some(true) {
        problems.push("la salida está en mute.".to_owned());
    }
    if volumes.windows(2).any(|pair| pair[0] != pair[1]) {
        let list: Vec<String> = volumes
            .iter()
            .map(|v| v.map_or("?".to_owned(), |v| format!("{:.0}%", v * 100.0)))
            .collect();
        problems.push(format!(
            "el volumen del sistema cambió durante la prueba ({}): las fases no sonaron igual de fuerte.",
            list.join(" -> ")
        ));
    }
    if let Some(a) = phases.iter().find(|p| p.spec.key == "A") {
        let l = levels(a);
        let echo = echo_of(a, source);
        if echo.correlation < MIN_REFERENCE_ECHO_CORRELATION {
            problems.push(format!(
                "en la fase A la grabación casi no sigue a la frase (r {:.2}): el ruido del ambiente compite con el eco.",
                echo.correlation
            ));
        }
        if l.playback_rms - l.floor < MIN_ECHO_MARGIN_DB {
            problems.push(format!(
                "en la fase A el eco quedó solo {:.1} dB sobre el piso: no hay eco suficiente \
                 para medir cancelación.",
                l.playback_rms - l.floor
            ));
        }
        if l.playback_peak >= CLIP_DB {
            problems.push(
                "en la fase A el micrófono saturó: la reducción real podría ser mayor.".to_owned(),
            );
        }
    }
    for phase in phases {
        let l = levels(phase);
        if l.silence_peak - l.floor > TRANSIENT_MARGIN_DB {
            problems.push(format!(
                "en la fase {} hubo un ruido fuerte durante el silencio (pico {:.1}, piso {:.1}): el ambiente no estaba quieto.",
                phase.spec.key, l.silence_peak, l.floor
            ));
        }
        if !phase.engine_running_at_end {
            problems.push(format!(
                "en la fase {} el motor ya no corría al final.",
                phase.spec.key
            ));
        }
        if phase.spec.voice_processing != phase.vp_reported {
            problems.push(format!(
                "en la fase {} se pidió voice processing {} y el sistema reporta {}.",
                phase.spec.key, phase.spec.voice_processing, phase.vp_reported
            ));
        }
        if phase.spec.bypass != phase.bypass_reported {
            problems.push(format!(
                "en la fase {} se pidió bypass {} y el sistema reporta {}.",
                phase.spec.key, phase.spec.bypass, phase.bypass_reported
            ));
        }
    }
    problems
}
