//! The plugin folder in the repo loads with Wawi's own manifest loader,
//! with the kinds it promises, and every example in its instructions is a
//! valid vigia.

use std::path::Path;

use wawi_instinto::spec::parse_vigia;
use wawi_plugins::manifest::{self, Expose, Kind, MAX_INSTRUCTIONS, Server};

#[test]
fn the_plugin_in_the_repo_loads() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/instinto");
    let m = manifest::load(&dir).unwrap();
    assert_eq!(m.name, "instinto");
    assert!(!m.observe_chat, "the observer is off by default");
    assert_eq!(m.expose, Expose::Listed);
    assert!(
        matches!(&m.server, Server::Stdio { command, args, .. } if command == "wawi-instinto" && args == &["mcp"])
    );
    assert_eq!(m.tools["vigilar"].kind, Some(Kind::Read));
    assert_eq!(m.tools["seguimientos"].kind, Some(Kind::Read));
    assert_eq!(m.tools["cancelar_seguimiento"].kind, Some(Kind::Act));
    assert_eq!(m.tools["cancelar_seguimiento"].confirm, Some(false));
    let text = std::fs::read_to_string(dir.join("instrucciones.md")).unwrap();
    assert!(
        text.chars().count() < MAX_INSTRUCTIONS,
        "fits without being cut"
    );
    let mut examples = 0;
    for line in text
        .lines()
        .filter(|l| l.starts_with(|c: char| c.is_ascii_digit()))
    {
        let json = line.split('`').nth(1).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(json).unwrap_or_else(|e| panic!("{line}: {e}"));
        parse_vigia(&value).unwrap_or_else(|e| panic!("{line}: {e}"));
        examples += 1;
    }
    assert_eq!(examples, 5);
}
