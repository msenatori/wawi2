//! The worker end to end on a temporary home, with a fake source, a fake
//! judge and a fake clock: questions in, state machine, facts out, crash
//! safety and restarts. No network, no model.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use wawi_instinto::colas;
use wawi_instinto::estado::{self, Fase};
use wawi_instinto::fuentes::{Ctx, Registro, TipoFuente};
use wawi_instinto::juez::Juez;
use wawi_instinto::log::Log;
use wawi_instinto::worker::{self, Worker};
use wawi_tools::{Civil, FakeClock, WallClock};

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("instinto-worker-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

type Script = Arc<Mutex<VecDeque<Result<Value, String>>>>;

/// A source that gives what the test queued, one per read.
struct Falsa(Script);

impl TipoFuente for Falsa {
    fn validar(&self, config: &Value, _: &Ctx<'_>) -> Result<(), String> {
        config
            .get("nombre")
            .map(|_| ())
            .ok_or("falsa necesita nombre".into())
    }
    fn leer(&self, _: &Value, _: &Ctx<'_>) -> Result<Value, String> {
        self.0
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Err("sin guion".into()))
    }
}

/// Says yes to items whose text contains "sí".
struct JuezFalso(Arc<Mutex<u32>>);

impl Juez for JuezFalso {
    fn juzgar(
        &self,
        _: &str,
        _: Option<&str>,
        items: &[Value],
        _: &str,
    ) -> Result<Option<Vec<usize>>, String> {
        *self.0.lock().unwrap() += 1;
        Ok(Some(
            items
                .iter()
                .enumerate()
                .filter(|(_, i)| i.to_string().contains("sí"))
                .map(|(n, _)| n)
                .collect(),
        ))
    }
}

struct Rig {
    home: PathBuf,
    clock: Rc<FakeClock>,
    script: Script,
    judged: Arc<Mutex<u32>>,
    worker: Worker,
}

fn rig(name: &str) -> Rig {
    let home = temp(name);
    let clock = FakeClock::at(Civil::date(2026, 11, 23).at(10, 0, 0), -3);
    let script: Script = Arc::default();
    let judged = Arc::new(Mutex::new(0));
    let worker = make(&home, &clock, &script, &judged);
    Rig {
        home,
        clock,
        script,
        judged,
        worker,
    }
}

fn make(home: &Path, clock: &Rc<FakeClock>, script: &Script, judged: &Arc<Mutex<u32>>) -> Worker {
    Worker {
        home: home.to_path_buf(),
        clock: clock.clone(),
        fuentes: Registro::base().con("falsa", Arc::new(Falsa(script.clone()))),
        juez: Box::new(JuezFalso(judged.clone())),
        log: Log::open(home, false),
        quiet: Some((23 * 60, 8 * 60)),
    }
}

impl Rig {
    fn push(&self, value: Result<Value, String>) {
        self.script.lock().unwrap().push_back(value);
    }
    fn ask(&self, q: Value) {
        colas::dejar_pregunta(&self.home, &q).unwrap();
    }
    fn state(&self, id: &str) -> estado::Estado {
        estado::load(&self.home, id).unwrap()
    }
    fn facts(&self) -> Vec<Value> {
        let dir = self.home.join("cola/hechos/new");
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .collect();
        names.sort();
        names
            .iter()
            .map(|p| serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap())
            .collect()
    }
    fn later(&mut self, seconds: i64) {
        self.clock.advance(seconds);
        self.worker.tick();
    }
}

fn pr(id: &str) -> Value {
    json!({
        "id": id, "version": 1, "origen": "wawi", "creado": "2026-11-23T10:00", "que": "el PR 7",
        "vigia": {
            "fuente": { "falsa": { "nombre": "github" } },
            "extraer": { "jsonpath": "$.reviewDecision" },
            "cuando": { "igual": "APPROVED" },
            "cada": "10m"
        },
        "al_cumplirse": { "decir": "Aprobaron el PR 7.", "contexto": "repo wawi2" }
    })
}

#[test]
fn a_question_comes_true_once_and_a_restart_does_not_repeat_it() {
    let mut r = rig("pr");
    r.ask(pr("pr-7"));
    r.push(Ok(json!({ "reviewDecision": "REVIEW_REQUIRED" })));
    r.worker.tick();
    // Taken: out of new/, acknowledged in cur/, with its state.
    assert!(!r.home.join("cola/preguntas/new/pr-7.json").exists());
    let cur: Value = serde_json::from_str(
        &std::fs::read_to_string(r.home.join("cola/preguntas/cur/pr-7.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(cur["resultado"], "tomada");
    let s = r.state("pr-7");
    assert_eq!(s.fase, Fase::Activa);
    assert_eq!(s.memoria.ultimo, Some(json!("REVIEW_REQUIRED")));
    assert_eq!(s.proxima, r.clock.now() + 600);
    // Not due yet: nothing is read.
    r.push(Ok(json!({ "reviewDecision": "APPROVED" })));
    r.later(300);
    assert_eq!(r.script.lock().unwrap().len(), 1);
    r.later(300);
    assert_eq!(r.state("pr-7").fase, Fase::Cumplida);
    let facts = r.facts();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0]["id"], "pr-7");
    assert_eq!(facts[0]["origen"], "instinto");
    assert_eq!(facts[0]["decir"], "Aprobaron el PR 7.");
    assert_eq!(facts[0]["contexto"]["valor"], "APPROVED");
    // A restart resumes from state: nothing more happens.
    r.worker = make(&r.home, &r.clock, &r.script, &r.judged);
    r.push(Ok(json!({ "reviewDecision": "APPROVED" })));
    r.later(3600);
    assert_eq!(r.facts().len(), 1);
    // The same question dropped again is a repeat, not a second watch.
    r.ask(pr("pr-7"));
    r.worker.tick();
    assert_eq!(r.state("pr-7").fase, Fase::Cumplida);
}

#[test]
fn a_crash_between_the_fact_and_the_state_does_not_speak_twice() {
    let mut r = rig("crash");
    r.ask(pr("pr-8"));
    r.push(Ok(json!({ "reviewDecision": "NO" })));
    r.worker.tick();
    let before = r.state("pr-8");
    r.push(Ok(json!({ "reviewDecision": "APPROVED" })));
    r.later(600);
    assert_eq!(r.facts().len(), 1);
    // Roll the state back, as if the process died before saving it.
    estado::save(&r.home, &before).unwrap();
    r.worker = make(&r.home, &r.clock, &r.script, &r.judged);
    r.push(Ok(json!({ "reviewDecision": "APPROVED" })));
    r.later(600);
    assert_eq!(r.state("pr-8").fase, Fase::Cumplida);
    assert_eq!(r.facts().len(), 1, "the fact id is the dedup key");
    // And once Wawi consumed it (moved to cur/), it is still a repeat.
    std::fs::rename(
        r.home.join("cola/hechos/new/pr-8.json"),
        r.home.join("cola/hechos/cur/pr-8.json"),
    )
    .unwrap();
    estado::save(&r.home, &before).unwrap();
    r.push(Ok(json!({ "reviewDecision": "APPROVED" })));
    r.later(600);
    assert!(r.facts().is_empty());
}

#[test]
fn failures_back_off_then_break_with_a_fact() {
    let mut r = rig("rota");
    r.ask(pr("pr-9"));
    r.push(Err("HTTP 502".into()));
    r.worker.tick();
    let s = r.state("pr-9");
    assert_eq!((s.fase, s.fallos), (Fase::Pendiente, 1));
    assert_eq!(s.proxima, r.clock.now() + 1200);
    r.push(Err("HTTP 502".into()));
    r.later(1200);
    assert_eq!(r.state("pr-9").proxima, r.clock.now() + 2400);
    r.push(Err("HTTP 502".into()));
    r.later(2400);
    let s = r.state("pr-9");
    assert_eq!(s.fase, Fase::Rota);
    let facts = r.facts();
    assert_eq!(facts[0]["id"], "pr-9.rota");
    assert_eq!(facts[0]["decir"], "Dejé de poder mirar el PR 7: HTTP 502");
}

#[test]
fn expiry_cancel_and_invalid_questions() {
    let mut r = rig("fin");
    let mut q = pr("vence-1");
    q["vence"] = json!("2026-11-23T10:30");
    r.ask(q);
    r.ask(pr("cancel-1"));
    r.ask(json!({ "id": "mala", "vigia": { "fuente": { "falsa": {} } }, "al_cumplirse": { "decir": "x" } }));
    r.ask(json!({ "id": "otra", "vigia": { "fuente": { "ftp": {} } }, "al_cumplirse": { "decir": "x" } }));
    for _ in 0..2 {
        r.push(Ok(json!({ "reviewDecision": "NO" })));
    }
    r.worker.tick();
    let mala: Value = serde_json::from_str(
        &std::fs::read_to_string(r.home.join("cola/preguntas/cur/mala.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(mala["resultado"], "invalida");
    assert!(mala["motivo"].as_str().unwrap().contains("nombre"));
    let otra: Value = serde_json::from_str(
        &std::fs::read_to_string(r.home.join("cola/preguntas/cur/otra.json")).unwrap(),
    )
    .unwrap();
    assert!(
        otra["motivo"]
            .as_str()
            .unwrap()
            .contains("no conozco la fuente")
    );
    // The look is capped by the expiry, and then it expires.
    assert_eq!(r.state("vence-1").proxima, r.clock.now() + 600);
    colas::pedir_cancelar(&r.home, "cancel-1").unwrap();
    r.later(1800);
    assert_eq!(r.state("vence-1").fase, Fase::Vencida);
    assert_eq!(r.state("cancel-1").fase, Fase::Cancelada);
    assert!(colas::cancelaciones(&r.home).is_empty());
    assert!(r.facts().is_empty());
}

#[test]
fn a_clock_question_fires_at_its_time_and_quiet_hours_wait() {
    let mut r = rig("reloj");
    r.ask(json!({ "id": "turno", "que": "el dentista",
        "vigia": { "fuente": { "reloj": { "at": "2026-11-23T18:00" } } },
        "al_cumplirse": { "decir": "En una hora tenés dentista." } }));
    r.ask(json!({ "id": "noche",
        "vigia": { "fuente": { "reloj": { "at": "2026-11-23T23:30" } } },
        "al_cumplirse": { "decir": "Es tarde." } }));
    r.worker.tick();
    let at = r
        .clock
        .epoch_of(Civil::date(2026, 11, 23).at(18, 0, 0))
        .unwrap();
    assert_eq!(r.state("turno").proxima, at);
    let morning = r
        .clock
        .epoch_of(Civil::date(2026, 11, 24).at(8, 0, 0))
        .unwrap();
    assert_eq!(
        r.state("noche").proxima,
        morning,
        "inside quiet hours it waits for 8:00"
    );
    r.later(8 * 3600);
    assert_eq!(r.state("turno").fase, Fase::Cumplida);
    assert_eq!(r.facts()[0]["id"], "turno");
    r.later(6 * 3600);
    assert_eq!(r.state("noche").fase, Fase::Pendiente);
    r.later(8 * 3600);
    assert_eq!(r.state("noche").fase, Fase::Cumplida);
}

#[test]
fn news_with_nuevos_and_a_judge_only_on_new_items() {
    let mut r = rig("rss");
    r.ask(json!({ "id": "dolar", "que": "noticias del dólar",
        "vigia": {
            "fuente": { "falsa": { "nombre": "rss" } },
            "extraer": "rss",
            "cuando": { "y": [ { "nuevos": { "id": "$.link" } }, { "coincide": "(?i)d[oó]lar", "en": "$.titulo" } ] },
            "juzgar": { "pregunta": "¿Es una suba fuerte?" },
            "cada": "30m", "repetir": true
        },
        "al_cumplirse": { "decir": "Hay noticias del dólar." } }));
    let feed = |items: &[(&str, &str)]| {
        let body: String = items
            .iter()
            .map(|(l, t)| format!("<item><title>{t}</title><link>{l}</link></item>"))
            .collect();
        Ok(Value::String(format!(
            "<rss><channel>{body}</channel></rss>"
        )))
    };
    r.push(feed(&[("1", "Dólar: sí sube")]));
    r.worker.tick();
    assert_eq!(*r.judged.lock().unwrap(), 0, "the first look only learns");
    r.push(feed(&[
        ("1", "Dólar: sí sube"),
        ("2", "Fútbol"),
        ("3", "El dólar se queda quieto"),
    ]));
    r.later(1800);
    assert_eq!(*r.judged.lock().unwrap(), 1);
    assert!(r.facts().is_empty(), "the judge said no");
    r.push(feed(&[("4", "Dólar: sí, otra suba")]));
    r.later(1800);
    let facts = r.facts();
    assert_eq!(facts[0]["id"], "dolar.1");
    assert!(
        facts[0]["contexto"]["nuevos"][0]
            .as_str()
            .unwrap()
            .contains("otra suba")
    );
    assert_eq!(r.state("dolar").fase, Fase::Activa);
    // Nothing new: the judge is not asked.
    r.push(feed(&[("4", "Dólar: sí, otra suba")]));
    r.later(1800);
    assert_eq!(*r.judged.lock().unwrap(), 2);
}

#[test]
fn one_worker_per_home() {
    let home = temp("lock");
    let first = worker::lock(&home).unwrap();
    assert!(first.is_some());
    assert!(worker::lock(&home).unwrap().is_none());
    drop(first);
    assert!(worker::lock(&home).unwrap().is_some());
}
