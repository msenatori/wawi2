//! The watchdog after a fix goes in. It never reverts on its own: it
//! offers the revert (a fact for Wawi), and the owner decides.
//!
//! The first run after the fix is the first `startup` in
//! `logs/session.log` after it was applied. During that run's first 30
//! minutes it counts serious incidents (the brain going down, a plugin
//! crash loop, a failed turn) and the fixed problem coming back. The run
//! looks bad when it ended without its clean `ended:`/`shutdown` lines
//! (a crash: the next `startup` came first), when there were two serious
//! incidents, or when the fixed problem came back. Otherwise, after a
//! clean end or 30 minutes, the fix is fine.

use std::path::Path;

use serde_json::json;
use wawi_tools::WallClock;

use crate::log::Log;
use crate::propuestas::{self, EstadoP, Propuesta};

/// How long the first run is watched.
pub const VENTANA_S: i64 = 30 * 60;
/// Serious incidents in that window that make it look bad.
pub const GRAVES_MAX: u32 = 2;
const SERIOUS: &[&str] = &["cerebro_caido", "plugin_caido", "turno_fallido"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sesion {
    pub inicio: i64,
    /// It logged its clean end (`ended:` or `shutdown`).
    pub limpia: bool,
    pub fin: Option<i64>,
}

/// The runs in `session.log` text, in order.
pub fn sesiones(text: &str, clock: &dyn WallClock) -> Vec<Sesion> {
    let mut out: Vec<Sesion> = Vec::new();
    for line in text.lines() {
        if line.len() < 20 {
            continue;
        }
        let Some(ts) = line
            .get(..19)
            .and_then(wawi_tools::clock::parse_local)
            .and_then(|c| clock.epoch_of(c))
        else {
            continue;
        };
        let rest = line.get(19..).unwrap_or("");
        let rest = rest
            .trim_start_matches(|c: char| c == '.' || c.is_ascii_digit())
            .trim();
        if rest == "startup" {
            out.push(Sesion {
                inicio: ts,
                limpia: false,
                fin: None,
            });
        } else if (rest.starts_with("ended:") || rest == "shutdown")
            && let Some(last) = out.last_mut()
        {
            last.limpia = true;
            last.fin = Some(ts);
        }
    }
    out
}

/// What the watch says now about one applied fix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Juicio {
    Esperando,
    Mirando,
    Bien,
    Mal(String),
}

/// Pure: the verdict from the runs, the counts and the time.
pub fn juzgar(p: &Propuesta, runs: &[Sesion], now: i64) -> (Juicio, Option<i64>) {
    let Some(v) = &p.vigia else {
        return (Juicio::Bien, None);
    };
    let arranque = v
        .arranque
        .or_else(|| runs.iter().find(|s| s.inicio > v.desde).map(|s| s.inicio));
    let Some(start) = arranque else {
        return (Juicio::Esperando, None);
    };
    if v.recurrencias > 0 {
        return (
            Juicio::Mal(format!(
                "el problema volvió a pasar {} {} después del arreglo",
                v.recurrencias,
                if v.recurrencias == 1 { "vez" } else { "veces" }
            )),
            Some(start),
        );
    }
    if v.graves >= GRAVES_MAX {
        return (
            Juicio::Mal(format!(
                "en el primer arranque después del arreglo hubo {} fallas graves",
                v.graves
            )),
            Some(start),
        );
    }
    let run = runs.iter().position(|s| s.inicio == start);
    if let Some(i) = run {
        let this = &runs[i];
        if !this.limpia && runs.get(i + 1).is_some() {
            return (
                Juicio::Mal("me cerré de golpe en el primer arranque después del arreglo".into()),
                Some(start),
            );
        }
        if this.limpia || now >= start + VENTANA_S {
            return (Juicio::Bien, Some(start));
        }
    } else if now >= start + VENTANA_S {
        return (Juicio::Bien, Some(start));
    }
    (Juicio::Mirando, Some(start))
}

fn watched(home: &Path) -> Vec<Propuesta> {
    propuestas::todas(home)
        .into_iter()
        .filter(|p| p.estado == EstadoP::Aplicada)
        .filter(|p| {
            p.vigia
                .as_ref()
                .is_some_and(|v| v.estado == "esperando" || v.estado == "mirando")
        })
        .collect()
}

/// One look at every watched fix. Offers the revert when one looks bad.
pub fn mirar(home: &Path, clock: &dyn WallClock, log: &Log) {
    let list = watched(home);
    if list.is_empty() {
        return;
    }
    let (text, _) = crate::revision::read_from(&home.join("logs/session.log"), 0);
    let runs = sesiones(&text, clock);
    for mut p in list {
        let (verdict, start) = juzgar(&p, &runs, clock.now());
        let Some(v) = p.vigia.as_mut() else { continue };
        v.arranque = start.or(v.arranque);
        let before = v.estado.clone();
        match &verdict {
            Juicio::Esperando => {}
            Juicio::Mirando => v.estado = "mirando".into(),
            Juicio::Bien => v.estado = "bien".into(),
            Juicio::Mal(why) => {
                v.estado = "sospechoso".into();
                v.motivo = why.clone();
            }
        }
        let changed = before != v.estado;
        if changed {
            log.line(&format!("vigía {}: {} -> {}", p.id, before, v.estado));
            let _ = propuestas::guardar(home, &p, None);
        }
        if let Juicio::Mal(why) = verdict
            && changed
        {
            let _ = propuestas::hecho(
                home,
                &format!("autoarreglo-revertir-{}", p.id),
                &crate::truncate(
                    &format!(
                        "El arreglo que apliqué para «{}» parece que me hizo mal: {why}. ¿Lo deshago?",
                        p.titulo
                    ),
                    300,
                ),
                json!({ "arreglo": p.id, "para_deshacerlo": "autoarreglo__revertir_arreglo" }),
                clock,
            );
        }
    }
}

/// Counts an incident against the fixes being watched, if it fell inside
/// their first run's window.
pub fn observar(home: &Path, ts: i64, problema: &str, tipo: &str) {
    for mut p in watched(home) {
        let Some(v) = p.vigia.as_mut() else { continue };
        let Some(start) = v.arranque else { continue };
        if ts < start || ts > start + VENTANA_S {
            continue;
        }
        let mut touched = false;
        if p.problema == problema && tipo != "reportado" {
            v.recurrencias += 1;
            touched = true;
        }
        if SERIOUS.contains(&tipo) {
            v.graves += 1;
            touched = true;
        }
        if touched {
            let _ = propuestas::guardar(home, &p, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::propuestas::Vigia;
    use wawi_tools::{Civil, FakeClock};

    #[test]
    fn a_crash_is_a_start_without_its_clean_end() {
        let clock = FakeClock::at(Civil::date(2026, 11, 21).at(12, 0, 0), -3);
        let text = "2026-11-21 10:00:00.000 startup\n2026-11-21 10:10:00.000 ended: Signal\n2026-11-21 10:10:00.100 shutdown\n2026-11-21 11:00:00.000 startup\n2026-11-21 11:00:05.000 event brain_failed turn 1\n2026-11-21 11:01:00.000 startup\n";
        let runs = sesiones(text, &*clock);
        assert_eq!(runs.len(), 3);
        assert!(runs[0].limpia && !runs[1].limpia);
        let applied = clock
            .epoch_of(Civil::date(2026, 11, 21).at(10, 30, 0))
            .unwrap();
        let p = Propuesta {
            id: "x".into(),
            estado: EstadoP::Aplicada,
            vigia: Some(Vigia {
                estado: "esperando".into(),
                desde: applied,
                ..Vigia::default()
            }),
            ..Propuesta::default()
        };
        let (j, start) = juzgar(&p, &runs, clock.now());
        assert!(matches!(j, Juicio::Mal(_)), "{j:?}");
        assert_eq!(start, Some(runs[1].inicio));
        let (j, _) = juzgar(&p, &runs[..2], runs[1].inicio + 60);
        assert_eq!(j, Juicio::Mirando);
        let (j, _) = juzgar(&p, &runs[..2], runs[1].inicio + VENTANA_S + 1);
        assert_eq!(j, Juicio::Bien);
        let (j, _) = juzgar(&p, &runs[..1], clock.now());
        assert_eq!(j, Juicio::Esperando);
    }
}
