//! `[cuerpo]`: a pet with a body falls, walks, climbs and comes down by
//! parachute (see `docs/MASCOTAS.md`). Without the section she stays where
//! she is put, as before.

use super::toml::Table;
use super::{Fields, Problem, error, warning};
use crate::anim::Clip;
use crate::body::{Loco, Params};
use std::collections::HashMap;

/// The pack's body: the clips for each way of moving and the numbers.
#[derive(Clone, Debug, PartialEq)]
pub struct Cuerpo {
    /// The art faces right (`mira = "derecha"`); mirrored when she walks
    /// the other way.
    pub faces_right: bool,
    pub walk: Option<String>,
    pub trot: Option<String>,
    pub climb: Option<String>,
    pub fall: Option<String>,
    pub held: Option<String>,
    pub land: Option<String>,
    pub flop: Option<String>,
    pub chute: Option<String>,
    pub chute_land: Option<String>,
    /// Idle while she sits on a window's top edge.
    pub perch: Option<String>,
    pub params: Params,
}

/// The animation keys, in the order the docs list them.
const CLIPS: &[&str] = &[
    "caminar",
    "trotar",
    "trepar",
    "caer",
    "agarrada",
    "aterrizar",
    "porrazo",
    "paracaidas",
    "aterrizar_paracaidas",
    "posada",
];

impl Cuerpo {
    fn slot(&mut self, key: &str) -> &mut Option<String> {
        match key {
            "caminar" => &mut self.walk,
            "trotar" => &mut self.trot,
            "trepar" => &mut self.climb,
            "caer" => &mut self.fall,
            "agarrada" => &mut self.held,
            "aterrizar" => &mut self.land,
            "porrazo" => &mut self.flop,
            "paracaidas" => &mut self.chute,
            "aterrizar_paracaidas" => &mut self.chute_land,
            _ => &mut self.perch,
        }
    }

    /// Every animation it names, with its key.
    pub fn named(&self) -> Vec<(&'static str, &str)> {
        let all = [
            &self.walk,
            &self.trot,
            &self.climb,
            &self.fall,
            &self.held,
            &self.land,
            &self.flop,
            &self.chute,
            &self.chute_land,
            &self.perch,
        ];
        CLIPS
            .iter()
            .zip(all)
            .filter_map(|(k, v)| v.as_deref().map(|v| (*k, v)))
            .collect()
    }

    /// The clip for a way of moving, with its fallbacks: no `trotar` trots
    /// with `caminar`; no `paracaidas` or `agarrada` uses `caer`; no
    /// `porrazo` or `aterrizar_paracaidas` uses `aterrizar`. `None`: she
    /// shows her idle look (and, landing, a squash drawn by code).
    pub fn clip_for(&self, loco: Loco) -> Option<&str> {
        let chain: &[&Option<String>] = match loco {
            Loco::Walk => &[&self.walk],
            Loco::Trot => &[&self.trot, &self.walk],
            Loco::Climb => &[&self.climb],
            Loco::Fall => &[&self.fall],
            Loco::Chute => &[&self.chute, &self.fall],
            Loco::Held => &[&self.held, &self.fall],
            Loco::Land => &[&self.land],
            Loco::Flop => &[&self.flop, &self.land],
            Loco::ChuteLand => &[&self.chute_land, &self.land],
        };
        chain.iter().find_map(|c| c.as_deref())
    }
}

fn ranged(
    f: &mut Fields,
    key: &'static str,
    lo: f64,
    hi: f64,
    what: &str,
    problems: &mut Vec<Problem>,
) -> Option<f64> {
    let n = f.number(key, problems)?;
    if (lo..=hi).contains(&n) {
        Some(n)
    } else {
        problems.push(error(format!(
            "[cuerpo] `{key}` tiene que estar entre {lo} y {hi} ({what})"
        )));
        None
    }
}

/// Reads `[cuerpo]`. Animation names are checked against `clips` once they
/// are all known ([`finish`]).
pub(super) fn parse(table: &Table, problems: &mut Vec<Problem>) -> Cuerpo {
    let mut f = Fields::new(table, "[cuerpo] ");
    let mut body = Cuerpo {
        faces_right: false,
        walk: None,
        trot: None,
        climb: None,
        fall: None,
        held: None,
        land: None,
        flop: None,
        chute: None,
        chute_land: None,
        perch: None,
        params: Params::default(),
    };
    match f.string("mira", problems).as_deref() {
        None | Some("izquierda") => {}
        Some("derecha") => body.faces_right = true,
        Some(other) => problems.push(error(format!(
            "[cuerpo] `mira = \"{other}\"`: puede ser \"izquierda\" o \"derecha\" (hacia dónde mira el dibujo)"
        ))),
    }
    for key in CLIPS {
        let value = f.string(key, problems);
        *body.slot(key) = value;
    }
    let p = &mut body.params;
    let speed = "puntos por segundo";
    if let Some(n) = ranged(
        &mut f,
        "gravedad",
        1.0,
        20000.0,
        "puntos por segundo al cuadrado; Wawi usa 1500",
        problems,
    ) {
        p.gravity = n;
    }
    if let Some(n) = ranged(&mut f, "velocidad_maxima", 1.0, 10000.0, speed, problems) {
        p.max_fall = n;
    }
    if let Some(n) = ranged(&mut f, "velocidad_caminar", 1.0, 2000.0, speed, problems) {
        p.walk_speed = n;
    }
    if let Some(n) = ranged(&mut f, "velocidad_trote", 1.0, 2000.0, speed, problems) {
        p.trot_speed = n;
    }
    if let Some(n) = ranged(&mut f, "velocidad_trepar", 1.0, 2000.0, speed, problems) {
        p.climb_speed = n;
    }
    if let Some(n) = ranged(&mut f, "velocidad_paracaidas", 1.0, 2000.0, speed, problems) {
        p.chute_speed = n;
    }
    if let Some(n) = ranged(&mut f, "rebote", 0.0, 0.9, "0 = no rebota", problems) {
        p.bounce = n;
    }
    if let Some(n) = ranged(&mut f, "aterrizaje", 0.0, 5.0, "segundos", problems) {
        p.land_secs = n;
    }
    if let Some(n) = ranged(
        &mut f,
        "porrazo_desde",
        0.0,
        100000.0,
        "puntos de caída; 0 = nunca",
        problems,
    ) {
        p.flop_from = n;
    }
    if let Some(n) = ranged(
        &mut f,
        "chance_paracaidas",
        0.0,
        1.0,
        "0 = nunca, 1 = siempre",
        problems,
    ) {
        p.chute_chance = n;
    }
    if let Some(n) = ranged(
        &mut f,
        "chance_trote",
        0.0,
        1.0,
        "0 = nunca, 1 = siempre",
        problems,
    ) {
        p.trot_chance = n;
    }
    if let Some((lo, hi)) = f.pair("cuanto_camina", problems) {
        if lo >= 0.5 && hi >= lo {
            p.wander_every = (lo * 60.0, hi * 60.0);
        } else {
            problems.push(error(
                "[cuerpo] `cuanto_camina` son minutos entre paseos, [mínimo, máximo], como [8, 25] (mínimo 0.5)".into(),
            ));
        }
    }
    for (key, slot) in [
        ("cada_trepada", &mut p.climb_every),
        ("cada_paracaidas", &mut p.chute_every),
        ("cada_trote", &mut p.trot_every),
    ] {
        if let Some(n) = ranged(&mut f, key, 0.0, 1440.0, "minutos", problems) {
            *slot = n * 60.0;
        }
    }
    f.finish(problems);
    body
}

/// Checks the names against the animations and fills in what depends on
/// them: which moves she has and how long the long landings last.
pub(super) fn finish(
    body: &mut Cuerpo,
    declared: &[String],
    clips: &HashMap<String, Clip>,
    problems: &mut Vec<Problem>,
) {
    for (key, name) in body.named() {
        if !declared.iter().any(|d| d == name) {
            problems.push(error(format!(
                "[cuerpo] {key} = \"{name}\", pero no hay [animaciones.{name}]"
            )));
        }
    }
    let has = |name: &Option<String>| name.as_ref().is_some_and(|n| clips.contains_key(n));
    let secs = |name: Option<&str>| name.and_then(|n| clips.get(n)).map(Clip::duration);
    let p = &mut body.params;
    p.walks = has(&body.walk);
    p.climbs = has(&body.climb) && p.walks;
    p.chutes = has(&body.chute);
    if let Some(s) = secs(body.flop.as_deref().or(body.land.as_deref())) {
        p.flop_secs = s.max(p.land_secs);
    } else {
        p.flop_secs = p.land_secs;
    }
    if let Some(s) = secs(body.chute_land.as_deref().or(body.land.as_deref())) {
        p.chute_land_secs = s.max(p.land_secs);
    } else {
        p.chute_land_secs = p.land_secs;
    }
    if body.fall.is_none() {
        problems.push(warning(
            "[cuerpo] no tiene `caer`: mientras cae se ve quieta (idle)".into(),
        ));
    }
    if body.walk.is_none() {
        problems.push(warning(
            "[cuerpo] no tiene `caminar`: no pasea, solo cae cuando la soltás".into(),
        ));
    }
    if p.chute_chance > 0.0 && !p.chutes {
        problems.push(warning(
            "[cuerpo] `chance_paracaidas` sin `paracaidas`: nunca usa el paracaídas".into(),
        ));
    }
    for (key, name) in [
        ("caer", &body.fall),
        ("caminar", &body.walk),
        ("trepar", &body.climb),
        ("paracaidas", &body.chute),
    ] {
        if let Some(clip) = name.as_deref().and_then(|n| clips.get(n))
            && !clip.looped
        {
            problems.push(warning(format!(
                "[cuerpo] {key}: [animaciones.{}] tiene `repetir = false`, y mientras se mueve tiene que repetirse",
                clip.name
            )));
        }
    }
}
