//! The phonemizer against espeak-ng, which Kokoro was trained with and Wawi 1
//! used. `espeak_es.tsv` holds espeak-ng's `es` output (through kokoro-onnx,
//! as Wawi 1 runs it) for 99 everyday sentences. The Castilian accent is
//! compared, since that is the one espeak-ng's `es` has.
//!
//! `cargo test -p wawi-tts-kokoro --test espeak_parity -- --nocapture` lists
//! every sentence that differs.

use wawi_tts_kokoro::g2p::{Accent, phonemize};

fn distance(a: &[char], b: &[char]) -> usize {
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let here = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != cb)).min(row[j] + 1).min(here + 1);
            prev = here;
        }
    }
    row[b.len()]
}

#[test]
fn close_to_espeak_ng() {
    let reference = include_str!("espeak_es.tsv");
    let (mut errors, mut total, mut exact, mut lines) = (0, 0, 0, 0);
    for line in reference.lines() {
        let Some((text, expected)) = line.split_once('\t') else {
            continue;
        };
        let expected: Vec<char> = expected.trim().chars().collect();
        let got: Vec<char> = phonemize(text, Accent::Castilian).chars().collect();
        let d = distance(&got, &expected);
        if d == 0 {
            exact += 1;
        } else {
            println!(
                "{d:>2}  {text}\n    ours:   {}\n    espeak: {}",
                got.iter().collect::<String>(),
                expected.iter().collect::<String>()
            );
        }
        errors += d;
        total += expected.len();
        lines += 1;
    }
    let rate = errors as f64 / total as f64;
    println!(
        "{exact}/{lines} identical, {errors} symbol edits in {total} ({:.2} %)",
        rate * 100.0
    );
    assert_eq!(lines, 99);
    assert!(rate < 0.01, "phoneme error rate {:.2} %", rate * 100.0);
}
