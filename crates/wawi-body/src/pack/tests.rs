use std::fs;
use std::path::{Path, PathBuf};

use super::*;

/// A scratch folder, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("wawi-pack-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn frame(dir: &Path, name: &str, w: u32, h: u32) {
    let path = dir.join("cuadros").join(format!("{name}.png"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        png::encode_rgba(w, h, &vec![255; (w * h * 4) as usize]),
    )
    .unwrap();
}

fn wawi() -> Pack {
    Pack::load(&builtin_dir(&crate::default_assets())).expect("Wawi loads")
}

#[test]
fn the_built_in_wawi_is_complete_and_clean() {
    let pack = wawi();
    assert_eq!(pack.id(), "wawi");
    assert_eq!(pack.name(), "Wawi");
    assert_eq!(pack.info.licencia, "CC BY-NC 4.0");
    assert!(pack.problems.is_empty(), "{:?}", pack.problems);
    for look in Look::ALL {
        assert_eq!(pack.own_look(look), Some(look.name()), "{look:?}");
    }
    assert_eq!(pack.sprite_size(), (124.0, 286.0 * 0.3875));
    assert!(pack.menubar_icon.is_some() && pack.muted_icon.is_some());
    let actions = pack.actions();
    for action in ["saludar", "festejar", "cafe", "bostezo"] {
        assert!(actions.contains(&action), "{actions:?}");
    }
    assert_eq!(pack.action_clip("cafe"), Some("coffee"));
    assert_eq!(pack.action_description("cafe"), Some("Se toma un café."));
    let voice = pack.voice().unwrap();
    assert_eq!(voice.id.as_deref(), Some("jf_alpha"));
    assert_eq!((voice.speed, voice.pitch), (Some(1.05), Some(3.0)));
    let report = validate(&pack.dir);
    assert!(report.ok(), "{report}");
    assert_eq!(report.warnings(), 0, "{report}");
}

#[test]
fn a_pack_with_only_idle_falls_back_everywhere() {
    let dir = Scratch::new("min");
    frame(dir.path(), "solo", 8, 8);
    let pack = Pack::from_text(
        dir.path(),
        "id = \"min\"\n[animaciones.idle]\ncuadros = [\"solo\"]\n",
    )
    .unwrap();
    assert_eq!(pack.frame_size, (8, 8), "the size comes from the frame");
    assert_eq!(pack.name(), "min");
    for look in [
        Look::Sleeping,
        Look::Listening,
        Look::Transcribing,
        Look::Thinking,
        Look::Speaking,
    ] {
        assert_eq!(pack.look_clip(look), Some("idle"), "{look:?}");
    }
    for look in [Look::Interrupted, Look::Error, Look::Muted] {
        assert_eq!(pack.look_clip(look), None, "{look:?}: no reaction");
    }
    assert!(pack.actions().is_empty());
}

#[test]
fn the_chains_go_transcribing_thinking_idle_and_interrupted_error() {
    let dir = Scratch::new("chain");
    for name in ["a", "t", "e"] {
        frame(dir.path(), name, 4, 4);
    }
    let pack = Pack::from_text(
        dir.path(),
        r#"
id = "chain"
[cuadro]
ancho = 4
alto = 4
[animaciones.idle]
cuadros = ["a"]
[animaciones.pensar]
cuadros = ["t"]
[animaciones.error]
cuadros = ["e"]
[estados]
thinking = "pensar"
"#,
    )
    .unwrap();
    assert_eq!(pack.look_clip(Look::Transcribing), Some("pensar"));
    assert_eq!(pack.look_clip(Look::Thinking), Some("pensar"));
    assert_eq!(pack.look_clip(Look::Interrupted), Some("error"));
    assert_eq!(pack.look_clip(Look::Listening), Some("idle"));
}

#[test]
fn without_idle_a_pack_does_not_load_and_says_why() {
    let dir = Scratch::new("noidle");
    frame(dir.path(), "x", 4, 4);
    let err = Pack::from_text(
        dir.path(),
        "id = \"x\"\n[cuadro]\nancho = 4\nalto = 4\n[animaciones.correr]\ncuadros = [\"x\"]\n",
    )
    .unwrap_err();
    assert!(err.to_string().contains("idle"), "{err}");
    // An idle whose frames are all missing is the same.
    let err = Pack::from_text(
        dir.path(),
        "id = \"x\"\n[cuadro]\nancho = 4\nalto = 4\n[animaciones.idle]\ncuadros = [\"nada\"]\n",
    )
    .unwrap_err();
    assert!(
        err.problems.iter().any(|p| p.message.contains("nada.png")),
        "{err:?}"
    );
    let err = Pack::from_text(dir.path(), "id = x\n").unwrap_err();
    assert!(err.to_string().contains("línea 1"), "{err}");
    assert!(Pack::load(&dir.path().join("no-existe")).is_err());
}

#[test]
fn mistakes_are_reported_but_do_not_stop_the_pack() {
    let dir = Scratch::new("typos");
    frame(dir.path(), "a", 4, 4);
    let pack = Pack::from_text(
        dir.path(),
        r#"
id = "typos"
nombre = "Typos"
[cuadro]
ancho = 4
alto = 4
[animaciones.idle]
cuadros = ["a", "falta"]
fsp = 3
siguiente = "nadie"
[animaciones.hola]
cuadros = ["a"]
[acciones]
saludar = "hola"
volar = "avion"
"#,
    )
    .unwrap();
    let all: Vec<String> = pack.problems.iter().map(|p| p.message.clone()).collect();
    let has = |needle: &str| all.iter().any(|m| m.contains(needle));
    assert!(has("¿quisiste decir `fps`?"), "{all:#?}");
    assert!(has("falta.png"), "{all:#?}");
    assert!(has("\"nadie\""), "{all:#?}");
    assert!(has("avion"), "{all:#?}");
    assert_eq!(
        pack.clip("idle").unwrap().steps.len(),
        1,
        "the missing frame is skipped"
    );
    assert_eq!(
        pack.actions(),
        vec!["saludar"],
        "the broken action is left out"
    );
}

#[test]
fn sheets_are_cut_in_cells_and_sequences_read_in_order() {
    let dir = Scratch::new("sheet");
    frame(dir.path(), "hoja", 12, 8); // 3 × 2 cells of 4 × 4
    for name in ["f1", "f2", "f10"] {
        frame(dir.path(), &format!("caminar/{name}"), 4, 4);
    }
    fs::write(
        dir.path().join(MANIFEST),
        r#"
id = "sheet"
[cuadro]
ancho = 4
alto = 4
[animaciones.idle]
hoja = "hoja.png"
columnas = 3
filas = 2
total = 5
[animaciones.caminar]
secuencia = "caminar"
fps = 10
[animaciones.ida_y_vuelta]
hoja = "hoja.png"
columnas = 3
filas = 2
orden = [0, 4, 0]
"#,
    )
    .unwrap();
    let pack = Pack::load(dir.path()).unwrap();
    let idle = pack.clip("idle").unwrap();
    assert_eq!(idle.steps.len(), 5);
    let cell = |step: &crate::anim::Step| pack.frame(step.frame).unwrap().cell.unwrap();
    assert_eq!(
        cell(&idle.steps[4]),
        Cell {
            x: 4,
            y: 4,
            w: 4,
            h: 4
        }
    );
    assert!((idle.steps[0].secs - 1.0 / DEFAULT_FPS).abs() < 1e-9);
    let walk = pack.clip("caminar").unwrap();
    let names: Vec<&str> = walk
        .steps
        .iter()
        .map(|s| pack.frame_name(s.frame))
        .collect();
    assert_eq!(names, ["caminar/f1", "caminar/f2", "caminar/f10"]);
    assert!((walk.steps[0].secs - 0.1).abs() < 1e-9);
    let back = pack.clip("ida_y_vuelta").unwrap();
    assert_eq!(
        back.steps[0].frame, back.steps[2].frame,
        "the same cell, one frame"
    );
    assert_eq!(pack.images().len(), 4, "one sheet, three files");
    let report = validate(dir.path());
    assert!(report.errors() > 0, "no license: {report}");
    assert!(report.to_string().contains("licencia"), "{report}");
}

#[test]
fn a_sheet_smaller_than_its_grid_is_an_error() {
    let dir = Scratch::new("smallsheet");
    frame(dir.path(), "hoja", 8, 4);
    fs::write(
        dir.path().join(MANIFEST),
        "id = \"smallsheet\"\nlicencia = \"CC0-1.0\"\n[cuadro]\nancho = 4\nalto = 4\n[animaciones.idle]\nhoja = \"hoja.png\"\ncolumnas = 4\nfilas = 1\n",
    )
    .unwrap();
    let report = validate(dir.path());
    assert!(!report.ok());
    assert!(report.to_string().contains("al menos 16×4"), "{report}");
}

#[test]
fn the_validator_speaks_to_artists() {
    let dir = Scratch::new("artist");
    frame(dir.path(), "a", 4, 4);
    frame(dir.path(), "grande", 8, 8);
    fs::write(
        dir.path().join(MANIFEST),
        r#"
id = "artist"
nombre = "Artista"
licencia = "CC BY 4.0"
autor = "Yo"
version = "1"
[cuadro]
ancho = 4
alto = 4
fps = 120
[animaciones.idle]
cuadros = ["a", "grande"]
[animaciones.olvidada]
cuadros = ["a"]
[estados]
dormir = "idle"
"#,
    )
    .unwrap();
    let report = validate(dir.path());
    let text = report.to_string();
    for needle in [
        "de más de 0 a 60",
        "se va a estirar",
        "nunca se usa",
        "no conozco el estado `dormir`",
        "transcribing",
    ] {
        assert!(text.contains(needle), "«{needle}» in:\n{text}");
    }
}

#[test]
fn installed_lists_wawi_first_and_active_falls_back_to_wawi() {
    let home = Scratch::new("home");
    let assets = crate::default_assets();
    let kitty = scaffold(&pets_dir(home.path()), "kitty", "Kitty").unwrap();
    // A broken pack: listed, but it never replaces Wawi.
    let broken = pets_dir(home.path()).join("rota");
    fs::create_dir_all(&broken).unwrap();
    fs::write(broken.join(MANIFEST), "id = \"rota\"\nnombre = \"Rota\"\n").unwrap();

    let list = installed(&assets, Some(home.path()));
    let ids: Vec<&str> = list.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, ["wawi", "kitty", "rota"]);
    assert_eq!(list[1].nombre, "Kitty");
    assert!(list[0].builtin && !list[1].builtin);

    let mut lines = Vec::new();
    let mut log = |line: String| lines.push(line);
    let active_one = |log: &mut dyn FnMut(String)| {
        active(&assets, Some(home.path()), None, log).map(|p| p.id().to_owned())
    };
    assert_eq!(active_one(&mut log).as_deref(), Some("wawi"));

    let placement = |pet: &str| crate::place::Placement {
        pet: Some(pet.into()),
        ..Default::default()
    };
    let state = home.path().join("mascot.json");
    placement("kitty").save(&state).unwrap();
    assert_eq!(active_one(&mut log).as_deref(), Some("kitty"));
    placement("rota").save(&state).unwrap();
    assert_eq!(active_one(&mut log).as_deref(), Some("wawi"));
    placement("nadie").save(&state).unwrap();
    assert_eq!(active_one(&mut log).as_deref(), Some("wawi"));
    let over = active(&assets, Some(home.path()), Some(&kitty), &mut log).unwrap();
    assert_eq!(over.name(), "Kitty");
    assert!(lines.iter().any(|l| l.contains("idle")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("nadie")), "{lines:?}");
    assert_eq!(find(&assets, Some(home.path()), "kitty"), Some(kitty));
    assert_eq!(find(&assets, None, "kitty"), None);
}

#[test]
fn voice_and_soul_are_offered_as_they_are() {
    let dir = Scratch::new("soul");
    frame(dir.path(), "a", 4, 4);
    fs::write(dir.path().join("alma.md"), "Sos una gata curiosa.\n").unwrap();
    let pack = Pack::from_text(
        dir.path(),
        "id = \"soul\"\n[animaciones.idle]\ncuadros = [\"a\"]\n[voz]\nid = \"ef_dora\"\n[alma]\narchivo = \"alma.md\"\n",
    )
    .unwrap();
    assert_eq!(pack.voice().unwrap().id.as_deref(), Some("ef_dora"));
    assert_eq!(pack.voice().unwrap().speed, None);
    assert_eq!(pack.soul().as_deref(), Some("Sos una gata curiosa.\n"));
    let inline = Pack::from_text(
        dir.path(),
        "id = \"soul\"\n[animaciones.idle]\ncuadros = [\"a\"]\n[alma]\ntexto = \"\"\"\nCuriosa.\"\"\"\n",
    )
    .unwrap();
    assert_eq!(inline.soul().as_deref(), Some("Curiosa."));
    assert!(wawi().soul().is_none());
}

#[test]
fn typo_distance_is_one_edit() {
    assert!(close("fps", "fsp"));
    assert!(close("cuadros", "cuadro"));
    assert!(close("fundido", "fundidp"));
    assert!(!close("fps", "fps"));
    assert!(!close("hoja", "boca"));
}

#[test]
fn wawi_has_a_body_with_wawi_1s_numbers() {
    use crate::body::Loco;
    let pack = wawi();
    let body = pack.body.as_ref().expect("Wawi has [cuerpo]");
    assert!(!body.faces_right);
    let p = &body.params;
    assert_eq!((p.gravity, p.max_fall, p.land_secs), (1500.0, 950.0, 0.35));
    assert_eq!(
        (p.walk_speed, p.trot_speed, p.climb_speed),
        (42.0, 130.0, 30.0)
    );
    assert_eq!((p.chute_speed, p.bounce), (75.0, 0.0));
    assert_eq!(p.wander_every, (8.0 * 60.0, 25.0 * 60.0));
    assert!(p.walks && p.climbs && p.chutes);
    assert_eq!(body.clip_for(Loco::Fall), Some("fall"));
    // Held, she dangles with the fall clip (Wawi 1).
    assert_eq!(body.clip_for(Loco::Held), Some("fall"));
    assert_eq!(body.clip_for(Loco::Flop), Some("flop"));
    // The long landings last their clips.
    assert!((p.flop_secs - 2.52).abs() < 1e-9, "{}", p.flop_secs);
    assert!((p.chute_land_secs - 2.3).abs() < 1e-9);
    // The odd-sized frames sit in their boxes, the canopy over her.
    let fall = pack.clip("fall").unwrap().steps[0].frame;
    let place = pack.frame(fall).unwrap().place.unwrap();
    assert!(place.w < 1.0 && place.y < 0.0, "{place:?}");
    let chute = pack.clip("chute").unwrap();
    assert_eq!(chute.layer.map(|l| l.at), Some((0.0, 0.99)));
    let (_, _, below, above) = pack.overhang();
    assert!(below > 0.1 && above > 0.3, "{below} {above}");
}

#[test]
fn a_pack_without_a_body_stays_put() {
    let dir = Scratch::new("nobody");
    frame(dir.path(), "solo", 8, 8);
    let pack = Pack::from_text(
        dir.path(),
        "id = \"nobody\"\n[animaciones.idle]\ncuadros = [\"solo\"]\n",
    )
    .unwrap();
    assert!(pack.body.is_none());
    assert_eq!(pack.overhang(), (0.0, 0.0, 0.0, 0.0));
}

#[test]
fn a_small_body_falls_back_and_the_mistakes_are_said() {
    use crate::body::Loco;
    let dir = Scratch::new("body");
    frame(dir.path(), "solo", 8, 8);
    frame(dir.path(), "cae", 4, 8);
    let pack = Pack::from_text(
        dir.path(),
        "id = \"body\"\n\
         [cuadro]\nancho = 8\nalto = 8\najuste = \"natural\"\n\
         [cuerpo]\nmira = \"derecha\"\ncaer = \"caida\"\ngravedad = -3\nrebote = 2\n\
         trepar = \"nada\"\nvelocidad_caminr = 40\n\
         [animaciones.idle]\ncuadros = [\"solo\"]\n\
         [animaciones.caida]\ncuadros = [\"cae\"]\nmovimiento = \"tumbo\"\n",
    )
    .unwrap();
    let body = pack.body.as_ref().unwrap();
    assert!(body.faces_right);
    // Bad numbers keep Wawi 1's.
    assert_eq!(body.params.gravity, 1500.0);
    assert_eq!(body.params.bounce, 0.0);
    // No walk: she only falls; the parachute and being held use the fall.
    assert!(!body.params.walks && !body.params.climbs);
    assert_eq!(body.clip_for(Loco::Chute), Some("caida"));
    assert_eq!(body.clip_for(Loco::Walk), None);
    assert_eq!(body.clip_for(Loco::Land), None);
    let said: Vec<&str> = pack.problems.iter().map(|p| p.message.as_str()).collect();
    for needle in [
        "`gravedad`",
        "`rebote`",
        "trepar = \"nada\"",
        "velocidad_caminar",
        "`caminar`",
    ] {
        assert!(
            said.iter().any(|m| m.contains(needle)),
            "{needle}: {said:?}"
        );
    }
    // A narrower picture stands at its own size, bottom-centre.
    let cae = pack.clip("caida").unwrap().steps[0].frame;
    let place = pack.frame(cae).unwrap().place.unwrap();
    assert_eq!((place.x, place.y, place.w, place.h), (0.25, 0.0, 0.5, 1.0));
}
