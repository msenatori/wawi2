//! The whole loop on temp repos, with a fake claude and fake gates:
//! report, attempt, gates, proposal, apply, watch, revert.

mod comun;

use std::path::Path;
use std::rc::Rc;

use comun::*;
use serde_json::{Value, json};
use wawi_autoarreglo::diario::{self, Estado};
use wawi_autoarreglo::incidentes::{self, Incidente};
use wawi_autoarreglo::intento::{self, EstadoServicio, Fin};
use wawi_autoarreglo::log::Log;
use wawi_autoarreglo::motor::{Falla, Rol};
use wawi_autoarreglo::propuestas::{self, EstadoP};
use wawi_autoarreglo::servidor::Servidor;
use wawi_autoarreglo::vigia;
use wawi_autoarreglo::worker::Worker;
use wawi_tools::{FakeClock, WallClock};

fn add_fix(dir: &Path) {
    std::fs::write(dir.join("src/fix.rs"), "pub fn fixed() -> bool { true }\n").unwrap();
}

fn incident(home: &Path, tipo: &str, clave: &str, n: usize) {
    for i in 0..n {
        incidentes::dejar(
            home,
            &format!("{tipo}-{i}-{}", clave.len()),
            &Incidente {
                tipo: tipo.into(),
                clave: clave.into(),
                resumen: format!("{clave} ({i})"),
                detalle: Value::Null,
                creado: "2026-11-21T02:00".into(),
                origen: "host".into(),
            },
        )
        .unwrap();
    }
}

fn worker(home: &Path, clock: Rc<FakeClock>) -> Worker {
    Worker {
        home: home.to_path_buf(),
        clock,
        motor: Box::new(FakeMotor::fixing(Box::new(add_fix))),
        comandos: Box::new(FakeComandos::green()),
        log: Log::none(),
        last_work: None,
    }
}

#[test]
fn a_report_becomes_a_tested_proposal_then_one_squash_commit_then_a_revert() {
    let root = temp("flujo");
    let (home, repo) = (root.join("home"), root.join("repo"));
    comun::repo(&repo, true);
    write_config(&home, &repo, "");
    let clock = clock();

    // The owner reports it; the worker folds it into the journal.
    let server = Servidor::new(&home, Log::none());
    let (text, err) = server.call_with(
        "reportar_problema",
        &json!({ "que_paso": "se me corta la voz al empezar, mi clave sk-ant-abcdefghijklmnopqrstuv" }),
        &*clock,
    );
    assert!(!err && text.starts_with("Anotado"), "{text}");
    let w = worker(&home, clock.clone());
    assert_eq!(w.absorber(), 1);
    let problemas = diario::load_all(&home);
    assert_eq!(problemas.len(), 1);
    assert_eq!(problemas[0].tipo, "reportado");
    assert!(!problemas[0].titulo.contains("sk-ant-abc"), "scrubbed");

    // One attempt.
    let config = config(&repo);
    let motor = FakeMotor::fixing(Box::new(add_fix));
    let cmds = FakeComandos::green();
    let log = Log::none();
    let fin = intento::intentar(
        &ctx(&home, &config, &clock, &motor, &cmds, &log),
        true,
        None,
    );
    let Fin::Propuesta(id) = fin else {
        panic!("{fin:?}")
    };

    // The author worked in a worktree outside the repo, with Opus and a
    // scrubbed brief; the reviewer read the same worktree.
    let calls = motor.calls.borrow();
    assert_eq!(calls.len(), 2);
    let (rol, model, dir, brief) = &calls[0];
    assert_eq!(*rol, Rol::Autor);
    assert_eq!(model, "claude-opus-5-5");
    assert!(dir.starts_with(home.join("autoarreglo/trabajo")) && !dir.starts_with(&repo));
    assert!(!brief.contains("sk-ant-abc"), "{brief}");
    assert_eq!(calls[1].0, Rol::Revisor);
    drop(calls);
    // Every gate command ran: the base first, then the fix and the speed tests.
    let runs = cmds.runs.borrow();
    assert!(
        runs.iter().filter(|r| r.contains("fmt")).count() >= 2,
        "{runs:?}"
    );
    assert!(
        runs.iter()
            .any(|r| r.contains("a_turn_never_waits_for_a_respawn"))
    );
    drop(runs);

    // The proposal: Markdown in Spanish, the diff, the branch, a fact.
    let folder = propuestas::dir(&home, &id);
    let md = std::fs::read_to_string(folder.join("propuesta.md")).unwrap();
    for section in [
        "Qué andaba mal",
        "Por qué pasaba",
        "Qué cambié",
        "Las pruebas",
        "Las compuertas",
        "[pasa] revisión",
    ] {
        assert!(md.contains(section), "{section}\n{md}");
    }
    assert!(
        std::fs::read_to_string(folder.join("cambios.diff"))
            .unwrap()
            .contains("src/fix.rs")
    );
    assert!(git(&repo, &["branch", "--list", "autoarreglo/*"]).contains(&id));
    assert!(
        !intento::worktree_path(&home, &id).exists(),
        "the worktree is gone"
    );
    let facts = facts(&home);
    assert_eq!(facts.len(), 1);
    assert!(
        facts[0].contains("Encontré por qué se me cortaba la voz"),
        "{}",
        facts[0]
    );
    assert_eq!(diario::load_all(&home)[0].estado, Estado::Propuesto);
    assert!(
        EstadoServicio::load(&home)
            .dias
            .values()
            .any(|d| d.intentos == 1 && d.usd == 3.0)
    );

    // contar_arreglo and arreglos.
    let (text, _) = server.call_with("contar_arreglo", &json!({ "id": id }), &*clock);
    assert!(
        text.contains("se me corta") || text.contains("andaba mal"),
        "{text}"
    );
    let (text, _) = server.call_with("arreglos", &json!({}), &*clock);
    assert!(text.contains(&id), "{text}");

    // A dirty tree: nothing happens, and she says why.
    std::fs::write(repo.join("notas.txt"), "a medio hacer").unwrap();
    let before = commits(&repo);
    let (text, err) = server.call_with("aplicar_arreglo", &json!({ "id": id }), &*clock);
    assert!(err && text.contains("sin commitear"), "{text}");
    assert_eq!(commits(&repo), before);
    std::fs::remove_file(repo.join("notas.txt")).unwrap();

    // A clean tree: ONE commit with the trailer, the branch gone.
    let (text, err) = server.call_with("aplicar_arreglo", &json!({ "id": id }), &*clock);
    assert!(!err, "{text}");
    assert!(text.contains("recompilarme"), "{text}");
    assert_eq!(commits(&repo), before + 1);
    let message = git(&repo, &["log", "-1", "--format=%B"]);
    assert!(message.starts_with("Autoarreglo: "), "{message}");
    assert!(message.contains(&format!("Autoarreglo: {id}")), "{message}");
    assert_eq!(
        git(&repo, &["log", "-1", "--format=%an"]).trim(),
        "Dueño de prueba"
    );
    assert!(repo.join("src/fix.rs").exists());
    assert!(
        git(&repo, &["branch", "--list", "autoarreglo/*"])
            .trim()
            .is_empty()
    );
    assert_eq!(diario::load_all(&home)[0].estado, Estado::Aplicado);
    let (_, err) = server.call_with("aplicar_arreglo", &json!({ "id": id }), &*clock);
    assert!(err, "applying twice does nothing");

    // The first run after it crashes: the watchdog offers the revert.
    let p = propuestas::cargar(&home, &id).unwrap();
    let start = clock.local(p.aplicada_ts.unwrap() + 60);
    let next = clock.local(p.aplicada_ts.unwrap() + 120);
    let stamp = |c: wawi_tools::Civil| {
        format!(
            "{} {:02}:{:02}:{:02}.000",
            c.iso_date(),
            c.hour,
            c.minute,
            c.second
        )
    };
    std::fs::create_dir_all(home.join("logs")).unwrap();
    std::fs::write(
        home.join("logs/session.log"),
        format!("{} startup\n{} startup\n", stamp(start), stamp(next)),
    )
    .unwrap();
    clock.advance(600);
    vigia::mirar(&home, &*clock, &Log::none());
    let p = propuestas::cargar(&home, &id).unwrap();
    assert_eq!(p.vigia.as_ref().unwrap().estado, "sospechoso");
    assert!(
        comun::facts(&home)
            .iter()
            .any(|f| f.contains("¿Lo deshago?"))
    );

    // The revert: a new commit, never a reset.
    let (text, err) = server.call_with("revertir_arreglo", &json!({ "id": id }), &*clock);
    assert!(!err, "{text}");
    assert_eq!(commits(&repo), before + 2);
    assert!(!repo.join("src/fix.rs").exists());
    assert!(git(&repo, &["log", "-1", "--format=%B"]).contains("Autoarreglo-Revierte: "));
    assert_eq!(
        propuestas::cargar(&home, &id).unwrap().estado,
        EstadoP::Revertida
    );
    let p = &diario::load_all(&home)[0];
    assert_eq!(p.estado, Estado::Abierto);
    assert!(p.intentos.iter().any(|i| i.resultado == "revertido"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_forbidden_path_discards_the_attempt_and_it_waits_a_week() {
    let root = temp("prohibido");
    let (home, repo) = (root.join("home"), root.join("repo"));
    comun::repo(&repo, true);
    incident(&home, "cerebro_caido", "claude se cerró", 1);
    let clock = clock();
    worker(&home, clock.clone()).absorber();
    let config = config(&repo);
    let motor = FakeMotor::fixing(Box::new(|dir: &Path| {
        std::fs::create_dir_all(dir.join("crates/wawi-autoarreglo/src")).unwrap();
        std::fs::write(
            dir.join("crates/wawi-autoarreglo/src/gates.rs"),
            "// nada\n",
        )
        .unwrap();
    }));
    let cmds = FakeComandos::green();
    let log = Log::none();
    let c = ctx(&home, &config, &clock, &motor, &cmds, &log);
    let fin = intento::intentar(&c, true, None);
    assert!(
        matches!(&fin, Fin::Fallo(r) if r.contains("prohibidos")),
        "{fin:?}"
    );
    assert!(
        git(&repo, &["branch", "--list", "autoarreglo/*"])
            .trim()
            .is_empty()
    );
    assert!(propuestas::todas(&home).is_empty());
    assert!(facts(&home).is_empty(), "nothing to say");
    let p = &diario::load_all(&home)[0];
    assert_eq!(p.intentos.len(), 1);
    assert_eq!(p.intentos[0].resultado, "rechazado");
    // The next try: nothing eligible for a week.
    let fin = intento::intentar(&c, true, None);
    assert!(
        matches!(&fin, Fin::Nada(r) if r.contains("nada para arreglar")),
        "{fin:?}"
    );
    clock.advance(8 * 86_400);
    let motor_ok = FakeMotor::fixing(Box::new(add_fix));
    let c = ctx(&home, &config, &clock, &motor_ok, &cmds, &log);
    assert!(matches!(
        intento::intentar(&c, true, None),
        Fin::Propuesta(_)
    ));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn each_gate_failure_is_remembered_in_the_next_brief() {
    let root = temp("gates");
    let (home, repo) = (root.join("home"), root.join("repo"));
    comun::repo(&repo, true);
    let clock = clock();
    let log = Log::none();
    let mut config = config(&repo);
    config.max_intentos_dia = 10;
    config.presupuesto_dia_usd = 100.0;
    config.reintento_dias = 0;
    incident(&home, "turno_lento", "cerebro", 3);
    worker(&home, clock.clone()).absorber();

    // A secret in the code.
    let motor = FakeMotor::fixing(Box::new(|dir: &Path| {
        std::fs::write(
            dir.join("src/k.rs"),
            "const K: &str = \"/Users/ana/claves\";\n",
        )
        .unwrap();
    }));
    let green = FakeComandos::green();
    let fin = intento::intentar(
        &ctx(&home, &config, &clock, &motor, &green, &log),
        true,
        None,
    );
    assert!(
        matches!(&fin, Fin::Fallo(r) if r.contains("secretos")),
        "{fin:?}"
    );

    // A dependency.
    let motor = FakeMotor::fixing(Box::new(|dir: &Path| {
        std::fs::write(dir.join("Cargo.toml"), "[dependencies]\nrand = \"0.9\"\n").unwrap();
    }));
    clock.advance(60);
    let fin = intento::intentar(
        &ctx(&home, &config, &clock, &motor, &green, &log),
        true,
        None,
    );
    assert!(
        matches!(&fin, Fin::Fallo(r) if r.contains("dependencias")),
        "{fin:?}"
    );

    // Clippy fails (only after the fix: the base is green).
    let motor = FakeMotor::fixing(Box::new(add_fix));
    let mut config_nobase = config.clone();
    config_nobase.probar_base = false;
    let red = FakeComandos {
        fail: Some("clippy".into()),
        ..FakeComandos::green()
    };
    clock.advance(60);
    let fin = intento::intentar(
        &ctx(&home, &config_nobase, &clock, &motor, &red, &log),
        true,
        None,
    );
    assert!(
        matches!(&fin, Fin::Fallo(r) if r.contains("clippy")),
        "{fin:?}"
    );

    // A speed test that no longer runs.
    let zero = FakeComandos {
        zero_tests: true,
        ..FakeComandos::green()
    };
    clock.advance(60);
    let fin = intento::intentar(
        &ctx(&home, &config, &clock, &motor, &zero, &log),
        true,
        None,
    );
    assert!(
        matches!(&fin, Fin::Fallo(r) if r.contains("velocidad")),
        "{fin:?}"
    );

    // The reviewer says no.
    let mut motor = FakeMotor::fixing(Box::new(add_fix));
    motor.review = Ok(json!({ "aprobado": false, "motivos": ["la prueba no reproduce nada"] }));
    clock.advance(60);
    let fin = intento::intentar(
        &ctx(&home, &config, &clock, &motor, &green, &log),
        true,
        None,
    );
    assert!(
        matches!(&fin, Fin::Fallo(r) if r.contains("revisión")),
        "{fin:?}"
    );
    // The brief of that last attempt listed the earlier failures.
    let brief = &motor.calls.borrow()[0].3;
    assert!(
        brief.contains("no repitas") && brief.contains("dependencias"),
        "{brief}"
    );

    // The author's budget runs out.
    let mut motor = FakeMotor::fixing(Box::new(add_fix));
    motor.author = Err(Falla {
        motivo: "claude falló: se pasó del presupuesto".into(),
        costo_usd: 4.0,
    });
    clock.advance(60);
    let fin = intento::intentar(
        &ctx(&home, &config, &clock, &motor, &green, &log),
        true,
        None,
    );
    assert!(
        matches!(&fin, Fin::Fallo(r) if r.contains("presupuesto")),
        "{fin:?}"
    );
    assert_eq!(diario::load_all(&home)[0].intentos.len(), 6);
    assert!(
        git(&repo, &["branch", "--list", "autoarreglo/*"])
            .trim()
            .is_empty()
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn caps_off_by_default_no_commit_and_a_red_base() {
    let root = temp("topes");
    let (home, repo) = (root.join("home"), root.join("repo"));
    let clock = clock();
    let log = Log::none();
    let motor = FakeMotor::fixing(Box::new(add_fix));
    let green = FakeComandos::green();
    incident(&home, "reportado", "no me entendiste", 1);
    incident(&home, "cerebro_caido", "claude se cerró", 1);
    incident(&home, "audio", "la reproducción nunca terminó", 2);
    worker(&home, clock.clone()).absorber();

    // Off by default: nothing at all.
    comun::repo(&repo, false);
    let off = wawi_autoarreglo::config::Config::default();
    let fin = intento::intentar(&ctx(&home, &off, &clock, &motor, &green, &log), true, None);
    assert!(
        matches!(&fin, Fin::Nada(r) if r.contains("apagado")),
        "{fin:?}"
    );

    // No commit yet: a friendly blocked request, once.
    let config = config(&repo);
    let fin = intento::intentar(
        &ctx(&home, &config, &clock, &motor, &green, &log),
        true,
        None,
    );
    assert!(
        matches!(&fin, Fin::Nada(r) if r.contains("ningún commit")),
        "{fin:?}"
    );
    intento::intentar(
        &ctx(&home, &config, &clock, &motor, &green, &log),
        true,
        None,
    );
    let facts = facts(&home);
    assert_eq!(facts.len(), 1, "{facts:?}");
    assert!(facts[0].contains("necesito que el repo tenga un primer commit"));
    assert!(motor.calls.borrow().is_empty(), "no model was called");
    assert_eq!(
        git(&repo, &["rev-list", "--all", "--count"]).trim(),
        "0",
        "she made no commit"
    );

    // Outside the window, the worker waits (a manual run does not).
    std::fs::write(repo.join("a.txt"), "a").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "primero"]);
    let mut night = config.clone();
    night.ventana = "04:00-05:00".into();
    assert!(intento::puede(&ctx(&home, &night, &clock, &motor, &green, &log), false).is_err());

    // A red base: no money spent, the owner told, and no retry until HEAD moves.
    let red = FakeComandos {
        fail: Some("cargo test".into()),
        ..FakeComandos::green()
    };
    let fin = intento::intentar(&ctx(&home, &config, &clock, &motor, &red, &log), true, None);
    assert!(
        matches!(&fin, Fin::Nada(r) if r.contains("base no pasa")),
        "{fin:?}"
    );
    assert!(motor.calls.borrow().is_empty());
    assert!(
        comun::facts(&home)
            .iter()
            .any(|f| f.contains("mis pruebas no pasan"))
    );
    let fin = intento::intentar(
        &ctx(&home, &config, &clock, &motor, &green, &log),
        true,
        None,
    );
    assert!(
        matches!(&fin, Fin::Nada(r) if r.contains("base")),
        "{fin:?}"
    );
    std::fs::write(repo.join("b.txt"), "b").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "segundo"]);

    // Two attempts a day at most.
    let mut failing = FakeMotor::fixing(Box::new(|_: &Path| {}));
    failing.cost = 0.5;
    let c = ctx(&home, &config, &clock, &failing, &green, &log);
    assert!(matches!(intento::intentar(&c, true, None), Fin::Fallo(_)));
    assert!(matches!(intento::intentar(&c, true, None), Fin::Fallo(_)));
    let fin = intento::intentar(&c, true, None);
    assert!(
        matches!(&fin, Fin::Nada(r) if r.contains("2 intentos")),
        "{fin:?}"
    );

    // And a daily budget.
    let mut tight = config.clone();
    tight.max_intentos_dia = 10;
    tight.presupuesto_dia_usd = 3.0;
    let fin = intento::intentar(
        &ctx(&home, &tight, &clock, &failing, &green, &log),
        true,
        None,
    );
    assert!(
        matches!(&fin, Fin::Nada(r) if r.contains("presupuesto")),
        "{fin:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn discarding_is_wont_fix_and_a_moved_branch_that_conflicts_is_refused() {
    let root = temp("descartar");
    let (home, repo) = (root.join("home"), root.join("repo"));
    comun::repo(&repo, true);
    write_config(&home, &repo, "max_intentos_dia = 5\n");
    let clock = clock();
    let log = Log::none();
    incident(&home, "reportado", "esto no anda", 1);
    incident(&home, "cerebro_caido", "claude se cerró", 1);
    worker(&home, clock.clone()).absorber();
    let mut config = config(&repo);
    config.max_intentos_dia = 5;
    let edit_lib = |dir: &Path| {
        std::fs::write(dir.join("src/lib.rs"), "pub fn uno() -> u32 { 1 + 0 }\n").unwrap();
    };
    let motor = FakeMotor::fixing(Box::new(edit_lib));
    let green = FakeComandos::green();
    let c = ctx(&home, &config, &clock, &motor, &green, &log);
    let Fin::Propuesta(first) = intento::intentar(&c, true, None) else {
        panic!()
    };
    let Fin::Propuesta(second) = intento::intentar(&c, true, None) else {
        panic!()
    };
    let server = Servidor::new(&home, Log::none());
    let (text, err) = server.call_with("descartar_arreglo", &json!({ "id": first }), &*clock);
    assert!(!err, "{text}");
    let discarded = diario::load_all(&home)
        .into_iter()
        .find(|p| p.propuesta.as_deref() == Some(first.as_str()))
        .unwrap();
    assert_eq!(discarded.estado, Estado::Descartado);
    assert!(!git(&repo, &["branch", "--list"]).contains(&first));
    // The owner changed the same line meanwhile: it does not apply.
    std::fs::write(repo.join("src/lib.rs"), "pub fn uno() -> u32 { 2 - 1 }\n").unwrap();
    git(&repo, &["commit", "-q", "-am", "cambio del dueño"]);
    let before = commits(&repo);
    let (text, err) = server.call_with("aplicar_arreglo", &json!({ "id": second }), &*clock);
    assert!(err && text.contains("choca"), "{text}");
    assert_eq!(commits(&repo), before);
    assert!(
        git(&repo, &["status", "--porcelain"]).trim().is_empty(),
        "the tree is untouched"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn out_of_quota_it_waits_and_when_the_model_breaks_the_owner_hears_once() {
    let root = temp("no-corrio");
    let (home, repo) = (root.join("home"), root.join("repo"));
    comun::repo(&repo, true);
    let clock = clock();
    let log = Log::none();
    let mut config = config(&repo);
    config.max_intentos_dia = 10;
    config.reintento_dias = 0;
    incident(&home, "turno_lento", "cerebro", 3);
    worker(&home, clock.clone()).absorber();
    let cmds = FakeComandos::green();
    // Out of Claude quota: no attempt, no model call, nothing to say.
    let now = clock.now() as u64;
    wawi_brain_claude::cupo::write(
        &home,
        &wawi_brain_claude::cupo::Cupo {
            hasta: now + 600,
            sabido: true,
            desde: now,
            origen: "wawi".into(),
        },
    )
    .unwrap();
    let motor = FakeMotor::fixing(Box::new(add_fix));
    let fin = intento::intentar(
        &ctx(&home, &config, &clock, &motor, &cmds, &log),
        true,
        None,
    );
    assert!(
        matches!(&fin, Fin::Nada(r) if r.contains("sin cupo")),
        "{fin:?}"
    );
    assert!(motor.calls.borrow().is_empty());
    assert!(facts(&home).is_empty());
    // After the reset the model breaks (a revoked login): one notice a day.
    clock.advance(601);
    let mut broken = FakeMotor::fixing(Box::new(add_fix));
    broken.author = Err(Falla {
        motivo: "claude falló: OAuth token revoked".into(),
        costo_usd: 0.0,
    });
    for _ in 0..2 {
        let fin = intento::intentar(
            &ctx(&home, &config, &clock, &broken, &cmds, &log),
            true,
            None,
        );
        assert!(
            matches!(&fin, Fin::Fallo(r) if r.contains("claude falló")),
            "{fin:?}"
        );
        clock.advance(60);
    }
    let said = facts(&home);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("no pude terminar") && said[0].contains(r#""origen":"aviso""#));
    let fallo = wawi_plugins::fallos::leer(&home, "autoarreglo").unwrap();
    assert_eq!(fallo.tipo, "no_corrio");
    let _ = std::fs::remove_dir_all(&root);
}
