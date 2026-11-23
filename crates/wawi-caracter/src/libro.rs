//! The files: `<home>/trato.md` (the owner's to read and edit),
//! `<home>/caracter/estado.json` (pending rules and vetoes) and
//! `<home>/caracter/cambios.jsonl` (every change with its evidence). One
//! lock for the learner thread and the tools, and every write goes to a
//! temporary file first, then a rename.
//!
//! The learner ([`Aprendiz`]) keeps the window, makes the model call for a
//! turn with a signal, and hands each proposal to [`aprender::decidir`].

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use wawi_core::normalize;
use wawi_instinto::claude::{Modelo, Pedido};
use wawi_memory::{TRATO_FILE, Trato};
use wawi_tools::WallClock;

use crate::aprender::{self, Decision, Estado, Op, Turno, VENTANA};
use crate::log::Log;

pub const ESTADO: &str = "caracter/estado.json";
pub const CAMBIOS: &str = "caracter/cambios.jsonl";
/// Turns waiting for the learner; beyond it the newest is dropped.
pub const QUEUE: usize = 8;
const TIMEOUT: Duration = Duration::from_secs(90);

pub struct Libro {
    pub home: PathBuf,
    pub log: Log,
    lock: Mutex<()>,
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

impl Libro {
    pub fn new(home: &Path, log: Log) -> Self {
        Self {
            home: home.to_path_buf(),
            log,
            lock: Mutex::new(()),
        }
    }

    pub fn trato_path(&self) -> PathBuf {
        self.home.join(TRATO_FILE)
    }

    fn load(&self) -> (Trato, Estado) {
        let trato = std::fs::read_to_string(self.trato_path())
            .map(|t| Trato::parse(&t))
            .unwrap_or_default();
        let mut estado: Estado = std::fs::read_to_string(self.home.join(ESTADO))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        estado.conciliar(&trato);
        (trato, estado)
    }

    /// Loads, runs `f`, saves. trato.md is written only when its rules
    /// changed, so an untouched file stays as the owner left it.
    pub fn con<R>(&self, f: impl FnOnce(&mut Trato, &mut Estado) -> R) -> R {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let (mut trato, mut estado) = self.load();
        let before = trato.clone();
        let out = f(&mut trato, &mut estado);
        if trato != before
            && let Err(e) = write_atomic(&self.trato_path(), &trato.render())
        {
            self.log.line(&format!("no pude escribir trato.md: {e}"));
        }
        estado.escritas = trato.rules.clone();
        if let Ok(text) = serde_json::to_string_pretty(&estado)
            && let Err(e) = write_atomic(&self.home.join(ESTADO), &text)
        {
            self.log.line(&format!("no pude guardar el estado: {e}"));
        }
        out
    }

    /// Appends a change, with its evidence, to `cambios.jsonl`: the owner's
    /// words live there and nowhere else.
    pub fn anotar(&self, decision: &Decision, fecha: &str, por: &str) {
        let Decision::Aplicada {
            op,
            regla,
            antes,
            evidencias,
        } = decision
        else {
            return;
        };
        let line = json!({
            "fecha": fecha,
            "por": por,
            "op": op,
            "regla": regla,
            "antes": antes,
            "evidencia": evidencias,
        });
        let path = self.home.join(CAMBIOS);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{line}");
        }
        self.log.line(&format!(
            "cambio: {} ({} evidencia(s), {por})",
            serde_json::to_value(op)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default(),
            evidencias.len()
        ));
    }

    /// The answer to «¿Cómo me tratás?».
    pub fn describir(&self) -> String {
        self.con(|trato, estado| {
            let mut text = if trato.is_empty() {
                "Todavía no aprendí nada de cómo te gusta que te hable.".to_owned()
            } else {
                let mut t = "Así te hablo, por lo que me fuiste diciendo:\n".to_owned();
                for rule in &trato.rules {
                    t.push_str(&format!("- {rule}\n"));
                }
                t
            };
            if !estado.pendientes.is_empty() {
                text.push_str(&format!(
                    "\nY tengo {} cosa(s) en duda, que todavía no vi las veces que hacen falta.",
                    estado.pendientes.len()
                ));
            }
            text.push_str(&format!(
                "\nEstá en {}: lo podés leer, cambiar o borrar.",
                self.trato_path().display()
            ));
            text
        })
    }

    /// The owner takes a rule out (the `quitar_regla` tool): it is vetoed.
    pub fn quitar(&self, regla: &str, fecha: &str) -> Result<String, String> {
        let regla = regla.trim();
        if regla.is_empty() {
            return Err("falta la regla".into());
        }
        let decision = self.con(|trato, estado| {
            let at = trato
                .rules
                .iter()
                .position(|r| normalize(r) == normalize(regla))
                .or_else(|| trato.rules.iter().position(|r| aprender::misma(r, regla)))?;
            let old = trato.rules.remove(at);
            if !estado.vetadas.contains(&old) {
                estado.vetadas.push(old.clone());
            }
            Some(Decision::Aplicada {
                op: Op::Quitar,
                regla: old,
                antes: None,
                evidencias: Vec::new(),
            })
        });
        match decision {
            Some(d) => {
                self.anotar(&d, fecha, "el dueño");
                let Decision::Aplicada { regla, .. } = d else {
                    unreachable!()
                };
                Ok(format!(
                    "Listo, saqué «{regla}». No la vuelvo a aprender sola."
                ))
            }
            None => Err(format!(
                "No tengo una regla como «{regla}». Las que tengo salen con caracter__trato."
            )),
        }
    }

    /// Clears the trato and what was pending. The vetoes stay.
    pub fn olvidar_todo(&self) -> String {
        let n = self.con(|trato, estado| {
            let n = trato.rules.len();
            for rule in trato.rules.drain(..) {
                if !estado.vetadas.contains(&rule) {
                    estado.vetadas.push(rule);
                }
            }
            estado.pendientes.clear();
            n
        });
        self.log
            .line(&format!("olvidé el trato entero ({n} reglas)"));
        format!("Listo, me olvidé de las {n} reglas del trato. Empiezo de cero.")
    }
}

/// The learner: the window and one model call per turn with a signal.
pub struct Aprendiz {
    pub libro: Arc<Libro>,
    pub modelo: Box<dyn Modelo>,
    pub clock: Rc<dyn WallClock>,
    pub nombre_modelo: String,
    pub esfuerzo: String,
    ventana: VecDeque<Turno>,
}

impl Aprendiz {
    pub fn new(
        libro: Arc<Libro>,
        modelo: Box<dyn Modelo>,
        clock: Rc<dyn WallClock>,
        nombre_modelo: &str,
        esfuerzo: &str,
    ) -> Self {
        Self {
            libro,
            modelo,
            clock,
            nombre_modelo: nombre_modelo.into(),
            esfuerzo: esfuerzo.into(),
            ventana: VecDeque::new(),
        }
    }

    /// One finished turn. Without a signal it only joins the window.
    pub fn procesar(&mut self, turno: Turno) -> Vec<Decision> {
        let mirar = aprender::vale_mirar(&turno);
        self.ventana.push_back(turno);
        while self.ventana.len() > VENTANA {
            self.ventana.pop_front();
        }
        if !mirar {
            return Vec::new();
        }
        let ventana: Vec<Turno> = self.ventana.iter().cloned().collect();
        let (trato, pendientes) = self.libro.con(|t, e| {
            e.podar(self.clock.now());
            (t.clone(), e.pendientes.clone())
        });
        let schema = aprender::esquema();
        let respuesta = self.modelo.preguntar(&Pedido {
            modelo: &self.nombre_modelo,
            esfuerzo: Some(self.esfuerzo.as_str()).filter(|e| !e.is_empty()),
            sistema: aprender::SISTEMA,
            mensaje: &aprender::mensaje(&ventana, &trato, &pendientes),
            esquema: &schema,
            timeout: TIMEOUT,
        });
        let respuesta = match respuesta {
            Ok(r) => r,
            Err(e) => {
                self.libro.log.line(&format!("el modelo falló: {e}"));
                return Vec::new();
            }
        };
        let propuestas = aprender::propuestas(&respuesta.valor);
        let ahora = self.clock.now();
        let fecha = self.clock.local_now().iso_minute();
        let decisions = self.libro.con(|trato, estado| {
            propuestas
                .iter()
                .map(|p| aprender::decidir(p, &ventana, trato, estado, ahora))
                .collect::<Vec<_>>()
        });
        for d in &decisions {
            match d {
                Decision::Aplicada { .. } => self.libro.anotar(d, &fecha, "aprendido"),
                Decision::Pendiente { id, faltan } => self
                    .libro
                    .log
                    .line(&format!("pendiente {id}: faltan {faltan} turno(s)")),
                Decision::Rechazo(why) => self.libro.log.line(&format!("descarté: {why}")),
            }
        }
        decisions
    }
}

/// Starts the learner on its own thread. Sending never blocks: when
/// [`QUEUE`] turns are waiting, a new one is dropped (and logged).
pub fn spawn(
    make: impl FnOnce() -> Aprendiz + Send + 'static,
    log: Log,
) -> impl Fn(Turno) + Send + Sync {
    let (tx, rx): (SyncSender<Turno>, Receiver<Turno>) = mpsc::sync_channel(QUEUE);
    let _ = std::thread::Builder::new()
        .name("caracter-aprendiz".into())
        .spawn(move || {
            let mut aprendiz = make();
            for turno in rx {
                aprendiz.procesar(turno);
            }
        });
    move |turno| match tx.try_send(turno) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => log.line("va lento; descarté un turno"),
        Err(TrySendError::Disconnected(_)) => log.line("el aprendiz no está corriendo"),
    }
}
