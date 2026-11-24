//! The plugin with a fake screen and a fake Mac: never a real screenshot.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use wawi_curiosidad::momento::Senales;
use wawi_curiosidad::{ESTADO, PAUSADA, SIN_PERMISO, Servidor};
use wawi_tools::{Civil, FakeClock, WallClock};

/// FakeClock is `Rc`; the server wants `Send + Sync`. A shared epoch does.
struct Clock(Arc<Mutex<i64>>);

impl WallClock for Clock {
    fn now(&self) -> i64 {
        *self.0.lock().unwrap()
    }
    fn local(&self, epoch: i64) -> Civil {
        FakeClock::at(Civil::date(2026, 11, 23).at(0, 0, 0), 0).local(epoch)
    }
    fn epoch_of(&self, local: Civil) -> Option<i64> {
        FakeClock::at(Civil::date(2026, 11, 23).at(0, 0, 0), 0).epoch_of(local)
    }
}

const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3, 0xFF, 0xD9];

struct Rig {
    server: Servidor,
    home: PathBuf,
    now: Arc<Mutex<i64>>,
    senales: Arc<Mutex<Senales>>,
    shots: Arc<AtomicUsize>,
}

fn rig(name: &str) -> Rig {
    let home = std::env::temp_dir().join(format!("wawi-curiosidad-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let start = FakeClock::at(Civil::date(2026, 11, 23).at(9, 0, 0), 0).now();
    let now = Arc::new(Mutex::new(start));
    let senales = Arc::new(Mutex::new(Senales {
        permiso: true,
        inactivo: Some(5),
        ..Senales::default()
    }));
    let shots = Arc::new(AtomicUsize::new(0));
    let (s1, s2, n) = (senales.clone(), senales.clone(), shots.clone());
    let server = Servidor::new(
        &home,
        Box::new(Clock(now.clone())),
        wawi_curiosidad::log::Log::none(),
        Box::new(move || {
            n.fetch_add(1, Ordering::SeqCst);
            Ok(JPEG.to_vec())
        }),
        Box::new(move || s1.lock().unwrap().clone()),
        Box::new(move || s2.lock().unwrap().clone()),
        Box::new(move |jpeg: &str, espontanea: bool| {
            assert!(!jpeg.is_empty());
            Ok(format!("Qué hay: una planilla. espontanea={espontanea}"))
        }),
        7,
    );
    Rig {
        server,
        home,
        now,
        senales,
        shots,
    }
}

fn text(result: &Value) -> String {
    result["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_owned()
}

#[test]
fn the_brain_gets_only_text_and_nothing_keeps_the_image() {
    let r = rig("texto");
    let result = r.server.call("mirar_pantalla", &json!({}));
    assert_eq!(result["isError"], false);
    let content = result["content"].as_array().unwrap();
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["type"], "text", "never an image to the brain");
    assert_eq!(text(&result), "Qué hay: una planilla. espontanea=false");
    let own = r
        .server
        .call("mirar_pantalla", &json!({ "espontanea": true }));
    assert!(text(&own).ends_with("espontanea=true"));
    // Nothing on disk holds it.
    let b64 = wawi_curiosidad::base64(JPEG);
    let estado = std::fs::read_to_string(r.home.join(ESTADO)).unwrap_or_default();
    assert!(!estado.contains(&b64));
    assert_eq!(r.shots.load(Ordering::SeqCst), 2);
    let _ = std::fs::remove_dir_all(&r.home);
}

#[test]
fn out_of_claude_quota_she_does_not_look_until_the_reset() {
    let r = rig("cupo");
    std::fs::create_dir_all(&r.home).unwrap();
    let now = *r.now.lock().unwrap() as u64;
    wawi_brain_claude::cupo::write(
        &r.home,
        &wawi_brain_claude::cupo::Cupo {
            hasta: now + 600,
            sabido: true,
            desde: now,
            origen: "wawi".into(),
        },
    )
    .unwrap();
    let result = r.server.call("mirar_pantalla", &json!({}));
    assert_eq!(result["isError"], true);
    assert!(text(&result).contains("sin cupo"), "{result}");
    assert_eq!(text(&r.server.call("momento", &json!({}))), "");
    assert_eq!(r.shots.load(Ordering::SeqCst), 0, "no capture");
    *r.now.lock().unwrap() += 601;
    assert_eq!(
        r.server.call("mirar_pantalla", &json!({}))["isError"],
        false
    );
    let _ = std::fs::remove_dir_all(&r.home);
}

#[test]
fn no_permission_a_locked_screen_or_a_pause_means_no_capture() {
    let r = rig("guardas");
    r.senales.lock().unwrap().permiso = false;
    let result = r.server.call("mirar_pantalla", &json!({}));
    assert_eq!(
        (result["isError"].as_bool(), text(&result)),
        (Some(true), SIN_PERMISO.to_owned())
    );
    r.senales.lock().unwrap().permiso = true;
    r.senales.lock().unwrap().bloqueada = true;
    let result = r.server.call("mirar_pantalla", &json!({}));
    assert!(text(&result).contains("bloqueada"), "{result}");
    r.senales.lock().unwrap().bloqueada = false;

    // «No mires la pantalla»: not on her own, not when asked.
    r.server.call("no_mirar", &json!({}));
    assert_eq!(text(&r.server.call("mirar_pantalla", &json!({}))), PAUSADA);
    assert_eq!(text(&r.server.call("momento", &json!({}))), "");
    r.server.call("mirar_de_nuevo", &json!({}));
    assert_eq!(
        r.server.call("mirar_pantalla", &json!({}))["isError"],
        false
    );
    assert_eq!(r.shots.load(Ordering::SeqCst), 1);
    let _ = std::fs::remove_dir_all(&r.home);
}

#[test]
fn spontaneous_looks_are_rare_and_never_right_after_the_owner() {
    let r = rig("momento");
    // A whole day, a check every 20 minutes, the owner talking every hour.
    let mut looks = Vec::new();
    for step in 0..(24 * 3) {
        let t = *r.now.lock().unwrap();
        if step % 3 == 0 {
            r.server.call(
                "observar_charla",
                &json!({ "dueno": "secreto del dueño", "wawi": "x", "iniciativa": false }),
            );
        }
        let answer = text(&r.server.call("momento", &json!({})));
        if !answer.is_empty() {
            assert!(answer.starts_with("Buen momento para mirar ("), "{answer}");
            looks.push(t);
        }
        *r.now.lock().unwrap() += 20 * 60;
    }
    assert!(looks.len() <= 2, "{looks:?}");
    if looks.len() == 2 {
        assert!(looks[1] - looks[0] >= 3 * 3600);
    }
    // The owner's words are never kept.
    let estado = std::fs::read_to_string(r.home.join(ESTADO)).unwrap_or_default();
    assert!(!estado.contains("secreto"));
    assert_eq!(r.shots.load(Ordering::SeqCst), 0, "momento never captures");
    let _ = std::fs::remove_dir_all(&r.home);
}

#[test]
fn a_full_screen_app_or_a_meeting_keeps_her_from_looking() {
    for tweak in [
        (|s: &mut Senales| s.pantalla_completa = true) as fn(&mut Senales),
        |s| s.reunion = true,
        |s| s.camara = true,
        |s| s.salvapantallas = true,
    ] {
        let r = rig("reunion");
        tweak(&mut r.senales.lock().unwrap());
        // Far from launch, many tries: never a yes.
        *r.now.lock().unwrap() += 3600;
        for _ in 0..200 {
            assert_eq!(text(&r.server.call("momento", &json!({}))), "");
        }
        let _ = std::fs::remove_dir_all(&r.home);
    }
}
