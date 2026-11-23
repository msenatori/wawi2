//! The menu: the same one in the menu bar and on a right click on her.
//! Titles name the action, not the state (Wawi 1: "Silenciado" read like a
//! label and nobody knew clicking it would turn her ears back on).

use crate::Command;
use crate::mood::Mood;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuItem {
    /// «Escuchar» / «Dejar de escuchar»: the mic gate.
    Listen,
    /// «Mostrar a Wawi» / «Ocultar a Wawi» (the active pet's `nombre`).
    ShowHide,
    /// «Mascota» ▸ the installed pet packs; a pick switches live.
    Pets,
    /// «Quedate quieta» / «Podés moverte»: gravity and wandering, for a pet
    /// with a body.
    Still,
    /// «Abrir registros»: Finder at `~/.wawi2/logs`.
    OpenLogs,
    /// «Salir»: the same clean exit as Enter or Ctrl+C.
    Quit,
}

/// The rows, top to bottom; `None` is a separator.
pub const LAYOUT: &[Option<MenuItem>] = &[
    Some(MenuItem::Listen),
    Some(MenuItem::ShowHide),
    Some(MenuItem::Still),
    Some(MenuItem::Pets),
    None,
    Some(MenuItem::OpenLogs),
    None,
    Some(MenuItem::Quit),
];

/// What the menu needs to know to name its rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MenuState<'a> {
    pub listening: bool,
    pub visible: bool,
    /// The active pet's name.
    pub name: &'a str,
    /// Her body moves (gravity, wandering).
    pub moving: bool,
}

/// The «Mascota» submenu's title.
pub const PETS_TITLE: &str = "Mascota";
/// Tags of the pet rows: `PET_TAG_BASE + index` in the listed packs.
pub const PET_TAG_BASE: isize = 1000;

pub fn pet_tag(index: usize) -> isize {
    PET_TAG_BASE + index as isize
}

pub fn pet_from_tag(tag: isize) -> Option<usize> {
    (tag >= PET_TAG_BASE).then(|| (tag - PET_TAG_BASE) as usize)
}

/// What a pick does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// For the program (the host) to carry out.
    Host(Command),
    /// The body handles these itself.
    ToggleVisible,
    ToggleMovement,
    OpenLogs,
    /// A submenu: it opens, it does nothing itself.
    Submenu,
}

impl MenuItem {
    pub fn title(self, state: MenuState) -> String {
        match self {
            MenuItem::Listen if state.listening => "Dejar de escuchar".into(),
            MenuItem::Listen => "Escuchar".into(),
            MenuItem::ShowHide if state.visible => format!("Ocultar a {}", state.name),
            MenuItem::ShowHide => format!("Mostrar a {}", state.name),
            MenuItem::Still if state.moving => "Quedate quieta".into(),
            MenuItem::Still => "Podés moverte".into(),
            MenuItem::Pets => PETS_TITLE.into(),
            MenuItem::OpenLogs => "Abrir registros".into(),
            MenuItem::Quit => "Salir".into(),
        }
    }

    pub fn action(self, state: MenuState) -> Action {
        match self {
            MenuItem::Listen => Action::Host(Command::SetListening(!state.listening)),
            MenuItem::ShowHide => Action::ToggleVisible,
            MenuItem::Still => Action::ToggleMovement,
            MenuItem::Pets => Action::Submenu,
            MenuItem::OpenLogs => Action::OpenLogs,
            MenuItem::Quit => Action::Host(Command::Quit),
        }
    }

    /// The tag the native menu item carries (never 0, AppKit's default).
    pub fn tag(self) -> isize {
        match self {
            MenuItem::Listen => 1,
            MenuItem::ShowHide => 2,
            MenuItem::OpenLogs => 3,
            MenuItem::Quit => 4,
            MenuItem::Pets => 5,
            MenuItem::Still => 6,
        }
    }

    pub fn from_tag(tag: isize) -> Option<MenuItem> {
        LAYOUT
            .iter()
            .flatten()
            .copied()
            .find(|item| item.tag() == tag)
    }
}

/// The menu bar glyph beside her face (Wawi 1's): the mic gate first,
/// because a muted Wawi looks exactly like an idle one.
pub fn status_glyph(mood: Mood, muted: bool) -> &'static str {
    if muted {
        return "◌";
    }
    match mood {
        Mood::Idle => "●",
        Mood::Listening => "◉",
        Mood::Transcribing | Mood::Thinking => "◍",
        Mood::Speaking => "◎",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ON: MenuState = MenuState {
        listening: true,
        visible: true,
        name: "Wawi",
        moving: true,
    };
    const OFF: MenuState = MenuState {
        listening: false,
        visible: false,
        name: "Wawi",
        moving: false,
    };

    #[test]
    fn quedate_quieta_toggles_her_body() {
        assert_eq!(MenuItem::Still.title(ON), "Quedate quieta");
        assert_eq!(MenuItem::Still.title(OFF), "Podés moverte");
        assert_eq!(MenuItem::Still.action(ON), Action::ToggleMovement);
    }

    #[test]
    fn listen_toggles_the_mic_gate() {
        assert_eq!(MenuItem::Listen.title(ON), "Dejar de escuchar");
        assert_eq!(
            MenuItem::Listen.action(ON),
            Action::Host(Command::SetListening(false))
        );
        assert_eq!(MenuItem::Listen.title(OFF), "Escuchar");
        assert_eq!(
            MenuItem::Listen.action(OFF),
            Action::Host(Command::SetListening(true))
        );
    }

    #[test]
    fn quit_goes_to_the_host_and_the_rest_stay_in_the_body() {
        assert_eq!(MenuItem::Quit.action(ON), Action::Host(Command::Quit));
        assert_eq!(MenuItem::Quit.title(ON), "Salir");
        assert_eq!(MenuItem::ShowHide.action(ON), Action::ToggleVisible);
        assert_eq!(MenuItem::ShowHide.title(ON), "Ocultar a Wawi");
        assert_eq!(MenuItem::ShowHide.title(OFF), "Mostrar a Wawi");
        assert_eq!(MenuItem::OpenLogs.action(OFF), Action::OpenLogs);
    }

    #[test]
    fn the_menu_names_the_active_pet_and_lists_the_packs() {
        let kitty = MenuState {
            name: "Kitty",
            ..ON
        };
        assert_eq!(MenuItem::ShowHide.title(kitty), "Ocultar a Kitty");
        assert_eq!(MenuItem::Pets.title(kitty), "Mascota");
        assert_eq!(MenuItem::Pets.action(kitty), Action::Submenu);
        assert_eq!(pet_from_tag(pet_tag(3)), Some(3));
        assert_eq!(pet_from_tag(MenuItem::Quit.tag()), None);
        assert_eq!(MenuItem::from_tag(pet_tag(0)), None);
    }

    #[test]
    fn tags_round_trip_and_are_never_zero() {
        for item in LAYOUT.iter().flatten() {
            assert_ne!(item.tag(), 0);
            assert_eq!(MenuItem::from_tag(item.tag()), Some(*item));
        }
        assert_eq!(MenuItem::from_tag(0), None);
        assert_eq!(LAYOUT.last(), Some(&Some(MenuItem::Quit)));
    }

    #[test]
    fn the_glyph_says_muted_first() {
        assert_eq!(status_glyph(Mood::Speaking, true), "◌");
        assert_eq!(status_glyph(Mood::Listening, false), "◉");
        assert_eq!(status_glyph(Mood::Idle, false), "●");
    }
}
