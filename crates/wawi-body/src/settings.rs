//! The settings window («Ajustes»), its pure side: what each section shows,
//! the words on each plugin row, which buttons it gets, the health-check
//! summary, and the read-only rows. The AppKit window (`mac/settings.rs`)
//! only lays these out; everything here is tested on any OS.
//!
//! The window gets its data from a [`SettingsSource`] the program hands the
//! body once ([`crate::Update::Settings`]): the plugins (through
//! [`PluginsPanel`], which the voice program backs with the plugin host's
//! `PluginControl`), the voice and instinct facts, and a way to read the
//! permissions. The pet section is the body's own.
//!
//! Speed: nothing here runs unless the window is open. The window is built
//! when it is first opened and dropped when it closes; plugin changes only
//! reach it while it is open.

use std::fmt;
use std::sync::Arc;

use wawi_core::GateConfig;

/// The window's sections, in toolbar order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Plugins,
    Mascota,
    Voz,
    Instinto,
    Privacidad,
}

impl Section {
    pub const ALL: [Section; 5] = [
        Section::Plugins,
        Section::Mascota,
        Section::Voz,
        Section::Instinto,
        Section::Privacidad,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Section::Plugins => "Plugins",
            Section::Mascota => "Mascota",
            Section::Voz => "Voz",
            Section::Instinto => "Instinto",
            Section::Privacidad => "Privacidad",
        }
    }

    /// The SF Symbol on its toolbar button.
    pub fn symbol(self) -> &'static str {
        match self {
            Section::Plugins => "puzzlepiece.extension",
            Section::Mascota => "pawprint",
            Section::Voz => "waveform",
            Section::Instinto => "sparkles",
            Section::Privacidad => "hand.raised",
        }
    }
}

/// How a plugin is doing (the plugin host's states, as the body sees them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginStatus {
    Connected,
    Connecting,
    /// Needs the owner: never authorized, signed out, or revoked.
    NeedsLogin,
    Off,
    Error,
}

/// How it signs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Login {
    None,
    /// OAuth in the browser.
    Browser,
    /// An API key in the secure dialog.
    Key,
}

/// One installed plugin (or one of Wawi's own, installed or not).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginRow {
    pub name: String,
    pub title: String,
    pub description: String,
    /// Ships with Wawi (instinto, caracter, curiosidad, autoarreglo).
    pub own: bool,
    pub installed: bool,
    pub enabled: bool,
    pub status: PluginStatus,
    pub login: Login,
    /// Why it is not connected, or how to fix it. Never a secret.
    pub detail: Option<String>,
    pub tools: usize,
    /// A health check is running for it.
    pub checking: bool,
    /// The last health check, if one ran since its state changed.
    pub check: Option<Result<(), String>>,
}

/// A suggested connector that is not installed yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuggestionRow {
    pub name: String,
    pub title: String,
    pub description: String,
}

/// What the plugins section asks of the program. Every call returns at
/// once: reads copy a snapshot, actions post a request.
pub trait PluginsPanel: Send + Sync {
    fn list(&self) -> Vec<PluginRow>;
    fn suggested(&self) -> Vec<SuggestionRow>;
    /// A plugin's name, a suggestion's name or a server URL.
    fn connect(&self, target: &str);
    fn disconnect(&self, name: &str);
    fn set_enabled(&self, name: &str, enabled: bool);
    fn remove(&self, name: &str);
    /// A health check of every running plugin; one in flight is joined.
    fn check_all(&self);
    /// `on_change` runs (on any thread) after every change to the list. It
    /// is called once per source; it must only post and return.
    fn subscribe(&self, on_change: Box<dyn Fn() + Send + Sync>);
}

/// A fact shown read-only: «Voz» · «jf_alpha».
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InfoRow {
    pub label: String,
    pub value: String,
}

impl InfoRow {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }
}

/// A permission's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grant {
    Granted,
    Missing,
    /// Nobody decided yet: macOS asks the first time it is needed.
    NotAsked,
    /// A profile or parental controls block it.
    Blocked,
}

impl Grant {
    pub fn label(self) -> &'static str {
        match self {
            Grant::Granted => "Permitido",
            Grant::Missing => "Falta",
            Grant::NotAsked => "Todavía no se pidió",
            Grant::Blocked => "Bloqueado por el sistema",
        }
    }

    pub fn tone(self) -> Tone {
        match self {
            Grant::Granted => Tone::Good,
            Grant::Missing => Tone::Bad,
            Grant::NotAsked => Tone::Warn,
            Grant::Blocked => Tone::Off,
        }
    }
}

/// One permission row: «Micrófono» · «para escucharte» · Permitido ·
/// [Abrir Ajustes del Sistema].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Permission {
    pub name: String,
    /// What Wawi needs it for.
    pub why: String,
    pub grant: Grant,
    /// The System Settings pane (`x-apple.systempreferences:…`).
    pub pane: String,
}

/// Reads the permissions now (cheap system queries, on the UI thread).
pub type PermissionsReader = Arc<dyn Fn() -> Vec<Permission> + Send + Sync>;

/// Everything the window shows that the body does not own.
#[derive(Clone, Default)]
pub struct SettingsSource {
    pub plugins: Option<Arc<dyn PluginsPanel>>,
    /// «Voz»: what speaks and hears, read-only.
    pub voice: Vec<InfoRow>,
    /// «Instinto»: the gate's limits, read-only.
    pub instinct: Vec<InfoRow>,
    pub permissions: Option<PermissionsReader>,
    /// Who macOS gives the permissions to («la app Terminal»).
    pub permissions_app: Option<String>,
}

impl fmt::Debug for SettingsSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SettingsSource")
            .field("plugins", &self.plugins.is_some())
            .field("voice", &self.voice)
            .field("instinct", &self.instinct)
            .field("permissions", &self.permissions.is_some())
            .finish()
    }
}

impl PartialEq for SettingsSource {
    fn eq(&self, other: &Self) -> bool {
        let same_plugins = match (&self.plugins, &other.plugins) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        let same_permissions = match (&self.permissions, &other.permissions) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        same_plugins
            && same_permissions
            && self.voice == other.voice
            && self.instinct == other.instinct
            && self.permissions_app == other.permissions_app
    }
}

// -- plugin rows ----------------------------------------------------------------

/// The colour of a status dot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Good,
    Busy,
    Warn,
    Bad,
    Off,
}

/// A button on a plugin row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowButton {
    Connect,
    Disconnect,
    Remove,
}

impl RowButton {
    pub fn title(self, row: &PluginRow) -> &'static str {
        match self {
            RowButton::Connect if row.status == PluginStatus::NeedsLogin => match row.login {
                Login::Browser => "Iniciar sesión",
                Login::Key => "Poner la clave",
                Login::None => "Conectar",
            },
            RowButton::Connect => "Conectar",
            RowButton::Disconnect => "Desconectar",
            RowButton::Remove => "Quitar",
        }
    }
}

/// What a plugin row shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowView {
    /// «Conectado · 3 herramientas», «Pide iniciar sesión», «Error: …».
    pub status: String,
    pub tone: Tone,
    /// Its on/off switch.
    pub on: bool,
    pub buttons: Vec<RowButton>,
}

/// The longest reason shown on a row; the rest is in the logs.
const SHORT: usize = 90;

/// A reason cut to one short line.
pub fn short(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() <= SHORT {
        return line.to_owned();
    }
    let cut: String = line.chars().take(SHORT - 1).collect();
    format!("{}…", cut.trim_end())
}

fn with_detail(head: &str, detail: Option<&str>) -> String {
    match detail.map(short).filter(|d| !d.is_empty()) {
        Some(detail) => format!("{head}: {detail}"),
        None => head.to_owned(),
    }
}

pub fn row_view(row: &PluginRow) -> RowView {
    let detail = row.detail.as_deref();
    let (mut status, mut tone) = match row.status {
        PluginStatus::Connected => match &row.check {
            Some(Err(why)) => (
                format!("Conectado, pero no contestó: {}", short(why)),
                Tone::Warn,
            ),
            _ => {
                let mut text = "Conectado".to_owned();
                match row.tools {
                    0 => {}
                    1 => text.push_str(" · 1 herramienta"),
                    n => text.push_str(&format!(" · {n} herramientas")),
                }
                if row.check == Some(Ok(())) {
                    text.push_str(" · anda bien");
                }
                (text, Tone::Good)
            }
        },
        PluginStatus::Connecting => (with_detail("Conectando", detail), Tone::Busy),
        PluginStatus::NeedsLogin => {
            let head = match row.login {
                Login::Browser => "Pide iniciar sesión",
                Login::Key => "Falta la clave",
                Login::None => "Desconectado",
            };
            (with_detail(head, detail), Tone::Warn)
        }
        PluginStatus::Off if !row.installed => (with_detail("Apagado", detail), Tone::Off),
        PluginStatus::Off => ("Apagado".to_owned(), Tone::Off),
        PluginStatus::Error => (with_detail("Error", detail), Tone::Bad),
    };
    if row.checking {
        status.push_str(" · revisando…");
        if tone == Tone::Good {
            tone = Tone::Busy;
        }
    }
    let mut buttons = Vec::new();
    if row.enabled && matches!(row.status, PluginStatus::NeedsLogin | PluginStatus::Error) {
        buttons.push(RowButton::Connect);
    }
    if row.installed
        && row.login != Login::None
        && matches!(
            row.status,
            PluginStatus::Connected | PluginStatus::Connecting | PluginStatus::Error
        )
    {
        buttons.push(RowButton::Disconnect);
    }
    if !row.own && row.installed {
        buttons.push(RowButton::Remove);
    }
    RowView {
        status,
        tone,
        on: row.enabled,
        buttons,
    }
}

/// Wawi's own first, then the connectors, each by title.
pub fn sorted(mut rows: Vec<PluginRow>) -> Vec<PluginRow> {
    rows.sort_by(|a, b| {
        b.own
            .cmp(&a.own)
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
    rows
}

/// It needs the owner to sign in again.
fn needs_login(row: &PluginRow) -> bool {
    row.enabled
        && (row.status == PluginStatus::NeedsLogin
            || matches!(&row.check, Some(Err(why)) if why.starts_with("pide iniciar sesión")))
}

fn has_trouble(row: &PluginRow) -> bool {
    row.enabled
        && !needs_login(row)
        && (row.status == PluginStatus::Error || matches!(row.check, Some(Err(_))))
}

fn names(titles: &[&str]) -> String {
    match titles {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} y {last}", rest.join(", ")),
    }
}

/// The line under «Revisar»: what the health check found.
pub fn health_summary(rows: &[PluginRow]) -> String {
    if rows.iter().any(|r| r.checking) {
        return "Revisando los plugins…".into();
    }
    let login: Vec<&str> = rows
        .iter()
        .filter(|r| needs_login(r))
        .map(|r| r.title.as_str())
        .collect();
    let trouble: Vec<&str> = rows
        .iter()
        .filter(|r| has_trouble(r))
        .map(|r| r.title.as_str())
        .collect();
    let mut parts = Vec::new();
    match login.len() {
        0 => {}
        1 => parts.push(format!("{} pide iniciar sesión de nuevo.", login[0])),
        _ => parts.push(format!("{} piden iniciar sesión de nuevo.", names(&login))),
    }
    match trouble.len() {
        0 => {}
        1 => parts.push(format!("{} tiene un problema.", trouble[0])),
        _ => parts.push(format!("{} tienen problemas.", names(&trouble))),
    }
    if !parts.is_empty() {
        return parts.join(" ");
    }
    let running = rows.iter().filter(|r| r.enabled).count();
    let checked = rows.iter().any(|r| r.check.is_some());
    match (running, checked) {
        (0, _) => "No hay plugins prendidos.".into(),
        (_, true) => "Todo en orden.".into(),
        (_, false) => String::new(),
    }
}

/// «Agregar por link»: the URL to add, or what to tell the owner.
pub fn link_to_add(text: &str) -> Result<String, &'static str> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Pegá el link del servidor MCP.");
    }
    if text.chars().any(char::is_whitespace) {
        return Err("El link no puede tener espacios.");
    }
    let rest = if let Some(rest) = text.strip_prefix("https://") {
        rest
    } else if let Some(rest) = text.strip_prefix("http://") {
        let host = rest.split(['/', ':']).next().unwrap_or("");
        if !matches!(host, "localhost" | "127.0.0.1") {
            return Err("Tiene que empezar con https://.");
        }
        rest
    } else {
        return Err("Tiene que empezar con https://.");
    };
    let host = rest.split(['/', ':', '?', '#']).next().unwrap_or("");
    if host.is_empty() || !(host.contains('.') || host == "localhost") {
        return Err("Ese link no tiene un servidor.");
    }
    Ok(text.to_owned())
}

// -- the read-only sections ---------------------------------------------------------

/// `1 h`, `10 min`, `1 h 30 min`, `45 s`.
pub fn duration(ms: u64) -> String {
    let secs = ms / 1000;
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    match (h, m) {
        (0, 0) => format!("{s} s"),
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

fn clock(minute: u16) -> String {
    format!("{}:{:02}", minute / 60, minute % 60)
}

/// «Instinto»: the gate's limits as the owner reads them.
pub fn instinct_rows(gate: &GateConfig) -> Vec<InfoRow> {
    let quiet = if gate.quiet_from == gate.quiet_until {
        "ninguna".to_owned()
    } else {
        format!(
            "de {} a {}",
            clock(gate.quiet_from),
            clock(gate.quiet_until)
        )
    };
    let budget = match gate.hourly_budget {
        1 => "1 aviso".to_owned(),
        n => format!("{n} avisos"),
    };
    vec![
        InfoRow::new("Horas de silencio", quiet),
        InfoRow::new("Tope por hora", budget),
        InfoRow::new("Espacio mínimo entre dos", duration(gate.min_gap_ms)),
        InfoRow::new(
            "Espera para el mismo tema",
            duration(gate.default_cooldown_ms),
        ),
        InfoRow::new("No repite lo mismo en", duration(gate.dedup_window_ms)),
    ]
}

// -- the pet section ------------------------------------------------------------------

/// What the «Mascota» section shows, from the body's state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PetPanel {
    /// The installed pets' names, in menu order.
    pub pets: Vec<String>,
    /// The one on screen.
    pub active: Option<usize>,
    /// The active pet's name.
    pub name: String,
    /// Its pack has a `[cuerpo]`.
    pub has_body: bool,
    pub moving: bool,
    pub visible: bool,
    /// Shown with `--mascota`: the choice is not saved.
    pub preview: bool,
}

impl PetPanel {
    /// The note under «Moverse sola».
    pub fn movement_note(&self) -> String {
        if self.has_body {
            "Cae, camina, trepa ventanas y baja en paracaídas.".into()
        } else {
            format!("{} no tiene cuerpo: se queda donde la dejás.", self.name)
        }
    }

    pub fn note(&self) -> &'static str {
        if self.preview {
            "Vista previa: lo que elijas acá no se guarda."
        } else {
            "Se cambia al toque y se guarda. Las mascotas nuevas van en ~/.wawi2/mascotas."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, status: PluginStatus) -> PluginRow {
        PluginRow {
            name: name.into(),
            title: name[..1].to_uppercase() + &name[1..],
            description: String::new(),
            own: false,
            installed: true,
            enabled: status != PluginStatus::Off,
            status,
            login: Login::Browser,
            detail: None,
            tools: 0,
            checking: false,
            check: None,
        }
    }

    #[test]
    fn every_section_has_a_title_and_a_symbol() {
        for section in Section::ALL {
            assert!(!section.title().is_empty());
            assert!(!section.symbol().is_empty());
        }
    }

    #[test]
    fn a_connected_connector_counts_its_tools_and_can_be_signed_out() {
        let mut linear = row("linear", PluginStatus::Connected);
        linear.tools = 3;
        let view = row_view(&linear);
        assert_eq!(view.status, "Conectado · 3 herramientas");
        assert_eq!(view.tone, Tone::Good);
        assert!(view.on);
        assert_eq!(view.buttons, [RowButton::Disconnect, RowButton::Remove]);
    }

    #[test]
    fn signed_out_says_how_to_sign_in() {
        let mut linear = row("linear", PluginStatus::NeedsLogin);
        linear.detail = Some("todavía no le diste permiso".into());
        let view = row_view(&linear);
        assert_eq!(
            view.status,
            "Pide iniciar sesión: todavía no le diste permiso"
        );
        assert_eq!(view.tone, Tone::Warn);
        assert_eq!(view.buttons, [RowButton::Connect, RowButton::Remove]);
        assert_eq!(RowButton::Connect.title(&linear), "Iniciar sesión");
        linear.login = Login::Key;
        assert_eq!(
            row_view(&linear).status.split(':').next(),
            Some("Falta la clave")
        );
        assert_eq!(RowButton::Connect.title(&linear), "Poner la clave");
    }

    #[test]
    fn wawis_own_are_never_removed_and_have_nothing_to_sign_out() {
        let mut instinto = row("instinto", PluginStatus::Connected);
        instinto.own = true;
        instinto.login = Login::None;
        assert!(row_view(&instinto).buttons.is_empty());
        let mut off = row("curiosidad", PluginStatus::Off);
        off.own = true;
        off.installed = false;
        off.detail = Some("de Wawi; prendelo para usarlo".into());
        let view = row_view(&off);
        assert_eq!(view.status, "Apagado: de Wawi; prendelo para usarlo");
        assert_eq!((view.on, view.tone), (false, Tone::Off));
        assert!(view.buttons.is_empty());
    }

    #[test]
    fn an_error_shows_a_short_reason_and_can_be_retried() {
        let mut broken = row("clima", PluginStatus::Error);
        broken.login = Login::None;
        broken.detail = Some(format!("{}\nsegunda línea", "x".repeat(200)));
        let view = row_view(&broken);
        assert!(view.status.starts_with("Error: xxx"));
        assert!(view.status.ends_with('…'));
        assert!(!view.status.contains("segunda"));
        assert!(view.status.chars().count() <= "Error: ".len() + SHORT);
        assert_eq!(view.buttons, [RowButton::Connect, RowButton::Remove]);
    }

    #[test]
    fn a_check_shows_while_it_runs_and_what_it_found() {
        let mut linear = row("linear", PluginStatus::Connected);
        linear.checking = true;
        let view = row_view(&linear);
        assert_eq!(view.status, "Conectado · revisando…");
        assert_eq!(view.tone, Tone::Busy);
        linear.checking = false;
        linear.check = Some(Ok(()));
        assert_eq!(row_view(&linear).status, "Conectado · anda bien");
        linear.check = Some(Err("no contestó en 6 s".into()));
        let view = row_view(&linear);
        assert_eq!(
            view.status,
            "Conectado, pero no contestó: no contestó en 6 s"
        );
        assert_eq!(view.tone, Tone::Warn);
    }

    #[test]
    fn the_summary_names_who_needs_signing_in_again() {
        let mut rows = vec![
            row("linear", PluginStatus::NeedsLogin),
            row("notion", PluginStatus::Connected),
            row("clima", PluginStatus::Off),
        ];
        assert_eq!(
            health_summary(&rows),
            "Linear pide iniciar sesión de nuevo."
        );
        rows[1].check = Some(Err(
            "pide iniciar sesión: el servidor revocó el permiso".into()
        ));
        assert_eq!(
            health_summary(&rows),
            "Linear y Notion piden iniciar sesión de nuevo."
        );
        rows[1].check = Some(Err("no contestó en 6 s".into()));
        assert_eq!(
            health_summary(&rows),
            "Linear pide iniciar sesión de nuevo. Notion tiene un problema."
        );
        rows[0].checking = true;
        assert_eq!(health_summary(&rows), "Revisando los plugins…");
    }

    #[test]
    fn the_summary_is_quiet_until_something_was_checked() {
        let mut rows = vec![row("linear", PluginStatus::Connected)];
        assert_eq!(health_summary(&rows), "");
        rows[0].check = Some(Ok(()));
        assert_eq!(health_summary(&rows), "Todo en orden.");
        assert_eq!(
            health_summary(&[row("clima", PluginStatus::Off)]),
            "No hay plugins prendidos."
        );
    }

    #[test]
    fn own_plugins_come_first_then_by_title() {
        let mut instinto = row("instinto", PluginStatus::Off);
        instinto.own = true;
        let rows = sorted(vec![
            row("zeta", PluginStatus::Connected),
            instinto,
            row("alfa", PluginStatus::Connected),
        ]);
        let order: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(order, ["instinto", "alfa", "zeta"]);
    }

    #[test]
    fn only_a_real_server_link_is_added() {
        assert_eq!(
            link_to_add("  https://mcp.linear.app/mcp \n"),
            Ok("https://mcp.linear.app/mcp".into())
        );
        assert_eq!(
            link_to_add("http://127.0.0.1:8080/mcp"),
            Ok("http://127.0.0.1:8080/mcp".into())
        );
        assert!(link_to_add("").is_err());
        assert!(link_to_add("linear").is_err());
        assert!(link_to_add("http://mcp.linear.app/mcp").is_err());
        assert!(link_to_add("https://").is_err());
        assert!(link_to_add("https://mcp linear.app").is_err());
        assert!(link_to_add("ftp://x.com").is_err());
    }

    #[test]
    fn the_instinct_limits_read_like_the_docs() {
        let rows = instinct_rows(&GateConfig::default());
        let text: Vec<String> = rows
            .iter()
            .map(|r| format!("{}: {}", r.label, r.value))
            .collect();
        assert_eq!(
            text,
            [
                "Horas de silencio: de 23:00 a 8:00",
                "Tope por hora: 3 avisos",
                "Espacio mínimo entre dos: 10 min",
                "Espera para el mismo tema: 1 h",
                "No repite lo mismo en: 12 h",
            ]
        );
        let none = GateConfig {
            quiet_from: 0,
            quiet_until: 0,
            hourly_budget: 1,
            ..GateConfig::default()
        };
        let rows = instinct_rows(&none);
        assert_eq!(rows[0].value, "ninguna");
        assert_eq!(rows[1].value, "1 aviso");
    }

    #[test]
    fn durations_read_short() {
        assert_eq!(duration(45_000), "45 s");
        assert_eq!(duration(90 * 60_000), "1 h 30 min");
        assert_eq!(duration(2 * 3_600_000), "2 h");
    }

    #[test]
    fn a_pet_without_a_body_explains_why_it_cannot_move() {
        let mut panel = PetPanel {
            pets: vec!["Wawi".into()],
            active: Some(0),
            name: "Kitty".into(),
            has_body: false,
            moving: true,
            visible: true,
            preview: false,
        };
        assert!(panel.movement_note().starts_with("Kitty no tiene cuerpo"));
        panel.has_body = true;
        assert!(panel.movement_note().starts_with("Cae, camina"));
        assert!(panel.note().contains("se guarda"));
        panel.preview = true;
        assert!(panel.note().contains("no se guarda"));
    }

    #[test]
    fn permissions_say_plainly_what_is_missing() {
        assert_eq!(Grant::Granted.label(), "Permitido");
        assert_eq!(Grant::Missing.tone(), Tone::Bad);
        assert_eq!(Grant::NotAsked.tone(), Tone::Warn);
    }

    #[test]
    fn a_source_compares_by_identity_and_never_prints_its_insides() {
        let source = SettingsSource {
            voice: vec![InfoRow::new("Voz", "jf_alpha")],
            ..SettingsSource::default()
        };
        assert_eq!(source.clone(), source);
        assert_ne!(source, SettingsSource::default());
        assert!(format!("{source:?}").contains("jf_alpha"));
    }
}
