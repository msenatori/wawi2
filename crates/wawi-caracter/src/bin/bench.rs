//! `caracter-bench`: decides whether the learner gets turned on. It runs
//! the learner's exact prompt, prefilter and validation (repetition
//! included) over hand-labeled conversations (`bench/turnos.tsv`), with the
//! real `claude` CLI, and reports precision first.
//!
//! ```sh
//! caracter-bench run --split dev      # tune here only
//! caracter-bench run --split test     # held out: run once
//! caracter-bench run --modelo claude-haiku-5-5 --esfuerzo ""
//! ```
//!
//! Every conversation starts from an empty temp home; `~/.wawi2` is never
//! touched. Each call spends from the owner's subscription.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use wawi_caracter::aprender::Turno;
use wawi_caracter::libro::{Aprendiz, Libro};
use wawi_caracter::log::Log;
use wawi_instinto::claude::{ClaudeCli, Modelo, Pedido, Respuesta};
use wawi_tools::{Civil, FakeClock};

/// The words a learned rule needs for each expected kind.
fn matches(kind: &str, rule: &str) -> bool {
    let rule = wawi_core::normalize(rule);
    let any = |words: &[&str]| words.iter().any(|w| rule.contains(w));
    match kind {
        "corto" => any(&[
            "cort",
            "breve",
            "resum",
            "menos",
            "al pie",
            "una frase",
            "dos frases",
            "explic",
            "direct",
            "justo",
            "larg",
            "concis",
            "grano",
            "vuelta",
        ]),
        "tranquila" => any(&[
            "tranq", "despac", "calma", "pausad", "lent", "relaj", "apur",
        ]),
        "humor_palabras" => any(&["juego", "palabra", "chiste", "humor", "gracia", "broma"]),
        "sin_chistes" => {
            any(&["chiste", "humor", "graci", "broma"])
                && any(&["sin", "no ", "evit", "menos", "deja"])
        }
        "sin_preguntas" => any(&["pregunt"]),
        "sin_che" => any(&[
            "boludo",
            "che",
            "groser",
            "malas palabras",
            "lenguaje",
            "insult",
        ]),
        _ => false,
    }
}

struct Row {
    id: String,
    split: String,
    expected: Option<String>,
    category: String,
    turns: Vec<Turno>,
}

fn parse_rows(text: &str) -> Result<Vec<Row>, String> {
    let mut rows = Vec::new();
    for (n, line) in text.lines().enumerate().skip(1) {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        let [id, split, expected, category, chat] = cols.as_slice() else {
            return Err(format!("línea {}: hacen falta 5 columnas", n + 1));
        };
        let turns = chat
            .split(" || ")
            .enumerate()
            .map(|(i, turn)| {
                let parts: Vec<&str> = turn.split(" | ").map(str::trim).collect();
                let owner = parts[0].strip_prefix("D:").unwrap_or(parts[0]).trim();
                let wawi = parts
                    .get(1)
                    .map(|w| w.strip_prefix("W:").unwrap_or(w).trim())
                    .unwrap_or("");
                Turno {
                    id: i as u64 + 1,
                    dueno: owner.to_owned(),
                    wawi: wawi.to_owned(),
                    interrumpida: parts.contains(&"cortada"),
                    iniciativa: parts.contains(&"sola"),
                }
            })
            .collect();
        rows.push(Row {
            id: (*id).to_owned(),
            split: (*split).to_owned(),
            expected: Some((*expected).to_owned()).filter(|e| e != "no"),
            category: (*category).to_owned(),
            turns,
        });
    }
    Ok(rows)
}

/// Counts calls and cost around the real model.
struct Counted {
    inner: ClaudeCli,
    calls: Arc<AtomicUsize>,
    cost: Arc<Mutex<f64>>,
    proposed: Arc<Mutex<Vec<Value>>>,
}

impl Modelo for Counted {
    fn preguntar(&self, pedido: &Pedido<'_>) -> Result<Respuesta, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let answer = self
            .inner
            .preguntar(pedido)
            .or_else(|_| self.inner.preguntar(pedido))?;
        *self.cost.lock().unwrap() += answer.costo_usd;
        self.proposed.lock().unwrap().push(answer.valor.clone());
        Ok(answer)
    }
}

fn evaluate(row: &Row, modelo: &str, esfuerzo: &str, scratch: &Path) -> Value {
    let home = scratch.join(&row.id);
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let cost = Arc::new(Mutex::new(0.0));
    let proposed = Arc::new(Mutex::new(Vec::new()));
    let model = Counted {
        inner: ClaudeCli::new(&scratch.join("claude")),
        calls: calls.clone(),
        cost: cost.clone(),
        proposed: proposed.clone(),
    };
    let libro = Arc::new(Libro::new(&home, Log::none()));
    let clock = FakeClock::at(Civil::date(2026, 11, 21).at(10, 0, 0), -3);
    let mut aprendiz = Aprendiz::new(libro, Box::new(model), clock.clone(), modelo, esfuerzo);
    let mut ms = 0;
    for turn in &row.turns {
        let started = std::time::Instant::now();
        aprendiz.procesar(turn.clone());
        ms += started.elapsed().as_millis();
        clock.advance(60);
    }
    let rules = wawi_memory::Home::open(&home)
        .map(|h| h.trato().rules)
        .unwrap_or_default();
    let hit = row
        .expected
        .as_deref()
        .is_some_and(|kind| rules.iter().any(|r| matches(kind, r)));
    let wrong = rules
        .iter()
        .filter(|r| row.expected.as_deref().is_none_or(|kind| !matches(kind, r)))
        .count();
    let _ = std::fs::remove_dir_all(&home);
    let n = calls.load(Ordering::SeqCst);
    json!({
        "id": row.id, "split": row.split, "esperado": row.expected, "categoria": row.category,
        "reglas": rules, "acierto": hit, "falsas": wrong, "llamadas": n,
        "ms_por_llamada": if n > 0 { ms / n as u128 } else { 0 },
        "costo_usd": *cost.lock().unwrap(),
        "propuso": *proposed.lock().unwrap(),
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("run") {
        eprintln!("uso: caracter-bench run [--split dev|test|todo] [--modelo M] [--esfuerzo E]");
        std::process::exit(2);
    }
    let (mut split, mut modelo, mut esfuerzo) = (
        "dev".to_owned(),
        wawi_caracter::MODELO.to_owned(),
        wawi_caracter::ESFUERZO.to_owned(),
    );
    let mut it = args.iter().skip(1);
    while let Some(flag) = it.next() {
        let value = it.next().cloned().unwrap_or_default();
        match flag.as_str() {
            "--split" => split = value,
            "--modelo" => modelo = value,
            "--esfuerzo" => esfuerzo = value,
            other => {
                eprintln!("no conozco {other}");
                std::process::exit(2);
            }
        }
    }
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("bench");
    let text = std::fs::read_to_string(base.join("turnos.tsv")).expect("bench/turnos.tsv");
    let rows: Vec<Row> = parse_rows(&text)
        .expect("turnos.tsv")
        .into_iter()
        .filter(|r| split == "todo" || r.split == split)
        .collect();
    let scratch: PathBuf =
        std::env::temp_dir().join(format!("caracter-bench-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    // Four conversations at a time, each on its own thread.
    let results = Arc::new(Mutex::new(Vec::new()));
    let next = Arc::new(AtomicUsize::new(0));
    let rows = Arc::new(rows);
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let (rows, next, results, scratch) =
                (rows.clone(), next.clone(), results.clone(), scratch.clone());
            let (modelo, esfuerzo) = (modelo.clone(), esfuerzo.clone());
            std::thread::spawn(move || {
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(row) = rows.get(i) else { break };
                    let result = evaluate(row, &modelo, &esfuerzo, &scratch);
                    eprintln!("{} {}", row.id, result["reglas"]);
                    results.lock().unwrap().push(result);
                }
            })
        })
        .collect();
    for w in workers {
        let _ = w.join();
    }
    let mut results = results.lock().unwrap().clone();
    results.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let out = base.join("resultados");
    let _ = std::fs::create_dir_all(&out);
    let file = out.join(format!("{modelo}-{esfuerzo}-{split}.jsonl"));
    let lines: Vec<String> = results.iter().map(Value::to_string).collect();
    let _ = std::fs::write(&file, lines.join("\n") + "\n");
    let (mut tp, mut fp, mut fn_, mut tn) = (0u64, 0u64, 0u64, 0u64);
    let (mut calls, mut cost, mut ms) = (0, 0.0, Vec::new());
    for r in &results {
        let positive = !r["esperado"].is_null();
        let hit = r["acierto"].as_bool().unwrap_or(false);
        let wrong = r["falsas"].as_u64().unwrap_or(0);
        match (positive, hit) {
            (true, true) => tp += 1,
            (true, false) => fn_ += 1,
            (false, _) if wrong == 0 => tn += 1,
            _ => {}
        }
        fp += wrong;
        calls += r["llamadas"].as_u64().unwrap_or(0);
        cost += r["costo_usd"].as_f64().unwrap_or(0.0);
        if r["llamadas"].as_u64().unwrap_or(0) > 0 {
            ms.push(r["ms_por_llamada"].as_u64().unwrap_or(0));
        }
    }
    let ratio = |a: u64, b: u64| {
        if a + b > 0 {
            a as f64 / (a + b) as f64
        } else {
            1.0
        }
    };
    let mean_ms = if ms.is_empty() {
        0
    } else {
        ms.iter().sum::<u64>() / ms.len() as u64
    };
    println!(
        "{modelo} {esfuerzo} {split}: n={} TP={tp} FP={fp} FN={fn_} TN={tn} precisión={:.0}% recall={:.0}% llamadas={calls} tiempo medio por llamada={:.1}s costo=US${cost:.3}",
        results.len(),
        ratio(tp, fp) * 100.0,
        ratio(tp, fn_) * 100.0,
        mean_ms as f64 / 1000.0
    );
    println!("resultados en {}", file.display());
    let _ = std::fs::remove_dir_all(&scratch);
}
