//! One attempt, start to end, and the daily caps.
//!
//! 1. Preconditions: on (`activo`), a repo with at least one commit (else a
//!    friendly blocked request), inside the time window, under the daily
//!    attempt cap and the daily budget, and a base that passes its own
//!    tests (else no money is spent on it).
//! 2. The most useful open problem ([`crate::elegir`]).
//! 3. A worktree OUTSIDE the repo, `<home>/autoarreglo/trabajo/<id>`, on a
//!    new branch `autoarreglo/<id>` from HEAD.
//! 4. The author (Opus) with the scrubbed brief. It never commits: the
//!    service commits what it left on the branch.
//! 5. The gates, in order; the first failure ends it.
//! 6. A proposal and a fact for Wawi, or the failure in the history.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wawi_tools::WallClock;

use crate::brief;
use crate::config::{Config, in_window, seconds};
use crate::diario::{self, Estado, Intento, Problema};
use crate::elegir;
use crate::gates::{self, Corrida, Resultado};
use crate::git;
use crate::log::Log;
use crate::motor::{Comandos, Motor, Rol, Tarea, cargo_env};
use crate::propuestas::{self, EstadoP, Propuesta};

pub const ESTADO: &str = "autoarreglo/estado.json";
/// The reviewer's time limit.
const REVIEW_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Days of spending kept in the state file.
const DAYS_KEPT: usize = 14;

/// What a day has spent.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Dia {
    pub usd: f64,
    pub intentos: u32,
}

/// `<home>/autoarreglo/estado.json`: the money, the review's bookmarks and
/// the attempt in progress.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EstadoServicio {
    pub dias: BTreeMap<String, Dia>,
    /// The HEAD whose own tests failed: no attempt until it moves.
    pub base_roja: Option<String>,
    pub en_curso: Option<String>,
    /// The last day the nightly review ran for.
    pub revision_dia: Option<String>,
    /// Bytes of each log already read by the review.
    pub revision_offsets: BTreeMap<String, u64>,
    /// When `compilar` last finished well.
    pub compilado_ts: Option<i64>,
}

impl EstadoServicio {
    pub fn load(home: &Path) -> Self {
        std::fs::read_to_string(home.join(ESTADO))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&mut self, home: &Path) {
        while self.dias.len() > DAYS_KEPT {
            let first = self.dias.keys().next().cloned();
            if let Some(first) = first {
                self.dias.remove(&first);
            }
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = crate::write_atomic(&home.join(ESTADO), &text);
        }
    }

    pub fn dia(&mut self, day: &str) -> &mut Dia {
        self.dias.entry(day.to_owned()).or_default()
    }

    /// Adds a cost to today, saved at once.
    pub fn gastar(home: &Path, day: &str, usd: f64) {
        let mut estado = Self::load(home);
        estado.dia(day).usd += usd.max(0.0);
        estado.save(home);
    }
}

pub fn worktree_path(home: &Path, id: &str) -> PathBuf {
    crate::dir(home).join("trabajo").join(id)
}

pub fn intento_dir(home: &Path, id: &str) -> PathBuf {
    crate::dir(home).join("intentos").join(id)
}

/// Everything an attempt needs, injected.
pub struct Contexto<'a> {
    pub home: &'a Path,
    pub config: &'a Config,
    pub clock: &'a dyn WallClock,
    pub motor: &'a dyn Motor,
    pub comandos: &'a dyn Comandos,
    pub log: &'a Log,
    /// For the secret scan: `$HOME` and `$USER`.
    pub home_dir: String,
    pub user: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Fin {
    /// Nothing was attempted, and why.
    Nada(String),
    /// A tested fix waits for the owner.
    Propuesta(String),
    /// It was attempted and failed: the reason.
    Fallo(String),
}

/// Whether an attempt may start now (`manual` skips the time window).
pub fn puede(ctx: &Contexto<'_>, manual: bool) -> Result<PathBuf, String> {
    let c = ctx.config;
    if !c.activo {
        return Err("apagado (activo = false en autoarreglo/config.toml)".into());
    }
    let repo = c
        .repo()
        .ok_or("no sé dónde está el repo (repo en autoarreglo/config.toml)")?;
    if !git::is_repo(&repo) {
        return Err(format!("{} no es un repo de git", repo.display()));
    }
    if !git::has_commits(&repo) {
        let _ = propuestas::avisar_sin_commit(ctx.home, ctx.clock);
        return Err("el repo no tiene ningún commit".into());
    }
    let now = ctx.clock.local_now();
    if !manual && !in_window(&c.ventana, u16::from(now.hour) * 60 + u16::from(now.minute))? {
        return Err(format!("fuera de la ventana ({})", c.ventana));
    }
    let mut estado = EstadoServicio::load(ctx.home);
    if let Some(id) = &estado.en_curso {
        return Err(format!("ya hay un intento en curso ({id})"));
    }
    if let (Some(red), Ok(head)) = (&estado.base_roja, git::head(&repo))
        && *red == head
    {
        return Err("las pruebas de la base no pasan; espero un commit nuevo".into());
    }
    let day = estado.dia(&now.iso_date()).clone();
    if day.intentos >= c.max_intentos_dia {
        return Err(format!("ya hice {} intentos hoy", day.intentos));
    }
    if day.usd + c.attempt_cap() > c.presupuesto_dia_usd {
        return Err(format!(
            "no alcanza el presupuesto de hoy (gastado US$ {:.2} de {:.2})",
            day.usd, c.presupuesto_dia_usd
        ));
    }
    Ok(repo)
}

/// The next problem to work on, or a specific one.
pub fn elegir(ctx: &Contexto<'_>, solo: Option<&str>) -> Result<Problema, String> {
    let all = diario::load_all(ctx.home);
    match solo {
        Some(id) => {
            let p = all
                .into_iter()
                .find(|p| p.id == id)
                .ok_or_else(|| format!("no hay un problema {id}"))?;
            elegir::elegible(&p, ctx.config, ctx.clock.now())
                .map_err(|why| format!("{id} no se puede intentar ahora: {why}"))?;
            Ok(p)
        }
        None => elegir::ordenar(&all, ctx.config, ctx.clock.now())
            .first()
            .map(|p| (*p).clone())
            .ok_or_else(|| "no hay nada para arreglar".to_owned()),
    }
}

/// One whole attempt. Never panics on a bad outcome: every failure is a
/// [`Fin`] with its reason, and the worktree is always cleaned up.
pub fn intentar(ctx: &Contexto<'_>, manual: bool, solo: Option<&str>) -> Fin {
    let repo = match puede(ctx, manual) {
        Ok(repo) => repo,
        Err(why) => return Fin::Nada(why),
    };
    let problema = match elegir(ctx, solo) {
        Ok(p) => p,
        Err(why) => return Fin::Nada(why),
    };
    let now = ctx.clock.local_now();
    let tail: String = problema
        .id
        .chars()
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let id = format!(
        "{:04}{:02}{:02}-{:02}{:02}-{tail}",
        now.year, now.month, now.day, now.hour, now.minute
    );
    let worktree = worktree_path(ctx.home, &id);
    let branch = format!("{}{id}", git::BRANCH_PREFIX);
    let base = match git::head(&repo) {
        Ok(h) => h,
        Err(e) => return Fin::Nada(e),
    };
    {
        let _lock = propuestas::repo_lock(ctx.home);
        if let Err(e) = git::worktree_add(&repo, &worktree, &branch, &base) {
            return Fin::Nada(format!("no pude crear el worktree: {e}"));
        }
    }
    let mut estado = EstadoServicio::load(ctx.home);
    estado.en_curso = Some(id.clone());
    estado.save(ctx.home);
    ctx.log.line(&format!(
        "intento {id}: {} ({})",
        problema.id, problema.tipo
    ));
    let mut gates_ok = Vec::new();
    let mut cost = 0.0;
    let outcome = pipeline(
        ctx,
        &repo,
        &problema,
        &id,
        &worktree,
        &base,
        &mut gates_ok,
        &mut cost,
    );
    let mut estado = EstadoServicio::load(ctx.home);
    estado.en_curso = None;
    estado.save(ctx.home);
    let keep_branch = matches!(outcome, Fin::Propuesta(_));
    {
        let _lock = propuestas::repo_lock(ctx.home);
        let _ = git::worktree_remove(&repo, &worktree, (!keep_branch).then_some(branch.as_str()));
    }
    let record = json!({
        "id": id, "problema": problema.id, "base": base,
        "resultado": match &outcome { Fin::Propuesta(_) => "propuesto".to_owned(), Fin::Fallo(r) | Fin::Nada(r) => r.clone() },
        "costo_usd": cost, "gates": gates_ok,
    });
    let _ = crate::write_atomic(
        &intento_dir(ctx.home, &id).join("intento.json"),
        &serde_json::to_string_pretty(&record).unwrap_or_default(),
    );
    match &outcome {
        Fin::Fallo(reason) => {
            ctx.log.line(&format!("intento {id}: falló: {reason}"));
            record_attempt(ctx, &problema.id, &id, "rechazado", reason, cost);
        }
        Fin::Nada(reason) => ctx.log.line(&format!("intento {id}: no siguió: {reason}")),
        Fin::Propuesta(_) => ctx.log.line(&format!("intento {id}: propuesto")),
    }
    outcome
}

fn record_attempt(
    ctx: &Contexto<'_>,
    problema: &str,
    id: &str,
    resultado: &str,
    motivo: &str,
    cost: f64,
) {
    if let Some(mut p) = diario::load(ctx.home, problema) {
        p.intentos.push(Intento {
            id: id.into(),
            ts: ctx.clock.now(),
            resultado: resultado.into(),
            motivo: crate::truncate(motivo, 300),
            costo_usd: cost,
        });
        let extra = p.intentos.len().saturating_sub(20);
        p.intentos.drain(..extra);
        if resultado == "propuesto" {
            p.estado = Estado::Propuesto;
            p.propuesta = Some(id.into());
        }
        let _ = diario::save(ctx.home, &p);
    }
}

fn run_commands(
    ctx: &Contexto<'_>,
    dir: &Path,
    which: &[(&str, &Vec<String>)],
) -> Result<Vec<Resultado>, Resultado> {
    let timeout = Duration::from_secs(seconds(&ctx.config.pruebas.tiempo_max).unwrap_or(1500));
    let mut done = Vec::new();
    for (name, argv) in which {
        if argv.is_empty() {
            continue;
        }
        let run = ctx.comandos.correr(dir, argv, timeout);
        let gate = gates::comando(name, &run);
        if !gate.ok {
            return Err(gate);
        }
        done.push(gate);
    }
    Ok(done)
}

#[allow(clippy::too_many_arguments)]
fn pipeline(
    ctx: &Contexto<'_>,
    repo: &Path,
    problema: &Problema,
    id: &str,
    worktree: &Path,
    base: &str,
    passed: &mut Vec<Resultado>,
    cost: &mut f64,
) -> Fin {
    let c = ctx.config;
    let day = ctx.clock.local_now().iso_date();
    let commands = [
        ("fmt", &c.pruebas.fmt),
        ("clippy", &c.pruebas.clippy),
        ("pruebas", &c.pruebas.test),
    ];
    // 0. The base must be green, or every attempt would fail for nothing.
    if c.probar_base
        && let Err(gate) = run_commands(ctx, worktree, &commands)
    {
        let mut estado = EstadoServicio::load(ctx.home);
        estado.base_roja = Some(base.to_owned());
        estado.save(ctx.home);
        let _ = propuestas::hecho(
            ctx.home,
            &format!("autoarreglo-base-roja-{}", &base[..base.len().min(12)]),
            &format!(
                "Quise arreglarme, pero mis pruebas no pasan en el repo tal como está ({}). Así no puedo probar un arreglo. ¿Lo miramos?",
                gate.linea()
            ),
            json!({ "commit": base }),
            ctx.clock,
        );
        return Fin::Nada(format!("la base no pasa: {}", gate.linea()));
    }

    // 1. The author.
    let fallos: Vec<String> = problema
        .intentos
        .iter()
        .filter(|i| i.resultado != "propuesto")
        .map(|i| i.motivo.clone())
        .collect();
    let text = brief::brief(problema, &fallos);
    let folder = intento_dir(ctx.home, id);
    let _ = crate::write_atomic(&folder.join("brief.md"), &text);
    {
        let mut estado = EstadoServicio::load(ctx.home);
        estado.dia(&day).intentos += 1;
        estado.save(ctx.home);
    }
    let rules = brief::reglas_autor(c.max_lineas, c.max_archivos);
    let schema = brief::esquema_autor();
    let target = c.target_dir(ctx.home);
    let author = ctx.motor.correr(&Tarea {
        rol: Rol::Autor,
        dir: worktree,
        modelo: &c.modelo,
        presupuesto_usd: c.presupuesto_intento_usd,
        timeout: Duration::from_secs(seconds(&c.tiempo_max).unwrap_or(1800)),
        sistema: &rules,
        mensaje: &text,
        esquema: &schema,
        env: cargo_env(&target),
    });
    let author = match author {
        Ok(salida) => {
            *cost += salida.costo_usd;
            EstadoServicio::gastar(ctx.home, &day, salida.costo_usd);
            salida
        }
        Err(falla) => {
            *cost += falla.costo_usd;
            EstadoServicio::gastar(ctx.home, &day, falla.costo_usd);
            return Fin::Fallo(format!("el autor: {}", falla.motivo));
        }
    };
    let field = |k: &str| {
        crate::scrub::scrub(
            author
                .valor
                .get(k)
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim(),
        )
    };
    let blocked = field("bloqueado");
    if !blocked.is_empty() {
        let _ = propuestas::hecho(
            ctx.home,
            &format!("autoarreglo-necesito-{id}"),
            &crate::truncate(
                &format!(
                    "Para arreglar «{}» necesito algo tuyo: {blocked}",
                    problema.titulo
                ),
                300,
            ),
            json!({ "problema": problema.id }),
            ctx.clock,
        );
        return Fin::Fallo(format!(
            "necesita al dueño: {}",
            crate::truncate(&blocked, 200)
        ));
    }

    // 2. The service commits what the agent left.
    let subject = field("titulo_commit");
    match git::commit_all(worktree, &format!("autoarreglo {id}: {subject}")) {
        Ok(Some(_)) => {}
        Ok(None) => return Fin::Fallo("el autor no cambió nada".into()),
        Err(e) => return Fin::Fallo(e),
    }
    let (diff, stats) = match (
        git::diff(worktree, base, "HEAD"),
        git::numstat(worktree, base, "HEAD"),
    ) {
        (Ok(d), Ok(s)) => (d, s),
        (Err(e), _) | (_, Err(e)) => return Fin::Fallo(e),
    };
    let _ = crate::write_atomic(&folder.join("cambios.diff"), &diff);

    // 3. The cheap gates.
    for gate in [
        gates::prohibidos(&stats, &diff),
        gates::tamano(&stats, c.max_lineas, c.max_archivos),
        gates::dependencias(&stats, &diff),
        gates::secretos(&diff, &ctx.home_dir, &ctx.user),
        gates::camino_rapido(&diff),
    ] {
        if !gate.ok {
            return Fin::Fallo(gate.linea());
        }
        passed.push(gate);
    }

    // 4. fmt, clippy, the full suite, then the speed tests by name.
    match run_commands(ctx, worktree, &commands) {
        Ok(done) => passed.extend(done),
        Err(gate) => return Fin::Fallo(gate.linea()),
    }
    let timeout = Duration::from_secs(seconds(&c.pruebas.tiempo_max).unwrap_or(1500));
    let speed: Vec<(String, Corrida)> = c
        .pruebas
        .velocidad
        .iter()
        .map(|argv| {
            let name = argv.last().cloned().unwrap_or_default();
            (name, ctx.comandos.correr(worktree, argv, timeout))
        })
        .collect();
    let gate = gates::velocidad(&speed);
    if !gate.ok {
        return Fin::Fallo(gate.linea());
    }
    passed.push(gate);

    // 5. The second opinion: Opus again, read-only.
    if EstadoServicio::load(ctx.home).dia(&day).usd + c.presupuesto_revision_usd
        > c.presupuesto_dia_usd
    {
        return Fin::Fallo("no quedó presupuesto para la revisión".into());
    }
    let review_schema = brief::esquema_revisor();
    let ask = brief::pedido_revisor(problema, &author.valor, &diff);
    let review = ctx.motor.correr(&Tarea {
        rol: Rol::Revisor,
        dir: worktree,
        modelo: &c.modelo,
        presupuesto_usd: c.presupuesto_revision_usd,
        timeout: REVIEW_TIMEOUT,
        sistema: brief::reglas_revisor(),
        mensaje: &ask,
        esquema: &review_schema,
        env: Vec::new(),
    });
    let (approved, reasons) = match review {
        Ok(salida) => {
            *cost += salida.costo_usd;
            EstadoServicio::gastar(ctx.home, &day, salida.costo_usd);
            brief::veredicto(&salida.valor)
        }
        Err(falla) => {
            *cost += falla.costo_usd;
            EstadoServicio::gastar(ctx.home, &day, falla.costo_usd);
            return Fin::Fallo(format!("la revisión: {}", falla.motivo));
        }
    };
    let gate = gates::revision(approved, &reasons);
    if !gate.ok {
        return Fin::Fallo(gate.linea());
    }
    passed.push(gate);

    // 6. The proposal.
    let propuesta = Propuesta {
        id: id.into(),
        problema: problema.id.clone(),
        titulo: problema.titulo.clone(),
        para_decir: field("para_decir"),
        titulo_commit: subject,
        causa: field("causa"),
        cambios: field("cambios"),
        pruebas: field("pruebas"),
        rama: format!("{}{id}", git::BRANCH_PREFIX),
        base: base.into(),
        creado_ts: ctx.clock.now(),
        estado: EstadoP::Lista,
        archivos: stats.iter().map(|s| s.path.clone()).collect(),
        lineas: stats.iter().map(|s| s.added + s.removed).sum(),
        gates: passed.clone(),
        costo_usd: *cost,
        ..Propuesta::default()
    };
    if let Err(e) = propuestas::guardar(ctx.home, &propuesta, Some(&diff)) {
        return Fin::Fallo(format!("no pude guardar la propuesta: {e}"));
    }
    record_attempt(ctx, &problema.id, id, "propuesto", "", *cost);
    let _ = propuestas::avisar_lista(ctx.home, &propuesta, ctx.clock);
    let _ = repo;
    Fin::Propuesta(id.into())
}

/// At start: an attempt that was cut (the process died) leaves a worktree
/// and a branch behind; both go, and nothing is counted against the
/// problem.
pub fn limpiar_cortado(home: &Path, config: &Config, log: &Log) {
    let mut estado = EstadoServicio::load(home);
    let Some(id) = estado.en_curso.take() else {
        return;
    };
    estado.save(home);
    if let Some(repo) = config.repo() {
        let _lock = propuestas::repo_lock(home);
        let _ = git::worktree_remove(
            &repo,
            &worktree_path(home, &id),
            Some(&format!("{}{id}", git::BRANCH_PREFIX)),
        );
    }
    log.line(&format!("intento {id}: quedó cortado; limpié su worktree"));
}
