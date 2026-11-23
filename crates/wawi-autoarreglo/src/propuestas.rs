//! Proposals: what a passing attempt leaves for the owner, and the only
//! code that writes to the owner's repo (apply, revert). Never on its own:
//! each of these runs because the owner said so, through a tool.
//!
//! `<home>/autoarreglo/propuestas/<id>/` holds `propuesta.md` (in Spanish:
//! what was wrong, the cause, what changed, the tests, the gates),
//! `cambios.diff` and `propuesta.json` (the state). The attempt's branch
//! `autoarreglo/<id>` stays in the repo until it is applied or discarded.

use std::fs::File;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wawi_plugins::inbox::{FACTS, make_queue};
use wawi_tools::WallClock;

use crate::diario::{self, Estado, Intento, Problema};
use crate::gates::Resultado;
use crate::git;

pub const DIR: &str = "autoarreglo/propuestas";
/// How long the "tengo un arreglo" fact may wait for the instinct gate.
const AVISO_DIAS: i64 = 3;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EstadoP {
    #[default]
    Lista,
    Aplicada,
    Descartada,
    Revertida,
}

impl EstadoP {
    pub fn nombre(self) -> &'static str {
        match self {
            EstadoP::Lista => "lista para aplicar",
            EstadoP::Aplicada => "aplicada",
            EstadoP::Descartada => "descartada",
            EstadoP::Revertida => "deshecha",
        }
    }
}

/// The watch after a fix goes in ([`crate::vigia`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Vigia {
    /// "esperando" (the next start), "mirando", "bien", "sospechoso".
    pub estado: String,
    pub desde: i64,
    #[serde(default)]
    pub arranque: Option<i64>,
    #[serde(default)]
    pub graves: u32,
    #[serde(default)]
    pub recurrencias: u32,
    #[serde(default)]
    pub motivo: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Propuesta {
    pub id: String,
    pub problema: String,
    pub titulo: String,
    pub para_decir: String,
    pub titulo_commit: String,
    pub causa: String,
    pub cambios: String,
    pub pruebas: String,
    pub rama: String,
    pub base: String,
    pub creado_ts: i64,
    #[serde(default)]
    pub estado: EstadoP,
    #[serde(default)]
    pub archivos: Vec<String>,
    #[serde(default)]
    pub lineas: u32,
    #[serde(default)]
    pub gates: Vec<Resultado>,
    #[serde(default)]
    pub costo_usd: f64,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub aplicada_ts: Option<i64>,
    #[serde(default)]
    pub rama_dueno: Option<String>,
    #[serde(default)]
    pub vigia: Option<Vigia>,
}

pub fn dir(home: &Path, id: &str) -> PathBuf {
    home.join(DIR).join(id)
}

pub fn cargar(home: &Path, id: &str) -> Option<Propuesta> {
    if !wawi_plugins::inbox::valid_id(id) {
        return None;
    }
    let text = std::fs::read_to_string(dir(home, id).join("propuesta.json")).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn todas(home: &Path) -> Vec<Propuesta> {
    let Ok(entries) = std::fs::read_dir(home.join(DIR)) else {
        return Vec::new();
    };
    let mut out: Vec<Propuesta> = entries
        .filter_map(Result::ok)
        .filter_map(|e| cargar(home, &e.file_name().to_string_lossy()))
        .collect();
    out.sort_by_key(|p| p.creado_ts);
    out
}

/// Saves the state (and, on creation, the diff and the Markdown).
pub fn guardar(home: &Path, p: &Propuesta, diff: Option<&str>) -> std::io::Result<()> {
    let folder = dir(home, &p.id);
    std::fs::create_dir_all(&folder)?;
    if let Some(diff) = diff {
        crate::write_atomic(&folder.join("cambios.diff"), diff)?;
    }
    let problema = diario::load(home, &p.problema);
    crate::write_atomic(
        &folder.join("propuesta.md"),
        &markdown(p, problema.as_ref()),
    )?;
    let text = serde_json::to_string_pretty(p).map_err(std::io::Error::other)?;
    crate::write_atomic(&folder.join("propuesta.json"), &text)
}

/// The proposal for the owner, in Spanish.
pub fn markdown(p: &Propuesta, problema: Option<&Problema>) -> String {
    let mut out = vec![
        format!("# Arreglo {}: {}", p.id, p.titulo),
        String::new(),
        format!("Estado: {}.", p.estado.nombre()),
        String::new(),
        "## Qué andaba mal".into(),
        String::new(),
        p.titulo.clone(),
    ];
    if let Some(problema) = problema {
        out.push(format!(
            "Pasó {} {}.",
            problema.cuenta,
            if problema.cuenta == 1 { "vez" } else { "veces" }
        ));
        for m in problema.muestras.iter().rev().take(3) {
            out.push(format!("- {m}"));
        }
    }
    out.extend([
        String::new(),
        "## Por qué pasaba".into(),
        String::new(),
        p.causa.clone(),
        String::new(),
        "## Qué cambié".into(),
        String::new(),
        p.cambios.clone(),
        String::new(),
        format!("{} líneas en estos archivos:", p.lineas),
    ]);
    out.extend(p.archivos.iter().map(|a| format!("- `{a}`")));
    out.extend([
        String::new(),
        "## Las pruebas".into(),
        String::new(),
        p.pruebas.clone(),
        String::new(),
        "## Las compuertas".into(),
        String::new(),
    ]);
    for g in &p.gates {
        out.push(format!(
            "- [{}] {}",
            if g.ok { "pasa" } else { "falla" },
            g.linea()
        ));
    }
    out.extend([
        String::new(),
        "## Cómo seguir".into(),
        String::new(),
        format!(
            "Los cambios están en `cambios.diff` y en la rama `{}`. Para aplicarlo, decile a Wawi «aplicá el arreglo {}»: queda un solo commit en tu rama actual, y solo si tu árbol está limpio. Para tirarlo, «descartá el arreglo {}».",
            p.rama, p.id, p.id
        ),
        String::new(),
        format!("Costó US$ {:.2}.", p.costo_usd),
    ]);
    if let Some(commit) = &p.commit {
        out.push(format!("Commit: `{commit}`."));
    }
    out.join("\n") + "\n"
}

/// Drops a fact for Wawi into `cola/hechos`, once per id.
pub fn hecho(
    home: &Path,
    id: &str,
    decir: &str,
    contexto: Value,
    clock: &dyn WallClock,
) -> std::io::Result<bool> {
    let root = home.join(FACTS);
    make_queue(&root)?;
    let name = format!("{id}.json");
    if root.join("new").join(&name).exists() || root.join("cur").join(&name).exists() {
        return Ok(false);
    }
    let now = clock.now();
    let value = json!({
        "id": id,
        "origen": "autoarreglo",
        "decir": decir,
        "contexto": contexto,
        "urgencia": "normal",
        "creado": crate::clock::stamp(clock, now),
        "vence": crate::clock::stamp(clock, now + AVISO_DIAS * 86_400),
    });
    let tmp = root.join("tmp").join(&name);
    std::fs::write(&tmp, value.to_string())?;
    std::fs::rename(&tmp, root.join("new").join(&name))?;
    Ok(true)
}

/// "Encontré por qué … y tengo un arreglo probado. ¿Te lo cuento?"
pub fn avisar_lista(home: &Path, p: &Propuesta, clock: &dyn WallClock) -> std::io::Result<bool> {
    let what = p.para_decir.trim().trim_end_matches('.');
    let decir = if what.is_empty() {
        format!(
            "Encontré la causa de un problema mío ({}) y tengo un arreglo probado. ¿Te lo cuento?",
            p.titulo
        )
    } else {
        format!("Encontré por qué {what} y tengo un arreglo probado. ¿Te lo cuento?")
    };
    hecho(
        home,
        &format!("autoarreglo-{}", p.id),
        &crate::truncate(&decir, 300),
        json!({ "arreglo": p.id, "para_contarlo": "autoarreglo__contar_arreglo", "para_aplicarlo": "autoarreglo__aplicar_arreglo" }),
        clock,
    )
}

/// The friendly blocked request when the repo has no commit yet. Once a
/// week at most (the id carries the week).
pub fn avisar_sin_commit(home: &Path, clock: &dyn WallClock) -> std::io::Result<bool> {
    let week = clock.now().div_euclid(7 * 86_400);
    hecho(
        home,
        &format!("autoarreglo-sin-commit-{week}"),
        "Para poder arreglarme necesito que el repo tenga un primer commit. Cuando lo hagas, me pongo.",
        json!({ "necesita": "un primer commit en el repo de Wawi" }),
        clock,
    )
}

/// The lock on the owner's repo: one git writer at a time (the worker's
/// worktrees, or a tool applying). Held while the file is open.
pub fn repo_lock(home: &Path) -> std::io::Result<File> {
    let path = crate::dir(home).join("repo.lock");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)?;
    file.lock()?;
    Ok(file)
}

/// `arreglos()`: every proposal, newest first.
pub fn lista(home: &Path) -> String {
    let mut all = todas(home);
    all.reverse();
    if all.is_empty() {
        return "No tengo arreglos propuestos.".into();
    }
    all.iter()
        .take(10)
        .map(|p| {
            let mut line = format!("- {} ({}): {}", p.id, p.estado.nombre(), p.titulo);
            if let Some(v) = &p.vigia
                && v.estado == "sospechoso"
            {
                line.push_str(&format!(". Ojo: {}", v.motivo));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `contar_arreglo(id)`: what she would tell him.
pub fn contar(home: &Path, id: &str) -> Result<String, String> {
    let p = cargar(home, id).ok_or_else(|| format!("No tengo un arreglo {id}."))?;
    let mut text = format!(
        "Arreglo {} ({}). Qué andaba mal: {}. Por qué: {} Qué cambié: {} Pruebas: {} Pasó todas las compuertas ({}). Son {} líneas en {} archivos y costó US$ {:.2}.",
        p.id,
        p.estado.nombre(),
        p.titulo,
        p.causa.trim(),
        p.cambios.trim(),
        p.pruebas.trim(),
        p.gates
            .iter()
            .map(|g| g.nombre.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        p.lineas,
        p.archivos.len(),
        p.costo_usd
    );
    if p.estado == EstadoP::Lista {
        text.push_str(&format!(
            " El detalle está en {}. Si te parece, lo aplico con un solo commit.",
            dir(home, id).join("propuesta.md").display()
        ));
    }
    Ok(crate::truncate(&text, 2500))
}

/// The commit message: subject, body, the trailer.
pub fn mensaje(p: &Propuesta) -> String {
    let mut subject = p.titulo_commit.trim().to_owned();
    if subject.is_empty() {
        subject = p.titulo.clone();
    }
    if !subject.starts_with("Autoarreglo:") {
        subject = format!("Autoarreglo: {subject}");
    }
    let subject = crate::truncate(subject.lines().next().unwrap_or(""), 72);
    format!(
        "{subject}\n\n{}\n\nCausa: {}\nPruebas: {}\n\nPropuesto por Wawi y aplicado por su dueño. Pasó las compuertas de autoarreglo.\n\n{}: {}\n",
        p.cambios.trim(),
        p.causa.trim(),
        p.pruebas.trim(),
        git::TRAILER,
        p.id
    )
}

fn check_repo(repo: &Path) -> Result<String, String> {
    if !git::is_repo(repo) {
        return Err(format!("{} no es un repo de git.", repo.display()));
    }
    if !git::has_commits(repo) {
        return Err("Para poder arreglarme necesito que el repo tenga un primer commit.".into());
    }
    let branch = git::current_branch(repo)
        .ok_or("Tu repo no está en una rama (HEAD suelto). Cambiá a una rama y decime de nuevo.")?;
    let dirty = git::dirty(repo)?;
    if !dirty.is_empty() {
        return Err(format!(
            "No lo apliqué: tu repo tiene cambios sin commitear ({} {}). Commitealos o guardalos y decime de nuevo.",
            dirty.len(),
            if dirty.len() == 1 {
                "archivo"
            } else {
                "archivos"
            }
        ));
    }
    Ok(branch)
}

/// `aplicar_arreglo(id)`: ONE squash commit on the owner's current branch,
/// only if the tree is clean. Otherwise it says why and does nothing.
pub fn aplicar(
    home: &Path,
    repo: &Path,
    id: &str,
    clock: &dyn WallClock,
) -> Result<String, String> {
    let mut p = cargar(home, id).ok_or_else(|| format!("No tengo un arreglo {id}."))?;
    if p.estado != EstadoP::Lista {
        return Err(format!("El arreglo {id} ya está {}.", p.estado.nombre()));
    }
    let _lock = repo_lock(home).map_err(|e| format!("no pude tomar el lock del repo: {e}"))?;
    let branch = check_repo(repo)?;
    if !git::branch_exists(repo, &p.rama) {
        return Err(format!(
            "No encuentro la rama {} en el repo; no lo puedo aplicar.",
            p.rama
        ));
    }
    let moved = git::head(repo).is_ok_and(|h| h != p.base);
    git::merges_cleanly(repo, &p.rama).map_err(|e| {
        format!("No lo apliqué: {e}. Tu rama cambió desde que lo probé; descartalo y lo rehago.")
    })?;
    let sha = git::squash_apply(repo, &p.rama, &mensaje(&p))?;
    let _ = git::worktree_remove(
        repo,
        &crate::intento::worktree_path(home, id),
        Some(&p.rama),
    );
    let now = clock.now();
    p.estado = EstadoP::Aplicada;
    p.commit = Some(sha.clone());
    p.aplicada_ts = Some(now);
    p.rama_dueno = Some(branch.clone());
    p.vigia = Some(Vigia {
        estado: "esperando".into(),
        desde: now,
        ..Vigia::default()
    });
    guardar(home, &p, None).map_err(|e| format!("lo apliqué pero no pude anotarlo: {e}"))?;
    if let Some(mut problema) = diario::load(home, &p.problema) {
        problema.estado = Estado::Aplicado;
        problema.cuenta_al_aplicar = problema.cuenta;
        let _ = diario::save(home, &problema);
    }
    let short: String = sha.chars().take(7).collect();
    let note = if moved {
        " Tu rama había avanzado desde que lo probé; mezcló limpio, pero conviene correr las pruebas."
    } else {
        ""
    };
    Ok(format!(
        "Listo: quedó en un solo commit ({short}) en tu rama {branch}.{note} Para que valga hay que recompilarme y reiniciarme. Si querés, lo compilo yo (autoarreglo__compilar) y después me reiniciás."
    ))
}

/// `descartar_arreglo(id)`: the proposal goes, and the problem is marked
/// won't-fix so it is never attempted again.
pub fn descartar(home: &Path, repo: Option<&Path>, id: &str) -> Result<String, String> {
    let mut p = cargar(home, id).ok_or_else(|| format!("No tengo un arreglo {id}."))?;
    if p.estado != EstadoP::Lista {
        return Err(format!("El arreglo {id} ya está {}.", p.estado.nombre()));
    }
    if let Some(repo) = repo {
        let _lock = repo_lock(home).ok();
        let _ = git::worktree_remove(
            repo,
            &crate::intento::worktree_path(home, id),
            Some(&p.rama),
        );
    }
    p.estado = EstadoP::Descartada;
    guardar(home, &p, None).map_err(|e| format!("no pude anotarlo: {e}"))?;
    if let Some(mut problema) = diario::load(home, &p.problema) {
        problema.estado = Estado::Descartado;
        let _ = diario::save(home, &problema);
    }
    Ok(format!(
        "Listo, lo descarté. No vuelvo a intentar arreglar «{}».",
        p.titulo
    ))
}

/// `revertir_arreglo(id)`: a revert commit of the applied fix. Never a
/// reset, never a force, and only a commit with this fix's trailer.
pub fn revertir(
    home: &Path,
    repo: &Path,
    id: &str,
    clock: &dyn WallClock,
) -> Result<String, String> {
    let mut p = cargar(home, id).ok_or_else(|| format!("No tengo un arreglo {id}."))?;
    let commit = match (&p.estado, &p.commit) {
        (EstadoP::Aplicada, Some(commit)) => commit.clone(),
        _ => return Err(format!("El arreglo {id} no está aplicado.")),
    };
    let _lock = repo_lock(home).map_err(|e| format!("no pude tomar el lock del repo: {e}"))?;
    check_repo(repo)?;
    let reason = p
        .vigia
        .as_ref()
        .map(|v| v.motivo.clone())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| "lo pidió el dueño".into());
    let sha = git::revert(
        repo,
        &commit,
        id,
        &format!("Wawi deshizo su arreglo: {reason}"),
    )?;
    p.estado = EstadoP::Revertida;
    if let Some(v) = &mut p.vigia {
        v.estado = "deshecha".into();
    }
    guardar(home, &p, None).map_err(|e| format!("lo deshice pero no pude anotarlo: {e}"))?;
    if let Some(mut problema) = diario::load(home, &p.problema) {
        problema.estado = Estado::Abierto;
        problema.propuesta = None;
        problema.intentos.push(Intento {
            id: id.into(),
            ts: clock.now(),
            resultado: "revertido".into(),
            motivo: reason,
            costo_usd: 0.0,
        });
        let _ = diario::save(home, &problema);
    }
    let short: String = sha.chars().take(7).collect();
    Ok(format!(
        "Listo, lo deshice con un commit nuevo ({short}); no borré nada de tu historia. Hay que recompilarme y reiniciarme para que valga."
    ))
}
