//! The worker: folds incidents into the journal, watches applied fixes,
//! runs the nightly review and, at most `max_intentos_dia` times a day,
//! one attempt. One worker per home (a lock file). It reads the config
//! again every minute, so turning it on or off needs no restart.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use wawi_tools::WallClock;

use crate::config::Config;
use crate::diario;
use crate::incidentes;
use crate::intento::{self, Contexto, EstadoServicio, Fin};
use crate::log::Log;
use crate::motor::{Comandos, Motor};
use crate::revision;
use crate::vigia;

pub const LOCK: &str = "autoarreglo/worker.lock";
pub const TICK: Duration = Duration::from_secs(2);
/// How often it considers the review and an attempt.
pub const WORK_EVERY_S: i64 = 60;

/// Takes the worker lock: `None` when another process holds it.
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

pub struct Worker {
    pub home: PathBuf,
    pub clock: Rc<dyn WallClock>,
    pub motor: Box<dyn Motor>,
    pub comandos: Box<dyn Comandos>,
    pub log: Log,
    pub last_work: Option<i64>,
}

impl Worker {
    /// Incidents from the queue into the journal. Returns how many.
    pub fn absorber(&self) -> usize {
        let mut n = 0;
        for (stem, text) in incidentes::nuevos(&self.home) {
            match incidentes::parse(&text) {
                Ok(incidente) => {
                    let ts = crate::clock::parse_epoch(&*self.clock, &incidente.creado)
                        .unwrap_or_else(|_| self.clock.now());
                    match diario::absorb(&self.home, &incidente, ts) {
                        Ok(p) => {
                            vigia::observar(&self.home, ts, &p.id, &p.tipo);
                            incidentes::archivar(&self.home, &stem, &text, "anotado", &p.id);
                            n += 1;
                        }
                        Err(e) => self
                            .log
                            .line(&format!("incidente {stem}: no pude anotarlo: {e}")),
                    }
                }
                Err(why) => {
                    self.log.line(&format!("incidente {stem}: inválido: {why}"));
                    incidentes::archivar(&self.home, &stem, &text, "invalido", "");
                }
            }
        }
        n
    }

    pub fn contexto<'a>(&'a self, config: &'a Config) -> Contexto<'a> {
        Contexto {
            home: &self.home,
            config,
            clock: &*self.clock,
            motor: &*self.motor,
            comandos: &*self.comandos,
            log: &self.log,
            home_dir: std::env::var("HOME").unwrap_or_default(),
            user: std::env::var("USER").unwrap_or_default(),
        }
    }

    /// One tick. The review and the attempt only once a minute.
    pub fn paso(&mut self) -> Option<Fin> {
        vigia::mirar(&self.home, &*self.clock, &self.log);
        self.absorber();
        let now = self.clock.now();
        if self.last_work.is_some_and(|t| now - t < WORK_EVERY_S) {
            return None;
        }
        self.last_work = Some(now);
        let (config, problem) = Config::load(&self.home);
        if let Some(problem) = problem {
            self.log.line(&problem);
        }
        if revision::toca(&config, &EstadoServicio::load(&self.home), &*self.clock) {
            revision::revisar(&self.home, &config, &*self.motor, &*self.clock, &self.log);
            self.absorber();
        }
        if !config.activo {
            return None;
        }
        let ctx = self.contexto(&config);
        if intento::puede(&ctx, false).is_err() {
            return None;
        }
        Some(intento::intentar(&ctx, false, None))
    }
}

/// Runs until `stop`.
pub fn run(mut worker: Worker, stop: Arc<AtomicBool>) {
    let (config, _) = Config::load(&worker.home);
    intento::limpiar_cortado(&worker.home, &config, &worker.log);
    let _ = incidentes::ensure(&worker.home);
    while !stop.load(Ordering::SeqCst) {
        if let Some(fin) = worker.paso() {
            worker.log.line(&format!("trabajo: {fin:?}"));
        }
        std::thread::sleep(TICK);
    }
}
