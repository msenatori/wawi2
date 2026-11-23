//! The worker: no model in the loop (except the optional judge). Every
//! tick it takes new questions from `<home>/cola/preguntas/new`, applies
//! cancellations, and looks at every question that is due. A question that
//! comes true becomes a fact in `<home>/cola/hechos`.
//!
//! Crash safety: every file is written to a temp file and renamed. A fact
//! is written before the state that says it was written, and its id is the
//! question's, so a crash in between means the next look finds the fact
//! already there and does not write it again.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;
use wawi_tools::WallClock;

use crate::colas;
use crate::cuando::{self, Memoria};
use crate::estado::{self, Estado, Fase};
use crate::extraer;
use crate::fuentes::{Ctx, Registro};
use crate::juez::Juez;
use crate::log::Log;
use crate::motor::{self, Lectura};
use crate::spec::{Pregunta, parse_pregunta};

pub const LOCK: &str = "instinto/worker.lock";
/// How often the worker wakes up.
pub const TICK: Duration = Duration::from_secs(2);
/// The longest a source may take in the worker.
pub const SOURCE_TIMEOUT: Duration = Duration::from_secs(30);

/// Takes the worker lock: only one worker per home. `None` when another
/// process holds it. The lock lasts as long as the file is open.
pub fn lock(home: &Path) -> std::io::Result<Option<File>> {
    let path = home.join(LOCK);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// One look: source, extraction, predicate and, if the rest passed and
/// there are new items, the judge. With no judge (`vigilar`'s dry run) the
/// judge stage is skipped. Gives the look and the extracted value.
pub fn mirar(
    fuentes: &Registro,
    ctx: &Ctx<'_>,
    pregunta: &Pregunta,
    memoria: &Memoria,
    juez: Option<&dyn Juez>,
    log: &Log,
) -> (Lectura, Option<Value>) {
    let raw = match fuentes.leer(&pregunta.vigia.fuente, ctx) {
        Ok(raw) => raw,
        Err(e) => return (Lectura::Error(e), None),
    };
    let valor = extraer::apply(&pregunta.vigia.extraer, raw);
    let (resultado, memoria) =
        match cuando::evaluar(pregunta.vigia.cuando.as_ref(), &valor, memoria) {
            Ok(r) => r,
            Err(e) => return (Lectura::Error(e), Some(valor)),
        };
    if !resultado.cumple {
        return (Lectura::NoCumple { memoria }, Some(valor));
    }
    let mut items = resultado.items;
    if let (Some(juzgar), Some(juez), Some(list)) = (&pregunta.vigia.juzgar, juez, items.as_mut()) {
        let fecha = ctx.clock.local_now().iso_date();
        let list_now: Vec<Value> = list.iter().take(crate::juez::MAX_ITEMS).cloned().collect();
        match juez.juzgar(
            &juzgar.pregunta,
            juzgar.modelo.as_deref(),
            &list_now,
            &fecha,
        ) {
            Ok(Some(yes)) => {
                *list = yes
                    .into_iter()
                    .filter_map(|i| list_now.get(i).cloned())
                    .collect();
            }
            Ok(None) => {
                log.line(&format!("{}: sin cupo para juzgar hoy", pregunta.id));
                list.clear();
            }
            Err(e) => return (Lectura::Error(format!("el juez falló: {e}")), Some(valor)),
        }
        if list.is_empty() {
            return (Lectura::NoCumple { memoria }, Some(valor));
        }
    }
    (
        Lectura::Cumple {
            valor: valor.clone(),
            items,
            memoria,
        },
        Some(valor),
    )
}

pub struct Worker {
    pub home: PathBuf,
    pub clock: Rc<dyn WallClock>,
    pub fuentes: Registro,
    pub juez: Box<dyn Juez>,
    pub log: Log,
    pub quiet: Option<(u16, u16)>,
}

impl Worker {
    fn ctx(&self) -> Ctx<'_> {
        Ctx {
            home: &self.home,
            clock: self.clock.as_ref(),
            timeout: SOURCE_TIMEOUT,
        }
    }

    fn stamp(&self) -> String {
        let now = self.clock.local_now();
        format!("{}:{:02}", now.iso_minute(), now.second)
    }

    fn quiet_adjust(&self, epoch: i64) -> i64 {
        motor::fuera_de_silencio(epoch, self.clock.as_ref(), self.quiet)
    }

    /// Everything one wake-up does.
    pub fn tick(&mut self) {
        let _ = colas::ensure(&self.home);
        self.ingest();
        self.cancels();
        self.run_due();
    }

    /// Takes new questions from the queue into state files.
    pub fn ingest(&mut self) {
        let now = self.clock.now();
        for (stem, text) in colas::nuevas(&self.home) {
            if estado::load(&self.home, &stem).is_some() {
                // Taken before a crash, or a repeat: idempotent.
                colas::archivar(
                    &self.home,
                    &stem,
                    &text,
                    "repetida",
                    "ya la tenía",
                    &self.stamp(),
                );
                continue;
            }
            let parsed = serde_json::from_str::<Value>(&text)
                .map_err(|_| "no es JSON".to_owned())
                .and_then(|v| parse_pregunta(&v))
                .and_then(|p| {
                    if p.id != stem {
                        return Err("el id no coincide con el nombre del archivo".into());
                    }
                    self.fuentes.validar(&p.vigia.fuente, &self.ctx())?;
                    Ok(p)
                });
            match parsed {
                Err(reason) => {
                    self.log
                        .line(&format!("pregunta {stem} inválida: {reason}"));
                    colas::archivar(&self.home, &stem, &text, "invalida", &reason, &self.stamp());
                }
                Ok(pregunta) => {
                    let first = self
                        .fuentes
                        .proxima(&pregunta.vigia.fuente, &self.ctx())
                        .unwrap_or(now);
                    let first = self.quiet_adjust(first);
                    let estado = motor::nuevo(&pregunta, first, now);
                    if let Err(e) = estado::save(&self.home, &estado) {
                        self.log
                            .line(&format!("pregunta {stem}: no pude guardar su estado: {e}"));
                        continue;
                    }
                    self.log
                        .line(&format!("pregunta {stem} tomada (de {})", pregunta.origen));
                    colas::archivar(&self.home, &stem, &text, "tomada", "", &self.stamp());
                }
            }
        }
    }

    /// Applies `cancelar_seguimiento` requests.
    pub fn cancels(&mut self) {
        let now = self.clock.now();
        for id in colas::cancelaciones(&self.home) {
            if let Some(estado) = estado::load(&self.home, &id) {
                let estado = motor::cancelar(estado, now);
                if estado::save(&self.home, &estado).is_err() {
                    continue;
                }
                self.log.line(&format!("{id}: cancelada"));
            }
            colas::cancelada(&self.home, &id);
        }
    }

    /// Looks at every question that is due.
    pub fn run_due(&mut self) {
        let now = self.clock.now();
        for estado in estado::load_all(&self.home) {
            if estado.fase.terminal() || estado.proxima > now {
                continue;
            }
            self.run_one(estado, now);
        }
    }

    fn save(&self, estado: &Estado) {
        if let Err(e) = estado::save(&self.home, estado) {
            self.log
                .line(&format!("{}: no pude guardar su estado: {e}", estado.id));
        }
    }

    fn run_one(&mut self, mut estado: Estado, now: i64) {
        let pregunta = match parse_pregunta(&estado.pregunta) {
            Ok(p) => p,
            Err(e) => {
                estado.fase = Fase::Rota;
                estado.motivo = Some(format!("la pregunta ya no se entiende: {e}"));
                estado.actualizado = now;
                self.save(&estado);
                return;
            }
        };
        let limite = motor::limite(&pregunta, self.clock.as_ref());
        if limite.is_some_and(|l| now >= l) {
            self.log.line(&format!("{}: vencida", estado.id));
            self.save(&motor::vencer(estado, now));
            return;
        }
        let quiet = self.quiet_adjust(now);
        if quiet > now {
            estado.proxima = quiet;
            self.save(&estado);
            return;
        }
        let ctx = self.ctx();
        let (lectura, _) = mirar(
            &self.fuentes,
            &ctx,
            &pregunta,
            &estado.memoria,
            Some(self.juez.as_ref()),
            &self.log,
        );
        let mut siguiente = self
            .fuentes
            .proxima(&pregunta.vigia.fuente, &ctx)
            .unwrap_or(now + motor::every(&pregunta));
        if let Some(limite) = limite {
            siguiente = siguiente.min(limite);
        }
        let repetir = pregunta.vigia.repetir || self.fuentes.repite(&pregunta.vigia.fuente);
        let outcome = match &lectura {
            Lectura::Cumple { .. } => "se cumplió".to_owned(),
            Lectura::NoCumple { .. } => "todavía no".to_owned(),
            Lectura::Error(e) => format!("error: {e}"),
        };
        let (mut next, hecho) = motor::despues(estado, &pregunta, lectura, now, siguiente, repetir);
        if !next.fase.terminal() {
            next.proxima = self.quiet_adjust(next.proxima);
            if let Some(limite) = limite {
                next.proxima = next.proxima.min(limite);
            }
        }
        if let Some(hecho) = &hecho {
            match colas::escribir_hecho(&self.home, hecho, &self.stamp()) {
                Ok(true) => self
                    .log
                    .line(&format!("{}: hecho {} escrito", next.id, hecho.id)),
                Ok(false) => self
                    .log
                    .line(&format!("{}: el hecho {} ya estaba", next.id, hecho.id)),
                Err(e) => {
                    // Keep the old state: the next tick tries again.
                    self.log
                        .line(&format!("{}: no pude escribir el hecho: {e}", next.id));
                    return;
                }
            }
        }
        self.log
            .line(&format!("{}: {outcome} → {}", next.id, next.fase.nombre()));
        self.save(&next);
    }
}

/// Ticks until `stop` is set.
pub fn run(mut worker: Worker, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::SeqCst) {
        worker.tick();
        let mut slept = Duration::ZERO;
        while slept < TICK && !stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(100));
            slept += Duration::from_millis(100);
        }
    }
}
