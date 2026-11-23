//! Right Command held = talk, like Wawi 1 (which used pynput's `cmd_r`, itself
//! a CGEventTap). Here: a listen-only CGEventTap for `flagsChanged`, on its
//! own thread with its own run loop. Listen-only means it can never swallow or
//! change a key press.
//!
//! The state comes from the device-dependent "right Command" flag bit, not
//! from counting presses, so a missed event cannot leave the key stuck: the
//! next modifier change corrects it, and after macOS disables the tap (it
//! does when a callback is slow) it is re-enabled and re-synced from the
//! current modifier state.

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::null_mut;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use crate::permissions::launching_app;

type CFMachPortRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CFStringRef = *const c_void;
type CGEventRef = *mut c_void;
type CGEventTapProxy = *mut c_void;
type CGEventMask = u64;
type CGEventFlags = u64;
type CGEventType = u32;

type TapCallback =
    extern "C" fn(CGEventTapProxy, CGEventType, CGEventRef, *mut c_void) -> CGEventRef;

const SESSION_EVENT_TAP: u32 = 1; // kCGSessionEventTap
const HEAD_INSERT: u32 = 0; // kCGHeadInsertEventTap
const LISTEN_ONLY: u32 = 1; // kCGEventTapOptionListenOnly
const FLAGS_CHANGED: CGEventType = 12; // kCGEventFlagsChanged
const TAP_DISABLED_BY_TIMEOUT: CGEventType = 0xFFFF_FFFE;
const TAP_DISABLED_BY_USER_INPUT: CGEventType = 0xFFFF_FFFF;
const COMBINED_SESSION_STATE: i32 = 0; // kCGEventSourceStateCombinedSessionState

/// kCGEventFlagMaskCommand: either Command key.
const FLAG_COMMAND: CGEventFlags = 0x0010_0000;
/// NX_DEVICERCMDKEYMASK, from IOKit's IOLLEvent.h: the right one.
const FLAG_RIGHT_COMMAND: CGEventFlags = 0x0000_0010;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: CGEventMask,
        callback: TapCallback,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetFlags(event: CGEventRef) -> CGEventFlags;
    fn CGEventSourceFlagsState(state: i32) -> CGEventFlags;
    fn CGPreflightListenEventAccess() -> bool;
    fn CGRequestListenEventAccess() -> bool;
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopCommonModes: CFStringRef;
    fn CFMachPortCreateRunLoopSource(
        allocator: *const c_void,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;
    fn CFMachPortInvalidate(port: CFMachPortRef);
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(run_loop: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRun();
    fn CFRunLoopStop(run_loop: CFRunLoopRef);
    fn CFRetain(cf: *const c_void) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

/// Whether the right Command key is down, from a modifier flags word.
pub fn right_command_down(flags: u64) -> bool {
    flags & FLAG_RIGHT_COMMAND != 0 && flags & FLAG_COMMAND != 0
}

/// Turns a stream of flag words into key-down/key-up changes.
#[derive(Debug, Default)]
pub struct KeyTracker {
    down: bool,
}

impl KeyTracker {
    /// `Some(down)` when the key changed.
    pub fn update(&mut self, flags: u64) -> Option<bool> {
        let down = right_command_down(flags);
        (down != self.down).then(|| {
            self.down = down;
            down
        })
    }
}

#[derive(Debug)]
pub enum HotkeyError {
    /// The launching app lacks Input Monitoring (or Accessibility).
    NoPermission,
    Failed(String),
}

impl std::fmt::Display for HotkeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HotkeyError::NoPermission => write!(
                f,
                "No puedo escuchar la tecla ⌘ derecha: falta el permiso de Monitoreo de entrada. \
                 Abrí Ajustes del Sistema > Privacidad y seguridad > Monitoreo de entrada y \
                 habilitá {} (si no aparece, agregala con +; también sirve habilitarla en \
                 Accesibilidad). Después cerrala del todo y volvé a abrirla.",
                launching_app()
            ),
            HotkeyError::Failed(reason) => {
                write!(f, "no pude escuchar la tecla ⌘ derecha: {reason}")
            }
        }
    }
}

impl std::error::Error for HotkeyError {}

/// True if this process may listen to the keyboard.
pub fn input_monitoring_granted() -> bool {
    // SAFETY: plain queries with no arguments.
    unsafe { CGPreflightListenEventAccess() || AXIsProcessTrusted() }
}

struct TapState {
    port: CFMachPortRef,
    tracker: KeyTracker,
    on_change: Box<dyn FnMut(bool) + Send>,
}

impl TapState {
    fn deliver(&mut self, flags: CGEventFlags) {
        if let Some(down) = self.tracker.update(flags) {
            let on_change = &mut self.on_change;
            // A panic must not unwind into CoreGraphics.
            let _ = catch_unwind(AssertUnwindSafe(|| on_change(down)));
        }
    }
}

extern "C" fn tap_callback(
    _proxy: CGEventTapProxy,
    kind: CGEventType,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    // SAFETY: `user_info` is the TapState owned by the tap thread, alive for
    // as long as the run loop runs, and only touched from that thread.
    let state = unsafe { &mut *user_info.cast::<TapState>() };
    match kind {
        TAP_DISABLED_BY_TIMEOUT | TAP_DISABLED_BY_USER_INPUT => {
            // SAFETY: the port is valid until the thread invalidates it.
            unsafe {
                CGEventTapEnable(state.port, true);
                state.deliver(CGEventSourceFlagsState(COMBINED_SESSION_STATE));
            }
        }
        // SAFETY: a flagsChanged event from the tap is a valid CGEvent.
        FLAGS_CHANGED => state.deliver(unsafe { CGEventGetFlags(event) }),
        _ => {}
    }
    event
}

/// Raw run loop pointer, retained, to stop it from another thread.
/// `CFRunLoopStop` is documented as safe to call from any thread.
struct RunLoop(CFRunLoopRef);
// SAFETY: only used for CFRunLoopStop and CFRelease, both thread-safe.
unsafe impl Send for RunLoop {}

/// Listens to the right Command key until dropped.
pub struct RightCommandKey {
    run_loop: RunLoop,
    thread: Option<JoinHandle<()>>,
}

impl RightCommandKey {
    /// Calls `on_change(true)` when the key goes down and `on_change(false)`
    /// when it goes up, on the listener thread. Keep it quick.
    ///
    /// Without permission, asks the system to list the launching app under
    /// Input Monitoring and returns [`HotkeyError::NoPermission`], whose
    /// message says what to enable.
    pub fn spawn(on_change: impl FnMut(bool) + Send + 'static) -> Result<Self, HotkeyError> {
        if !input_monitoring_granted() {
            // SAFETY: no arguments; shows the system prompt at most once.
            unsafe { CGRequestListenEventAccess() };
            return Err(HotkeyError::NoPermission);
        }
        let (ready_tx, ready_rx) = mpsc::channel::<Result<RunLoop, HotkeyError>>();
        let on_change: Box<dyn FnMut(bool) + Send> = Box::new(on_change);
        let thread = thread::Builder::new()
            .name("wawi-hotkey".into())
            .spawn(move || run_tap(on_change, ready_tx))
            .map_err(|e| HotkeyError::Failed(e.to_string()))?;
        match ready_rx.recv() {
            Ok(Ok(run_loop)) => Ok(Self {
                run_loop,
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => Err(HotkeyError::Failed("el hilo del teclado terminó".into())),
        }
    }
}

fn run_tap(
    on_change: Box<dyn FnMut(bool) + Send>,
    ready: mpsc::Sender<Result<RunLoop, HotkeyError>>,
) {
    let mut state = Box::new(TapState {
        port: null_mut(),
        tracker: KeyTracker::default(),
        on_change,
    });
    let user_info = (&mut *state as *mut TapState).cast::<c_void>();
    // SAFETY: standard CGEventTap setup on this thread's run loop. `state`
    // outlives the run loop, and the port and source are released below.
    unsafe {
        let port = CGEventTapCreate(
            SESSION_EVENT_TAP,
            HEAD_INSERT,
            LISTEN_ONLY,
            1 << FLAGS_CHANGED,
            tap_callback,
            user_info,
        );
        if port.is_null() {
            CGRequestListenEventAccess();
            let _ = ready.send(Err(HotkeyError::NoPermission));
            return;
        }
        state.port = port;
        let source = CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0);
        if source.is_null() {
            CFMachPortInvalidate(port);
            CFRelease(port);
            let _ = ready.send(Err(HotkeyError::Failed(
                "no se pudo crear la fuente del run loop".into(),
            )));
            return;
        }
        let run_loop = CFRunLoopGetCurrent();
        CFRunLoopAddSource(run_loop, source, kCFRunLoopCommonModes);
        CGEventTapEnable(port, true);
        // Start from the real state, in case the key is already down.
        state
            .tracker
            .update(CGEventSourceFlagsState(COMBINED_SESSION_STATE));
        CFRetain(run_loop);
        if ready.send(Ok(RunLoop(run_loop))).is_err() {
            CFRelease(run_loop);
        } else {
            CFRunLoopRun();
        }
        CGEventTapEnable(port, false);
        CFMachPortInvalidate(port);
        CFRelease(source);
        CFRelease(port);
    }
    drop(state);
}

impl Drop for RightCommandKey {
    fn drop(&mut self) {
        // SAFETY: the run loop was retained for us; stopping is thread-safe.
        unsafe {
            CFRunLoopStop(self.run_loop.0);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // SAFETY: balances the CFRetain in `run_tap`.
        unsafe { CFRelease(self.run_loop.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_right_command_key_counts() {
        assert!(right_command_down(FLAG_COMMAND | FLAG_RIGHT_COMMAND));
        // Left Command: the generic bit plus NX_DEVICELCMDKEYMASK (0x08).
        assert!(!right_command_down(FLAG_COMMAND | 0x08));
        assert!(!right_command_down(0));
        // Right Option (0x40) with the Option mask.
        assert!(!right_command_down(0x0008_0000 | 0x40));
    }

    #[test]
    fn tracker_reports_changes_only() {
        let mut key = KeyTracker::default();
        let right = FLAG_COMMAND | FLAG_RIGHT_COMMAND;
        assert_eq!(key.update(right), Some(true));
        // Left Command goes down too: still down, nothing new.
        assert_eq!(key.update(right | 0x08), None);
        assert_eq!(key.update(FLAG_COMMAND | 0x08), Some(false));
        assert_eq!(key.update(0), None);
        assert_eq!(key.update(right), Some(true));
        assert_eq!(key.update(0), Some(false));
    }
}
