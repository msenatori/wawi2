//! The AppKit backend: the mascot's window, the bubble's window and the
//! status item, all on the main thread under the accessory activation policy
//! (no Dock icon, like Wawi 1).
//!
//! The windows are borderless non-activating panels: transparent, floating
//! above normal windows, on every Space (and beside full-screen apps), and
//! never key, so clicking her never steals focus from what you are doing.
//!
//! Updates arrive on a channel; [`BodyLink`]'s wake schedules a drain on the
//! main queue. Redraws are driven by a one-shot timer set to
//! [`Animator::next_frame_in`], so while she sits or sleeps the process
//! hardly wakes at all.
//!
//! The pet comes from a pack ([`crate::pack`]). Its pictures are decoded
//! lazily by a loader thread ([`frames`]); only the idle look is decoded
//! before the first frame. «Mascota» in the menu switches packs live.
//!
//! A pack with a body (`[cuerpo]`) moves her window: the pure
//! [`crate::body::Walker`] runs here at the frame rate (cheap math), fed by
//! the window list that [`desktop`] reads on its own thread.

mod desktop;
mod frames;
mod secret;
mod settings;
mod views;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AnyThread, MainThreadMarker, MainThreadOnly, sel};
use objc2_app_kit::NSAttributedStringNSExtendedStringDrawing;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSCellImagePosition, NSColor,
    NSControlStateValueOff, NSControlStateValueOn, NSEvent, NSEventModifierFlags, NSEventType,
    NSFloatingWindowLevel, NSImage, NSMenu, NSMenuItem, NSPanel, NSScreen, NSStatusBar,
    NSStatusItem, NSStringDrawingOptions, NSVariableStatusItemLength, NSWindowCollectionBehavior,
    NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{
    NSPoint, NSRect, NSRunLoop, NSRunLoopCommonModes, NSSize, NSString, NSTimer, NSURL,
};

use self::desktop::{Desktop, Watch};
use self::frames::Book;
use self::views::{BubbleView, MascotArt, MascotView, Target, bubble_string};
use crate::anim::{Animator, FrameId, Scene};
use crate::body::{Ctx, Loco, Screen, Snapshot, Touch, Walker};
use crate::bubble::{Bubble, Voice};
use crate::menu::{self, Action, LAYOUT, MenuItem, MenuState};
use crate::mood::Mood;
use crate::pack::{self, Installed, Look, Pack};
use crate::place::{self, Placement, Rect};
use crate::settings::{PetPanel, SettingsSource};
use crate::{BodyConfig, BodyError, BodyLink, Command, OnCommand, Update};

/// Room around the sprite for the hop, the tilt and the badge.
pub(crate) const PAD_X: f64 = 16.0;
pub(crate) const PAD_TOP: f64 = 30.0;
pub(crate) const PAD_BOTTOM: f64 = 2.0;
/// The bubble: inner padding, tail height, widest line, gap over her head.
pub(crate) const BUBBLE_PAD: f64 = 9.0;
pub(crate) const BUBBLE_TAIL: f64 = 9.0;
const BUBBLE_MAX_W: f64 = 210.0;
const BUBBLE_GAP: f64 = 2.0;
/// A press that moves less than this (points) is a click, not a drag.
const CLICK_SLOP: f64 = 3.0;
/// Hidden, she only needs an occasional tick.
const HIDDEN_TICK: f64 = 5.0;
/// Menu bar icon height, in points.
const STATUS_ICON_H: f64 = 18.0;

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

/// `WAWI2_BODY_TIMING=1`: print when the first frame is drawn and what the
/// pictures take (to measure startup; off by default).
static TIMING: OnceLock<Option<Instant>> = OnceLock::new();
static FIRST_DRAWN: AtomicBool = AtomicBool::new(false);

fn timing() -> Option<Instant> {
    *TIMING.get_or_init(|| std::env::var_os("WAWI2_BODY_TIMING").map(|_| Instant::now()))
}

/// Called by the view after drawing a frame: logs the very first one.
pub(crate) fn first_frame() {
    if FIRST_DRAWN.swap(true, Ordering::Relaxed) {
        return;
    }
    // Her body starts now (the window list's thread), off the first frame.
    DispatchQueue::main().exec_async(|| {
        with_ui(|ui| ui.tick());
    });
    if let Some(start) = timing() {
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_micros())
            .unwrap_or(0);
        eprintln!(
            "TIMING first_frame_epoch_us={epoch} body_ms={:.1}",
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
}

/// Runs `f` on the UI if it is there and not already busy (a callback that
/// arrives while the UI is mid-update is dropped; the next tick catches up).
pub(crate) fn with_ui<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|cell| {
        let mut guard = cell.try_borrow_mut().ok()?;
        guard.as_mut().map(f)
    })
}

pub(crate) fn run(
    config: BodyConfig,
    on_command: OnCommand,
    start: impl FnOnce(BodyLink),
) -> Result<(), BodyError> {
    let mtm = MainThreadMarker::new().ok_or(BodyError::NotMainThread)?;
    timing();
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let (tx, rx) = mpsc::channel();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {
        DispatchQueue::main().exec_async(|| {
            with_ui(|ui| ui.tick());
        });
    });
    let link = BodyLink::new(tx, wake);
    let ui = Ui::new(mtm, app.clone(), config, on_command, link.clone(), rx);
    UI.with(|cell| *cell.borrow_mut() = Some(ui));
    with_ui(|ui| ui.tick());
    start(link);
    app.run();
    if let Some(ui) = UI.with(|cell| cell.borrow_mut().take()) {
        ui.teardown();
    }
    Ok(())
}

struct Drag {
    press: NSPoint,
    origin: NSPoint,
    moved: bool,
}

/// Her body, for a pack with `[cuerpo]` and movement on.
struct Moving {
    walker: Walker,
    /// The window list, read on its own thread (started after the first
    /// frame: nothing about her body delays it).
    desktop: Option<Desktop>,
    /// Screens and windows as the walker sees them.
    world: Snapshot,
    /// The desktop's version last copied into `world`.
    seen: u64,
    /// The ground point her window is at.
    shown: (f64, f64),
}

/// The looks that may come next in a turn, decoded in the background while
/// the current one shows: by the time the dialogue gets there, its pictures
/// are ready, and the voice path never waits on one.
fn looks_after(mood: Mood) -> &'static [Look] {
    match mood {
        Mood::Idle => &[Look::Listening],
        Mood::Listening => &[Look::Transcribing, Look::Thinking],
        Mood::Transcribing => &[Look::Thinking, Look::Speaking],
        Mood::Thinking => &[Look::Speaking, Look::Error],
        Mood::Speaking => &[Look::Interrupted, Look::Listening],
    }
}

pub(crate) struct Ui {
    mtm: MainThreadMarker,
    app: Retained<NSApplication>,
    config: BodyConfig,
    on_command: OnCommand,
    link: BodyLink,
    rx: Receiver<Update>,
    epoch: Instant,
    last: f64,
    pack: Arc<Pack>,
    anim: Animator,
    book: Book,
    wanted: Vec<FrameId>,
    bubble: Bubble,
    mood: Mood,
    muted: bool,
    reduce_motion: bool,
    placement: Placement,
    mascot: Option<(Retained<NSPanel>, Retained<MascotView>)>,
    bubble_window: Retained<NSPanel>,
    bubble_view: Retained<BubbleView>,
    bubble_shown: Option<(Voice, String)>,
    bubble_alpha: f64,
    status: Retained<NSStatusItem>,
    menu: Retained<NSMenu>,
    items: Vec<(MenuItem, Retained<NSMenuItem>)>,
    /// The packs the «Mascota» submenu listed last time it opened.
    pets_listed: Vec<Installed>,
    target: Retained<Target>,
    timer: Option<Retained<NSTimer>>,
    drag: Option<Drag>,
    glyph: &'static str,
    closing: bool,
    moving: Option<Moving>,
    /// What the settings window shows that is not hers (plugins, voice…).
    settings: SettingsSource,
}

fn log(line: String) {
    eprintln!("  cuerpo: {line}");
}

impl Ui {
    fn new(
        mtm: MainThreadMarker,
        app: Retained<NSApplication>,
        config: BodyConfig,
        on_command: OnCommand,
        link: BodyLink,
        rx: Receiver<Update>,
    ) -> Self {
        let placement = config
            .state_file
            .as_deref()
            .map(Placement::load)
            .unwrap_or_default();
        let target = Target::new(mtm);
        let pack = Arc::new(config.active_pack(&mut log).unwrap_or_else(|| {
            log(format!(
                "no encuentro los dibujos de Wawi en {}; sigo solo con el menú",
                config.assets.display()
            ));
            Pack::empty(config.assets.clone())
        }));
        let reduce_motion = NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {
            DispatchQueue::main().exec_async(|| {
                with_ui(|ui| ui.tick());
            });
        });
        let book = Book::new(wake);
        let mut anim = Animator::new(pack.clone());
        anim.set_reduce_motion(reduce_motion);
        let bubble_frame = NSRect::new(NSPoint::ZERO, NSSize::new(120.0, 40.0));
        let bubble_window = make_panel(mtm, bubble_frame, NSFloatingWindowLevel + 1);
        let bubble_view = BubbleView::new(mtm, bubble_frame);
        bubble_window.setContentView(Some(&bubble_view));

        let (status, menu, items) = make_status_item(mtm, &target);
        let mut ui = Self {
            mtm,
            app,
            config,
            on_command,
            link,
            rx,
            epoch: Instant::now(),
            last: 0.0,
            pack,
            anim,
            book,
            wanted: Vec::new(),
            bubble: Bubble::default(),
            mood: Mood::Idle,
            muted: false,
            reduce_motion,
            placement,
            mascot: None,
            bubble_window,
            bubble_view,
            bubble_shown: None,
            bubble_alpha: 0.0,
            status,
            menu,
            items,
            pets_listed: Vec::new(),
            target,
            timer: None,
            drag: None,
            glyph: "",
            closing: false,
            moving: None,
            settings: SettingsSource::default(),
        };
        ui.dress();
        ui.refresh_glyph();
        ui
    }

    /// Puts the current pack on: her window, the idle look decoded, the
    /// rest of the dialogue looks on their way, the menu bar icon and names.
    fn dress(&mut self) {
        if let Some((window, _)) = self.mascot.take() {
            window.orderOut(None);
            window.close();
        }
        self.book.reset();
        if self.anim.has_frames() {
            let started = Instant::now();
            let view = self.make_mascot();
            // Idle's first picture, here: the first frame never waits. It
            // is decoded at its first draw, as any image; the rest of idle
            // comes from the loader, decoded.
            let mut idle = Vec::new();
            if let Some(name) = self.pack.look_clip(Look::Idle) {
                self.pack.clip_frames(name, &mut idle);
            }
            if let Some(image) = idle.first().and_then(|f| self.pack.image_of(*f)) {
                let path = &self.pack.images()[image].path;
                match frames::decode(path, false) {
                    Ok(decoded) => {
                        let (picture, bytes) = decoded.into_image();
                        view.insert_image(image, picture);
                        self.book.add(image, bytes);
                    }
                    Err(error) => {
                        if self.book.fail(image) {
                            log(error);
                        }
                    }
                }
            }
            self.request(&idle);
            if timing().is_some() {
                eprintln!(
                    "TIMING idle_decoded_ms={:.1} pack={}",
                    started.elapsed().as_secs_f64() * 1000.0,
                    self.pack.id()
                );
            }
            // What comes next, in the background.
            self.prefetch_after(self.mood);
        } else {
            log(format!(
                "«{}» no tiene nada que mostrar; sigo solo con el menú",
                self.pack.name()
            ));
        }
        set_status_icon(self.mtm, &self.status, &self.pack);
        self.refresh_menu();
        self.place_mascot();
        self.setup_body();
        if self.placement.visible {
            self.show_mascot();
        }
    }

    fn make_mascot(&mut self) -> Retained<MascotView> {
        let muted_badge = self.pack.muted_icon.as_deref().and_then(load_image);
        let art = MascotArt {
            frames: self
                .pack
                .frames()
                .iter()
                .map(|f| (f.image, f.cell, f.place))
                .collect(),
            images: HashMap::new(),
            muted_badge,
            sprite: self.pack.sprite_size(),
            anchor: self.pack.anchor,
            pad_bottom: self.pad_bottom(),
            scene: None,
            last_drawn: None,
        };
        let (w, h) = self.window_size();
        let frame = NSRect::new(NSPoint::ZERO, NSSize::new(w, h));
        // Floating: alone, she is the whole interface (Wawi 1).
        let window = make_panel(self.mtm, frame, NSFloatingWindowLevel);
        let view = MascotView::new(self.mtm, frame, art);
        window.setContentView(Some(&view));
        self.mascot = Some((window, view.clone()));
        view
    }

    fn prefetch_after(&mut self, mood: Mood) {
        let mut soon = Vec::new();
        for look in looks_after(mood) {
            if let Some(name) = self.pack.look_clip(*look) {
                self.pack.clip_frames(name, &mut soon);
            }
        }
        if mood == Mood::Idle {
            for name in self.pack.life.blink.iter() {
                self.pack.clip_frames(name, &mut soon);
            }
        }
        self.request(&soon);
    }

    /// Asks the loader for the pictures of `frames` that are not decoded.
    fn request(&mut self, frames: &[FrameId]) {
        for frame in frames {
            if let Some(image) = self.pack.image_of(*frame)
                && let Some(file) = self.pack.images().get(image)
                && file.present
            {
                self.book.want(image, &file.path);
            }
        }
    }

    /// Wraps what the loader decoded and drops what is over the cap. True
    /// if anything new arrived.
    fn collect_pictures(&mut self) -> bool {
        let done = self.book.collect();
        if done.is_empty() {
            return false;
        }
        let Some((_, view)) = &self.mascot else {
            return false;
        };
        let mut arrived = false;
        for done in done {
            match done.result {
                Ok(decoded) => {
                    let (picture, bytes) = decoded.into_image();
                    view.insert_image(done.image, picture);
                    self.book.add(done.image, bytes);
                    arrived = true;
                }
                Err(error) => {
                    if self.book.fail(done.image) {
                        log(error);
                    }
                }
            }
        }
        let keep: HashSet<usize> = self
            .wanted
            .iter()
            .filter_map(|f| self.pack.image_of(*f))
            .collect();
        for image in self.book.evict(&keep) {
            view.remove_image(image);
        }
        if timing().is_some() {
            eprintln!(
                "TIMING pictures_kb={} peak_kb={}",
                self.book.total() / 1024,
                self.book.peak / 1024
            );
        }
        arrived
    }

    fn now(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64()
    }

    fn menu_state(&self) -> MenuState<'_> {
        MenuState {
            listening: !self.muted,
            visible: self.placement.visible,
            name: self.pack.name(),
            moving: self.placement.movement,
        }
    }

    /// Drains the updates, advances the animation, redraws what changed and
    /// sets the timer for the next change.
    pub(crate) fn tick(&mut self) {
        let now = self.now();
        while let Ok(update) = self.rx.try_recv() {
            self.apply(update, now);
        }
        if self.closing {
            return;
        }
        let arrived = self.collect_pictures();
        let dt = now - self.last;
        self.last = now;
        let visible = self.placement.visible && self.mascot.is_some();
        if visible {
            self.move_body(now);
        } else if let Some(m) = &self.moving {
            // Hidden, nothing she stands on matters.
            if let Some(desktop) = &m.desktop {
                desktop.watch(Watch::Resting);
            }
        }
        let scene = self.anim.update(dt);
        if visible {
            self.wanted.clear();
            self.anim.wanted(&mut self.wanted);
            self.wanted.extend(scene.frame);
            self.wanted.extend(scene.previous);
            let wanted = std::mem::take(&mut self.wanted);
            self.request(&wanted);
            self.wanted = wanted;
            self.draw(scene, arrived);
            self.refresh_bubble(now);
        }
        let mut wait = if visible {
            self.anim.next_frame_in()
        } else {
            HIDDEN_TICK
        };
        if visible && let Some(change) = self.bubble.next_change_in(now) {
            wait = wait.min(change.max(1.0 / crate::anim::FPS));
        }
        if visible
            && let Some(m) = &self.moving
            && (m.walker.moving() || self.drag.as_ref().is_some_and(|d| d.moved))
        {
            wait = wait.min(1.0 / crate::anim::FPS);
        }
        self.schedule(wait);
    }

    fn apply(&mut self, update: Update, now: f64) {
        match update {
            Update::Mood(mood) => {
                self.mood = mood;
                self.anim.set_mood(mood);
                self.prefetch_after(mood);
                self.bubble.mood(mood, now);
                self.refresh_glyph();
            }
            Update::Flash(flash) => self.anim.flash(flash),
            Update::Heard(text) => self.bubble.heard(&text),
            Update::Said(text) => self.bubble.said(&text),
            Update::Muted(muted) => {
                self.muted = muted;
                self.anim.set_muted(muted);
                self.refresh_glyph();
                self.refresh_menu();
            }
            Update::Play(name) => {
                if !self.anim.play_action(&name) {
                    log(format!(
                        "«{}» no tiene la acción «{name}»",
                        self.pack.name()
                    ));
                }
            }
            Update::Snapshot(path) => self.snapshot(&path),
            Update::Drop(height) => {
                if let Some(m) = &mut self.moving {
                    m.walker.drop_from(height);
                }
            }
            Update::Wander => {
                if let Some(m) = &mut self.moving {
                    m.walker.wander_soon(now);
                }
            }
            Update::AskSecret(ask) => {
                // After this update, outside the UI's borrow: the dialog
                // runs its own loop and her timers keep firing.
                DispatchQueue::main().exec_async(move || {
                    if let Some(mtm) = MainThreadMarker::new() {
                        secret::ask(mtm, ask);
                    }
                });
            }
            Update::Settings(source) => {
                settings::subscribe(&source);
                self.settings = source;
            }
            Update::ShowSettings => {
                // After this update, outside the UI's borrow: building the
                // window may call back into it.
                let (source, pets) = (self.settings.clone(), self.pet_panel());
                DispatchQueue::main().exec_async(move || {
                    if let Some(mtm) = MainThreadMarker::new() {
                        settings::show(mtm, &source, pets);
                    }
                });
            }
            Update::Close => self.close(),
        }
    }

    fn snapshot(&mut self, path: &Path) {
        let Some((_, view)) = &self.mascot else {
            log("no hay mascota para fotografiar".into());
            return;
        };
        let scene = self.anim.update(0.0);
        view.set_scene(scene);
        match view.snapshot_png() {
            Some(png) if std::fs::write(path, &png).is_ok() => {
                log(format!("foto en {}", path.display()));
            }
            _ => log(format!("no pude guardar la foto en {}", path.display())),
        }
    }

    fn draw(&mut self, scene: Scene, force: bool) {
        if let Some((_, view)) = &self.mascot
            && (view.set_scene(scene) || force)
        {
            view.setNeedsDisplay(true);
        }
    }

    fn schedule(&mut self, wait: f64) {
        if let Some(timer) = self.timer.take() {
            timer.invalidate();
        }
        // SAFETY: the target implements `tick:`, taking the timer.
        let timer = unsafe {
            NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
                wait,
                &self.target,
                sel!(tick:),
                None,
                false,
            )
        };
        // Common modes: it keeps firing while she is dragged or a menu is open.
        // SAFETY: a fresh, unscheduled timer, on the main run loop.
        unsafe { NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
        self.timer = Some(timer);
    }

    // -- the bubble ---------------------------------------------------------

    fn refresh_bubble(&mut self, now: f64) {
        let current = self
            .bubble
            .current(now)
            .map(|(voice, text)| (voice, text.to_owned()));
        let alpha = self.bubble.alpha(now);
        if current != self.bubble_shown {
            self.bubble_shown = current.clone();
            match &current {
                Some((voice, text)) => {
                    self.bubble_view.set_text(current.clone());
                    self.size_bubble(text, *voice);
                    self.bubble_view.setNeedsDisplay(true);
                    self.bubble_window.setAlphaValue(alpha);
                    self.bubble_window.orderFrontRegardless();
                }
                None => {
                    self.bubble_view.set_text(None);
                    self.bubble_window.orderOut(None);
                }
            }
        } else if current.is_some() && (alpha - self.bubble_alpha).abs() > 1e-3 {
            self.bubble_window.setAlphaValue(alpha);
        }
        self.bubble_alpha = alpha;
    }

    fn size_bubble(&self, text: &str, voice: Voice) {
        let string = bubble_string(text, voice);
        let rect = string.boundingRectWithSize_options_context(
            NSSize::new(BUBBLE_MAX_W, 400.0),
            NSStringDrawingOptions::UsesLineFragmentOrigin,
            None,
        );
        let bw = rect.size.width.min(BUBBLE_MAX_W).ceil() + 2.0 * BUBBLE_PAD + 4.0;
        let bh = rect.size.height.ceil() + 2.0 * BUBBLE_PAD + BUBBLE_TAIL + 2.0;
        self.bubble_window
            .setFrame_display(self.bubble_rect(bw, bh), false);
    }

    /// Over her head, the tail at her head, kept on screen. The pose is left
    /// out on purpose: a bubble that bobs with every syllable is unreadable.
    fn bubble_rect(&self, bw: f64, bh: f64) -> NSRect {
        let Some((window, _)) = &self.mascot else {
            return NSRect::new(NSPoint::ZERO, NSSize::new(bw, bh));
        };
        let f = window.frame();
        let (sw, sh) = self.pack.sprite_size();
        let ax = self.pack.anchor.0;
        let (hx, hy) = self.pack.head;
        let side = if self.anim.mirrored() { -1.0 } else { 1.0 };
        let head_x = f.origin.x + f.size.width / 2.0 + side * (hx - ax) * sw;
        let mut x = head_x - bw * 0.38;
        let mut y = f.origin.y + PAD_BOTTOM + hy * sh + BUBBLE_GAP;
        if let Some(screen) = window.screen().or_else(|| NSScreen::mainScreen(self.mtm)) {
            let v = screen.visibleFrame();
            x = x
                .max(v.origin.x + 4.0)
                .min(v.origin.x + v.size.width - bw - 4.0);
            y = y.min(v.origin.y + v.size.height - bh - 2.0);
        }
        NSRect::new(NSPoint::new(x, y), NSSize::new(bw, bh))
    }

    fn move_bubble(&self) {
        if self.bubble_shown.is_some() {
            let size = self.bubble_window.frame().size;
            self.bubble_window
                .setFrame_display(self.bubble_rect(size.width, size.height), false);
        }
    }

    pub(crate) fn dismiss_bubble(&mut self) {
        self.bubble.clear();
        self.tick();
    }

    // -- her window -----------------------------------------------------------

    /// Wide enough for the sprite on either side of the anchor, plus room
    /// for the hop, the tilt and the badge.
    fn window_size(&self) -> (f64, f64) {
        let (w, h) = self.pack.sprite_size();
        let ax = self.pack.anchor.0;
        let (left, right, below, above) = self.pack.overhang();
        (
            2.0 * (ax + left).max(1.0 - ax + right) * w + 2.0 * PAD_X,
            (1.0 + below + above) * h + PAD_TOP + PAD_BOTTOM,
        )
    }

    /// Room under the ground line: art that hangs below it (her legs over a
    /// window's edge).
    fn pad_bottom(&self) -> f64 {
        PAD_BOTTOM + self.pack.overhang().2 * self.pack.sprite_size().1
    }

    /// Her ground point for a window origin, and back.
    fn ground_of(&self, origin: NSPoint) -> (f64, f64) {
        let (w, _) = self.window_size();
        let lift = self.pad_bottom() + self.pack.anchor.1 * self.pack.sprite_size().1;
        (origin.x + w / 2.0, origin.y + lift)
    }

    fn origin_of(&self, ground: (f64, f64)) -> NSPoint {
        let (w, _) = self.window_size();
        let lift = self.pad_bottom() + self.pack.anchor.1 * self.pack.sprite_size().1;
        NSPoint::new(ground.0 - w / 2.0, ground.1 - lift)
    }

    fn screens(&self) -> (Vec<Rect>, Option<Rect>) {
        let rect = |r: NSRect| (r.origin.x, r.origin.y, r.size.width, r.size.height);
        let screens = NSScreen::screens(self.mtm)
            .iter()
            .map(|s| rect(s.visibleFrame()))
            .collect();
        let main = NSScreen::mainScreen(self.mtm).map(|s| rect(s.visibleFrame()));
        (screens, main)
    }

    /// Every screen, whole and without the menu bar and the Dock.
    fn body_screens(&self) -> Vec<Screen> {
        let rect = |r: NSRect| (r.origin.x, r.origin.y, r.size.width, r.size.height);
        NSScreen::screens(self.mtm)
            .iter()
            .map(|s| Screen {
                frame: rect(s.frame()),
                visible: rect(s.visibleFrame()),
            })
            .collect()
    }

    // -- her body -------------------------------------------------------------

    /// Gives her a body if the pack has one and movement is on; otherwise
    /// she stays where she is put, as always.
    fn setup_body(&mut self) {
        self.anim.set_locomotion(None);
        self.anim.set_perched(false);
        let body = self.pack.body.clone();
        let (Some(body), true, Some((window, _))) = (body, self.placement.movement, &self.mascot)
        else {
            self.moving = None;
            return;
        };
        let ground = self.ground_of(window.frame().origin);
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1);
        let mut walker = Walker::new(body.params.clone(), self.pack.sprite_size(), ground, seed);
        walker.face(body.faces_right);
        self.anim.set_facing(body.faces_right);
        // Keep the old thread if there is one: switching pets keeps it.
        let desktop = self.moving.take().and_then(|old| old.desktop);
        self.moving = Some(Moving {
            walker,
            desktop,
            world: Snapshot::default(),
            seen: u64::MAX,
            shown: ground,
        });
    }

    /// One step of her body: physics, the clip it calls for, her window
    /// following it.
    fn move_body(&mut self, now: f64) {
        let Some(mut m) = self.moving.take() else {
            return;
        };
        if m.desktop.is_none() && FIRST_DRAWN.load(Ordering::Relaxed) {
            let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {
                DispatchQueue::main().exec_async(|| {
                    with_ui(|ui| ui.tick());
                });
            });
            m.desktop = Some(Desktop::start(wake));
        }
        // Until the first window list arrives she stays where she was put:
        // she may be sitting on one of those windows.
        let Some(desktop) = m.desktop.as_ref().filter(|d| d.version() > 0) else {
            self.moving = Some(m);
            return;
        };
        let version = desktop.version();
        if version != m.seen {
            m.seen = version;
            m.world.windows = desktop.windows().as_ref().clone();
            m.world.screens = self.body_screens();
            m.world.version += 1;
        }
        let ctx = Ctx {
            calm: self.mood != Mood::Idle || self.anim.busy(),
            free: self.anim.free(),
            asleep: self.anim.asleep(),
            still: self.reduce_motion,
            cursor: Some(NSEvent::mouseLocation()).map(|p| (p.x, p.y)),
        };
        let trips = m.walker.trips.len();
        let out = m.walker.tick(now, &m.world, ctx);
        if timing().is_some() {
            if let Some(trip) = m.walker.trips.get(trips) {
                eprintln!(
                    "TIMING trip={trip:?} from=({:.0},{:.0}) windows={}",
                    out.ground.0,
                    out.ground.1,
                    m.world.windows.len()
                );
            }
            if let Some(touch) = out.touchdown {
                eprintln!(
                    "TIMING touchdown={touch:?} at=({:.0},{:.0})",
                    out.ground.0, out.ground.1
                );
            }
        }
        if out.wake {
            self.anim.rouse();
        }
        if let Some(body) = &self.pack.body {
            let clip = out.loco.and_then(|loco| body.clip_for(loco));
            self.anim.set_locomotion(clip);
            if out.touchdown.is_some_and(|t| t != Touch::Chute)
                && body.clip_for(Loco::Land).is_none()
            {
                self.anim.squash(body.params.land_secs);
            }
        }
        self.anim.set_facing(out.facing_right);
        self.anim.set_perched(out.perched);
        let dragging = self.drag.as_ref().is_some_and(|d| d.moved);
        desktop.watch(if m.walker.moving() || dragging {
            Watch::Moving
        } else if let Some(id) = m.walker.body().ledge.and_then(|l| l.window) {
            Watch::Perch(id)
        } else {
            Watch::Resting
        });
        if !dragging && out.ground != m.shown {
            m.shown = out.ground;
            if let Some((window, _)) = &self.mascot {
                window.setFrameOrigin(self.origin_of(out.ground));
            }
            self.move_bubble();
        }
        if out.settled && !dragging {
            let o = self.origin_of(out.ground);
            self.placement.origin = Some((o.x, o.y));
            self.save();
        }
        self.moving = Some(m);
    }

    fn toggle_movement(&mut self) {
        self.placement.movement = !self.placement.movement;
        self.setup_body();
        self.save();
        self.refresh_menu();
        settings::pets_changed(&self.pet_panel());
        self.tick();
    }

    // -- the settings window's «Mascota» -------------------------------------

    /// The packs, in the same order as the «Mascota» submenu.
    fn pets(&self) -> Vec<Installed> {
        let mut pets = pack::installed(&self.config.assets, self.config.home.as_deref());
        if !pets.iter().any(|p| p.dir == self.pack.dir) {
            pets.insert(
                0,
                Installed {
                    id: self.pack.id().to_owned(),
                    nombre: self.pack.name().to_owned(),
                    dir: self.pack.dir.clone(),
                    builtin: self.pack.builtin,
                },
            );
        }
        pets
    }

    fn pet_panel(&self) -> PetPanel {
        let pets = self.pets();
        PetPanel {
            active: pets.iter().position(|p| p.dir == self.pack.dir),
            pets: pets.into_iter().map(|p| p.nombre).collect(),
            name: self.pack.name().to_owned(),
            has_body: self.pack.body.is_some(),
            moving: self.placement.movement,
            visible: self.placement.visible,
            preview: self.config.pet_dir.is_some() || self.config.state_file.is_none(),
        }
    }

    pub(crate) fn settings_pick_pet(&mut self, index: usize) {
        if let Some(pet) = self.pets().get(index).cloned() {
            self.switch_pet(&pet);
        }
        settings::pets_changed(&self.pet_panel());
    }

    pub(crate) fn settings_set_moving(&mut self, on: bool) {
        if self.placement.movement != on {
            self.toggle_movement();
        }
    }

    pub(crate) fn settings_set_visible(&mut self, on: bool) {
        if self.placement.visible != on {
            self.set_visible(on);
        }
    }

    pub(crate) fn open_pets_folder(&self) {
        let Some(home) = &self.config.home else {
            return;
        };
        let dir = home.join("mascotas");
        let _ = std::fs::create_dir_all(&dir);
        let url = NSURL::fileURLWithPath(&NSString::from_str(&dir.to_string_lossy()));
        NSWorkspace::sharedWorkspace().openURL(&url);
    }

    fn place_mascot(&mut self) {
        let Some((window, _)) = &self.mascot else {
            return;
        };
        let (screens, main) = self.screens();
        if let Some((x, y)) =
            place::on_screen(self.placement.origin, self.window_size(), &screens, main)
        {
            window.setFrameOrigin(NSPoint::new(x, y));
        }
    }

    fn show_mascot(&mut self) {
        if let Some((window, _)) = &self.mascot {
            window.orderFrontRegardless();
        }
        self.last = self.now();
    }

    fn set_visible(&mut self, visible: bool) {
        self.placement.visible = visible;
        if visible {
            self.place_mascot();
            self.show_mascot();
        } else {
            if let Some((window, _)) = &self.mascot {
                window.orderOut(None);
            }
            self.bubble_window.orderOut(None);
            self.bubble_shown = None;
        }
        self.save();
        self.refresh_menu();
        settings::pets_changed(&self.pet_panel());
        self.tick();
    }

    fn save(&self) {
        if let Some(path) = &self.config.state_file
            && let Err(error) = self.placement.save(path)
        {
            log(format!("no pude guardar {}: {error}", path.display()));
        }
    }

    pub(crate) fn press(&mut self) {
        if let Some((window, _)) = &self.mascot {
            self.drag = Some(Drag {
                press: NSEvent::mouseLocation(),
                origin: window.frame().origin,
                moved: false,
            });
        }
    }

    pub(crate) fn drag(&mut self) {
        let (Some(drag), Some((window, _))) = (&mut self.drag, &self.mascot) else {
            return;
        };
        let now = NSEvent::mouseLocation();
        let (dx, dy) = (now.x - drag.press.x, now.y - drag.press.y);
        if !drag.moved && dx.abs() < CLICK_SLOP && dy.abs() < CLICK_SLOP {
            // A hand is never perfectly still: still a click.
            return;
        }
        let grabbed = !drag.moved;
        drag.moved = true;
        let origin = NSPoint::new(drag.origin.x + dx, drag.origin.y + dy);
        window.setFrameOrigin(origin);
        let ground = self.ground_of(origin);
        if let Some(m) = &mut self.moving {
            if grabbed {
                m.walker.grab();
            }
            m.walker.carry(ground.0, ground.1);
            m.shown = ground;
        }
        self.move_bubble();
        if grabbed {
            // Dangling: the held clip right away.
            self.tick();
        }
    }

    pub(crate) fn release(&mut self) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        if drag.moved {
            if let Some((window, _)) = &self.mascot {
                let o = window.frame().origin;
                self.placement.origin = Some((o.x, o.y));
            }
            match &mut self.moving {
                // Let go: gravity takes her (the spot is saved again when
                // she lands).
                Some(m) => {
                    m.walker.release();
                    self.save();
                }
                None => {
                    self.place_mascot();
                    self.move_bubble();
                    self.save();
                }
            }
        } else {
            self.anim.poke();
        }
        self.tick();
    }

    // -- another pet ----------------------------------------------------------

    /// Puts another pack on, live. A pack that does not load is reported and
    /// the current one stays.
    fn switch_pet(&mut self, pet: &Installed) {
        if pet.dir == self.pack.dir {
            return;
        }
        let Some(mut pack) = pack::load_logged(&pet.dir, pet.builtin, &mut log) else {
            log(format!("sigo con {}", self.pack.name()));
            return;
        };
        pack.builtin = pet.builtin;
        // Keep the bottom-centre where it was: a bigger pet grows upwards.
        let old = self
            .mascot
            .as_ref()
            .map(|(window, _)| window.frame())
            .filter(|_| self.placement.origin.is_some());
        self.pack = Arc::new(pack);
        let mut anim = Animator::new(self.pack.clone());
        anim.set_reduce_motion(self.reduce_motion);
        anim.set_mood(self.mood);
        anim.set_muted(self.muted);
        self.anim = anim;
        if let Some(f) = old {
            let (w, _) = self.window_size();
            self.placement.origin = Some((f.origin.x + (f.size.width - w) / 2.0, f.origin.y));
        }
        self.placement.pet = (!pet.builtin).then(|| pet.id.clone());
        self.dress();
        self.move_bubble();
        if self.config.pet_dir.is_none() {
            self.save();
        }
        log(format!("ahora la mascota es {}", self.pack.name()));
        settings::pets_changed(&self.pet_panel());
        self.tick();
    }

    /// Lists the installed packs in the «Mascota» submenu, the active one
    /// checked (called each time it opens).
    pub(crate) fn fill_pets_menu(&mut self, menu: &NSMenu) {
        menu.removeAllItems();
        let mut pets = pack::installed(&self.config.assets, self.config.home.as_deref());
        if !pets.iter().any(|p| p.dir == self.pack.dir) {
            // Shown with --mascota and not installed: listed too, first.
            pets.insert(
                0,
                Installed {
                    id: self.pack.id().to_owned(),
                    nombre: self.pack.name().to_owned(),
                    dir: self.pack.dir.clone(),
                    builtin: self.pack.builtin,
                },
            );
        }
        for (i, pet) in pets.iter().enumerate() {
            // SAFETY: `menuAction:` is implemented by the target, which
            // `Ui` keeps alive as long as the menu.
            let item = unsafe {
                let item = NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(self.mtm),
                    &NSString::from_str(&pet.nombre),
                    Some(sel!(menuAction:)),
                    &NSString::from_str(""),
                );
                item.setTarget(Some(&self.target as &AnyObject));
                item
            };
            item.setTag(menu::pet_tag(i));
            item.setState(if pet.dir == self.pack.dir {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
            menu.addItem(&item);
        }
        self.pets_listed = pets;
    }

    // -- the menu -------------------------------------------------------------

    pub(crate) fn menu_for_popup(&mut self) -> Retained<NSMenu> {
        self.drag = None;
        self.refresh_menu();
        self.menu.clone()
    }

    pub(crate) fn menu_pick(&mut self, tag: isize) {
        if let Some(i) = menu::pet_from_tag(tag) {
            if let Some(pet) = self.pets_listed.get(i).cloned() {
                self.switch_pet(&pet);
            }
            return;
        }
        let Some(item) = MenuItem::from_tag(tag) else {
            return;
        };
        match item.action(self.menu_state()) {
            Action::Host(command) => {
                if command == Command::Quit {
                    // Feedback right away; the program closes the rest.
                    if let Some((window, _)) = &self.mascot {
                        window.orderOut(None);
                    }
                    self.bubble_window.orderOut(None);
                }
                (self.on_command)(command, &self.link);
            }
            Action::ToggleVisible => self.set_visible(!self.placement.visible),
            Action::ToggleMovement => self.toggle_movement(),
            Action::OpenLogs => self.open_logs(),
            Action::Submenu => {}
        }
    }

    fn open_logs(&self) {
        let Some(dir) = &self.config.logs_dir else {
            return;
        };
        let _ = std::fs::create_dir_all(dir);
        let url = NSURL::fileURLWithPath(&NSString::from_str(&dir.to_string_lossy()));
        NSWorkspace::sharedWorkspace().openURL(&url);
    }

    fn refresh_menu(&self) {
        let state = self.menu_state();
        for (item, entry) in &self.items {
            entry.setTitle(&NSString::from_str(&item.title(state)));
            if *item == MenuItem::Still {
                // Only a pet with a body can stay still.
                entry.setHidden(self.pack.body.is_none());
            }
        }
    }

    fn refresh_glyph(&mut self) {
        let glyph = menu::status_glyph(self.mood, self.muted);
        if glyph != self.glyph {
            self.glyph = glyph;
            if let Some(button) = self.status.button(self.mtm) {
                button.setTitle(&NSString::from_str(glyph));
            }
        }
    }

    // -- the end --------------------------------------------------------------

    fn close(&mut self) {
        if self.closing {
            return;
        }
        self.closing = true;
        settings::close_now();
        if let Some(timer) = self.timer.take() {
            timer.invalidate();
        }
        if let Some((window, _)) = &self.mascot {
            window.orderOut(None);
        }
        self.bubble_window.orderOut(None);
        self.app.stop(None);
        // `stop` only takes effect after the next event: post one.
        if let Some(event) = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
            NSEventType::ApplicationDefined,
            NSPoint::ZERO,
            NSEventModifierFlags::empty(),
            0.0,
            0,
            None,
            0,
            0,
            0,
        ) {
            self.app.postEvent_atStart(&event, true);
        }
    }

    fn teardown(self) {
        if let Some(timer) = &self.timer {
            timer.invalidate();
        }
        NSStatusBar::systemStatusBar().removeStatusItem(&self.status);
        if let Some((window, _)) = &self.mascot {
            window.close();
        }
        self.bubble_window.close();
    }
}

fn load_image(path: &Path) -> Option<Retained<NSImage>> {
    if !path.is_file() {
        return None;
    }
    NSImage::initWithContentsOfFile(
        NSImage::alloc(),
        &NSString::from_str(&path.to_string_lossy()),
    )
}

fn make_panel(mtm: MainThreadMarker, frame: NSRect, level: isize) -> Retained<NSPanel> {
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        frame,
        NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
        NSBackingStoreType::Buffered,
        false,
    );
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setHasShadow(false);
    panel.setLevel(level);
    panel.setHidesOnDeactivate(false);
    panel.setIgnoresMouseEvents(false);
    // SAFETY: the panels are kept in `Ui` and closed in `teardown`.
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    panel
}

type StatusParts = (
    Retained<NSStatusItem>,
    Retained<NSMenu>,
    Vec<(MenuItem, Retained<NSMenuItem>)>,
);

/// The pet's menu bar icon, or its name if it has none.
fn set_status_icon(mtm: MainThreadMarker, item: &NSStatusItem, pack: &Pack) {
    let Some(button) = item.button(mtm) else {
        return;
    };
    match pack.menubar_icon.as_deref().and_then(load_image) {
        Some(icon) => {
            let size = icon.size();
            let w = STATUS_ICON_H * size.width / size.height.max(1.0);
            icon.setSize(NSSize::new(w, STATUS_ICON_H));
            button.setImage(Some(&icon));
            button.setImagePosition(NSCellImagePosition::ImageLeft);
        }
        None => {
            button.setImage(None);
            button.setImagePosition(NSCellImagePosition::NoImage);
            let name = if pack.name().is_empty() {
                "Wawi"
            } else {
                pack.name()
            };
            button.setTitle(&NSString::from_str(name));
        }
    }
}

fn make_status_item(mtm: MainThreadMarker, target: &Target) -> StatusParts {
    let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    let menu = NSMenu::new(mtm);
    let mut items = Vec::new();
    for row in LAYOUT {
        match row {
            None => menu.addItem(&NSMenuItem::separatorItem(mtm)),
            Some(entry) => {
                // SAFETY: `menuAction:` is implemented by the target, which
                // `Ui` keeps alive as long as the menu.
                let native = unsafe {
                    let native = NSMenuItem::initWithTitle_action_keyEquivalent(
                        NSMenuItem::alloc(mtm),
                        &NSString::from_str(""),
                        Some(sel!(menuAction:)),
                        &NSString::from_str(""),
                    );
                    native.setTarget(Some(target as &AnyObject));
                    native
                };
                native.setTag(entry.tag());
                if *entry == MenuItem::Pets {
                    let pets = NSMenu::new(mtm);
                    pets.setDelegate(Some(ProtocolObject::from_ref(target)));
                    native.setSubmenu(Some(&pets));
                }
                menu.addItem(&native);
                items.push((*entry, native));
            }
        }
    }
    item.setMenu(Some(&menu));
    (item, menu, items)
}
