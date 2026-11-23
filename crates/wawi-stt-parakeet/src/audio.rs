//! Audio in the shape the model wants: 16 kHz, mono, f32 in [-1, 1].

use std::path::Path;

/// The only rate Parakeet accepts.
pub const MODEL_RATE: u32 = 16_000;

/// A mono clip at a known rate.
#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

impl Clip {
    pub fn seconds(&self) -> f64 {
        if self.sample_rate == 0 {
            return 0.0;
        }
        self.samples.len() as f64 / f64::from(self.sample_rate)
    }

    /// The same clip at 16 kHz.
    pub fn to_model_rate(&self) -> Vec<f32> {
        resample(&self.samples, self.sample_rate, MODEL_RATE)
    }
}

/// Reads a WAV of any common format (8-32 bit int or 32 bit float, any
/// channel count) and downmixes it to mono.
pub fn read_wav(path: &Path) -> Result<Clip, String> {
    let mut reader =
        hound::WavReader::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let spec = reader.spec();
    let channels = usize::from(spec.channels.max(1));
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<_, _>>()
            .map_err(|e| format!("{}: {e}", path.display()))?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1_u64 << (spec.bits_per_sample.clamp(1, 32) - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|s| s as f32 * scale))
                .collect::<Result<_, _>>()
                .map_err(|e| format!("{}: {e}", path.display()))?
        }
    };
    Ok(Clip {
        samples: downmix(&interleaved, channels),
        sample_rate: spec.sample_rate,
    })
}

/// Writes 16-bit mono PCM.
pub fn write_wav(path: &Path, clip: &Clip) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: clip.sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer =
        hound::WavWriter::create(path, spec).map_err(|e| format!("{}: {e}", path.display()))?;
    for &sample in &clip.samples {
        let value = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
        writer
            .write_sample(value)
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    writer
        .finalize()
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Averages interleaved channels into one.
pub fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Half-width of the sinc kernel, in input samples at the lower of the two
/// rates. 16 taps per side keeps aliasing well under speech levels and costs
/// a few milliseconds per second of 48 kHz audio.
const SINC_HALF_WIDTH: f64 = 16.0;

/// Band-limited resampling with a Hann-windowed sinc. When going down, the
/// cutoff follows the output Nyquist, so 48 kHz microphone audio does not fold
/// its 8-24 kHz content into the speech band.
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() || from == 0 || to == 0 {
        return input.to_vec();
    }
    let ratio = f64::from(to) / f64::from(from);
    let cutoff = ratio.min(1.0);
    let half = SINC_HALF_WIDTH / cutoff;
    let out_len = ((input.len() as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for n in 0..out_len {
        let center = n as f64 / ratio;
        let first = (center - half).ceil().max(0.0) as usize;
        let last = ((center + half).floor() as usize).min(input.len() - 1);
        let mut acc = 0.0_f64;
        let mut weight = 0.0_f64;
        for (i, &sample) in input.iter().enumerate().take(last + 1).skip(first) {
            let x = i as f64 - center;
            let w = cutoff * sinc(cutoff * x) * hann(x / half);
            acc += w * f64::from(sample);
            weight += w;
        }
        // Normalizing by the kernel sum keeps DC gain at 1 near the edges too.
        out.push(if weight.abs() > 1e-9 {
            (acc / weight) as f32
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
        let px = std::f64::consts::PI * x;
        px.sin() / px
    }
}

/// Hann window over [-1, 1].
fn hann(x: f64) -> f64 {
    if x.abs() >= 1.0 {
        0.0
    } else {
        0.5 * (1.0 + (std::f64::consts::PI * x).cos())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f64, rate: u32, seconds: f64) -> Vec<f32> {
        let len = (f64::from(rate) * seconds) as usize;
        (0..len)
            .map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / f64::from(rate)).sin() as f32)
            .collect()
    }

    fn rms(samples: &[f32]) -> f64 {
        let sum: f64 = samples.iter().map(|&s| f64::from(s).powi(2)).sum();
        (sum / samples.len().max(1) as f64).sqrt()
    }

    #[test]
    fn same_rate_is_a_copy() {
        let input = vec![0.1, -0.2, 0.3];
        assert_eq!(resample(&input, 16_000, 16_000), input);
    }

    #[test]
    fn length_follows_the_ratio() {
        assert_eq!(resample(&vec![0.0; 48_000], 48_000, 16_000).len(), 16_000);
        assert_eq!(resample(&vec![0.0; 44_100], 44_100, 16_000).len(), 16_000);
        assert_eq!(resample(&vec![0.0; 8_000], 8_000, 16_000).len(), 16_000);
    }

    #[test]
    fn speech_band_survives_downsampling() {
        let out = resample(&tone(1_000.0, 48_000, 0.5), 48_000, 16_000);
        // Skip the edges, where the kernel is cut off.
        let middle = &out[400..out.len() - 400];
        assert!((rms(middle) - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.01);
    }

    #[test]
    fn content_above_the_new_nyquist_is_removed() {
        // 12 kHz would alias to 4 kHz at 16 kHz without the low-pass.
        let out = resample(&tone(12_000.0, 48_000, 0.5), 48_000, 16_000);
        let middle = &out[400..out.len() - 400];
        assert!(rms(middle) < 0.01, "{}", rms(middle));
    }

    #[test]
    fn dc_is_kept() {
        let out = resample(&[0.5; 4_800], 48_000, 16_000);
        assert!(out.iter().all(|&s| (s - 0.5).abs() < 1e-4));
    }

    #[test]
    fn downmix_averages_frames() {
        assert_eq!(downmix(&[1.0, 0.0, 0.5, 0.5], 2), vec![0.5, 0.5]);
        assert_eq!(downmix(&[0.25, 0.75], 1), vec![0.25, 0.75]);
    }

    #[test]
    fn wav_round_trip() {
        let dir = std::env::temp_dir().join(format!("wawi-stt-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.wav");
        let clip = Clip {
            samples: vec![0.0, 0.5, -0.5, 0.25],
            sample_rate: 16_000,
        };
        write_wav(&path, &clip).unwrap();
        let back = read_wav(&path).unwrap();
        assert_eq!(back.sample_rate, 16_000);
        for (a, b) in back.samples.iter().zip(&clip.samples) {
            assert!((a - b).abs() < 1e-3);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
