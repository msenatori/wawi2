//! The secure key dialog: an `NSAlert` with an `NSSecureTextField`, titled
//! with the plugin's name, with «Guardar», «Cancelar» and, when there is a
//! help URL, «Dónde la consigo» (opens it and shows the dialog again).
//!
//! It runs from the main queue, outside the UI's borrow, so her animation
//! keeps going while it is open. The typed text goes to the reply and is
//! cleared from the field; it is never logged.

use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertThirdButtonReturn, NSApplication, NSSecureTextField,
    NSWorkspace,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSURL};

use crate::SecretAsk;

pub(crate) fn ask(mtm: MainThreadMarker, ask: SecretAsk) {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(&ask.title));
    alert.setInformativeText(&NSString::from_str(&ask.message));
    alert.addButtonWithTitle(&NSString::from_str("Guardar"));
    alert.addButtonWithTitle(&NSString::from_str("Cancelar"));
    let help = ask
        .help_url
        .as_deref()
        .filter(|url| url.starts_with("https://"));
    if help.is_some() {
        alert.addButtonWithTitle(&NSString::from_str("Dónde la consigo"));
    }
    let field = NSSecureTextField::initWithFrame(
        NSSecureTextField::alloc(mtm),
        NSRect::new(NSPoint::ZERO, NSSize::new(300.0, 24.0)),
    );
    alert.setAccessoryView(Some(&field));
    alert.window().setInitialFirstResponder(Some(&field));
    // An accessory app: bring the dialog to the front.
    #[allow(deprecated)]
    NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    loop {
        let pressed = alert.runModal();
        if pressed == NSAlertFirstButtonReturn {
            let text = field.stringValue().to_string();
            field.setStringValue(&NSString::from_str(""));
            (ask.reply.0)(Some(text));
            return;
        }
        if pressed == NSAlertThirdButtonReturn
            && let Some(url) = help
            && let Some(url) = NSURL::URLWithString(&NSString::from_str(url))
        {
            NSWorkspace::sharedWorkspace().openURL(&url);
            continue;
        }
        (ask.reply.0)(None);
        return;
    }
}
