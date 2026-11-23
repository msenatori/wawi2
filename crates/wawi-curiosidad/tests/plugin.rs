//! The plugin folder in the repo loads with Wawi's own manifest loader, and
//! its watch is the rare, low-priority look.

use std::path::Path;
use std::time::Duration;

use wawi_core::Priority;
use wawi_plugins::manifest::{self, Expose, Kind, MAX_INSTRUCTIONS};
use wawi_plugins::schedule::When;

#[test]
fn the_plugin_in_the_repo_loads() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/curiosidad");
    let m = manifest::load(&dir).unwrap();
    assert_eq!(m.name, "curiosidad");
    assert_eq!(m.expose, Expose::Listed);
    assert!(
        !m.tools.contains_key("momento"),
        "the brain never sees momento"
    );
    assert_eq!(m.tools["mirar_pantalla"].kind, Some(Kind::Read));
    for act in ["no_mirar", "mirar_de_nuevo"] {
        assert_eq!(m.tools[act].kind, Some(Kind::Act));
        assert_eq!(m.tools[act].confirm, Some(false));
    }
    let [watch] = m.watches.as_slice() else {
        panic!("one watch")
    };
    assert_eq!(watch.tool, "momento");
    assert_eq!(watch.priority, Priority::Low);
    assert_eq!(watch.cooldown, Some(Duration::from_secs(3 * 3600)));
    assert!(matches!(watch.when, When::Every(every) if every == Duration::from_secs(20 * 60)));
    assert!(watch.say.contains("curiosidad__mirar_pantalla"));
    assert!(watch.say.contains("espontanea en true"));
    let text = std::fs::read_to_string(dir.join("instrucciones.md")).unwrap();
    assert!(text.chars().count() < MAX_INSTRUCTIONS);
}
