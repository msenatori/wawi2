//! The state machine, pure. Given a question, its state, what one look
//! found and the time, it gives the next state and maybe a fact to write.
//! No files, no clock of its own, no network: the worker does the I/O.

use serde_json::{Value, json};
use wawi_tools::{Civil, WallClock};

use crate::cuando::Memoria;
use crate::estado::{Estado, Fase};
use crate::extraer::as_text;
use crate::spec::{DEFAULT_EVERY_S, Pregunta};
use crate::texto::truncate;

/// The longest wait between looks after failures.
pub const MAX_BACKOFF_S: i64 = 6 * 3600;
/// Items described in a fact, at most.
const MAX_FACT_ITEMS: usize = 10;

/// What one look found.
#[derive(Clone, Debug, PartialEq)]
pub enum Lectura {
    /// It came true. `items` are the new items that made it true.
    Cumple {
        valor: Value,
        items: Option<Vec<Value>>,
        memoria: Memoria,
    },
    NoCumple {
        memoria: Memoria,
    },
    /// The source, the extraction or the predicate failed.
    Error(String),
}

/// A fact for `<home>/cola/hechos`.
#[derive(Clone, Debug, PartialEq)]
pub struct Hecho {
    pub id: String,
    pub decir: String,
    pub contexto: Value,
    pub vence: Option<String>,
}

/// A fresh state for a question just taken from the queue.
pub fn nuevo(pregunta: &Pregunta, primera: i64, now: i64) -> Estado {
    Estado {
        id: pregunta.id.clone(),
        fase: Fase::Pendiente,
        pregunta: pregunta.raw.clone(),
        memoria: Memoria::default(),
        corridas: 0,
        fallos: 0,
        cumplidas: 0,
        proxima: primera,
        ultimo_error: None,
        actualizado: now,
        motivo: None,
    }
}

/// The earliest of `vence` and `hasta`, as an epoch.
pub fn limite(pregunta: &Pregunta, clock: &dyn WallClock) -> Option<i64> {
    [pregunta.vence.as_deref(), pregunta.vigia.hasta.as_deref()]
        .into_iter()
        .flatten()
        .filter_map(|t| crate::clock::parse_epoch(clock, t).ok())
        .min()
}

/// The seconds between good looks.
pub fn every(pregunta: &Pregunta) -> i64 {
    pregunta.vigia.cada.unwrap_or(DEFAULT_EVERY_S) as i64
}

/// After `fallos` failures in a row: the interval doubled each time, up to
/// [`MAX_BACKOFF_S`] (but never less than the interval itself).
pub fn backoff(every_s: i64, fallos: u32) -> i64 {
    let factor = 1i64 << fallos.min(16);
    every_s
        .saturating_mul(factor)
        .min(MAX_BACKOFF_S)
        .max(every_s)
}

/// Ends a question whose time ran out.
pub fn vencer(mut estado: Estado, now: i64) -> Estado {
    estado.fase = Fase::Vencida;
    estado.motivo = Some("se pasó la fecha sin cumplirse".into());
    estado.actualizado = now;
    estado
}

pub fn cancelar(mut estado: Estado, now: i64) -> Estado {
    if !estado.fase.terminal() {
        estado.fase = Fase::Cancelada;
        estado.motivo = Some("la cancelaron".into());
        estado.actualizado = now;
    }
    estado
}

/// What a look means. `siguiente` is when the next look would be if the
/// question goes on (the source's own schedule or `now + cada`);
/// `repetir` keeps it going after it comes true.
pub fn despues(
    mut estado: Estado,
    pregunta: &Pregunta,
    lectura: Lectura,
    now: i64,
    siguiente: i64,
    repetir: bool,
) -> (Estado, Option<Hecho>) {
    if estado.fase.terminal() {
        return (estado, None);
    }
    estado.actualizado = now;
    match lectura {
        Lectura::Error(error) => {
            estado.fallos += 1;
            estado.ultimo_error = Some(truncate(&error, 300));
            if estado.fallos >= pregunta.vigia.fallos_max {
                estado.fase = Fase::Rota;
                estado.motivo = Some(format!("{} fallos seguidos", estado.fallos));
                let hecho = Hecho {
                    id: format!("{}.rota", estado.id),
                    decir: format!(
                        "Dejé de poder mirar {}: {}",
                        pregunta.que().trim_end_matches('.'),
                        truncate(&error, 200)
                    ),
                    contexto: json!({ "seguimiento": estado.id, "fallos": estado.fallos }),
                    vence: None,
                };
                return (estado, Some(hecho));
            }
            estado.proxima = now + backoff(every(pregunta), estado.fallos);
            (estado, None)
        }
        Lectura::NoCumple { memoria } => {
            estado.memoria = memoria;
            estado.fase = Fase::Activa;
            estado.corridas += 1;
            estado.fallos = 0;
            estado.ultimo_error = None;
            estado.proxima = siguiente;
            (estado, None)
        }
        Lectura::Cumple {
            valor,
            items,
            memoria,
        } => {
            estado.memoria = memoria;
            estado.corridas += 1;
            estado.fallos = 0;
            estado.ultimo_error = None;
            estado.cumplidas += 1;
            let id = if repetir {
                format!("{}.{}", estado.id, estado.cumplidas)
            } else {
                estado.id.clone()
            };
            let hecho = Hecho {
                id,
                decir: pregunta.al_cumplirse.decir.clone(),
                contexto: fact_context(pregunta, &valor, items.as_deref()),
                vence: pregunta.vence.clone(),
            };
            if repetir {
                estado.fase = Fase::Activa;
                estado.proxima = siguiente;
            } else {
                estado.fase = Fase::Cumplida;
                estado.motivo = Some("se cumplió".into());
            }
            (estado, Some(hecho))
        }
    }
}

fn fact_context(pregunta: &Pregunta, valor: &Value, items: Option<&[Value]>) -> Value {
    let mut context = json!({ "seguimiento": pregunta.id });
    if !pregunta.al_cumplirse.contexto.is_null() {
        context["contexto"] = pregunta.al_cumplirse.contexto.clone();
    }
    match items {
        Some(items) => {
            context["nuevos"] = Value::Array(
                items
                    .iter()
                    .take(MAX_FACT_ITEMS)
                    .map(|i| Value::String(truncate(&as_text(i), 300)))
                    .collect(),
            );
        }
        None => context["valor"] = Value::String(truncate(&as_text(valor), 1000)),
    }
    context
}

/// Quiet hours, local, `[desde, hasta)` in minutes since midnight. A look
/// due inside them waits until they end, so a fact never lands while the
/// gate would throw it away.
pub fn fuera_de_silencio(epoch: i64, clock: &dyn WallClock, silencio: Option<(u16, u16)>) -> i64 {
    let Some((from, until)) = silencio else {
        return epoch;
    };
    let local = clock.local(epoch);
    let minute = u16::from(local.hour) * 60 + u16::from(local.minute);
    let inside = if from <= until {
        minute >= from && minute < until
    } else {
        minute >= from || minute < until
    };
    if !inside {
        return epoch;
    }
    let mut day = Civil::date(local.year, local.month, local.day);
    if from > until && minute >= from {
        // Past midnight's eve: the quiet ends tomorrow.
        let (y, m, d) = wawi_tools::clock::civil_from_days(day.days() + 1);
        day = Civil::date(y, m, d);
    }
    let end = day.at((until / 60) as u8, (until % 60) as u8, 0);
    clock.epoch_of(end).unwrap_or(epoch).max(epoch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::parse_pregunta;
    use wawi_tools::FakeClock;

    fn pregunta(repetir: bool) -> Pregunta {
        parse_pregunta(&json!({
            "id": "q1", "que": "el PR 7",
            "vigia": { "fuente": { "reloj": { "at": "2026-11-23T10:00" } }, "cada": "10m", "repetir": repetir },
            "al_cumplirse": { "decir": "Aprobaron el PR 7.", "contexto": "wawi2" }
        }))
        .unwrap()
    }

    fn mem(v: &str) -> Memoria {
        Memoria {
            ultimo: Some(json!(v)),
            vistos: vec![],
        }
    }

    #[test]
    fn true_once_writes_the_fact_under_the_question_id() {
        let p = pregunta(false);
        let e = nuevo(&p, 100, 100);
        assert_eq!(e.fase, Fase::Pendiente);
        let (e, hecho) = despues(
            e,
            &p,
            Lectura::NoCumple { memoria: mem("a") },
            100,
            700,
            false,
        );
        assert_eq!((e.fase, e.proxima, e.corridas), (Fase::Activa, 700, 1));
        assert!(hecho.is_none());
        let lectura = Lectura::Cumple {
            valor: json!("APPROVED"),
            items: None,
            memoria: mem("APPROVED"),
        };
        let (e, hecho) = despues(e, &p, lectura.clone(), 700, 1300, false);
        assert_eq!(e.fase, Fase::Cumplida);
        let hecho = hecho.unwrap();
        assert_eq!(hecho.id, "q1");
        assert_eq!(hecho.decir, "Aprobaron el PR 7.");
        assert_eq!(hecho.contexto["valor"], "APPROVED");
        assert_eq!(hecho.contexto["contexto"], "wawi2");
        // A terminal state ignores later looks.
        let (again, none) = despues(e.clone(), &p, lectura, 800, 900, false);
        assert_eq!(again, e);
        assert!(none.is_none());
    }

    #[test]
    fn repetir_numbers_each_fact() {
        let p = pregunta(true);
        let e = nuevo(&p, 0, 0);
        let hit = || Lectura::Cumple {
            valor: json!(1),
            items: Some(vec![json!({ "titulo": "a" })]),
            memoria: mem("x"),
        };
        let (e, h1) = despues(e, &p, hit(), 10, 610, true);
        let (e, h2) = despues(e, &p, hit(), 610, 1210, true);
        assert_eq!(e.fase, Fase::Activa);
        assert_eq!(h1.unwrap().id, "q1.1");
        let h2 = h2.unwrap();
        assert_eq!(h2.id, "q1.2");
        assert!(
            h2.contexto["nuevos"][0]
                .as_str()
                .unwrap()
                .contains("titulo")
        );
    }

    #[test]
    fn failures_back_off_then_break_with_a_fact() {
        let p = pregunta(false);
        let e = nuevo(&p, 0, 0);
        let (e, h) = despues(e, &p, Lectura::Error("HTTP 500".into()), 0, 600, false);
        assert!(h.is_none());
        assert_eq!((e.fallos, e.proxima), (1, 1200));
        let (e, _) = despues(e, &p, Lectura::Error("HTTP 500".into()), 1200, 0, false);
        assert_eq!(e.proxima, 1200 + 2400);
        // A good look resets the count.
        let (e, _) = despues(
            e,
            &p,
            Lectura::NoCumple { memoria: mem("a") },
            3600,
            4200,
            false,
        );
        assert_eq!(e.fallos, 0);
        let mut e = e;
        let mut last = None;
        for t in 0..3 {
            let (next, h) = despues(e, &p, Lectura::Error("sin red".into()), 5000 + t, 0, false);
            e = next;
            last = h;
        }
        assert_eq!(e.fase, Fase::Rota);
        let h = last.unwrap();
        assert_eq!(h.id, "q1.rota");
        assert_eq!(h.decir, "Dejé de poder mirar el PR 7: sin red");
        assert_eq!(backoff(600, 10), MAX_BACKOFF_S);
        assert_eq!(backoff(10 * 3600, 3), 10 * 3600);
    }

    #[test]
    fn expiry_and_cancel() {
        let clock = FakeClock::at(Civil::date(2026, 11, 23).at(9, 0, 0), -3);
        let mut raw = pregunta(false).raw;
        raw["vence"] = json!("2026-11-30T09:00");
        raw["vigia"]["hasta"] = json!("2026-11-25T09:00");
        let p = parse_pregunta(&raw).unwrap();
        let hasta = clock
            .epoch_of(Civil::date(2026, 11, 25).at(9, 0, 0))
            .unwrap();
        assert_eq!(limite(&p, clock.as_ref()), Some(hasta));
        let e = vencer(nuevo(&p, 0, 0), 5);
        assert_eq!(e.fase, Fase::Vencida);
        assert_eq!(cancelar(e.clone(), 9), e, "a terminal state stays");
        assert_eq!(cancelar(nuevo(&p, 0, 0), 9).fase, Fase::Cancelada);
    }

    #[test]
    fn quiet_hours_push_a_look_to_the_morning() {
        let clock = FakeClock::at(Civil::date(2026, 11, 23).at(23, 30, 0), -3);
        let quiet = Some((23 * 60, 8 * 60));
        let morning = clock
            .epoch_of(Civil::date(2026, 11, 24).at(8, 0, 0))
            .unwrap();
        assert_eq!(
            fuera_de_silencio(clock.now(), clock.as_ref(), quiet),
            morning
        );
        let early = clock
            .epoch_of(Civil::date(2026, 11, 24).at(3, 0, 0))
            .unwrap();
        assert_eq!(fuera_de_silencio(early, clock.as_ref(), quiet), morning);
        let noon = clock
            .epoch_of(Civil::date(2026, 11, 24).at(12, 0, 0))
            .unwrap();
        assert_eq!(fuera_de_silencio(noon, clock.as_ref(), quiet), noon);
        assert_eq!(fuera_de_silencio(noon, clock.as_ref(), None), noon);
    }
}
