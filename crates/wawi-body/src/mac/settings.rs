//! The «Ajustes» window, native AppKit: a toolbar of sections like System
//! Settings (an `NSTabViewController` in toolbar style), each one a stack of
//! standard controls. What each row says comes from [`crate::settings`];
//! this file only lays it out and forwards clicks.
//!
//! Speed: the window is built the first time it opens and dropped (every
//! view, the target, the toolbar) when it closes. There are no timers.
//! Plugin changes are posted to the main queue only while it is open
//! ([`OPEN`]); closed, a change costs one atomic load on the host thread.
//! Health checks run on each plugin's own thread, never here.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSAutoresizingMaskOptions, NSBox, NSBoxType,
    NSButton, NSColor, NSControlSize, NSControlStateValueOff, NSControlStateValueOn, NSFont,
    NSImage, NSLayoutAttribute, NSLayoutConstraintOrientation, NSMenu, NSMenuItem, NSPopUpButton,
    NSScrollView, NSStackView, NSSwitch, NSTabViewController, NSTabViewControllerTabStyle,
    NSTabViewItem, NSTextField, NSUserInterfaceLayoutOrientation, NSView, NSViewController,
    NSWindow, NSWindowDelegate, NSWindowStyleMask, NSWindowToolbarStyle, NSWorkspace,
};
use objc2_foundation::{
    NSArray, NSEdgeInsets, NSNotification, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
    NSURL,
};

use super::with_ui;
use crate::settings::{
    Grant, InfoRow, Permission, PetPanel, PluginRow, RowButton, Section, SettingsSource,
    SuggestionRow, Tone, health_summary, link_to_add, row_view, sorted,
};

/// Every section is this size, so the window does not jump between them.
const WIDTH: f64 = 640.0;
const HEIGHT: f64 = 500.0;
const INSET: f64 = 24.0;
/// Wrapping text inside a plugin row.
const TEXT_W: f64 = 470.0;

thread_local! {
    static WINDOW: RefCell<Option<Settings>> = const { RefCell::new(None) };
}

/// True while the window is open: plugin changes are posted only then.
static OPEN: AtomicBool = AtomicBool::new(false);

/// Subscribes the window to the plugins' changes, once per source. The
/// callback runs on the host thread: one atomic load while closed.
pub(crate) fn subscribe(source: &SettingsSource) {
    if let Some(plugins) = &source.plugins {
        plugins.subscribe(Box::new(|| {
            if OPEN.load(Ordering::Acquire) {
                DispatchQueue::main().exec_async(|| {
                    with_settings(|s| s.refresh_plugins());
                });
            }
        }));
    }
}

/// Opens the window (building it if it is not open), on the pet panel's
/// section if asked. Called from the UI, with the body's pet state.
pub(crate) fn show(mtm: MainThreadMarker, source: &SettingsSource, pets: PetPanel) {
    let shown = with_settings(|s| {
        s.pets_changed(&pets);
        s.front();
    });
    if shown.is_some() {
        return;
    }
    let settings = Settings::new(mtm, source.clone(), &pets);
    WINDOW.with(|cell| *cell.borrow_mut() = Some(settings));
    OPEN.store(true, Ordering::Release);
    with_settings(|s| {
        s.refresh_plugins();
        s.refresh_permissions();
        s.front();
        // A health check every time it opens (joined if one is running).
        if let Some(plugins) = &s.source.plugins {
            plugins.check_all();
        }
    });
    // `WAWI2_AJUSTES_FOTO=DIR`: a picture of every section, drawn off
    // screen (works without the screen recording permission).
    if let Some(dir) = std::env::var_os("WAWI2_AJUSTES_FOTO") {
        let dir = std::path::PathBuf::from(dir);
        DispatchQueue::main().exec_async(move || {
            with_settings(|s| s.photos(&dir));
        });
    }
}

/// The body's pet changed (from the menu or from here): shows it, if open.
pub(crate) fn pets_changed(pets: &PetPanel) {
    with_settings(|s| s.pets_changed(pets));
}

/// Closes and drops the window now (the program is ending).
pub(crate) fn close_now() {
    OPEN.store(false, Ordering::Release);
    if let Some(settings) =
        WINDOW.with(|cell| cell.try_borrow_mut().ok().and_then(|mut s| s.take()))
    {
        settings.window.setDelegate(None);
        settings.window.close();
    }
}

/// The window closed: everything goes, after AppKit is done closing it.
fn closed() {
    OPEN.store(false, Ordering::Release);
    DispatchQueue::main().exec_async(|| {
        let settings = WINDOW.with(|cell| cell.try_borrow_mut().ok().and_then(|mut s| s.take()));
        if let Some(settings) = settings {
            settings.window.setDelegate(None);
            drop(settings);
        }
    });
}

fn with_settings<R>(f: impl FnOnce(&mut Settings) -> R) -> Option<R> {
    WINDOW.with(|cell| {
        let mut guard = cell.try_borrow_mut().ok()?;
        guard.as_mut().map(f)
    })
}

/// Reads a value out of the open window, releasing it before acting (an
/// action may call back into the window or into the body's UI).
fn read<R>(f: impl FnOnce(&Settings) -> Option<R>) -> Option<R> {
    WINDOW.with(|cell| cell.try_borrow().ok().and_then(|s| s.as_ref().and_then(f)))
}

define_class!(
    /// Every control's target and the window's delegate.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "WawiSettingsTarget"]
    struct Target;

    unsafe impl NSObjectProtocol for Target {}

    unsafe impl NSWindowDelegate for Target {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            closed();
        }
    }

    impl Target {
        #[unsafe(method(toggle:))]
        fn toggle(&self, sender: &NSSwitch) {
            let on = sender.state() == NSControlStateValueOn;
            let Some((plugins, name)) = read(|s| {
                let row = s.rows.get(sender.tag() as usize)?;
                Some((s.source.plugins.clone()?, row.name.clone()))
            }) else {
                return;
            };
            plugins.set_enabled(&name, on);
        }

        #[unsafe(method(rowConnect:))]
        fn row_connect(&self, sender: &NSButton) {
            if let Some((plugins, name)) = self.row_of(sender) {
                plugins.connect(&name);
            }
        }

        #[unsafe(method(rowDisconnect:))]
        fn row_disconnect(&self, sender: &NSButton) {
            if let Some((plugins, name)) = self.row_of(sender) {
                plugins.disconnect(&name);
            }
        }

        #[unsafe(method(rowRemove:))]
        fn row_remove(&self, sender: &NSButton) {
            let Some((plugins, name)) = self.row_of(sender) else {
                return;
            };
            let title = read(|s| s.rows.iter().find(|r| r.name == name).map(|r| r.title.clone()))
                .unwrap_or_else(|| name.clone());
            if confirm_remove(self.mtm(), &title) {
                plugins.remove(&name);
            }
        }

        #[unsafe(method(suggest:))]
        fn suggest(&self, sender: &NSButton) {
            let Some((plugins, name)) = read(|s| {
                let suggestion = s.suggestions.get(sender.tag() as usize)?;
                Some((s.source.plugins.clone()?, suggestion.name.clone()))
            }) else {
                return;
            };
            plugins.connect(&name);
        }

        #[unsafe(method(addLink:))]
        fn add_link(&self, _sender: &AnyObject) {
            with_settings(|s| s.add_link());
        }

        #[unsafe(method(check:))]
        fn check(&self, _sender: &AnyObject) {
            if let Some(plugins) = read(|s| s.source.plugins.clone()) {
                plugins.check_all();
            }
        }

        #[unsafe(method(pickPet:))]
        fn pick_pet(&self, sender: &NSPopUpButton) {
            let index = sender.indexOfSelectedItem();
            if index >= 0 {
                with_ui(|ui| ui.settings_pick_pet(index as usize));
            }
        }

        #[unsafe(method(moving:))]
        fn moving(&self, sender: &NSSwitch) {
            let on = sender.state() == NSControlStateValueOn;
            with_ui(|ui| ui.settings_set_moving(on));
        }

        #[unsafe(method(visible:))]
        fn visible(&self, sender: &NSSwitch) {
            let on = sender.state() == NSControlStateValueOn;
            with_ui(|ui| ui.settings_set_visible(on));
        }

        #[unsafe(method(openPets:))]
        fn open_pets(&self, _sender: &AnyObject) {
            with_ui(|ui| ui.open_pets_folder());
        }

        #[unsafe(method(openPane:))]
        fn open_pane(&self, sender: &NSButton) {
            if let Some(pane) = read(|s| s.permissions.get(sender.tag() as usize).map(|p| p.pane.clone())) {
                open_url(&pane);
            }
        }

        #[unsafe(method(recheckPermissions:))]
        fn recheck_permissions(&self, _sender: &AnyObject) {
            with_settings(|s| s.refresh_permissions());
        }
    }
);

impl Target {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }

    fn row_of(
        &self,
        sender: &NSButton,
    ) -> Option<(std::sync::Arc<dyn crate::settings::PluginsPanel>, String)> {
        read(|s| {
            let row = s.rows.get(sender.tag() as usize)?;
            Some((s.source.plugins.clone()?, row.name.clone()))
        })
    }
}

define_class!(
    /// A flipped document view: the scrolled content starts at the top.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "WawiSettingsFlipped"]
    struct Flipped;

    impl Flipped {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

impl Flipped {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] }
    }
}

struct Settings {
    mtm: MainThreadMarker,
    window: Retained<NSWindow>,
    tabs: Retained<NSTabViewController>,
    target: Retained<Target>,
    source: SettingsSource,
    // Plugins.
    installed: Retained<NSStackView>,
    suggested: Retained<NSStackView>,
    suggested_title: Retained<NSTextField>,
    summary: Retained<NSTextField>,
    link: Retained<NSTextField>,
    link_note: Retained<NSTextField>,
    rows: Vec<PluginRow>,
    suggestions: Vec<SuggestionRow>,
    // Mascota.
    pet_popup: Retained<NSPopUpButton>,
    pet_names: Vec<String>,
    moving: Retained<NSSwitch>,
    moving_note: Retained<NSTextField>,
    visible: Retained<NSSwitch>,
    pet_note: Retained<NSTextField>,
    // Privacidad.
    permission_rows: Retained<NSStackView>,
    permissions: Vec<Permission>,
}

impl Settings {
    fn new(mtm: MainThreadMarker, source: SettingsSource, pets: &PetPanel) -> Self {
        install_edit_menu(mtm);
        let target = Target::new(mtm);
        let tabs = NSTabViewController::new(mtm);
        tabs.setTabStyle(NSTabViewControllerTabStyle::Toolbar);

        // Plugins.
        let summary = note(mtm, "", WIDTH - 2.0 * INSET);
        let installed = stack(mtm, &[], true, 14.0);
        let suggested = stack(mtm, &[], true, 10.0);
        let suggested_title = heading(mtm, "Sugeridos");
        let link = NSTextField::textFieldWithString(&NSString::from_str(""), mtm);
        link.setPlaceholderString(Some(&NSString::from_str("https://servidor.ejemplo/mcp")));
        link.setContentHuggingPriority_forOrientation(
            1.0,
            NSLayoutConstraintOrientation::Horizontal,
        );
        link.widthAnchor()
            .constraintGreaterThanOrEqualToConstant(320.0)
            .setActive(true);
        // SAFETY: the target outlives the window (both live in `Settings`).
        unsafe {
            link.setTarget(Some(&target as &AnyObject));
            link.setAction(Some(sel!(addLink:)));
        }
        let link_note = note(mtm, "", WIDTH - 2.0 * INSET);
        let plugins_view = plugins_section(
            mtm,
            &target,
            &source,
            PluginParts {
                summary: &summary,
                installed: &installed,
                suggested: &suggested,
                suggested_title: &suggested_title,
                link: &link,
                link_note: &link_note,
            },
        );

        // Mascota.
        let pet_popup = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(mtm),
            NSRect::new(NSPoint::ZERO, NSSize::new(200.0, 26.0)),
            false,
        );
        unsafe {
            pet_popup.setTarget(Some(&target as &AnyObject));
            pet_popup.setAction(Some(sel!(pickPet:)));
        }
        let moving = switch(mtm, false, &target, sel!(moving:), 0);
        let moving_note = note(mtm, "", TEXT_W);
        let visible = switch(mtm, true, &target, sel!(visible:), 0);
        let pet_note = note(mtm, "", WIDTH - 2.0 * INSET);
        let pet_view = pet_section(
            mtm,
            &target,
            &pet_popup,
            (&moving, &moving_note),
            &visible,
            &pet_note,
        );

        // Voz, Instinto.
        let voice_view = facts_section(
            mtm,
            "Voz",
            "Qué la hace oír, pensar y hablar.",
            &source.voice,
            "Solo para mirar: por ahora no se cambia desde acá, y cambiarlo pide reiniciar Wawi.",
        );
        let instinct_view = facts_section(
            mtm,
            "Instinto",
            "Cuándo Wawi te puede hablar sola. Lo urgente se saltea el tope y el espacio mínimo, pero no las horas de silencio.",
            &source.instinct,
            "Solo para mirar: por ahora estos límites no se cambian desde acá.",
        );

        // Privacidad.
        let permission_rows = stack(mtm, &[], true, 14.0);
        let privacy_view = privacy_section(mtm, &target, &source, &permission_rows);

        for (section, view) in Section::ALL.into_iter().zip([
            plugins_view,
            pet_view,
            voice_view,
            instinct_view,
            privacy_view,
        ]) {
            let controller = NSViewController::new(mtm);
            controller.setView(&view);
            controller.setTitle(Some(&NSString::from_str(section.title())));
            let item = NSTabViewItem::tabViewItemWithViewController(&controller);
            item.setLabel(&NSString::from_str(section.title()));
            item.setImage(
                NSImage::imageWithSystemSymbolName_accessibilityDescription(
                    &NSString::from_str(section.symbol()),
                    Some(&NSString::from_str(section.title())),
                )
                .as_deref(),
            );
            tabs.addTabViewItem(&item);
        }

        let window = NSWindow::windowWithContentViewController(&tabs);
        window.setStyleMask(
            NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::Miniaturizable,
        );
        window.setToolbarStyle(NSWindowToolbarStyle::Preference);
        // SAFETY: `Settings` owns the window; AppKit must not free it too.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setDelegate(Some(ProtocolObject::from_ref(&*target)));
        window.center();

        let mut settings = Self {
            mtm,
            window,
            tabs,
            target,
            source,
            installed,
            suggested,
            suggested_title,
            summary,
            link,
            link_note,
            rows: Vec::new(),
            suggestions: Vec::new(),
            pet_popup,
            pet_names: Vec::new(),
            moving,
            moving_note,
            visible,
            pet_note,
            permission_rows,
            permissions: Vec::new(),
        };
        settings.pets_changed(pets);
        settings
    }

    fn front(&self) {
        // An accessory app: bring the window to the front.
        #[allow(deprecated)]
        NSApplication::sharedApplication(self.mtm).activateIgnoringOtherApps(true);
        self.window.makeKeyAndOrderFront(None);
    }

    /// Saves `ajustes-<n>-<section>.png` for each section, then goes back
    /// to the first one.
    fn photos(&self, dir: &std::path::Path) {
        let _ = std::fs::create_dir_all(dir);
        for (i, section) in Section::ALL.iter().enumerate() {
            self.tabs.setSelectedTabViewItemIndex(i as isize);
            let Some(content) = self.window.contentView() else {
                continue;
            };
            // The frame view: the title bar and the toolbar too.
            let view = unsafe { content.superview() }.unwrap_or(content);
            view.layoutSubtreeIfNeeded();
            view.displayIfNeeded();
            let bounds = view.bounds();
            let Some(rep) = view.bitmapImageRepForCachingDisplayInRect(bounds) else {
                continue;
            };
            view.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
            // SAFETY: an empty properties dictionary is valid for PNG.
            let data = unsafe {
                rep.representationUsingType_properties(
                    objc2_app_kit::NSBitmapImageFileType::PNG,
                    &objc2_foundation::NSDictionary::new(),
                )
            };
            if let Some(data) = data {
                let name = format!("ajustes-{}-{}.png", i + 1, section.title().to_lowercase());
                let _ = std::fs::write(dir.join(name), data.to_vec());
            }
        }
        self.tabs.setSelectedTabViewItemIndex(0);
    }

    // -- plugins ----------------------------------------------------------------

    fn refresh_plugins(&mut self) {
        let Some(plugins) = self.source.plugins.clone() else {
            return;
        };
        self.rows = sorted(plugins.list());
        self.suggestions = plugins.suggested();
        clear(&self.installed);
        for (i, row) in self.rows.iter().enumerate() {
            let view = self.plugin_row(i, row);
            add_full_width(&self.installed, &view);
        }
        if self.rows.is_empty() {
            let empty = note(self.mtm, "Todavía no hay plugins.", TEXT_W);
            self.installed.addArrangedSubview(&empty);
        }
        clear(&self.suggested);
        for (i, suggestion) in self.suggestions.iter().enumerate() {
            let view = self.suggestion_row(i, suggestion);
            add_full_width(&self.suggested, &view);
        }
        self.suggested_title.setHidden(self.suggestions.is_empty());
        self.suggested.setHidden(self.suggestions.is_empty());
        set_text(&self.summary, &health_summary(&self.rows));
    }

    fn plugin_row(&self, i: usize, row: &PluginRow) -> Retained<NSView> {
        let mtm = self.mtm;
        let view = row_view(row);
        let title = bold(mtm, &row.title, 13.0);
        let dot = text(mtm, "●");
        dot.setTextColor(Some(&tone_color(view.tone)));
        dot.setFont(Some(&NSFont::systemFontOfSize(10.0)));
        let status = note(mtm, &view.status, TEXT_W - 20.0);
        status.setTextColor(Some(&NSColor::labelColor()));
        let status_line = stack(mtm, &[&dot, &status], false, 5.0);
        status_line.setAlignment(NSLayoutAttribute::FirstBaseline);
        let mut left: Vec<Retained<NSView>> = vec![view_of(&*title), view_of(&*status_line)];
        if !row.description.trim().is_empty() {
            left.push(view_of(&*note(mtm, row.description.trim(), TEXT_W)));
        }
        if !view.buttons.is_empty() {
            let buttons: Vec<Retained<NSView>> = view
                .buttons
                .iter()
                .map(|b| {
                    let action = match b {
                        RowButton::Connect => sel!(rowConnect:),
                        RowButton::Disconnect => sel!(rowDisconnect:),
                        RowButton::Remove => sel!(rowRemove:),
                    };
                    view_of(&*small_button(mtm, b.title(row), &self.target, action, i))
                })
                .collect();
            left.push(view_of(&*stack_of(mtm, &buttons, false, 8.0)));
        }
        let left = stack_of(mtm, &left, true, 3.0);
        stretch(&left);
        let toggle = switch(mtm, view.on, &self.target, sel!(toggle:), i);
        toggle.setToolTip(Some(&NSString::from_str(if view.on {
            "Apagarlo"
        } else {
            "Prenderlo"
        })));
        let row = stack(mtm, &[&left, &toggle], false, 12.0);
        row.setAlignment(NSLayoutAttribute::Top);
        view_of(&*row)
    }

    fn suggestion_row(&self, i: usize, suggestion: &SuggestionRow) -> Retained<NSView> {
        let mtm = self.mtm;
        let title = bold(mtm, &suggestion.title, 13.0);
        let about = note(mtm, &suggestion.description, TEXT_W);
        let left = stack(mtm, &[&title, &about], true, 2.0);
        stretch(&left);
        let connect = small_button(mtm, "Conectar", &self.target, sel!(suggest:), i);
        let row = stack(mtm, &[&left, &connect], false, 12.0);
        row.setAlignment(NSLayoutAttribute::CenterY);
        view_of(&*row)
    }

    fn add_link(&mut self) {
        let text = self.link.stringValue().to_string();
        match link_to_add(&text) {
            Ok(url) => {
                if let Some(plugins) = &self.source.plugins {
                    plugins.connect(&url);
                    self.link.setStringValue(&NSString::from_str(""));
                    set_text(
                        &self.link_note,
                        "Lo reviso y lo agrego. Si pide permiso, se abre el navegador o la ventanita de la clave.",
                    );
                }
            }
            Err(why) => set_text(&self.link_note, why),
        }
    }

    // -- mascota ----------------------------------------------------------------

    fn pets_changed(&mut self, pets: &PetPanel) {
        if self.pet_names != pets.pets {
            self.pet_popup.removeAllItems();
            for name in &pets.pets {
                self.pet_popup.addItemWithTitle(&NSString::from_str(name));
            }
            self.pet_names = pets.pets.clone();
        }
        if let Some(active) = pets.active {
            self.pet_popup.selectItemAtIndex(active as isize);
        }
        set_state(&self.moving, pets.moving && pets.has_body);
        self.moving.setEnabled(pets.has_body);
        set_text(&self.moving_note, &pets.movement_note());
        set_state(&self.visible, pets.visible);
        set_text(&self.pet_note, pets.note());
    }

    // -- privacidad -------------------------------------------------------------

    fn refresh_permissions(&mut self) {
        let Some(reader) = self.source.permissions.clone() else {
            return;
        };
        self.permissions = reader();
        clear(&self.permission_rows);
        for (i, permission) in self.permissions.iter().enumerate() {
            let mtm = self.mtm;
            let name = bold(mtm, &permission.name, 13.0);
            let why = note(mtm, &permission.why, 360.0);
            let left = stack(mtm, &[&name, &why], true, 2.0);
            stretch(&left);
            let dot = text(mtm, "●");
            dot.setTextColor(Some(&tone_color(permission.grant.tone())));
            dot.setFont(Some(&NSFont::systemFontOfSize(10.0)));
            let state = text(mtm, permission.grant.label());
            let status = stack(mtm, &[&dot, &state], false, 5.0);
            status
                .widthAnchor()
                .constraintEqualToConstant(170.0)
                .setActive(true);
            let open = small_button(
                mtm,
                if permission.grant == Grant::Granted {
                    "Ver"
                } else {
                    "Abrir Ajustes"
                },
                &self.target,
                sel!(openPane:),
                i,
            );
            let row = stack(mtm, &[&left, &status, &open], false, 14.0);
            row.setAlignment(NSLayoutAttribute::CenterY);
            add_full_width(&self.permission_rows, &row);
        }
    }
}

impl Drop for Settings {
    fn drop(&mut self) {
        // Nothing keeps calling into a window that is gone.
        self.window.setDelegate(None);
    }
}

// -- the sections ---------------------------------------------------------------

struct PluginParts<'a> {
    summary: &'a NSTextField,
    installed: &'a NSStackView,
    suggested: &'a NSStackView,
    suggested_title: &'a NSTextField,
    link: &'a NSTextField,
    link_note: &'a NSTextField,
}

fn plugins_section(
    mtm: MainThreadMarker,
    target: &Target,
    source: &SettingsSource,
    parts: PluginParts<'_>,
) -> Retained<NSView> {
    let title = bold(mtm, "Plugins", 15.0);
    title.setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);
    let check = push_button(mtm, "Revisar", target, sel!(check:));
    check.setToolTip(Some(&NSString::from_str(
        "Pregunta a cada plugin prendido si contesta y si su permiso sigue andando.",
    )));
    let header = stack(mtm, &[&title, &check], false, 12.0);
    header.setAlignment(NSLayoutAttribute::CenterY);
    let main = stack(mtm, &[], true, 12.0);
    main.setEdgeInsets(NSEdgeInsets {
        top: 20.0,
        left: INSET,
        bottom: 24.0,
        right: INSET,
    });
    add_full_width(&main, &header);
    if source.plugins.is_none() {
        main.addArrangedSubview(&note(
            mtm,
            "Los plugins se ven acá cuando Wawi está prendida con su voz.",
            WIDTH - 2.0 * INSET,
        ));
        return scrolled(mtm, &main);
    }
    main.addArrangedSubview(parts.summary);
    main.addArrangedSubview(&heading(mtm, "Instalados"));
    add_full_width(&main, parts.installed);
    add_full_width(&main, &separator(mtm));
    main.addArrangedSubview(parts.suggested_title);
    add_full_width(&main, parts.suggested);
    add_full_width(&main, &separator(mtm));
    main.addArrangedSubview(&heading(mtm, "Agregar por link"));
    main.addArrangedSubview(&note(
        mtm,
        "Pegá la dirección de un servidor MCP. Si pide permiso, te lo pregunto.",
        WIDTH - 2.0 * INSET,
    ));
    let add = push_button(mtm, "Agregar", target, sel!(addLink:));
    let link_row = stack(mtm, &[parts.link, &add], false, 8.0);
    link_row.setAlignment(NSLayoutAttribute::CenterY);
    add_full_width(&main, &link_row);
    main.addArrangedSubview(parts.link_note);
    scrolled(mtm, &main)
}

fn pet_section(
    mtm: MainThreadMarker,
    target: &Target,
    popup: &NSPopUpButton,
    (moving, moving_note): (&NSSwitch, &NSTextField),
    visible: &NSSwitch,
    note_field: &NSTextField,
) -> Retained<NSView> {
    let main = top_stack(mtm);
    main.addArrangedSubview(&bold(mtm, "Mascota", 15.0));
    let label = text(mtm, "La que está en pantalla");
    label.setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);
    let pick = stack(mtm, &[&label, popup], false, 12.0);
    pick.setAlignment(NSLayoutAttribute::CenterY);
    add_full_width(&main, &pick);
    add_full_width(&main, &separator(mtm));
    add_full_width(&main, &switch_row(mtm, "Moverse sola", moving_note, moving));
    let visible_note = note(
        mtm,
        "Oculta, te sigue escuchando: la encontrás en la barra de menú.",
        TEXT_W,
    );
    add_full_width(
        &main,
        &switch_row(mtm, "Mostrarla en pantalla", &visible_note, visible),
    );
    add_full_width(&main, &separator(mtm));
    let folder = push_button(mtm, "Abrir carpeta de mascotas", target, sel!(openPets:));
    main.addArrangedSubview(note_field);
    main.addArrangedSubview(&folder);
    pinned_top(mtm, &main)
}

fn facts_section(
    mtm: MainThreadMarker,
    title: &str,
    about: &str,
    rows: &[InfoRow],
    footer: &str,
) -> Retained<NSView> {
    let main = top_stack(mtm);
    main.addArrangedSubview(&bold(mtm, title, 15.0));
    main.addArrangedSubview(&note(mtm, about, WIDTH - 2.0 * INSET));
    add_full_width(&main, &separator(mtm));
    if rows.is_empty() {
        main.addArrangedSubview(&note(
            mtm,
            "Se ve acá cuando Wawi está prendida con su voz.",
            WIDTH - 2.0 * INSET,
        ));
    }
    for row in rows {
        let label = text(mtm, &row.label);
        label.setTextColor(Some(&NSColor::secondaryLabelColor()));
        label
            .widthAnchor()
            .constraintEqualToConstant(220.0)
            .setActive(true);
        let value = note(mtm, &row.value, WIDTH - 2.0 * INSET - 240.0);
        value.setTextColor(Some(&NSColor::labelColor()));
        value.setSelectable(true);
        let line = stack(mtm, &[&label, &value], false, 20.0);
        line.setAlignment(NSLayoutAttribute::FirstBaseline);
        add_full_width(&main, &line);
    }
    add_full_width(&main, &separator(mtm));
    main.addArrangedSubview(&note(mtm, footer, WIDTH - 2.0 * INSET));
    pinned_top(mtm, &main)
}

fn privacy_section(
    mtm: MainThreadMarker,
    target: &Target,
    source: &SettingsSource,
    rows: &NSStackView,
) -> Retained<NSView> {
    let main = top_stack(mtm);
    main.addArrangedSubview(&bold(mtm, "Privacidad y permisos", 15.0));
    let who = match &source.permissions_app {
        Some(app) => format!(
            "macOS le da estos permisos a {app}. Si cambiás uno, cerrá Wawi del todo y volvé a abrirla."
        ),
        None => "Si cambiás un permiso, cerrá Wawi del todo y volvé a abrirla.".to_owned(),
    };
    main.addArrangedSubview(&note(mtm, &who, WIDTH - 2.0 * INSET));
    add_full_width(&main, &separator(mtm));
    if source.permissions.is_none() {
        main.addArrangedSubview(&note(
            mtm,
            "Se ve acá cuando Wawi está prendida con su voz.",
            WIDTH - 2.0 * INSET,
        ));
        return pinned_top(mtm, &main);
    }
    add_full_width(&main, rows);
    add_full_width(&main, &separator(mtm));
    let again = push_button(mtm, "Volver a mirar", target, sel!(recheckPermissions:));
    main.addArrangedSubview(&again);
    pinned_top(mtm, &main)
}

// -- small pieces ----------------------------------------------------------------

fn view_of<T: AsRef<NSView> + ?Sized>(view: &T) -> Retained<NSView> {
    view.as_ref().retain()
}

fn text(mtm: MainThreadMarker, value: &str) -> Retained<NSTextField> {
    NSTextField::labelWithString(&NSString::from_str(value), mtm)
}

fn bold(mtm: MainThreadMarker, value: &str, size: f64) -> Retained<NSTextField> {
    let label = text(mtm, value);
    label.setFont(Some(&NSFont::boldSystemFontOfSize(size)));
    label
}

fn heading(mtm: MainThreadMarker, value: &str) -> Retained<NSTextField> {
    let label = text(mtm, value);
    label.setFont(Some(&NSFont::boldSystemFontOfSize(11.0)));
    label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    label
}

/// Wrapping secondary text.
fn note(mtm: MainThreadMarker, value: &str, width: f64) -> Retained<NSTextField> {
    let label = NSTextField::wrappingLabelWithString(&NSString::from_str(value), mtm);
    label.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    label.setPreferredMaxLayoutWidth(width);
    label.setSelectable(false);
    label
}

fn set_text(field: &NSTextField, value: &str) {
    field.setStringValue(&NSString::from_str(value));
    field.setHidden(value.is_empty());
}

fn set_state(toggle: &NSSwitch, on: bool) {
    toggle.setState(if on {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    });
}

fn tone_color(tone: Tone) -> Retained<NSColor> {
    match tone {
        Tone::Good => NSColor::systemGreenColor(),
        Tone::Busy => NSColor::systemBlueColor(),
        Tone::Warn => NSColor::systemOrangeColor(),
        Tone::Bad => NSColor::systemRedColor(),
        Tone::Off => NSColor::systemGrayColor(),
    }
}

fn push_button(
    mtm: MainThreadMarker,
    title: &str,
    target: &Target,
    action: Sel,
) -> Retained<NSButton> {
    // SAFETY: the target lives as long as the window that holds the button.
    unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str(title),
            Some(target as &AnyObject),
            Some(action),
            mtm,
        )
    }
}

fn small_button(
    mtm: MainThreadMarker,
    title: &str,
    target: &Target,
    action: Sel,
    tag: usize,
) -> Retained<NSButton> {
    let button = push_button(mtm, title, target, action);
    button.setControlSize(NSControlSize::Small);
    button.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    button.setTag(tag as isize);
    button
}

fn switch(
    mtm: MainThreadMarker,
    on: bool,
    target: &Target,
    action: Sel,
    tag: usize,
) -> Retained<NSSwitch> {
    let toggle = NSSwitch::new(mtm);
    set_state(&toggle, on);
    // SAFETY: the target lives as long as the window that holds the switch.
    unsafe {
        toggle.setTarget(Some(target as &AnyObject));
        toggle.setAction(Some(action));
    }
    toggle.setTag(tag as isize);
    toggle
}

fn switch_row(
    mtm: MainThreadMarker,
    title: &str,
    about: &NSTextField,
    toggle: &NSSwitch,
) -> Retained<NSStackView> {
    let title = text(mtm, title);
    let left = stack(mtm, &[&title, about], true, 2.0);
    stretch(&left);
    let row = stack(mtm, &[&left, toggle], false, 12.0);
    row.setAlignment(NSLayoutAttribute::CenterY);
    row
}

/// The stack that takes the row's spare width (its switch or button goes
/// to the right edge).
fn stretch(stack: &NSStackView) {
    stack.setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);
    stack.setHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);
}

fn separator(mtm: MainThreadMarker) -> Retained<NSBox> {
    let line = NSBox::new(mtm);
    line.setBoxType(NSBoxType::Separator);
    line
}

fn stack(
    mtm: MainThreadMarker,
    views: &[&NSView],
    vertical: bool,
    spacing: f64,
) -> Retained<NSStackView> {
    let views = NSArray::from_slice(views);
    let stack = NSStackView::stackViewWithViews(&views, mtm);
    stack.setOrientation(if vertical {
        NSUserInterfaceLayoutOrientation::Vertical
    } else {
        NSUserInterfaceLayoutOrientation::Horizontal
    });
    if vertical {
        stack.setAlignment(NSLayoutAttribute::Leading);
    }
    stack.setSpacing(spacing);
    stack.setDetachesHiddenViews(true);
    stack
}

fn stack_of(
    mtm: MainThreadMarker,
    views: &[Retained<NSView>],
    vertical: bool,
    spacing: f64,
) -> Retained<NSStackView> {
    let refs: Vec<&NSView> = views.iter().map(|v| &**v).collect();
    stack(mtm, &refs, vertical, spacing)
}

/// A section's vertical stack, with the usual margins.
fn top_stack(mtm: MainThreadMarker) -> Retained<NSStackView> {
    let main = stack(mtm, &[], true, 14.0);
    main.setEdgeInsets(NSEdgeInsets {
        top: 20.0,
        left: INSET,
        bottom: 20.0,
        right: INSET,
    });
    main
}

/// Adds `view` to a vertical stack, as wide as the stack's content.
fn add_full_width(stack: &NSStackView, view: &NSView) {
    stack.addArrangedSubview(view);
    let insets = stack.edgeInsets();
    view.widthAnchor()
        .constraintEqualToAnchor_constant(&stack.widthAnchor(), -(insets.left + insets.right))
        .setActive(true);
}

fn clear(stack: &NSStackView) {
    for view in stack.arrangedSubviews().iter() {
        stack.removeArrangedSubview(&view);
        view.removeFromSuperview();
    }
}

/// A fixed-size section root.
fn root(mtm: MainThreadMarker) -> Retained<NSView> {
    let root = NSView::initWithFrame(
        NSView::alloc(mtm),
        NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH, HEIGHT)),
    );
    root.widthAnchor()
        .constraintEqualToConstant(WIDTH)
        .setActive(true);
    root.heightAnchor()
        .constraintEqualToConstant(HEIGHT)
        .setActive(true);
    root
}

/// `main` pinned to the top of a section, as wide as it.
fn pinned_top(mtm: MainThreadMarker, main: &NSStackView) -> Retained<NSView> {
    let root = root(mtm);
    main.setTranslatesAutoresizingMaskIntoConstraints(false);
    root.addSubview(main);
    main.topAnchor()
        .constraintEqualToAnchor(&root.topAnchor())
        .setActive(true);
    main.leadingAnchor()
        .constraintEqualToAnchor(&root.leadingAnchor())
        .setActive(true);
    main.trailingAnchor()
        .constraintEqualToAnchor(&root.trailingAnchor())
        .setActive(true);
    root
}

/// `main` in a vertical scroll view that fills the section.
fn scrolled(mtm: MainThreadMarker, main: &NSStackView) -> Retained<NSView> {
    let root = root(mtm);
    let scroll = NSScrollView::initWithFrame(
        NSScrollView::alloc(mtm),
        NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH, HEIGHT)),
    );
    scroll.setHasVerticalScroller(true);
    scroll.setDrawsBackground(false);
    scroll.setAutohidesScrollers(true);
    scroll.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    let document = Flipped::new(mtm);
    document.setTranslatesAutoresizingMaskIntoConstraints(false);
    scroll.setDocumentView(Some(&document));
    let clip = scroll.contentView();
    document
        .leadingAnchor()
        .constraintEqualToAnchor(&clip.leadingAnchor())
        .setActive(true);
    document
        .trailingAnchor()
        .constraintEqualToAnchor(&clip.trailingAnchor())
        .setActive(true);
    document
        .topAnchor()
        .constraintEqualToAnchor(&clip.topAnchor())
        .setActive(true);
    main.setTranslatesAutoresizingMaskIntoConstraints(false);
    document.addSubview(main);
    main.topAnchor()
        .constraintEqualToAnchor(&document.topAnchor())
        .setActive(true);
    main.leadingAnchor()
        .constraintEqualToAnchor(&document.leadingAnchor())
        .setActive(true);
    main.trailingAnchor()
        .constraintEqualToAnchor(&document.trailingAnchor())
        .setActive(true);
    main.bottomAnchor()
        .constraintEqualToAnchor(&document.bottomAnchor())
        .setActive(true);
    root.addSubview(&scroll);
    root
}

fn open_url(url: &str) {
    if let Some(url) = NSURL::URLWithString(&NSString::from_str(url)) {
        NSWorkspace::sharedWorkspace().openURL(&url);
    }
}

/// «Quitar» deletes its folder and its permission: asked once.
fn confirm_remove(mtm: MainThreadMarker, title: &str) -> bool {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(&format!("¿Quitar {title}?")));
    alert.setInformativeText(&NSString::from_str(
        "Se cierra, se borra su permiso del llavero y su carpeta va a plugins/.borrados.",
    ));
    alert.addButtonWithTitle(&NSString::from_str("Quitar"));
    alert.addButtonWithTitle(&NSString::from_str("Cancelar"));
    alert.runModal() == NSAlertFirstButtonReturn
}

/// An accessory app has no menu bar, so ⌘V, ⌘C, ⌘A and ⌘W would do
/// nothing in the window. A hidden main menu with those items makes the
/// usual shortcuts work (pasting a link, closing the window).
fn install_edit_menu(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    if app.mainMenu().is_some() {
        return;
    }
    let item = |title: &str, action: Sel, key: &str| unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            &NSString::from_str(key),
        )
    };
    let main = NSMenu::new(mtm);
    let edit = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("Edición"));
    for (title, action, key) in [
        ("Deshacer", sel!(undo:), "z"),
        ("Cortar", sel!(cut:), "x"),
        ("Copiar", sel!(copy:), "c"),
        ("Pegar", sel!(paste:), "v"),
        ("Seleccionar todo", sel!(selectAll:), "a"),
        ("Cerrar", sel!(performClose:), "w"),
    ] {
        edit.addItem(&item(title, action, key));
    }
    let holder = NSMenuItem::new(mtm);
    holder.setSubmenu(Some(&edit));
    main.addItem(&holder);
    app.setMainMenu(Some(&main));
}
