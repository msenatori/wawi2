//! The Objective-C classes: her view, the bubble's view, and one target for
//! the timer and the menu. They only draw what they were handed and forward
//! events to the [`super::Ui`]; no Rust state is borrowed across a call back
//! into AppKit.

use std::cell::RefCell;
use std::collections::HashMap;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAffineTransformNSAppKitAdditions, NSAttributedStringNSStringDrawing, NSBezierPath,
    NSBitmapImageFileType, NSColor, NSCompositingOperation, NSEvent, NSEventModifierFlags, NSFont,
    NSFontAttributeName, NSForegroundColorAttributeName, NSGraphicsContext, NSImage, NSMenu,
    NSMenuDelegate, NSMenuItem, NSView,
};
use objc2_foundation::NSObjectProtocol;
use objc2_foundation::{
    NSAffineTransform, NSAttributedString, NSDictionary, NSPoint, NSRect, NSSize, NSString,
};

use super::{BUBBLE_PAD, BUBBLE_TAIL, first_frame, with_ui};
use crate::anim::{Badge, FrameId, Scene, crossfade_alphas};
use crate::bubble::Voice;
use crate::pack::{Cell, Place};

/// What her view draws.
pub struct MascotArt {
    /// Each frame of the pack: the picture it is cut from, and where (a
    /// sheet's cell, in pixels from the top-left).
    pub frames: Vec<(usize, Option<Cell>, Option<Place>)>,
    /// The decoded pictures (the rest are on their way, or dropped).
    pub images: HashMap<usize, Retained<NSImage>>,
    pub muted_badge: Option<Retained<NSImage>>,
    /// The sprite's size in points.
    pub sprite: (f64, f64),
    /// The point of the frame on the ground, 0..1.
    pub anchor: (f64, f64),
    /// Room under her in the window, points: where the ground is.
    pub pad_bottom: f64,
    pub scene: Option<Scene>,
    /// The last frame really drawn: shown while a new one is still decoding.
    pub last_drawn: Option<FrameId>,
}

impl MascotArt {
    /// The picture and the part of it (in its own coordinates, bottom-left
    /// origin) for a frame, if decoded.
    fn picture(&self, frame: FrameId) -> Option<(&NSImage, NSRect)> {
        let (image, cell, _) = self.frames.get(usize::from(frame))?;
        let picture = self.images.get(image)?;
        let rect = match cell {
            None => NSRect::ZERO,
            Some(c) => {
                let h = picture.size().height;
                NSRect::new(
                    NSPoint::new(f64::from(c.x), h - f64::from(c.y + c.h)),
                    NSSize::new(f64::from(c.w), f64::from(c.h)),
                )
            }
        };
        Some((picture, rect))
    }

    /// Where a frame is drawn, around the ground point: the whole sprite,
    /// or its box for a picture of another size.
    fn rect_of(&self, frame: FrameId, shift: (f64, f64)) -> NSRect {
        let (w, h) = self.sprite;
        let (ax, ay) = self.anchor;
        let place = self
            .frames
            .get(usize::from(frame))
            .and_then(|f| f.2)
            .unwrap_or(Place {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
            });
        NSRect::new(
            NSPoint::new((place.x + shift.0 - ax) * w, (place.y + shift.1 - ay) * h),
            NSSize::new(place.w * w, place.h * h),
        )
    }
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "WawiMascotView"]
    #[ivars = RefCell<MascotArt>]
    pub struct MascotView;

    impl MascotView {
        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            false
        }

        // Her window never becomes key: the first click is the only click.
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.paint();
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            if event.modifierFlags().contains(NSEventModifierFlags::Control) {
                self.pop_menu(event);
                return;
            }
            with_ui(|ui| ui.press());
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, _event: &NSEvent) {
            with_ui(|ui| ui.drag());
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            with_ui(|ui| ui.release());
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            self.pop_menu(event);
        }
    }
);

impl MascotView {
    pub fn new(mtm: MainThreadMarker, frame: NSRect, art: MascotArt) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(RefCell::new(art));
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    pub fn insert_image(&self, image: usize, picture: Retained<NSImage>) {
        self.ivars().borrow_mut().images.insert(image, picture);
    }

    pub fn remove_image(&self, image: usize) {
        self.ivars().borrow_mut().images.remove(&image);
    }

    /// The view drawn offscreen, as PNG bytes.
    pub fn snapshot_png(&self) -> Option<Vec<u8>> {
        let bounds = self.bounds();
        let rep = self.bitmapImageRepForCachingDisplayInRect(bounds)?;
        self.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
        // SAFETY: an empty properties dictionary is valid for PNG.
        let data = unsafe {
            rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
        }?;
        Some(data.to_vec())
    }

    /// Hands the view a new scene; true if it differs from the drawn one.
    pub fn set_scene(&self, scene: Scene) -> bool {
        let mut art = self.ivars().borrow_mut();
        if art.scene.as_ref() == Some(&scene) {
            return false;
        }
        art.scene = Some(scene);
        true
    }

    /// The menu-bar menu, at the click. The UI is not borrowed while the
    /// menu tracks (its picks call back into it).
    fn pop_menu(&self, event: &NSEvent) {
        let menu: Option<Retained<NSMenu>> = with_ui(|ui| ui.menu_for_popup());
        if let Some(menu) = menu {
            NSMenu::popUpContextMenu_withEvent_forView(&menu, event, self);
        }
    }

    fn paint(&self) {
        let mut art = self.ivars().borrow_mut();
        let Some(scene) = art.scene.clone() else {
            return;
        };
        // Still decoding: keep showing what was there.
        let frame = scene
            .frame
            .filter(|f| art.picture(*f).is_some())
            .or(art.last_drawn.filter(|f| art.picture(*f).is_some()));
        if frame.is_some() && scene.frame == frame {
            art.last_drawn = frame;
        }
        let art = &*art;
        let bounds = self.bounds();
        let (_, h) = art.sprite;
        let (_, ay) = art.anchor;
        let pose = scene.pose;
        let previous = scene.previous.and_then(|f| art.picture(f));
        let (prev_alpha, alpha) = if previous.is_some() {
            crossfade_alphas(scene.mix)
        } else {
            (0.0, 1.0)
        };
        NSGraphicsContext::saveGraphicsState_class();
        let xf = NSAffineTransform::transform();
        xf.translateXBy_yBy(
            bounds.size.width / 2.0 + pose.dx,
            art.pad_bottom + ay * h + pose.dy,
        );
        xf.rotateByDegrees(pose.rot);
        let flip = if scene.mirror { -1.0 } else { 1.0 };
        xf.scaleXBy_yBy(pose.sx * flip, pose.sy);
        xf.concat();
        if let Some((image, from)) = previous
            && prev_alpha > 0.0
        {
            image.drawInRect_fromRect_operation_fraction(
                art.rect_of(scene.previous.unwrap_or_default(), (0.0, 0.0)),
                from,
                NSCompositingOperation::SourceOver,
                prev_alpha,
            );
        }
        if let Some(f) = frame
            && let Some((image, from)) = art.picture(f)
            && alpha > 0.0
        {
            image.drawInRect_fromRect_operation_fraction(
                art.rect_of(f, (0.0, 0.0)),
                from,
                NSCompositingOperation::SourceOver,
                alpha,
            );
            first_frame();
        }
        // What she carries along (the parachute's canopy), over her.
        if let Some(layer) = scene.layer
            && let Some((image, from)) = art.picture(layer.frame)
        {
            image.drawInRect_fromRect_operation_fraction(
                art.rect_of(layer.frame, layer.at),
                from,
                NSCompositingOperation::SourceOver,
                1.0,
            );
        }
        NSGraphicsContext::restoreGraphicsState_class();
        if let Some(badge) = scene.badge {
            paint_badge(art, badge, bounds, &scene);
        }
    }
}

const BADGE_D: f64 = 7.0;
const BADGE_ICON_H: f64 = 16.0;
const MUSING_D: f64 = 4.0;
const MUSING_GAP: f64 = 2.5;
const MUSING_HOP: f64 = 3.0;

/// Wawi 1's status mark over her head: outside the transform, so it rides up
/// and down with her but never tilts or squashes.
fn paint_badge(art: &MascotArt, badge: Badge, bounds: NSRect, scene: &Scene) {
    let (w, h) = art.sprite;
    let (ax, ay) = art.anchor;
    let pose = scene.pose;
    let x = bounds.size.width / 2.0 + (0.72 - ax) * w + pose.dx;
    let top = art.pad_bottom + ay * h + pose.dy + (1.0 - ay) * h * pose.sy;
    let y = (top + 4.0).min(bounds.size.height - BADGE_D - 2.0);
    match badge {
        Badge::Listening => {
            let dot = NSBezierPath::bezierPathWithOvalInRect(NSRect::new(
                NSPoint::new(x, y),
                NSSize::new(BADGE_D, BADGE_D),
            ));
            NSColor::colorWithSRGBRed_green_blue_alpha(0.30, 0.85, 0.45, 0.95).setFill();
            dot.fill();
        }
        Badge::Muted => match &art.muted_badge {
            Some(icon) => {
                let size = icon.size();
                let ih = BADGE_ICON_H;
                let iw = ih * size.width / size.height.max(1.0);
                let iy = y.min(bounds.size.height - ih - 1.0);
                icon.drawInRect_fromRect_operation_fraction(
                    NSRect::new(NSPoint::new(x, iy), NSSize::new(iw, ih)),
                    NSRect::ZERO,
                    NSCompositingOperation::SourceOver,
                    0.95,
                );
            }
            None => {
                let ring = NSBezierPath::bezierPathWithOvalInRect(NSRect::new(
                    NSPoint::new(x, y),
                    NSSize::new(BADGE_D, BADGE_D),
                ));
                NSColor::colorWithSRGBRed_green_blue_alpha(0.55, 0.55, 0.55, 0.9).setStroke();
                ring.setLineWidth(1.5);
                ring.stroke();
            }
        },
        Badge::Musing(heights) => {
            let y = y.min(bounds.size.height - MUSING_D - MUSING_HOP - 1.0);
            for (i, hop) in heights.iter().enumerate() {
                NSColor::colorWithSRGBRed_green_blue_alpha(0.16, 0.20, 0.36, 0.45 + 0.5 * hop)
                    .setFill();
                NSBezierPath::bezierPathWithOvalInRect(NSRect::new(
                    NSPoint::new(x + i as f64 * (MUSING_D + MUSING_GAP), y + MUSING_HOP * hop),
                    NSSize::new(MUSING_D, MUSING_D),
                ))
                .fill();
            }
        }
    }
}

/// What the bubble's view draws.
#[derive(Default)]
pub struct BubbleArt {
    pub text: Option<(Voice, String)>,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "WawiBubbleView"]
    #[ivars = RefCell<BubbleArt>]
    pub struct BubbleView;

    impl BubbleView {
        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            false
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.paint();
        }

        // A click on the bubble dismisses it.
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            with_ui(|ui| ui.dismiss_bubble());
        }
    }
);

impl BubbleView {
    pub fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(RefCell::new(BubbleArt::default()));
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    pub fn set_text(&self, text: Option<(Voice, String)>) {
        self.ivars().borrow_mut().text = text;
    }

    /// Cream paper, a navy crayon outline drawn twice a hair apart (it is
    /// hand-drawn, like her) and a little tail down to her head.
    fn paint(&self) {
        let art = self.ivars().borrow();
        let Some((voice, text)) = &art.text else {
            return;
        };
        let b = self.bounds();
        let (w, h) = (b.size.width, b.size.height);
        let tail = BUBBLE_TAIL;
        let body = NSRect::new(
            NSPoint::new(2.0, tail + 1.0),
            NSSize::new(w - 4.0, h - tail - 3.0),
        );
        let fill = NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.985, 0.94, 0.97);
        let box_path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(body, 10.0, 10.0);
        // The tail, off-centre left: her head is on the left.
        let tx = (w * 0.38).max(18.0).min(w - 24.0);
        let wedge = NSBezierPath::bezierPath();
        wedge.moveToPoint(NSPoint::new(tx - 7.0, tail + 2.5));
        wedge.lineToPoint(NSPoint::new(tx - 2.0, 1.0));
        wedge.lineToPoint(NSPoint::new(tx + 8.0, tail + 2.5));
        fill.setFill();
        box_path.fill();
        wedge.fill();
        for (dx, dy, width, alpha) in [(0.0, 0.0, 1.6, 1.0), (0.7, -0.5, 0.8, 0.45)] {
            NSGraphicsContext::saveGraphicsState_class();
            let xf = NSAffineTransform::transform();
            xf.translateXBy_yBy(dx, dy);
            xf.concat();
            NSColor::colorWithSRGBRed_green_blue_alpha(0.16, 0.20, 0.42, 0.95 * alpha).setStroke();
            box_path.setLineWidth(width);
            box_path.stroke();
            wedge.setLineWidth(width);
            wedge.stroke();
            NSGraphicsContext::restoreGraphicsState_class();
        }
        // Cover the outline where the tail joins the box.
        fill.setFill();
        NSBezierPath::fillRect(NSRect::new(
            NSPoint::new(tx - 5.5, tail + 0.6),
            NSSize::new(12.0, 2.6),
        ));
        let string = bubble_string(text, *voice);
        string.drawInRect(NSRect::new(
            NSPoint::new(BUBBLE_PAD, tail + BUBBLE_PAD - 1.0),
            NSSize::new(w - 2.0 * BUBBLE_PAD, h - tail - 2.0 * BUBBLE_PAD + 2.0),
        ));
    }
}

/// Her handwriting: Chalkboard SE in navy; what she heard, a lighter ink.
pub fn bubble_string(text: &str, voice: Voice) -> Retained<NSAttributedString> {
    let font = NSFont::fontWithName_size(&NSString::from_str("Chalkboard SE"), 12.5)
        .unwrap_or_else(|| NSFont::systemFontOfSize(12.0));
    let ink = match voice {
        Voice::Said => NSColor::colorWithSRGBRed_green_blue_alpha(0.14, 0.17, 0.32, 1.0),
        Voice::Heard => NSColor::colorWithSRGBRed_green_blue_alpha(0.36, 0.40, 0.52, 1.0),
    };
    // SAFETY: both keys take exactly these value types (an NSFont and an
    // NSColor).
    unsafe {
        let keys = [NSFontAttributeName, NSForegroundColorAttributeName];
        let values: [&AnyObject; 2] = [&font, &ink];
        let attributes = NSDictionary::from_slices(&keys, &values);
        NSAttributedString::new_with_attributes(&NSString::from_str(text), &attributes)
    }
}

define_class!(
    /// The timer's and the menu's target, and the «Mascota» submenu's
    /// delegate (it lists the installed pets each time it opens).
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "WawiBodyTarget"]
    pub struct Target;

    unsafe impl NSObjectProtocol for Target {}

    unsafe impl NSMenuDelegate for Target {
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            with_ui(|ui| ui.fill_pets_menu(menu));
        }
    }

    impl Target {
        #[unsafe(method(tick:))]
        fn tick(&self, _timer: &AnyObject) {
            with_ui(|ui| ui.tick());
        }

        #[unsafe(method(menuAction:))]
        fn menu_action(&self, sender: &NSMenuItem) {
            let tag = sender.tag();
            with_ui(|ui| ui.menu_pick(tag));
        }
    }
);

impl Target {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}
