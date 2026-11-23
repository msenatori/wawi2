//! Which packs are installed and which one is active.
//!
//! Installed: the built-in Wawi (in the assets folder) plus every
//! `<home>/mascotas/<id>/mascota.toml`. Active: `"mascota"` in
//! `<home>/mascot.json` (what the menu writes), or Wawi.

use std::fs;
use std::path::{Path, PathBuf};

use super::{BUILTIN_ID, MANIFEST, Pack, toml};
use crate::place::Placement;

/// One pack in the «Mascota» menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    /// The folder's name: what `mascot.json` remembers.
    pub id: String,
    /// `nombre` from the manifest (the id if it has none).
    pub nombre: String,
    pub dir: PathBuf,
    pub builtin: bool,
}

/// `$WAWI2_HOME`, or `~/.wawi2`.
pub fn home_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("WAWI2_HOME") {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".wawi2"))
}

/// Where people put their own packs.
pub fn pets_dir(home: &Path) -> PathBuf {
    home.join("mascotas")
}

/// The built-in Wawi pack, inside the assets folder of built-in packs.
pub fn builtin_dir(assets: &Path) -> PathBuf {
    assets.join(BUILTIN_ID)
}

/// Just the name, without loading the art (the menu reads this every time
/// it opens).
fn nombre_of(dir: &Path) -> Option<String> {
    let text = fs::read_to_string(dir.join(MANIFEST)).ok()?;
    let table = toml::parse(&text).ok()?;
    toml::get(&table, "nombre")
        .and_then(toml::Value::as_str)
        .filter(|n| !n.trim().is_empty())
        .map(str::to_owned)
}

/// The built-in pack first, then the home packs by id. A home pack with the
/// built-in's id is left out: Wawi is always the one in the assets.
pub fn installed(assets: &Path, home: Option<&Path>) -> Vec<Installed> {
    let mut out = Vec::new();
    let builtin = builtin_dir(assets);
    if builtin.join(MANIFEST).is_file() {
        out.push(Installed {
            id: BUILTIN_ID.into(),
            nombre: nombre_of(&builtin).unwrap_or_else(|| "Wawi".into()),
            dir: builtin,
            builtin: true,
        });
    }
    let Some(home) = home else {
        return out;
    };
    let Ok(entries) = fs::read_dir(pets_dir(home)) else {
        return out;
    };
    let mut mine: Vec<Installed> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|dir| dir.join(MANIFEST).is_file())
        .filter_map(|dir| {
            let id = dir.file_name()?.to_string_lossy().into_owned();
            if id == BUILTIN_ID || id.starts_with('.') {
                return None;
            }
            Some(Installed {
                nombre: nombre_of(&dir).unwrap_or_else(|| id.clone()),
                id,
                dir,
                builtin: false,
            })
        })
        .collect();
    mine.sort_by(|a, b| a.id.cmp(&b.id));
    out.extend(mine);
    out
}

/// The folder of the pack called `id`.
pub fn find(assets: &Path, home: Option<&Path>, id: &str) -> Option<PathBuf> {
    if id == BUILTIN_ID {
        return Some(builtin_dir(assets));
    }
    let dir = pets_dir(home?).join(id);
    dir.join(MANIFEST).is_file().then_some(dir)
}

/// The pack that should be on screen, loaded: `override_dir` (`--mascota`),
/// else the one `mascot.json` names, else Wawi. A pack that fails to load
/// is reported through `log` and Wawi takes its place. `None` only if even
/// the built-in pack is missing.
///
/// Also for the rest of the program (a future `hacer(accion)` tool, the
/// voice, the soul): it reads the manifest and checks files, it decodes no
/// image.
pub fn active(
    assets: &Path,
    home: Option<&Path>,
    override_dir: Option<&Path>,
    log: &mut dyn FnMut(String),
) -> Option<Pack> {
    let wanted: Option<PathBuf> = match override_dir {
        Some(dir) => Some(dir.to_owned()),
        None => home
            .map(|h| Placement::load(&h.join("mascot.json")))
            .and_then(|p| p.pet)
            .filter(|id| id != BUILTIN_ID)
            .and_then(|id| match find(assets, home, &id) {
                Some(dir) => Some(dir),
                None => {
                    log(format!(
                        "no encuentro la mascota «{id}» en {}; sigo con Wawi",
                        home.map(|h| pets_dir(h).display().to_string())
                            .unwrap_or_default()
                    ));
                    None
                }
            }),
    };
    if let Some(dir) = wanted {
        match load_logged(&dir, false, log) {
            Some(pack) => return Some(pack),
            None => log("sigo con Wawi".into()),
        }
    }
    load_logged(&builtin_dir(assets), true, log)
}

/// Loads a pack and tells `log` what is wrong with it, briefly.
pub(crate) fn load_logged(dir: &Path, builtin: bool, log: &mut dyn FnMut(String)) -> Option<Pack> {
    match Pack::load(dir) {
        Ok(mut pack) => {
            pack.builtin = builtin;
            if !pack.problems.is_empty() && !builtin {
                let errors = pack
                    .problems
                    .iter()
                    .filter(|p| p.level >= super::Level::Error)
                    .count();
                if errors > 0 {
                    log(format!(
                        "la mascota «{}» tiene {errors} problema(s) (ver `mascota validar {}`); la uso igual",
                        pack.name(),
                        dir.display()
                    ));
                }
            }
            Some(pack)
        }
        Err(error) => {
            log(error.to_string());
            None
        }
    }
}
