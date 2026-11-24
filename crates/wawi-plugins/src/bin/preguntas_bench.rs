//! The bench for unanswered questions (`preguntas.rs`): hand-labeled
//! chats in `bench/preguntas.tsv`, run through the same tracker the host
//! uses. No model, no network, no files: it runs in milliseconds.
//!
//! ```sh
//! cargo run -p wawi-plugins --bin preguntas-bench
//! cargo run -p wawi-plugins --bin preguntas-bench -- --split test
//! ```
//!
//! A chat is turns separated by ` || `. A turn is `D: <owner> | W: <Wawi>`,
//! `I: W: <Wawi>` for her initiative, `| cortada` at the end when the owner
//! cut her off, and `@<minutes> ` in front when it came that long after the
//! one before (1 minute otherwise). After the last turn the owner is gone:
//! the clock jumps past the window. `retomar` means Wawi should bring a
//! question back later; `no`, that she should not.

use wawi_plugins::preguntas::{Config, Seen, Tracker, Turn};

const DATA: &str = include_str!("../../bench/preguntas.tsv");

#[derive(Debug)]
struct Case {
    id: String,
    split: String,
    expected: bool,
    category: String,
    chat: String,
}

struct Outcome {
    case: Case,
    retaken: Vec<String>,
}

fn parse(data: &str) -> Vec<Case> {
    data.lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let cols: Vec<&str> = line.split('\t').collect();
            assert_eq!(cols.len(), 5, "bad line: {line}");
            Case {
                id: cols[0].into(),
                split: cols[1].into(),
                expected: match cols[2] {
                    "retomar" => true,
                    "no" => false,
                    other => panic!("{}: bad label {other}", cols[0]),
                },
                category: cols[3].into(),
                chat: cols[4].into(),
            }
        })
        .collect()
}

/// Runs one chat through a fresh tracker. Gives the questions handed over
/// and not taken back.
fn run(case: &Case) -> Vec<String> {
    let config = Config::default();
    let window = config.window_s;
    let mut tracker = Tracker::new(config);
    let mut now: i64 = 0;
    let mut handed: Vec<(String, String)> = Vec::new();
    let apply = |seen: Vec<Seen>, handed: &mut Vec<(String, String)>| {
        for seen in seen {
            match seen {
                Seen::Unanswered(q) => handed.push((q.id, q.question)),
                Seen::Answered { id } => handed.retain(|(h, _)| *h != id),
            }
        }
    };
    for (index, raw) in case.chat.split(" || ").enumerate() {
        let mut raw = raw.trim();
        if let Some(rest) = raw.strip_prefix('@') {
            let (minutes, rest) = rest.split_once(' ').expect("@N turn");
            now += minutes.parse::<i64>().expect("minutes") * 60;
            raw = rest;
        } else if index > 0 {
            now += 60;
        }
        let initiative = raw.starts_with("I:");
        let raw = raw.trim_start_matches("I:").trim();
        let interrupted = raw.ends_with("| cortada");
        let raw = raw.trim_end_matches("| cortada").trim();
        let (owner, wawi) = match raw.split_once("| W:") {
            Some((owner, wawi)) => (owner.trim_start_matches("D:").trim(), wawi.trim()),
            None => ("", raw.trim_start_matches("W:").trim()),
        };
        let turn = Turn {
            owner,
            wawi,
            interrupted,
            initiative,
        };
        let seen = tracker.turn(&turn, now);
        apply(seen, &mut handed);
        // The host ticks all along.
        let seen = tracker.tick(now);
        apply(seen, &mut handed);
    }
    // The owner is gone.
    let seen = tracker.tick(now + window + 60);
    apply(seen, &mut handed);
    handed.into_iter().map(|(_, q)| q).collect()
}

#[derive(Default)]
struct Score {
    tp: usize,
    fp: usize,
    fn_: usize,
    tn: usize,
}

impl Score {
    fn add(&mut self, expected: bool, got: bool) {
        match (expected, got) {
            (true, true) => self.tp += 1,
            (false, true) => self.fp += 1,
            (true, false) => self.fn_ += 1,
            (false, false) => self.tn += 1,
        }
    }

    fn line(&self, name: &str) -> String {
        let pct = |a: usize, b: usize| {
            if b == 0 {
                "—".to_owned()
            } else {
                format!("{:.0} %", 100.0 * a as f64 / b as f64)
            }
        };
        format!(
            "| {name} | {} | {} | {} | {} | {} | {} | {} |",
            self.tp + self.fp + self.fn_ + self.tn,
            self.tp,
            self.fp,
            self.fn_,
            self.tn,
            pct(self.tp, self.tp + self.fp),
            pct(self.tp, self.tp + self.fn_),
        )
    }
}

fn evaluate(split: &str) -> (Vec<Outcome>, Vec<(String, Score)>) {
    let cases: Vec<Case> = parse(DATA)
        .into_iter()
        .filter(|c| split == "todo" || c.split == split)
        .collect();
    let mut outcomes = Vec::new();
    let mut scores: Vec<(String, Score)> = Vec::new();
    for case in cases {
        let retaken = run(&case);
        let got = !retaken.is_empty();
        let index = match scores.iter().position(|(s, _)| *s == case.split) {
            Some(index) => index,
            None => {
                scores.push((case.split.clone(), Score::default()));
                scores.len() - 1
            }
        };
        scores[index].1.add(case.expected, got);
        outcomes.push(Outcome { case, retaken });
    }
    (outcomes, scores)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let split = args
        .iter()
        .position(|a| a == "--split")
        .and_then(|i| args.get(i + 1))
        .map_or("todo", String::as_str);
    let (outcomes, scores) = evaluate(split);
    println!("| Parte | n | TP | FP | FN | TN | Precisión | Recall |");
    println!("| --- | --- | --- | --- | --- | --- | --- | --- |");
    for (name, score) in &scores {
        println!("{}", score.line(name));
    }
    let misses: Vec<&Outcome> = outcomes
        .iter()
        .filter(|o| o.case.expected == o.retaken.is_empty())
        .collect();
    if !misses.is_empty() {
        println!("\nErrores:");
        for miss in misses {
            println!(
                "- {} ({}): esperaba {}, {}",
                miss.case.id,
                miss.case.category,
                if miss.case.expected {
                    "retomar"
                } else {
                    "nada"
                },
                if miss.retaken.is_empty() {
                    "no retomó nada".to_owned()
                } else {
                    format!("retomó {:?}", miss.retaken)
                }
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rules never bring back a question that should not be: a wrong
    /// retake bothers more than a missed one.
    #[test]
    fn the_bench_has_no_false_alarms() {
        let (outcomes, scores) = evaluate("todo");
        assert!(outcomes.len() >= 40);
        for (name, score) in &scores {
            assert_eq!(score.fp, 0, "{name}: {}", score.line(name));
        }
        // Each retake is the question Wawi asked, word for word.
        for outcome in outcomes.iter().filter(|o| o.case.expected) {
            for question in &outcome.retaken {
                assert!(outcome.case.chat.contains(question.as_str()));
            }
        }
    }
}
