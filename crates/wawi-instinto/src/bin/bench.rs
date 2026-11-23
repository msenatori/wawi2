//! `instinto-bench`: the eval harness that decides whether the observer
//! gets turned on. It runs the observer's exact prompt and the code's
//! exact validation over hand-labeled conversation snippets
//! (`bench/charlas.tsv`), with the real `claude` CLI, and reports
//! precision (what matters most), recall and false positives per model.
//!
//! ```sh
//! instinto-bench run --split dev                      # tune here only
//! instinto-bench run --split test                     # held out: run once
//! instinto-bench run --modelo claude-haiku-5-5 --esfuerzo ""
//! instinto-bench report
//! ```
//!
//! Each call spends from the owner's subscription. It never touches
//! `~/.wawi2`: the model runs in an empty temp folder and nothing is
//! written to any queue.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use wawi_instinto::claude::{ClaudeCli, Modelo, Pedido};
use wawi_instinto::observador::{self, Decision, Reglas, Turno};
use wawi_tools::FakeClock;

#[derive(Clone, Debug)]
struct Row {
    id: String,
    split: String,
    positive: bool,
    category: String,
    now: String,
    date: Option<String>,
    turns: Vec<Turno>,
}

fn parse_rows(text: &str) -> Result<Vec<Row>, String> {
    let mut rows = Vec::new();
    for (n, line) in text.lines().enumerate().skip(1) {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        let [id, split, label, category, now, date, chat] = cols.as_slice() else {
            return Err(format!("línea {}: hacen falta 7 columnas", n + 1));
        };
        let turns = chat
            .split(" || ")
            .map(|turn| {
                let turn = turn.trim().strip_prefix("D:").unwrap_or(turn).trim();
                let (dueno, wawi) = turn.split_once(" | W:").unwrap_or((turn, ""));
                Turno {
                    dueno: dueno.trim().to_owned(),
                    wawi: wawi.trim().to_owned(),
                    iniciativa: false,
                }
            })
            .collect();
        rows.push(Row {
            id: (*id).to_owned(),
            split: (*split).to_owned(),
            positive: *label == "si",
            category: (*category).to_owned(),
            now: (*now).to_owned(),
            date: Some((*date).to_owned()).filter(|d| d != "-"),
            turns,
        });
    }
    Ok(rows)
}

struct Options {
    modelo: String,
    esfuerzo: String,
    split: String,
    paralelo: usize,
    charlas: PathBuf,
    salida: PathBuf,
}

fn options(args: &[String]) -> Result<Options, String> {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("bench");
    let mut o = Options {
        modelo: "claude-sonnet-5-5".into(),
        esfuerzo: "low".into(),
        split: "dev".into(),
        paralelo: 4,
        charlas: base.join("charlas.tsv"),
        salida: base.join("resultados"),
    };
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or(format!("{flag} necesita un valor"))
        };
        match flag.as_str() {
            "--modelo" => o.modelo = value()?,
            "--esfuerzo" => o.esfuerzo = value()?,
            "--split" => o.split = value()?,
            "--paralelo" => o.paralelo = value()?.parse().map_err(|_| "--paralelo es un número")?,
            "--charlas" => o.charlas = value()?.into(),
            "--salida" => o.salida = value()?.into(),
            other => return Err(format!("no conozco {other}")),
        }
    }
    if !["dev", "test", "todo"].contains(&o.split.as_str()) {
        return Err("--split es dev, test o todo".into());
    }
    Ok(o)
}

/// One snippet through the observer's prompt and the code's validation.
fn evaluate(row: &Row, model: &dyn Modelo, o: &Options) -> Value {
    let Some(now) = wawi_tools::clock::parse_local(&row.now) else {
        return json!({ "id": row.id, "error": "ahora inválido" });
    };
    let clock = FakeClock::at(now, -3);
    let schema = observador::esquema();
    let message = observador::mensaje(&now, &row.turns, &[]);
    let effort = Some(o.esfuerzo.as_str()).filter(|e| !e.is_empty());
    let pedido = Pedido {
        modelo: &o.modelo,
        esfuerzo: effort,
        sistema: observador::SISTEMA,
        mensaje: &message,
        esquema: &schema,
        timeout: std::time::Duration::from_secs(120),
    };
    let answer = model
        .preguntar(&pedido)
        .or_else(|_| model.preguntar(&pedido));
    let base = json!({ "id": row.id, "split": row.split, "esperado": row.positive, "categoria": row.category,
        "modelo": o.modelo, "esfuerzo": o.esfuerzo });
    let answer = match answer {
        Ok(a) => a,
        Err(e) => {
            let mut v = base;
            v["error"] = json!(e);
            return v;
        }
    };
    let reglas = Reglas {
        confianza: wawi_instinto::config::Observador::default().confianza,
        max_dia: u32::MAX,
    };
    let acciones = observador::acciones(&answer.valor);
    let raw = acciones
        .iter()
        .any(|a| a.op == "crear" && matches!(a.tipo.as_str(), "fecha" | "compromiso"));
    let mut created = Vec::new();
    let mut reasons = Vec::new();
    for a in &acciones {
        match observador::validar(a, &row.turns, &[], &reglas, 0, clock.as_ref(), &|_| false) {
            Decision::Crear { .. } => created.push(a.cuando.clone().unwrap_or_default()),
            Decision::Rechazo(why) | Decision::Sugerencia(why) => reasons.push(why),
            other => reasons.push(format!("{other:?}")),
        }
    }
    let date_ok = match &row.date {
        Some(want) => created.iter().any(|c| c.get(..10) == Some(want.as_str())),
        None => true,
    };
    let mut v = base;
    v["crudo"] = json!(raw);
    v["validado"] = json!(!created.is_empty());
    v["fecha_ok"] = json!(date_ok);
    v["fechas"] = json!(created);
    v["motivos"] = json!(reasons);
    v["acciones"] = answer.valor["acciones"].clone();
    v["ms"] = json!(answer.ms as u64);
    v["costo_usd"] = json!(answer.costo_usd);
    v
}

fn run(o: &Options) -> Result<(), String> {
    let text =
        std::fs::read_to_string(&o.charlas).map_err(|e| format!("{}: {e}", o.charlas.display()))?;
    let rows: Vec<Row> = parse_rows(&text)?
        .into_iter()
        .filter(|r| o.split == "todo" || r.split == o.split)
        .collect();
    let dir = std::env::temp_dir().join(format!("instinto-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let model = Arc::new(ClaudeCli::new(&dir));
    let next = Arc::new(AtomicUsize::new(0));
    let results = Arc::new(Mutex::new(Vec::new()));
    let rows = Arc::new(rows);
    let opts = Arc::new(Options {
        modelo: o.modelo.clone(),
        esfuerzo: o.esfuerzo.clone(),
        split: o.split.clone(),
        paralelo: o.paralelo,
        charlas: o.charlas.clone(),
        salida: o.salida.clone(),
    });
    eprintln!(
        "{} charlas con {} {} ({} a la vez)…",
        rows.len(),
        o.modelo,
        o.esfuerzo,
        o.paralelo
    );
    let workers: Vec<_> = (0..o.paralelo.max(1))
        .map(|_| {
            let (model, next, results, rows, opts) = (
                model.clone(),
                next.clone(),
                results.clone(),
                rows.clone(),
                opts.clone(),
            );
            std::thread::spawn(move || {
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(row) = rows.get(i) else { break };
                    let v = evaluate(row, model.as_ref(), &opts);
                    eprintln!(
                        "  {} {}",
                        row.id,
                        if v.get("error").is_some() {
                            "error"
                        } else {
                            "listo"
                        }
                    );
                    results.lock().unwrap().push(v);
                }
            })
        })
        .collect();
    for w in workers {
        let _ = w.join();
    }
    let _ = std::fs::remove_dir_all(&dir);
    let mut results = results.lock().unwrap().clone();
    results.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    std::fs::create_dir_all(&o.salida).map_err(|e| e.to_string())?;
    let name = format!(
        "{}-{}-{}.jsonl",
        o.modelo,
        if o.esfuerzo.is_empty() {
            "def"
        } else {
            &o.esfuerzo
        },
        o.split
    );
    let path = o.salida.join(name);
    let mut file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
    for r in &results {
        let _ = writeln!(file, "{r}");
    }
    eprintln!("resultados en {}", path.display());
    print!("{}", report_rows(&results));
    Ok(())
}

#[derive(Default)]
struct Counts {
    n: u32,
    tp: u32,
    fp: u32,
    fn_: u32,
    tn: u32,
    raw_tp: u32,
    raw_fp: u32,
    errors: u32,
    ms: u64,
    cost: f64,
    fps: Vec<String>,
    fns: Vec<String>,
}

fn report_rows(results: &[Value]) -> String {
    let mut groups: BTreeMap<(String, String, String), Counts> = BTreeMap::new();
    for r in results {
        let key = (
            r["modelo"].as_str().unwrap_or("?").to_owned(),
            r["esfuerzo"].as_str().unwrap_or("").to_owned(),
            r["split"].as_str().unwrap_or("?").to_owned(),
        );
        let c = groups.entry(key).or_default();
        c.n += 1;
        let id = r["id"].as_str().unwrap_or("?").to_owned();
        if r.get("error").is_some() {
            c.errors += 1;
            continue;
        }
        let positive = r["esperado"].as_bool().unwrap_or(false);
        let said = r["validado"].as_bool().unwrap_or(false);
        let right = said && r["fecha_ok"].as_bool().unwrap_or(false);
        let raw = r["crudo"].as_bool().unwrap_or(false);
        c.ms += r["ms"].as_u64().unwrap_or(0);
        c.cost += r["costo_usd"].as_f64().unwrap_or(0.0);
        match (positive, said) {
            (true, true) if right => c.tp += 1,
            (true, true) => {
                // A reminder for the wrong day: a false alarm and a miss.
                c.fp += 1;
                c.fn_ += 1;
                c.fps.push(format!("{id}(fecha)"));
            }
            (true, false) => {
                c.fn_ += 1;
                c.fns.push(id.clone());
            }
            (false, true) => {
                c.fp += 1;
                c.fps.push(id.clone());
            }
            (false, false) => c.tn += 1,
        }
        match (positive, raw) {
            (true, true) => c.raw_tp += 1,
            (false, true) => c.raw_fp += 1,
            _ => {}
        }
    }
    let pct = |a: u32, b: u32| {
        if b == 0 {
            "-".to_owned()
        } else {
            format!("{:.0}%", 100.0 * a as f64 / b as f64)
        }
    };
    let mut out = String::from(
        "modelo\tesfuerzo\tsplit\tn\tTP\tFP\tFN\tTN\tprecisión\trecall\tcrudo(TP/FP)\terrores\tms medio\tcosto USD\n",
    );
    for ((m, e, s), c) in &groups {
        let ok = c.n - c.errors;
        out.push_str(&format!(
            "{m}\t{e}\t{s}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}/{}\t{}\t{}\t{:.3}\n",
            c.n,
            c.tp,
            c.fp,
            c.fn_,
            c.tn,
            pct(c.tp, c.tp + c.fp),
            pct(c.tp, c.tp + c.fn_),
            c.raw_tp,
            c.raw_fp,
            c.errors,
            if ok == 0 { 0 } else { c.ms / u64::from(ok) },
            c.cost
        ));
        if !c.fps.is_empty() {
            out.push_str(&format!("  falsos positivos: {}\n", c.fps.join(", ")));
        }
        if !c.fns.is_empty() {
            out.push_str(&format!("  se le pasaron: {}\n", c.fns.join(", ")));
        }
    }
    out
}

fn report(o: &Options) -> Result<(), String> {
    let mut results = Vec::new();
    let entries =
        std::fs::read_dir(&o.salida).map_err(|e| format!("{}: {e}", o.salida.display()))?;
    let mut paths: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    paths.sort();
    for path in paths
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
    {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        results.extend(
            text.lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok()),
        );
    }
    print!("{}", report_rows(&results));
    Ok(())
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mode, rest) = args
        .split_first()
        .map_or(("", &[][..]), |(m, r)| (m.as_str(), r));
    let outcome = options(rest).and_then(|o| match mode {
        "run" => run(&o),
        "report" => report(&o),
        _ => Err("uso: instinto-bench run|report [--modelo M] [--esfuerzo E] [--split dev|test|todo] [--paralelo N] [--charlas F] [--salida D]".into()),
    });
    match outcome {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            std::process::ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_labeled_set_is_well_formed() {
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("bench/charlas.tsv"),
        )
        .unwrap();
        let rows = parse_rows(&text).unwrap();
        assert_eq!(rows.len(), 60);
        let positives = rows.iter().filter(|r| r.positive).count();
        assert_eq!(positives, 20);
        assert!(rows.iter().all(|r| r.positive == r.date.is_some()));
        assert!(
            rows.iter()
                .all(|r| wawi_tools::clock::parse_local(&r.now).is_some())
        );
        assert_eq!(rows.iter().filter(|r| r.split == "dev").count(), 20);
        let multi = rows.iter().find(|r| r.id == "d05").unwrap();
        assert_eq!(multi.turns.len(), 2);
        assert!(multi.turns[1].dueno.contains("Bariloche"));
        assert!(multi.turns[1].wawi.contains("abrigo"));
    }

    #[test]
    fn report_counts_a_wrong_date_as_both() {
        let r = |id: &str, pos: bool, said: bool, ok: bool| {
            json!({ "id": id, "split": "dev", "modelo": "m", "esfuerzo": "low",
            "esperado": pos, "validado": said, "fecha_ok": ok, "crudo": said, "ms": 10, "costo_usd": 0.01 })
        };
        let out = report_rows(&[
            r("a", true, true, true),
            r("b", true, true, false),
            r("c", false, true, true),
            r("d", false, false, true),
        ]);
        assert!(
            out.contains("m\tlow\tdev\t4\t1\t2\t1\t1\t33%\t50%"),
            "{out}"
        );
    }
}
