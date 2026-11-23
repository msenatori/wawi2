//! The host's incident hook, for autoarreglo (docs/AUTOARREGLO.md). When
//! something goes wrong on a turn, one small JSON file goes into
//! `<home>/cola/incidentes` (maildir: `tmp/`, then a rename into `new/`).
//! The host only writes; the autoarreglo service reads.
//!
//! Fire and forget: [`Detector`] is pure and cheap, and the files are
//! written by a thread of their own, so a turn never waits on a disk.
//! Nothing the owner or Wawi said goes in: kinds, counts, latencies and
//! error reasons only (and a reason that looks like a secret is dropped).

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wawi_memory::looks_like_secret;
use wawi_plugins::inbox::{INCIDENTS, make_queue};

use super::clock::local_now;
use super::turns::TurnReport;

/// Plugin downs that make a crash loop, and in how long.
const DOWNS: usize = 3;
const DOWNS_WINDOW: Duration = Duration::from_secs(10 * 60);
/// Empty or garbled transcripts in a row, and in how long.
const EMPTIES: usize = 3;
const EMPTIES_WINDOW: Duration = Duration::from_secs(3 * 60);
/// A held segment this long that comes back empty was speech, not a tap.
const SPOKE_S: f64 = 0.8;
/// Turns kept for the latency baseline, and how many it needs.
const BASELINE: usize = 20;
const BASELINE_MIN: usize = 5;
/// Slow is more than this many times the median, and at least this much.
const SLOW_FACTOR: u64 = 2;
const SLOW_FLOOR_MS: u64 = 1500;
/// The same incident is written at most once in this long.
const SAME_GAP: Duration = Duration::from_secs(60);
/// Files waiting in `new/` beyond which nothing more is written (nobody is
/// reading: the autoarreglo plugin is not installed).
const MAX_WAITING: usize = 500;

/// One incident, before it is written.
#[derive(Clone, Debug, PartialEq)]
pub struct Incident {
    pub kind: &'static str,
    pub key: String,
    pub summary: String,
    pub detail: Value,
}

fn incident(kind: &'static str, key: &str, summary: String, detail: Value) -> Incident {
    Incident {
        kind,
        key: key.to_owned(),
        summary,
        detail,
    }
}

/// A reason fit for a file: short, one line, never a secret.
fn reason(text: &str) -> String {
    if looks_like_secret(text) {
        return "[parece un secreto: no se guarda]".into();
    }
    let line = text.replace(['\n', '\r'], " ");
    line.chars().take(200).collect()
}

/// What counts as a problem. Pure: every call takes `now`.
#[derive(Debug, Default)]
pub struct Detector {
    felt: VecDeque<u64>,
    downs: HashMap<String, VecDeque<Instant>>,
    empties: VecDeque<Instant>,
    last_segment_s: f64,
}

impl Detector {
    /// The brain failed a turn. "claude se cerró" is the process dying.
    pub fn brain_failed(&mut self, why: &str) -> Incident {
        if why.starts_with("claude se cerró") {
            incident(
                "cerebro_caido",
                "claude se cerró",
                format!("el proceso del cerebro se cerró: {}", reason(why)),
                Value::Null,
            )
        } else {
            let key: String = why.chars().take(60).collect();
            incident(
                "turno_fallido",
                &reason(&key),
                format!("el cerebro falló un turno: {}", reason(why)),
                Value::Null,
            )
        }
    }

    pub fn turn_failed(&mut self, what: &str) -> Incident {
        incident("turno_fallido", what, what.to_owned(), Value::Null)
    }

    pub fn audio(&mut self, what: &str) -> Incident {
        incident("audio", what, what.to_owned(), Value::Null)
    }

    /// A plugin went down; three times in ten minutes is a crash loop.
    pub fn plugin_down(&mut self, plugin: &str, why: &str, now: Instant) -> Option<Incident> {
        let downs = self.downs.entry(plugin.to_owned()).or_default();
        downs.push_back(now);
        while downs
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) > DOWNS_WINDOW)
        {
            downs.pop_front();
        }
        if downs.len() < DOWNS {
            return None;
        }
        downs.clear();
        Some(incident(
            "plugin_caido",
            plugin,
            format!(
                "el plugin {plugin} se cayó {DOWNS} veces en 10 minutos: {}",
                reason(why)
            ),
            json!({ "plugin": plugin }),
        ))
    }

    /// The segment about to be transcribed, in seconds.
    pub fn segment(&mut self, seconds: f64) {
        self.last_segment_s = seconds;
    }

    /// A clear transcript: the run of empty ones is over.
    pub fn heard(&mut self) {
        self.empties.clear();
    }

    /// An empty transcript. Only one of real speech (not a tap) counts.
    pub fn empty(&mut self, now: Instant) -> Option<Incident> {
        if self.last_segment_s < SPOKE_S {
            return None;
        }
        self.not_understood(now, "vacía")
    }

    /// A transcript the core did not understand (another language).
    pub fn unclear(&mut self, now: Instant) -> Option<Incident> {
        self.not_understood(now, "no entendida")
    }

    fn not_understood(&mut self, now: Instant, how: &str) -> Option<Incident> {
        self.empties.push_back(now);
        while self
            .empties
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) > EMPTIES_WINDOW)
        {
            self.empties.pop_front();
        }
        if self.empties.len() < EMPTIES {
            return None;
        }
        self.empties.clear();
        Some(incident(
            "transcripcion_vacia",
            "seguidas",
            format!("{EMPTIES} transcripciones seguidas sin entender (la última, {how})"),
            json!({ "segundos": (self.last_segment_s * 10.0).round() / 10.0 }),
        ))
    }

    /// A finished turn: slow if its first audio took more than twice the
    /// median of the recent brain turns.
    pub fn turn(&mut self, report: &TurnReport) -> Option<Incident> {
        if report.via != "cerebro" || report.interrupted {
            return None;
        }
        let felt = u64::try_from(report.first_audio?.as_millis()).unwrap_or(u64::MAX);
        let slow = if self.felt.len() >= BASELINE_MIN {
            let mut sorted: Vec<u64> = self.felt.iter().copied().collect();
            sorted.sort_unstable();
            let median = sorted[sorted.len() / 2];
            (felt > SLOW_FACTOR * median && felt >= SLOW_FLOOR_MS).then_some(median)
        } else {
            None
        };
        self.felt.push_back(felt);
        if self.felt.len() > BASELINE {
            self.felt.pop_front();
        }
        let median = slow?;
        let ms = |d: Option<Duration>| d.map(|d| d.as_millis() as u64);
        Some(incident(
            "turno_lento",
            "cerebro",
            format!("primer audio a los {felt} ms; la mediana es {median} ms"),
            json!({
                "felt_ms": felt,
                "mediana_ms": median,
                "transcript_ms": ms(report.transcript),
                "first_sentence_ms": ms(report.first_sentence),
            }),
        ))
    }
}

/// The detector plus the writer thread.
pub struct Incidents {
    pub detector: Detector,
    tx: Option<Sender<Value>>,
    writer: Option<JoinHandle<()>>,
    last: HashMap<(&'static str, String), Instant>,
    seq: u64,
}

impl Incidents {
    /// Starts the writer for `<home>/cola/incidentes`.
    pub fn open(home: &Path) -> Self {
        let root: PathBuf = home.join(INCIDENTS);
        let (tx, rx) = mpsc::channel::<Value>();
        let writer = std::thread::Builder::new()
            .name("wawi-incidents".into())
            .spawn(move || {
                if make_queue(&root).is_err() {
                    return;
                }
                for value in rx {
                    let waiting = std::fs::read_dir(root.join("new")).map_or(0, |d| d.count());
                    if waiting >= MAX_WAITING {
                        continue;
                    }
                    let id = value["id"].as_str().unwrap_or("x").to_owned();
                    let tmp = root.join("tmp").join(format!("{id}.json"));
                    if std::fs::write(&tmp, value.to_string()).is_ok() {
                        let _ = std::fs::rename(&tmp, root.join("new").join(format!("{id}.json")));
                    }
                }
            })
            .ok();
        Self {
            detector: Detector::default(),
            tx: writer.is_some().then_some(tx),
            writer,
            last: HashMap::new(),
            seq: 0,
        }
    }

    /// Queues one incident for the writer. Never blocks.
    pub fn report(&mut self, incident: Option<Incident>, now: Instant) {
        let Some(incident) = incident else { return };
        let key = (incident.kind, incident.key.clone());
        if self
            .last
            .get(&key)
            .is_some_and(|t| now.saturating_duration_since(*t) < SAME_GAP)
        {
            return;
        }
        self.last.insert(key, now);
        self.seq += 1;
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let stamp = local_now().stamp();
        let value = json!({
            "id": format!("{millis}-{}", self.seq),
            "version": 1,
            "tipo": incident.kind,
            "clave": incident.key,
            "resumen": incident.summary,
            "detalle": incident.detail,
            "creado": stamp.get(..19).unwrap_or(&stamp).replace(' ', "T"),
            "origen": "host",
        });
        if let Some(tx) = &self.tx {
            let _ = tx.send(value);
        }
    }
}

impl Drop for Incidents {
    /// Lets the writer finish what is queued.
    fn drop(&mut self) {
        self.tx = None;
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(felt_ms: u64) -> TurnReport {
        TurnReport {
            heard: "privado".into(),
            said: "también".into(),
            transcript: Some(Duration::from_millis(400)),
            first_sentence: Some(Duration::from_millis(felt_ms - 300)),
            first_audio: Some(Duration::from_millis(felt_ms)),
            via: "cerebro".into(),
            interrupted: false,
        }
    }

    #[test]
    fn a_turn_far_over_the_median_is_slow() {
        let mut d = Detector::default();
        for _ in 0..5 {
            assert_eq!(d.turn(&report(1800)), None);
        }
        assert_eq!(d.turn(&report(3000)), None, "not over twice");
        let slow = d.turn(&report(4200)).unwrap();
        assert_eq!(slow.kind, "turno_lento");
        assert!(slow.summary.contains("4200") && slow.summary.contains("1800"));
        assert!(!format!("{slow:?}").contains("privado"), "never the texts");
        let mut cut = report(9000);
        cut.interrupted = true;
        assert_eq!(d.turn(&cut), None);
    }

    #[test]
    fn three_downs_in_ten_minutes_is_a_crash_loop() {
        let mut d = Detector::default();
        let t = Instant::now();
        assert!(d.plugin_down("linear", "se cerró", t).is_none());
        assert!(
            d.plugin_down("linear", "se cerró", t + Duration::from_secs(700))
                .is_none()
        );
        assert!(
            d.plugin_down("linear", "se cerró", t + Duration::from_secs(710))
                .is_none()
        );
        let loop_ = d
            .plugin_down("linear", "se cerró", t + Duration::from_secs(720))
            .unwrap();
        assert_eq!((loop_.kind, loop_.key.as_str()), ("plugin_caido", "linear"));
    }

    #[test]
    fn empty_speech_repeated_but_not_taps() {
        let mut d = Detector::default();
        let t = Instant::now();
        d.segment(0.1);
        for _ in 0..5 {
            assert!(d.empty(t).is_none(), "taps are not speech");
        }
        d.segment(2.0);
        assert!(d.empty(t).is_none());
        assert!(d.unclear(t).is_none());
        d.heard();
        assert!(d.empty(t).is_none());
        assert!(d.empty(t).is_none());
        assert_eq!(d.empty(t).unwrap().kind, "transcripcion_vacia");
    }

    #[test]
    fn the_brain_dying_is_its_own_kind() {
        let mut d = Detector::default();
        assert_eq!(
            d.brain_failed("claude se cerró: boom").kind,
            "cerebro_caido"
        );
        let other = d.brain_failed("API Error: sk-ant-abcdefghijklmnopqrstuvwxyz0123");
        assert_eq!(other.kind, "turno_fallido");
        assert!(!format!("{other:?}").contains("sk-ant-abc"));
    }

    #[test]
    fn written_in_the_background_and_deduplicated() {
        let home = std::env::temp_dir().join(format!("wawi-incidents-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let mut incidents = Incidents::open(&home);
        let t = Instant::now();
        let started = Instant::now();
        incidents.report(
            Some(Detector::default().audio("la reproducción nunca terminó")),
            t,
        );
        incidents.report(
            Some(Detector::default().audio("la reproducción nunca terminó")),
            t,
        );
        incidents.report(None, t);
        assert!(started.elapsed() < Duration::from_millis(50), "never waits");
        drop(incidents);
        let files: Vec<_> = std::fs::read_dir(home.join("cola/incidentes/new"))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert_eq!(files.len(), 1);
        let text = std::fs::read_to_string(files[0].path()).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["tipo"], "audio");
        assert_eq!(value["origen"], "host");
        assert_eq!(value["creado"].as_str().unwrap().len(), 19);
        let _ = std::fs::remove_dir_all(&home);
    }
}
