//! Sample-rate conversion and channel helpers. Pure functions.

use std::f64::consts::PI;

/// Zero crossings of the sinc kept on each side of the output sample.
const ZERO_CROSSINGS: f64 = 16.0;
/// Passband edge as a fraction of the lower Nyquist frequency. A little below
/// 1 so the window's transition band stays out of the alias region.
const ROLLOFF: f64 = 0.95;

/// Converts mono audio from `from_hz` to `to_hz` with a Blackman-windowed
/// sinc. Downsampling low-passes at the new Nyquist, so nothing aliases.
/// Each call stands alone: the edges are treated as silence.
pub fn resample(input: &[f32], from_hz: f64, to_hz: f64) -> Vec<f32> {
    if input.is_empty() || from_hz <= 0.0 || to_hz <= 0.0 {
        return Vec::new();
    }
    if (from_hz - to_hz).abs() < 1e-6 {
        return input.to_vec();
    }
    let ratio = to_hz / from_hz;
    let cutoff = ratio.min(1.0) * ROLLOFF;
    let half_width = ZERO_CROSSINGS / cutoff;
    let last = input.len() as f64 - 1.0;
    let out_len = (input.len() as f64 * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for n in 0..out_len {
        let center = n as f64 / ratio;
        let lo = (center - half_width).ceil().max(0.0) as usize;
        let hi = (center + half_width).floor().min(last) as usize;
        let mut acc = 0.0;
        let mut weights = 0.0;
        for (k, &sample) in input.iter().enumerate().take(hi + 1).skip(lo) {
            let x = k as f64 - center;
            let weight = sinc(cutoff * x) * blackman(x / half_width);
            acc += f64::from(sample) * weight;
            weights += weight;
        }
        // Normalizing keeps unity gain at DC whatever the fractional phase.
        out.push(if weights.abs() > 1e-9 {
            (acc / weights) as f32
        } else {
            0.0
        });
    }
    out
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    }
}

/// Blackman window over `u` in [-1, 1].
fn blackman(u: f64) -> f64 {
    if u.abs() >= 1.0 {
        return 0.0;
    }
    0.42 + 0.5 * (PI * u).cos() + 0.08 * (2.0 * PI * u).cos()
}

/// One channel out of a buffer with `channels` channels, interleaved or not.
pub fn first_channel(data: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return data.to_vec();
    }
    data.iter().step_by(channels).copied().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, rate: f64, len: usize) -> Vec<f32> {
        (0..len)
            .map(|n| (2.0 * PI * freq * n as f64 / rate).sin() as f32)
            .collect()
    }

    fn rms(samples: &[f32]) -> f64 {
        let sum: f64 = samples.iter().map(|&s| f64::from(s).powi(2)).sum();
        (sum / samples.len() as f64).sqrt()
    }

    #[test]
    fn same_rate_and_empty_input_are_passthrough() {
        assert_eq!(resample(&[0.1, 0.2], 24_000.0, 24_000.0), vec![0.1, 0.2]);
        assert!(resample(&[], 24_000.0, 48_000.0).is_empty());
        assert!(resample(&[0.1], 0.0, 48_000.0).is_empty());
    }

    #[test]
    fn length_follows_the_ratio() {
        assert_eq!(resample(&[0.0; 2400], 24_000.0, 48_000.0).len(), 4800);
        assert_eq!(resample(&[0.0; 2400], 24_000.0, 44_100.0).len(), 4410);
        assert_eq!(resample(&[0.0; 4800], 48_000.0, 16_000.0).len(), 1600);
    }

    #[test]
    fn dc_keeps_its_level() {
        let out = resample(&[0.5; 2400], 24_000.0, 44_100.0);
        assert!(out.iter().all(|&s| (s - 0.5).abs() < 1e-3), "{out:?}");
    }

    #[test]
    fn upsampling_keeps_a_tone() {
        let out = resample(&sine(1000.0, 24_000.0, 2400), 24_000.0, 48_000.0);
        let expected = sine(1000.0, 48_000.0, 4800);
        // Skip the edges, which see silence beyond the input.
        let worst = out[200..4600]
            .iter()
            .zip(&expected[200..4600])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0_f32, f32::max);
        assert!(worst < 0.01, "worst error {worst}");
    }

    #[test]
    fn downsampling_removes_what_would_alias() {
        // 10 kHz does not fit under the 8 kHz Nyquist of 16 kHz.
        let out = resample(&sine(10_000.0, 48_000.0, 4800), 48_000.0, 16_000.0);
        assert!(rms(&out[100..1500]) < 0.01, "rms {}", rms(&out[100..1500]));
        // 1 kHz passes untouched (a sine has rms 1/sqrt(2)).
        let out = resample(&sine(1000.0, 48_000.0, 4800), 48_000.0, 16_000.0);
        assert!((rms(&out[100..1500]) - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.01);
    }

    #[test]
    fn first_channel_of_interleaved_audio() {
        assert_eq!(first_channel(&[1.0, 9.0, 2.0, 9.0], 2), vec![1.0, 2.0]);
        assert_eq!(first_channel(&[1.0, 2.0], 1), vec![1.0, 2.0]);
    }
}
