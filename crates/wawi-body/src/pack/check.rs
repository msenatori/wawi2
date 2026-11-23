//! `mascota validar`: everything loading checks, plus what only an artist
//! needs to hear about (image sizes, transparency, the license, animations
//! nobody uses). Reads PNG headers, never decodes an image.

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

use super::{Level, Look, Pack, Problem, png};

/// Over this much decoded art, the validator suggests smaller frames.
const HEAVY_BYTES: usize = 96 * 1024 * 1024;

#[derive(Debug)]
pub struct Report {
    pub dir: PathBuf,
    /// `None` when the pack cannot load at all.
    pub pack: Option<Pack>,
    pub problems: Vec<Problem>,
    /// How each look is shown, in plain words.
    pub looks: Vec<String>,
    /// What her body does (`[cuerpo]`), in plain words.
    pub body: Vec<String>,
}

impl Report {
    pub fn errors(&self) -> usize {
        self.problems
            .iter()
            .filter(|p| p.level >= Level::Error)
            .count()
    }

    pub fn warnings(&self) -> usize {
        self.problems
            .iter()
            .filter(|p| p.level == Level::Warning)
            .count()
    }

    /// No errors (warnings are fine).
    pub fn ok(&self) -> bool {
        self.errors() == 0
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.pack {
            Some(pack) => {
                let i = &pack.info;
                writeln!(
                    f,
                    "Mascota «{}» ({}){}{}",
                    i.nombre,
                    i.id,
                    if i.version.is_empty() {
                        String::new()
                    } else {
                        format!(", versión {}", i.version)
                    },
                    if i.autor.is_empty() {
                        String::new()
                    } else {
                        format!(", de {}", i.autor)
                    }
                )?;
                if !i.licencia.is_empty() {
                    writeln!(f, "Licencia del arte: {}", i.licencia)?;
                }
                let (w, h) = pack.frame_size;
                let (pw, ph) = pack.sprite_size();
                writeln!(
                    f,
                    "Cuadro: {w}×{h} píxeles, en pantalla {pw:.0}×{ph:.0} puntos, {} cuadros en {} imágenes",
                    pack.frames().len(),
                    pack.images().len()
                )?;
            }
            None => writeln!(f, "La mascota de {} no se puede usar.", self.dir.display())?,
        }
        if !self.looks.is_empty() {
            writeln!(f, "\nEstados:")?;
            for line in &self.looks {
                writeln!(f, "  {line}")?;
            }
        }
        if !self.body.is_empty() {
            writeln!(f, "\nCuerpo:")?;
            for line in &self.body {
                writeln!(f, "  {line}")?;
            }
        }
        if let Some(pack) = &self.pack {
            let actions = pack.actions();
            if !actions.is_empty() {
                writeln!(f, "\nAcciones: {}", actions.join(", "))?;
            }
        }
        let mut problems: Vec<&Problem> = self.problems.iter().collect();
        problems.sort_by(|a, b| b.level.cmp(&a.level));
        if !problems.is_empty() {
            writeln!(f)?;
            for p in problems {
                writeln!(f, "  {p}")?;
            }
        }
        writeln!(f)?;
        match (self.errors(), self.warnings()) {
            (0, 0) => write!(f, "Todo bien: la mascota está lista."),
            (0, w) => write!(f, "Lista para usar, con {w} aviso(s)."),
            (e, w) if self.pack.is_some() => write!(
                f,
                "{e} error(es) y {w} aviso(s). Wawi la puede mostrar igual, salteando lo que falla, pero conviene arreglarlo."
            ),
            (e, _) => write!(
                f,
                "{e} error(es). Así no se puede usar: Wawi mostraría a Wawi en su lugar."
            ),
        }
    }
}

fn push(problems: &mut Vec<Problem>, level: Level, message: String) {
    problems.push(Problem { level, message });
}

/// Checks the pack in `dir` and says, in Spanish, what is wrong.
pub fn validate(dir: &Path) -> Report {
    let pack = match Pack::load(dir) {
        Ok(pack) => pack,
        Err(error) => {
            return Report {
                dir: dir.to_owned(),
                pack: None,
                problems: error.problems,
                looks: Vec::new(),
                body: Vec::new(),
            };
        }
    };
    let mut problems = pack.problems.clone();
    let info = &pack.info;

    if info.licencia.trim().is_empty() {
        push(
            &mut problems,
            Level::Error,
            "falta `licencia`: decí bajo qué licencia compartís el arte (por ejemplo \"CC BY 4.0\" o \"CC BY-NC 4.0\")".into(),
        );
    }
    if info.autor.trim().is_empty() {
        push(
            &mut problems,
            Level::Warning,
            "falta `autor`: ¿quién hizo el dibujo?".into(),
        );
    }
    if info.version.trim().is_empty() {
        push(
            &mut problems,
            Level::Warning,
            "falta `version` (por ejemplo \"1.0\")".into(),
        );
    }
    if let Some(folder) = dir.file_name().map(|n| n.to_string_lossy().into_owned())
        && !info.id.is_empty()
        && folder != info.id
        && dir
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|p| p == "mascotas")
    {
        push(
            &mut problems,
            Level::Error,
            format!(
                "`id = \"{}\"` y la carpeta se llama «{folder}»: tienen que ser iguales",
                info.id
            ),
        );
    }
    if pack.menubar_icon.is_none() {
        push(
            &mut problems,
            Level::Warning,
            "no hay [iconos] barra: en la barra de menú va a aparecer el nombre".into(),
        );
    }

    // The images: readable, the right size, see-through.
    let (fw, fh) = pack.frame_size;
    let mut total_bytes = 0usize;
    for (index, image) in pack.images().iter().enumerate() {
        if !image.present {
            continue;
        }
        let shown = image
            .path
            .strip_prefix(dir)
            .unwrap_or(&image.path)
            .display()
            .to_string();
        let header = match png::header(&image.path) {
            Ok(h) => h,
            Err(_) => {
                push(
                    &mut problems,
                    Level::Error,
                    format!("{shown} no es un PNG que pueda leer (¿lo exportaste como PNG?)"),
                );
                continue;
            }
        };
        total_bytes += header.width as usize * header.height as usize * 4;
        if !header.may_have_alpha() {
            push(
                &mut problems,
                Level::Warning,
                format!(
                    "{shown} no tiene transparencia: se va a ver un fondo alrededor de la mascota"
                ),
            );
        }
        let cells: Vec<_> = pack
            .frames()
            .iter()
            .filter(|f| f.image == index)
            .filter_map(|f| f.cell)
            .collect();
        let placed = pack
            .frames()
            .iter()
            .any(|f| f.image == index && f.place.is_some());
        if cells.is_empty() {
            if (header.width, header.height) != (fw, fh) && !placed {
                push(
                    &mut problems,
                    Level::Warning,
                    format!(
                        "{shown} mide {}×{} y el cuadro es {fw}×{fh}: se va a estirar para que entre",
                        header.width, header.height
                    ),
                );
            }
        } else {
            let need_w = cells.iter().map(|c| c.x + c.w).max().unwrap_or(0);
            let need_h = cells.iter().map(|c| c.y + c.h).max().unwrap_or(0);
            if header.width < need_w || header.height < need_h {
                push(
                    &mut problems,
                    Level::Error,
                    format!(
                        "la hoja {shown} mide {}×{}, y con cuadros de {fw}×{fh} hacen falta al menos {need_w}×{need_h}",
                        header.width, header.height
                    ),
                );
            } else if header.width % fw != 0 || header.height % fh != 0 {
                push(
                    &mut problems,
                    Level::Warning,
                    format!(
                        "la hoja {shown} mide {}×{}, que no es un número entero de cuadros de {fw}×{fh}",
                        header.width, header.height
                    ),
                );
            }
        }
    }
    if total_bytes > HEAVY_BYTES {
        push(
            &mut problems,
            Level::Warning,
            format!(
                "todo el arte junto ocupa {} MB en memoria: Wawi carga solo lo que usa, pero cuadros más chicos van mejor",
                total_bytes / (1024 * 1024)
            ),
        );
    }
    if fw > 1024 || fh > 1024 {
        push(
            &mut problems,
            Level::Warning,
            format!("el cuadro de {fw}×{fh} es muy grande: con 512 o menos alcanza"),
        );
    }

    // The looks, and their fallbacks.
    let mut looks = Vec::new();
    for look in Look::ALL {
        let own = pack.own_look(look);
        let line = match (own, pack.look_clip(look)) {
            (Some(name), _) if name == look.name() => {
                format!("{:<13} sí ({})", look.name(), look.meaning())
            }
            (Some(name), _) => format!("{:<13} usa «{name}» ({})", look.name(), look.meaning()),
            (None, Some(name)) => format!(
                "{:<13} no tiene: usa la de «{name}» ({})",
                look.name(),
                look.meaning()
            ),
            (None, None) => format!(
                "{:<13} no tiene: no hace nada especial ({})",
                look.name(),
                look.meaning()
            ),
        };
        looks.push(line);
        if let Some(mapped) = pack.look_names().get(&look)
            && pack.clip(mapped).is_none()
            && pack.declared().iter().any(|d| d == mapped)
        {
            push(
                &mut problems,
                Level::Error,
                format!(
                    "el estado {} usa «{mapped}», que no tiene ningún cuadro que exista: cae en {}",
                    look.name(),
                    pack.look_clip(look).unwrap_or("nada")
                ),
            );
        }
        let state = matches!(
            look,
            Look::Idle | Look::Sleeping | Look::Listening | Look::Thinking | Look::Speaking
        );
        if state
            && let Some(name) = own
            && let Some(clip) = pack.clip(name)
            && !clip.looped
            && clip.next.is_none()
            && clip.steps.len() > 1
        {
            push(
                &mut problems,
                Level::Warning,
                format!(
                    "[animaciones.{name}] tiene `repetir = false` y no tiene `siguiente`: en «{}» se queda quieta en el último cuadro",
                    look.name()
                ),
            );
        }
    }

    // Animations nobody plays.
    let mut used: HashSet<&str> = HashSet::new();
    for look in Look::ALL {
        if let Some(name) = pack.look_names().get(&look) {
            used.insert(name);
        }
    }
    for (_, anim) in pack.action_pairs() {
        used.insert(anim);
    }
    let life = &pack.life;
    for name in life
        .blink
        .iter()
        .chain(&life.gestures)
        .chain(&life.lie_down)
        .chain(&life.wake)
        .chain(&life.poke)
    {
        used.insert(name);
    }
    for name in pack.clip_names() {
        if let Some(clip) = pack.clip(name) {
            for other in clip.next.iter().chain(&clip.variants) {
                used.insert(other);
            }
        }
    }
    let body = describe_body(&pack);
    if let Some(cuerpo) = &pack.body {
        for (_, name) in cuerpo.named() {
            used.insert(name);
        }
    }
    for name in pack.declared() {
        if !used.contains(name.as_str()) {
            push(
                &mut problems,
                Level::Warning,
                format!(
                    "[animaciones.{name}] nunca se usa: no es un estado, ni una acción, ni está en [vida] ni en [cuerpo]"
                ),
            );
        }
    }
    for clip in pack.clip_names().filter_map(|n| pack.clip(n)) {
        if clip.mouth.is_some() && clip.name != pack.look_clip(Look::Speaking).unwrap_or("") {
            push(
                &mut problems,
                Level::Warning,
                format!(
                    "[animaciones.{}] tiene `boca`, que solo se usa en el estado speaking",
                    clip.name
                ),
            );
        }
    }

    Report {
        dir: dir.to_owned(),
        pack: Some(pack),
        problems,
        looks,
        body,
    }
}

/// `[cuerpo]` in plain words: what she does and with which animation.
fn describe_body(pack: &Pack) -> Vec<String> {
    use crate::body::Loco;
    let Some(body) = &pack.body else {
        return vec!["no tiene [cuerpo]: se queda donde la ponés, sin gravedad".into()];
    };
    let p = &body.params;
    let shown = |loco: Loco| match body.clip_for(loco) {
        Some(name) => format!("«{name}»"),
        None => "quieta (idle)".into(),
    };
    let mut out = vec![
        format!(
            "cae con {} (gravedad {:.0}, hasta {:.0} puntos/s{})",
            shown(Loco::Fall),
            p.gravity,
            p.max_fall,
            if p.bounce > 0.0 {
                format!(", rebota {:.0}%", p.bounce * 100.0)
            } else {
                String::new()
            }
        ),
        format!(
            "aterriza con {}{}",
            match body.clip_for(Loco::Land) {
                Some(name) => format!("«{name}»"),
                None => "un aplastón dibujado por el programa".into(),
            },
            if p.flop_from > 0.0 {
                format!(
                    "; desde {:.0} puntos o más, porrazo con {}",
                    p.flop_from,
                    shown(Loco::Flop)
                )
            } else {
                String::new()
            }
        ),
    ];
    if p.walks {
        out.push(format!(
            "camina con {} a {:.0} puntos/s, cada {:.0} a {:.0} minutos",
            shown(Loco::Walk),
            p.walk_speed,
            p.wander_every.0 / 60.0,
            p.wander_every.1 / 60.0
        ));
        if p.trot_chance > 0.0 {
            out.push(format!(
                "a veces trota con {} ({:.0}%)",
                shown(Loco::Trot),
                p.trot_chance * 100.0
            ));
        }
    } else {
        out.push("no camina (no tiene `caminar`)".into());
    }
    if p.climbs {
        out.push(format!(
            "trepa ventanas con {}, como mucho una vez cada {:.0} minutos",
            shown(Loco::Climb),
            p.climb_every / 60.0
        ));
    }
    if p.chutes && p.chute_chance > 0.0 {
        out.push(format!(
            "baja de las ventanas en paracaídas con {} ({:.0}% de las veces)",
            shown(Loco::Chute),
            p.chute_chance * 100.0
        ));
    }
    if let Some(perch) = &body.perch {
        out.push(format!("sobre una ventana, se sienta con «{perch}»"));
    }
    out
}
