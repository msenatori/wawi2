//! Level measurements. Pure functions, no audio I/O.

/// What "digital silence" is reported as, instead of negative infinity.
pub const MIN_DB: f64 = -120.0;
/// Voice processing can gate silence to near zero; a floor below this would make
/// "floor + 10 dB" a meaningless threshold.
pub const FLOOR_CLAMP_DB: f64 = -90.0;
pub const FRAME_SECONDS: f64 = 0.030;

fn to_db(linear: f64) -> f64 {
    if linear <= 0.0 {
        MIN_DB
    } else {
        (20.0 * linear.log10()).max(MIN_DB)
    }
}

pub fn rms_db(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return MIN_DB;
    }
    let sum: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    to_db((sum / samples.len() as f64).sqrt())
}

pub fn peak_db(samples: &[f32]) -> f64 {
    to_db(
        samples
            .iter()
            .fold(0.0_f64, |m, &s| m.max(f64::from(s.abs()))),
    )
}

pub fn is_all_zero(samples: &[f32]) -> bool {
    samples.iter().all(|&s| s == 0.0)
}

fn frame_len(sample_rate: f64) -> usize {
    ((sample_rate * FRAME_SECONDS).round() as usize).max(1)
}

/// How much of a window would look like sound to a level-based detector.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trigger {
    pub threshold_db: f64,
    pub fraction: f64,
    pub longest_run_ms: f64,
}

/// Fraction of 30 ms frames above `threshold_db`, and the longest unbroken run of them.
pub fn trigger(samples: &[f32], sample_rate: f64, threshold_db: f64) -> Trigger {
    let (mut total, mut above, mut run, mut longest) = (0_usize, 0_usize, 0_usize, 0_usize);
    for frame in samples.chunks_exact(frame_len(sample_rate)) {
        total += 1;
        if rms_db(frame) > threshold_db {
            above += 1;
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    Trigger {
        threshold_db,
        fraction: if total == 0 {
            0.0
        } else {
            above as f64 / total as f64
        },
        longest_run_ms: longest as f64 * FRAME_SECONDS * 1000.0,
    }
}

/// Median 30 ms frame level, clamped. Unlike RMS, one knock on the desk during the
/// silence doesn't move it.
pub fn noise_floor_db(samples: &[f32], sample_rate: f64) -> f64 {
    let mut frames: Vec<f64> = samples
        .chunks_exact(frame_len(sample_rate))
        .map(rms_db)
        .collect();
    if frames.is_empty() {
        return FLOOR_CLAMP_DB;
    }
    frames.sort_by(f64::total_cmp);
    frames[frames.len() / 2].max(FLOOR_CLAMP_DB)
}

fn envelope(samples: &[f32], hop: usize) -> Vec<f64> {
    samples
        .chunks_exact(hop)
        .map(|c| (c.iter().map(|&s| f64::from(s) * f64::from(s)).sum::<f64>() / hop as f64).sqrt())
        .collect()
}

fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    if n < 2 {
        return 0.0;
    }
    let (a, b) = (&a[..n], &b[..n]);
    let mean_a = a.iter().sum::<f64>() / n as f64;
    let mean_b = b.iter().sum::<f64>() / n as f64;
    let (mut cov, mut var_a, mut var_b) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        cov += (x - mean_a) * (y - mean_b);
        var_a += (x - mean_a) * (x - mean_a);
        var_b += (y - mean_b) * (y - mean_b);
    }
    if var_a <= 0.0 || var_b <= 0.0 {
        0.0
    } else {
        cov / (var_a * var_b).sqrt()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EchoMatch {
    pub correlation: f64,
    pub lag_ms: f64,
}

/// Does the capture follow the loudness contour of what was played? Separates residual
/// echo from unrelated room sound, which energy alone can't do. Works on 10 ms envelopes,
/// so it is immune to gain changes and only roughly locates the delay.
pub fn echo_match(source: &[f32], capture: &[f32], sample_rate: f64, max_lag_ms: f64) -> EchoMatch {
    let hop = ((sample_rate * 0.010).round() as usize).max(1);
    let reference = envelope(source, hop);
    let captured = envelope(capture, hop);
    let mut best = EchoMatch {
        correlation: 0.0,
        lag_ms: 0.0,
    };
    for lag in 0..=(max_lag_ms / 10.0) as usize {
        if lag >= captured.len() {
            break;
        }
        let correlation = pearson(&reference, &captured[lag..]);
        if correlation > best.correlation {
            best = EchoMatch {
                correlation,
                lag_ms: lag as f64 * 10.0,
            };
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_scale_square_is_zero_db() {
        let s = [1.0_f32, -1.0, 1.0, -1.0];
        assert!(rms_db(&s).abs() < 1e-9);
        assert!(peak_db(&s).abs() < 1e-9);
    }

    #[test]
    fn half_amplitude_is_about_minus_six_db() {
        assert!((rms_db(&[0.5_f32; 100]) + 6.0206).abs() < 0.001);
    }

    #[test]
    fn silence_and_empty_report_the_minimum() {
        assert_eq!(rms_db(&[0.0; 10]), MIN_DB);
        assert_eq!(rms_db(&[]), MIN_DB);
        assert_eq!(peak_db(&[]), MIN_DB);
    }

    #[test]
    fn all_zero_is_exact() {
        assert!(is_all_zero(&[0.0; 4]));
        assert!(!is_all_zero(&[0.0, 1e-9, 0.0]));
    }

    #[test]
    fn trigger_counts_frames_and_longest_run() {
        // 1000 Hz sample rate, so one frame is 30 samples.
        let loud = [0.5_f32; 30];
        let quiet = [0.0_f32; 30];
        let mut s = Vec::new();
        for frame in [&loud, &loud, &quiet, &loud] {
            s.extend_from_slice(frame);
        }
        let t = trigger(&s, 1000.0, -20.0);
        assert_eq!(t.fraction, 0.75);
        assert!((t.longest_run_ms - 60.0).abs() < 1e-9);
    }

    #[test]
    fn trigger_ignores_a_partial_trailing_frame_and_empty_input() {
        assert_eq!(trigger(&[0.5; 29], 1000.0, -20.0).fraction, 0.0);
        assert_eq!(trigger(&[], 1000.0, -20.0).fraction, 0.0);
    }

    #[test]
    fn floor_ignores_a_single_loud_frame_and_clamps_gated_silence() {
        let mut s = vec![0.001_f32; 3000];
        s[100..130].fill(1.0);
        let floor = noise_floor_db(&s, 1000.0);
        assert!((floor + 60.0).abs() < 0.01, "{floor}");
        assert_eq!(noise_floor_db(&[0.0; 3000], 1000.0), FLOOR_CLAMP_DB);
        assert_eq!(noise_floor_db(&[], 1000.0), FLOOR_CLAMP_DB);
    }

    fn bursts(len: usize) -> Vec<f32> {
        // Irregular on/off contour, so only the right lag lines up.
        (0..len)
            .map(|i| {
                let block = i / 100;
                if (block * block + block / 3) % 5 < 2 {
                    0.5
                } else {
                    0.0
                }
            })
            .collect()
    }

    #[test]
    fn echo_match_finds_a_scaled_delayed_copy() {
        let source = bursts(5000);
        // 1000 Hz sample rate: 50 samples is 50 ms.
        let mut capture = vec![0.0_f32; 50];
        capture.extend(source.iter().map(|s| s * 0.01));
        let found = echo_match(&source, &capture, 1000.0, 200.0);
        assert!(found.correlation > 0.99, "{found:?}");
        assert_eq!(found.lag_ms, 50.0);
    }

    #[test]
    fn echo_match_is_zero_for_a_flat_capture() {
        let found = echo_match(&bursts(5000), &[0.2_f32; 5000], 1000.0, 200.0);
        assert_eq!(found.correlation, 0.0);
    }
}
