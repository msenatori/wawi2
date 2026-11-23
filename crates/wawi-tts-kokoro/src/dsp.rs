//! Small audio helpers: silence trimming and the pitch shift Wawi 1 applies.

/// Half-width of the sinc kernel, in samples at the lower of the two rates.
const SINC_HALF_WIDTH: f64 = 16.0;

/// Kernel table steps per input sample.
const TABLE_STEPS: f64 = 512.0;

/// Band-limited resampling by any ratio (`output rate / input rate`), with a
/// Hann-windowed sinc. The kernel is tabulated once per call and read with
/// linear interpolation, which keeps it a few milliseconds per second of
/// audio.
pub fn resample_ratio(input: &[f32], ratio: f64) -> Vec<f32> {
    if input.is_empty() || ratio.is_nan() || ratio <= 0.0 || (ratio - 1.0).abs() < 1e-9 {
        return input.to_vec();
    }
    let cutoff = ratio.min(1.0);
    let half = SINC_HALF_WIDTH / cutoff;
    let table: Vec<f32> = (0..=(half * TABLE_STEPS).ceil() as usize + 1)
        .map(|k| {
            let x = k as f64 / TABLE_STEPS;
            (cutoff * sinc(cutoff * x) * hann(x / half)) as f32
        })
        .collect();
    let kernel = |x: f64| -> f32 {
        let pos = x.abs() * TABLE_STEPS;
        let k = pos as usize;
        if k + 1 >= table.len() {
            return 0.0;
        }
        let frac = (pos - k as f64) as f32;
        table[k] + (table[k + 1] - table[k]) * frac
    };
    let out_len = ((input.len() as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for n in 0..out_len {
        let center = n as f64 / ratio;
        let first = (center - half).ceil().max(0.0) as usize;
        let last = ((center + half).floor() as usize).min(input.len() - 1);
        let mut acc = 0.0_f32;
        let mut weight = 0.0_f32;
        for (i, &sample) in input.iter().enumerate().take(last + 1).skip(first) {
            let w = kernel(i as f64 - center);
            acc += w * sample;
            weight += w;
        }
        // Normalizing by the kernel sum keeps the gain at 1 near the edges.
        out.push(if weight.abs() > 1e-6 {
            acc / weight
        } else {
            0.0
        });
    }
    out
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-9 {
        1.0
    } else {
        let px = std::f64::consts::PI * x;
        px.sin() / px
    }
}

/// Hann window over [-1, 1].
fn hann(t: f64) -> f64 {
    if t.abs() >= 1.0 {
        0.0
    } else {
        0.5 * (1.0 + (std::f64::consts::PI * t).cos())
    }
}

/// The ratio a pitch shift of `semitones` reads the audio faster by.
pub fn pitch_ratio(semitones: f32) -> f64 {
    2.0_f64.powf(f64::from(semitones) / 12.0)
}

/// Wawi 1's pitch shift: play the audio `pitch_ratio` times faster, at the
/// same sample rate. Pitch and formants go up together (a smaller, younger
/// voice) and the audio gets shorter by the same ratio, which the caller makes
/// up for by synthesizing that much slower.
pub fn retune(samples: &[f32], semitones: f32) -> Vec<f32> {
    if semitones.abs() < 1e-3 {
        return samples.to_vec();
    }
    resample_ratio(samples, 1.0 / pitch_ratio(semitones))
}

/// Cuts leading and trailing silence like Kokoro's Python package does
/// (librosa's `trim`: 2048-sample frames, 512 hop, 60 dB under the loudest
/// frame).
pub fn trim_silence(samples: &[f32]) -> &[f32] {
    const FRAME: usize = 2048;
    const HOP: usize = 512;
    if samples.len() < FRAME {
        return samples;
    }
    let frames = (samples.len() - FRAME) / HOP + 1;
    let rms: Vec<f32> = (0..frames)
        .map(|f| {
            let frame = &samples[f * HOP..f * HOP + FRAME];
            (frame.iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt()
        })
        .collect();
    let loudest = rms.iter().copied().fold(0.0_f32, f32::max);
    if loudest <= 0.0 {
        return &samples[..0];
    }
    let floor = loudest * 1e-3;
    let first = rms.iter().position(|r| *r > floor).unwrap_or(0);
    let last = rms.iter().rposition(|r| *r > floor).unwrap_or(frames - 1);
    let start = first * HOP;
    let end = ((last * HOP) + FRAME).min(samples.len());
    &samples[start..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f64, rate: f64, seconds: f64) -> Vec<f32> {
        (0..(rate * seconds) as usize)
            .map(|n| (2.0 * std::f64::consts::PI * freq * n as f64 / rate).sin() as f32 * 0.5)
            .collect()
    }

    /// Frequency estimate from zero crossings.
    fn frequency(samples: &[f32], rate: f64) -> f64 {
        let crossings = samples
            .windows(2)
            .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
            .count();
        crossings as f64 * rate / samples.len() as f64
    }

    #[test]
    fn retune_raises_pitch_and_shortens_by_the_same_ratio() {
        let input = tone(200.0, 24_000.0, 1.0);
        let out = retune(&input, 12.0);
        assert!((out.len() as f64 - 12_000.0).abs() < 2.0, "{}", out.len());
        let f = frequency(&out, 24_000.0);
        assert!((f - 400.0).abs() < 5.0, "{f}");
        assert_eq!(retune(&input, 0.0).len(), input.len());
    }

    #[test]
    fn trim_drops_silence_around_the_sound() {
        let mut samples = vec![0.0_f32; 24_000];
        samples.extend(tone(300.0, 24_000.0, 0.5));
        samples.extend(vec![0.0_f32; 24_000]);
        let trimmed = trim_silence(&samples);
        assert!(trimmed.len() < 12_000 + 2 * 2048, "{}", trimmed.len());
        assert!(trimmed.len() >= 12_000);
        assert!(trim_silence(&[0.0; 5000]).is_empty());
    }
}
