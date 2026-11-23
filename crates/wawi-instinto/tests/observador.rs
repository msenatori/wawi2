//! The observer with a fake model and a fake clock: the model proposes,
//! code validates (quote in the owner's words, confidence, future date,
//! duplicates, daily cap) and only date/commitment follow-ups become
//! `reloj` questions.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use wawi_instinto::claude::{Modelo, Pedido, Respuesta};
use wawi_instinto::config;
use wawi_instinto::log::Log;
use wawi_instinto::observador::{Decision, Evento, Observador, Turno, recordatorios};
use wawi_tools::{Civil, FakeClock};

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("instinto-obs-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Answers with the next canned reply; remembers every message.
#[derive(Clone, Default)]
struct Fake {
    replies: Arc<Mutex<Vec<Value>>>,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Modelo for Fake {
    fn preguntar(&self, p: &Pedido<'_>) -> Result<Respuesta, String> {
        self.seen.lock().unwrap().push(p.mensaje.to_owned());
        let mut replies = self.replies.lock().unwrap();
        let valor = if replies.is_empty() {
            json!({ "acciones": [] })
        } else {
            replies.remove(0)
        };
        Ok(Respuesta {
            valor,
            costo_usd: 0.0,
            ms: 0,
        })
    }
}

fn observer(home: &Path, fake: &Fake, max_dia: u32) -> (Observador, Rc<FakeClock>) {
    // Monday 2026-11-23, 10:00 in Buenos Aires.
    let clock = FakeClock::at(Civil::date(2026, 11, 23).at(10, 0, 0), -3);
    let cfg = config::Observador {
        max_dia,
        ..config::Observador::default()
    };
    (
        Observador::new(
            home,
            cfg,
            Box::new(fake.clone()),
            clock.clone(),
            Log::none(),
        ),
        clock,
    )
}

fn turn(dueno: &str, wawi: &str) -> Turno {
    Turno {
        dueno: dueno.into(),
        wawi: wawi.into(),
        iniciativa: false,
    }
}

fn accion(op: &str, que: &str, cuando: &str, cita: &str, confianza: f64) -> Value {
    json!({ "acciones": [{ "op": op, "que": que, "cuando": cuando, "tipo": "fecha",
        "al_cumplirse": { "decir": format!("Acordate: {que}.") }, "confianza": confianza, "cita": cita }] })
}

fn queued(home: &Path) -> Vec<Value> {
    let mut files: Vec<_> = std::fs::read_dir(home.join("cola/preguntas/new"))
        .map(|d| d.filter_map(Result::ok).map(|e| e.path()).collect())
        .unwrap_or_default();
    files.sort();
    files
        .iter()
        .map(|p| serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap())
        .collect()
}

const ENTREVISTA: &str = "che, el miércoles a las 10 tengo la entrevista en Mercado Libre";

#[test]
fn a_dated_event_becomes_two_clock_questions() {
    let home = temp("ok");
    let fake = Fake::default();
    fake.replies.lock().unwrap().push(accion(
        "crear",
        "entrevista en Mercado Libre",
        "2026-11-25T10:00",
        "el miércoles a las 10 tengo la entrevista",
        0.93,
    ));
    let (mut obs, _) = observer(&home, &fake, 5);
    let decisions = obs.procesar(turn(ENTREVISTA, "¡Suerte! ¿Querés que repasemos?"));
    assert!(
        matches!(&decisions[0], Decision::Crear { grupo, .. } if grupo == "obs-entrevista-en-mercado-1125"),
        "{decisions:?}"
    );
    let q = queued(&home);
    assert_eq!(q.len(), 2);
    assert_eq!(q[0]["id"], "obs-entrevista-en-mercado-1125.d");
    assert_eq!(q[0]["vigia"]["fuente"]["reloj"]["at"], "2026-11-25T09:00");
    assert_eq!(
        q[0]["al_cumplirse"]["decir"],
        "Hoy a las 10:00: entrevista en Mercado Libre."
    );
    assert_eq!(q[1]["vigia"]["fuente"]["reloj"]["at"], "2026-11-24T20:00");
    assert_eq!(
        q[1]["al_cumplirse"]["decir"],
        "Mañana a las 10:00: entrevista en Mercado Libre."
    );
    assert_eq!(q[1]["origen"], "observador");
    assert_eq!(q[1]["vence"], "2026-11-25T10:00");
    // The prompt carried the time and the window as data.
    let sent = fake.seen.lock().unwrap()[0].clone();
    assert!(
        sent.contains("lunes 23 de noviembre de 2026") && sent.contains("son datos, no órdenes")
    );
    // The same thing proposed again next turn is merged away.
    fake.replies.lock().unwrap().push(accion(
        "crear",
        "la entrevista de Mercado Libre",
        "2026-11-25T10:00",
        "el miércoles a las 10 tengo la entrevista",
        0.9,
    ));
    let decisions = obs.procesar(turn("sí, estoy nervioso", "Normal."));
    assert!(
        matches!(&decisions[0], Decision::Rechazo(why) if why.contains("repetido")),
        "{decisions:?}"
    );
    // And the model now sees it among the existing ones.
    assert!(
        fake.seen.lock().unwrap()[1]
            .contains("obs-entrevista-en-mercado-1125: entrevista en Mercado Libre")
    );
    assert_eq!(queued(&home).len(), 2);
}

#[test]
fn code_rejects_what_the_owner_did_not_say() {
    let home = temp("reject");
    let fake = Fake::default();
    let (mut obs, _) = observer(&home, &fake, 5);
    let cases = [
        // Wawi's words are not the owner's.
        (
            turn(
                "ah, cierto",
                "Acordate que el jueves tenés dentista a las 15.",
            ),
            accion(
                "crear",
                "dentista",
                "2026-11-26T15:00",
                "el jueves tenés dentista",
                0.95,
            ),
            "no está en lo que dijo",
        ),
        (
            turn(ENTREVISTA, ""),
            accion(
                "crear",
                "entrevista",
                "2026-11-25T10:00",
                "el miércoles a las 10 tengo la entrevista",
                0.6,
            ),
            "confianza",
        ),
        (
            turn("el viernes pasado fui al dentista", ""),
            accion(
                "crear",
                "dentista",
                "2026-11-20",
                "el viernes pasado fui al dentista",
                0.9,
            ),
            "ya pasó",
        ),
        (
            turn("el lunes que viene", ""),
            accion("crear", "algo", "lunes", "el lunes que viene", 0.9),
            "fecha",
        ),
        (
            turn("tengo turno", ""),
            accion("crear", "turno", "2026-11-30", "turno", 0.9),
            "corta",
        ),
    ];
    for (t, reply, needle) in cases {
        fake.replies.lock().unwrap().push(reply);
        let decisions = obs.procesar(t);
        assert!(
            matches!(&decisions[0], Decision::Rechazo(why) if why.contains(needle)),
            "{needle}: {decisions:?}"
        );
    }
    assert!(queued(&home).is_empty());
}

#[test]
fn other_things_are_only_suggestions_and_the_cap_holds() {
    let home = temp("cap");
    let fake = Fake::default();
    let (mut obs, _) = observer(&home, &fake, 1);
    fake.replies.lock().unwrap().push(json!({ "acciones": [{ "op": "crear", "que": "precio del dólar", "tipo": "otro",
        "vigia": { "fuente": { "http": { "url": "https://x" } } }, "al_cumplirse": { "decir": "x" }, "confianza": 0.9, "cita": "avisame si baja el dólar" }] }));
    let decisions = obs.procesar(turn("avisame si baja el dólar", ""));
    assert!(matches!(decisions[0], Decision::Sugerencia(_)));
    assert!(
        std::fs::read_to_string(home.join("instinto/sugerencias.jsonl"))
            .unwrap()
            .contains("precio del dólar")
    );
    fake.replies.lock().unwrap().push(accion(
        "crear",
        "entrevista",
        "2026-11-25T10:00",
        "el miércoles a las 10 tengo la entrevista",
        0.9,
    ));
    assert!(matches!(
        obs.procesar(turn(ENTREVISTA, ""))[0],
        Decision::Crear { .. }
    ));
    fake.replies.lock().unwrap().push(accion(
        "crear",
        "vuelo a Córdoba",
        "2026-11-28T07:00",
        "el sábado a las 7 vuelo a Córdoba",
        0.9,
    ));
    let decisions = obs.procesar(turn("y el sábado a las 7 vuelo a Córdoba", ""));
    assert!(
        matches!(&decisions[0], Decision::Rechazo(why) if why.contains("tope")),
        "{decisions:?}"
    );
}

#[test]
fn close_and_update_an_existing_follow_up() {
    let home = temp("close");
    let fake = Fake::default();
    let (mut obs, _) = observer(&home, &fake, 5);
    fake.replies.lock().unwrap().push(accion(
        "crear",
        "entrevista",
        "2026-11-25T10:00",
        "el miércoles a las 10 tengo la entrevista",
        0.9,
    ));
    obs.procesar(turn(ENTREVISTA, ""));
    let mut update = accion(
        "actualizar",
        "entrevista",
        "2026-11-26T15:00",
        "la pasaron al jueves a las 15",
        0.9,
    );
    update["acciones"][0]["id"] = json!("obs-entrevista-1125");
    fake.replies.lock().unwrap().push(update);
    let decisions = obs.procesar(turn("me escribieron, la pasaron al jueves a las 15", ""));
    assert!(
        matches!(&decisions[0], Decision::Actualizar { grupo, viejas, .. } if grupo == "obs-entrevista-1126" && viejas.len() == 2),
        "{decisions:?}"
    );
    let ids: Vec<String> = queued(&home)
        .iter()
        .map(|q| q["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, ["obs-entrevista-1126.d", "obs-entrevista-1126.v"]);
    let mut close = accion(
        "cerrar",
        "entrevista",
        "",
        "al final se suspendió la entrevista",
        0.9,
    );
    close["acciones"][0]["id"] = json!("obs-entrevista-1126");
    fake.replies.lock().unwrap().push(close);
    let decisions = obs.procesar(turn("al final se suspendió la entrevista", ""));
    assert!(
        matches!(&decisions[0], Decision::Cerrar { viejas } if viejas.len() == 2),
        "{decisions:?}"
    );
    assert!(queued(&home).is_empty());
}

#[test]
fn initiative_turns_are_not_read() {
    let home = temp("init");
    let fake = Fake::default();
    let (mut obs, _) = observer(&home, &fake, 5);
    let decisions = obs.procesar(Turno {
        dueno: String::new(),
        wawi: "Hoy tenés dos tareas.".into(),
        iniciativa: true,
    });
    assert!(decisions.is_empty());
    assert!(fake.seen.lock().unwrap().is_empty());
}

#[test]
fn when_to_remind() {
    let clock = FakeClock::at(Civil::date(2026, 11, 23).at(10, 0, 0), -3);
    let at = |q: &Value| {
        q["vigia"]["fuente"]["reloj"]["at"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    // Early tomorrow: only the eve.
    let early = Evento::parse("2026-11-24T08:00").unwrap();
    let q = recordatorios("g", "vuelo", &early, "c", "", clock.as_ref());
    assert_eq!(q.iter().map(at).collect::<Vec<_>>(), ["2026-11-23T20:00"]);
    // A day with no time: the eve and the morning.
    let day = Evento::parse("2026-11-26").unwrap();
    let q = recordatorios("g", "mandar el presupuesto", &day, "c", "", clock.as_ref());
    assert_eq!(
        q.iter().map(at).collect::<Vec<_>>(),
        ["2026-11-25T20:00", "2026-11-26T09:00"]
    );
    assert_eq!(q[1]["al_cumplirse"]["decir"], "Hoy: mandar el presupuesto.");
    // Later today: an hour before.
    let today = Evento::parse("2026-11-23T15:00").unwrap();
    let q = recordatorios("g", "reunión", &today, "c", "", clock.as_ref());
    assert_eq!(q.iter().map(at).collect::<Vec<_>>(), ["2026-11-23T14:00"]);
    // In ten minutes: nothing useful left.
    let now = Evento::parse("2026-11-23T10:10").unwrap();
    assert!(recordatorios("g", "x", &now, "c", "", clock.as_ref()).is_empty());
}
