//! The plugin folder in the repo loads with Wawi's own manifest loader,
//! learning off by default.

use std::path::Path;

use wawi_plugins::manifest::{self, Expose, Kind, MAX_INSTRUCTIONS, Server};

#[test]
fn the_plugin_in_the_repo_loads_with_learning_off() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/caracter");
    let m = manifest::load(&dir).unwrap();
    assert_eq!(m.name, "caracter");
    assert!(!m.observe_chat, "learning is off by default");
    assert_eq!(m.expose, Expose::Listed);
    assert!(
        matches!(&m.server, Server::Stdio { command, args, .. } if command == "wawi-caracter" && args == &["mcp"])
    );
    assert_eq!(m.tools["trato"].kind, Some(Kind::Read));
    assert_eq!(m.tools["quitar_regla"].kind, Some(Kind::Act));
    assert_eq!(m.tools["quitar_regla"].confirm, Some(false));
    assert_eq!(m.tools["olvidar_trato"].kind, Some(Kind::Act));
    let text = std::fs::read_to_string(dir.join("instrucciones.md")).unwrap();
    assert!(text.chars().count() < MAX_INSTRUCTIONS);
}
