//! What the voice program hands the «Ajustes» window (`wawi-body`): the
//! plugins through the host's `PluginControl`, the voice and the instinct
//! gate as read-only facts, and the macOS permissions.
//!
//! Nothing here runs on the turn path. The window reads the plugin list as
//! a snapshot and posts actions to the host; the health check runs on each
//! plugin's own thread.

use std::sync::Arc;

use wawi_body::settings::{
    InfoRow, Login, PluginRow, PluginStatus, PluginsPanel, SettingsSource, SuggestionRow,
    instinct_rows,
};
use wawi_core::GateConfig;
use wawi_plugins::control::{PluginControl, PluginInfo, PluginKind, PluginState};
use wawi_tts_kokoro::{Accent, VoiceSettings};

use super::describe_choice;

/// The plugin host, as the window sees it.
pub struct Panel(pub PluginControl);

impl PluginsPanel for Panel {
    fn list(&self) -> Vec<PluginRow> {
        self.0.list().iter().map(row).collect()
    }

    fn suggested(&self) -> Vec<SuggestionRow> {
        self.0
            .suggested()
            .into_iter()
            .map(|s| SuggestionRow {
                name: s.name,
                title: s.title,
                description: s.description,
            })
            .collect()
    }

    fn connect(&self, target: &str) {
        self.0.connect(target);
    }

    fn disconnect(&self, name: &str) {
        self.0.disconnect(name);
    }

    fn set_enabled(&self, name: &str, enabled: bool) {
        self.0.set_enabled(name, enabled);
    }

    fn remove(&self, name: &str) {
        self.0.remove(name);
    }

    fn check_all(&self) {
        self.0.check_all();
    }

    fn subscribe(&self, on_change: Box<dyn Fn() + Send + Sync>) {
        self.0.subscribe(move |_| on_change());
    }
}

/// One plugin as a row of the window.
pub fn row(info: &PluginInfo) -> PluginRow {
    PluginRow {
        name: info.name.clone(),
        title: info.title.clone(),
        description: info.description.clone(),
        own: info.kind == PluginKind::Propio,
        installed: info.installed,
        enabled: info.enabled,
        status: match info.state {
            PluginState::Conectado => PluginStatus::Connected,
            PluginState::Conectando => PluginStatus::Connecting,
            PluginState::Desconectado => PluginStatus::NeedsLogin,
            PluginState::Apagado => PluginStatus::Off,
            PluginState::Error => PluginStatus::Error,
        },
        login: match info.auth {
            "oauth" => Login::Browser,
            "clave" => Login::Key,
            _ => Login::None,
        },
        detail: info.detail.clone(),
        tools: info.tools,
        checking: info.checking,
        check: info.check.clone(),
    }
}

fn accent(accent: Accent) -> &'static str {
    match accent {
        Accent::Castilian => "castellano",
        Accent::Rioplatense => "rioplatense",
        Accent::Neutral => "neutro",
    }
}

/// «Voz»: what hears, thinks and speaks, as it runs now.
pub fn voice_rows(
    voice: &VoiceSettings,
    model: Option<&str>,
    effort: Option<&str>,
) -> Vec<InfoRow> {
    vec![
        InfoRow::new("Para hablarle", "mantené ⌘ derecha y hablá"),
        InfoRow::new("Oído", "Parakeet TDT 0.6B v3, en tu Mac"),
        InfoRow::new("Cerebro", "Claude Code, con tu suscripción"),
        InfoRow::new(
            "Modelo y esfuerzo",
            format!(
                "{} · {} (WAWI2_CLAUDE_MODEL y WAWI2_CLAUDE_EFFORT)",
                describe_choice(model),
                describe_choice(effort)
            ),
        ),
        InfoRow::new("Voz", format!("Kokoro, {}", voice.voice)),
        InfoRow::new(
            "Velocidad y tono",
            format!(
                "{} · {:+} semitonos",
                format!("{:.2}", voice.speed).replace('.', ","),
                voice.pitch_semitones
            ),
        ),
        InfoRow::new("Acento", accent(voice.accent)),
    ]
}

/// Everything the window needs from the voice program.
pub fn source(
    plugins: Option<PluginControl>,
    voice: &VoiceSettings,
    model: Option<&str>,
    effort: Option<&str>,
    gate: &GateConfig,
) -> SettingsSource {
    SettingsSource {
        plugins: plugins.map(|control| Arc::new(Panel(control)) as Arc<dyn PluginsPanel>),
        voice: voice_rows(voice, model, effort),
        instinct: instinct_rows(gate),
        permissions: permissions::reader(),
        permissions_app: permissions::app(),
    }
}

#[cfg(target_os = "macos")]
mod permissions {
    use std::sync::Arc;

    use wawi_audio_mac::hotkey::input_monitoring_granted;
    use wawi_audio_mac::permissions::{MicPermission, launching_app, microphone_permission};
    use wawi_body::settings::{Grant, Permission, PermissionsReader};

    use crate::launch::Pane;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
    }

    /// Plain status queries: no prompt, nothing asked of the owner.
    pub fn reader() -> Option<PermissionsReader> {
        Some(Arc::new(|| {
            let mic = match microphone_permission() {
                MicPermission::Granted => Grant::Granted,
                MicPermission::Denied => Grant::Missing,
                MicPermission::Restricted => Grant::Blocked,
                MicPermission::NotDetermined => Grant::NotAsked,
            };
            let keys = if input_monitoring_granted() {
                Grant::Granted
            } else {
                Grant::Missing
            };
            // SAFETY: a plain query with no arguments; it never prompts.
            let screen = if unsafe { CGPreflightScreenCaptureAccess() } {
                Grant::Granted
            } else {
                Grant::Missing
            };
            vec![
                Permission {
                    name: Pane::Microphone.name().into(),
                    why: "Para escucharte mientras mantenés ⌘ derecha.".into(),
                    grant: mic,
                    pane: Pane::Microphone.url().into(),
                },
                Permission {
                    name: Pane::InputMonitoring.name().into(),
                    why: "Para saber cuándo apretás ⌘ derecha. Solo mira esa tecla.".into(),
                    grant: keys,
                    pane: Pane::InputMonitoring.url().into(),
                },
                Permission {
                    name: Pane::ScreenRecording.name().into(),
                    why: "Solo para el plugin curiosidad, que mira tu pantalla de vez en cuando."
                        .into(),
                    grant: screen,
                    pane: Pane::ScreenRecording.url().into(),
                },
            ]
        }))
    }

    pub fn app() -> Option<String> {
        Some(launching_app())
    }
}

#[cfg(not(target_os = "macos"))]
mod permissions {
    use wawi_body::settings::PermissionsReader;

    pub fn reader() -> Option<PermissionsReader> {
        None
    }

    pub fn app() -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use wawi_body::settings::{RowButton, row_view};

    use super::*;

    fn info(state: PluginState, auth: &'static str) -> PluginInfo {
        PluginInfo {
            name: "linear".into(),
            title: "Linear".into(),
            description: "Tareas.".into(),
            kind: PluginKind::Conector,
            installed: true,
            enabled: true,
            state,
            auth,
            url: Some("https://mcp.linear.app/mcp".into()),
            detail: Some("todavía no le diste permiso".into()),
            tools: 0,
            checking: false,
            check: None,
        }
    }

    #[test]
    fn a_signed_out_connector_asks_to_sign_in_in_the_browser() {
        let row = row(&info(PluginState::Desconectado, "oauth"));
        assert_eq!(row.status, PluginStatus::NeedsLogin);
        assert_eq!(row.login, Login::Browser);
        assert!(!row.own);
        let view = row_view(&row);
        assert_eq!(view.buttons, [RowButton::Connect, RowButton::Remove]);
        assert_eq!(RowButton::Connect.title(&row), "Iniciar sesión");
    }

    #[test]
    fn every_state_and_login_maps() {
        for (state, status) in [
            (PluginState::Conectado, PluginStatus::Connected),
            (PluginState::Conectando, PluginStatus::Connecting),
            (PluginState::Desconectado, PluginStatus::NeedsLogin),
            (PluginState::Apagado, PluginStatus::Off),
            (PluginState::Error, PluginStatus::Error),
        ] {
            assert_eq!(row(&info(state, "ninguna")).status, status);
        }
        assert_eq!(
            row(&info(PluginState::Conectado, "clave")).login,
            Login::Key
        );
        assert_eq!(
            row(&info(PluginState::Conectado, "ninguna")).login,
            Login::None
        );
        let mut own = info(PluginState::Apagado, "ninguna");
        own.kind = PluginKind::Propio;
        assert!(row(&own).own);
    }

    #[test]
    fn a_check_in_flight_and_its_result_reach_the_row() {
        let mut checking = info(PluginState::Conectado, "oauth");
        checking.checking = true;
        checking.check = Some(Err("no contestó en 6 s".into()));
        let row = row(&checking);
        assert!(row.checking);
        assert_eq!(row.check, Some(Err("no contestó en 6 s".into())));
    }

    #[test]
    fn the_voice_shows_what_runs_and_where_the_model_comes_from() {
        let rows = voice_rows(&VoiceSettings::default(), None, Some("low"));
        let text: Vec<String> = rows
            .iter()
            .map(|r| format!("{}: {}", r.label, r.value))
            .collect();
        assert!(
            text.contains(&"Voz: Kokoro, jf_alpha".to_owned()),
            "{text:?}"
        );
        assert!(
            text.contains(&"Velocidad y tono: 1,05 · +3 semitonos".to_owned()),
            "{text:?}"
        );
        assert!(text.contains(&"Acento: castellano".to_owned()), "{text:?}");
        assert!(
            text.iter()
                .any(|t| t.starts_with("Modelo y esfuerzo: el predeterminado del CLI · low")),
            "{text:?}"
        );
    }

    #[test]
    fn the_source_carries_the_gate_as_read_only_rows() {
        let source = source(
            None,
            &VoiceSettings::default(),
            None,
            None,
            &GateConfig::default(),
        );
        assert!(source.plugins.is_none());
        assert_eq!(source.instinct[0].value, "de 23:00 a 8:00");
        assert!(!source.voice.is_empty());
    }
}
