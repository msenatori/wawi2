//! The other apps' windows, for her body to stand on: read on a thread of
//! its own, a few times a second at most, never on the voice thread nor the
//! UI thread.
//!
//! `CGWindowListCopyWindowInfo` with on-screen windows only, no desktop
//! elements. Bounds, owner, process and layer need no permission; window
//! titles would need Screen Recording, and are never read. The pure part
//! (which windows count, the flip to AppKit coordinates) is
//! [`crate::body::world::parse_windows`].
//!
//! The list is compared with the last one: the UI is woken only when it
//! changed (a window moved, opened or closed), so her perch going away makes
//! her fall within a quarter of a second, and an unchanged desktop costs the
//! UI nothing.
//!
//! The whole list costs about 3 ms, so it is read four times a second only
//! while she moves. Sitting on a window, only that window is asked about
//! (much cheaper), and the whole list is read when it changed and every few
//! seconds. On the floor nothing can pull it from under her: every few
//! seconds.

use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::body::world::{RawWindow, Win, parse_windows};

/// How often the list is read while her footing matters (on a window,
/// moving, falling, held) and while it does not (resting on a floor).
pub const FAST: Duration = Duration::from_millis(250);
pub const SLOW: Duration = Duration::from_secs(5);

/// What her footing needs from the thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Watch {
    /// Moving, falling or held: the whole list, often.
    Moving,
    /// Sitting on this window: that window often, the list now and then.
    Perch(u32),
    /// On a floor, or hidden: now and then.
    Resting,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

type CFTypeRef = *const c_void;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CFTypeRef;
    fn CGMainDisplayID() -> u32;
    fn CGDisplayBounds(display: u32) -> CGRect;
    fn CGRectMakeWithDictionaryRepresentation(dict: CFTypeRef, rect: *mut CGRect) -> bool;
    static kCGWindowNumber: CFTypeRef;
    static kCGWindowLayer: CFTypeRef;
    static kCGWindowOwnerPID: CFTypeRef;
    static kCGWindowOwnerName: CFTypeRef;
    static kCGWindowAlpha: CFTypeRef;
    static kCGWindowBounds: CFTypeRef;
    static kCGWindowIsOnscreen: CFTypeRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFArrayGetCount(array: CFTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CFTypeRef, index: isize) -> CFTypeRef;
    fn CFDictionaryGetValue(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    fn CFNumberGetValue(number: CFTypeRef, kind: isize, out: *mut c_void) -> bool;
    fn CFStringGetCString(s: CFTypeRef, buf: *mut c_char, len: isize, encoding: u32) -> bool;
    fn CFRelease(object: CFTypeRef);
    fn CFBooleanGetValue(boolean: CFTypeRef) -> bool;
}

const ON_SCREEN_ONLY: u32 = 1 << 0;
const INCLUDING_WINDOW: u32 = 1 << 3;
const EXCLUDE_DESKTOP: u32 = 1 << 4;
const NUMBER_SINT64: isize = 4;
const NUMBER_FLOAT64: isize = 6;
const UTF8: u32 = 0x0800_0100;

/// # Safety
/// `dict` is a live CFDictionary and `key` a CFString constant.
unsafe fn number(dict: CFTypeRef, key: CFTypeRef, float: bool) -> Option<f64> {
    // SAFETY: the caller's contract; a missing key is null.
    let value = unsafe { CFDictionaryGetValue(dict, key) };
    if value.is_null() {
        return None;
    }
    if float {
        let mut out = 0.0f64;
        // SAFETY: `out` is a valid f64 for kCFNumberFloat64Type.
        unsafe { CFNumberGetValue(value, NUMBER_FLOAT64, (&raw mut out).cast()) }.then_some(out)
    } else {
        let mut out = 0i64;
        // SAFETY: `out` is a valid i64 for kCFNumberSInt64Type.
        unsafe { CFNumberGetValue(value, NUMBER_SINT64, (&raw mut out).cast()) }
            .then_some(out as f64)
    }
}

/// # Safety
/// As [`number`].
unsafe fn string(dict: CFTypeRef, key: CFTypeRef) -> String {
    // SAFETY: the caller's contract.
    let value = unsafe { CFDictionaryGetValue(dict, key) };
    if value.is_null() {
        return String::new();
    }
    let mut buf = [0 as c_char; 128];
    // SAFETY: the buffer and its length match; the result is NUL-ended.
    if unsafe { CFStringGetCString(value, buf.as_mut_ptr(), buf.len() as isize, UTF8) } {
        // SAFETY: CFStringGetCString wrote a NUL-terminated string.
        unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    } else {
        String::new()
    }
}

/// The system's list of on-screen windows, front to back. Thread-safe.
fn read_raw() -> Vec<RawWindow> {
    let mut out = Vec::new();
    // SAFETY: a plain CoreGraphics call; the array is released below.
    let list = unsafe { CGWindowListCopyWindowInfo(ON_SCREEN_ONLY | EXCLUDE_DESKTOP, 0) };
    if list.is_null() {
        return out;
    }
    // SAFETY: `list` is a CFArray of CFDictionary we own until CFRelease;
    // the keys are CoreGraphics' own constants.
    unsafe {
        for i in 0..CFArrayGetCount(list) {
            let dict = CFArrayGetValueAtIndex(list, i);
            if dict.is_null() {
                continue;
            }
            let bounds = CFDictionaryGetValue(dict, kCGWindowBounds);
            let mut rect = CGRect::default();
            if bounds.is_null() || !CGRectMakeWithDictionaryRepresentation(bounds, &mut rect) {
                continue;
            }
            out.push(RawWindow {
                id: number(dict, kCGWindowNumber, false).unwrap_or(0.0) as u32,
                layer: number(dict, kCGWindowLayer, false).unwrap_or(0.0) as i64,
                pid: number(dict, kCGWindowOwnerPID, false).unwrap_or(-1.0) as i64,
                owner: string(dict, kCGWindowOwnerName),
                alpha: number(dict, kCGWindowAlpha, true).unwrap_or(1.0),
                bounds: (
                    rect.origin.x,
                    rect.origin.y,
                    rect.size.width,
                    rect.size.height,
                ),
            });
        }
        CFRelease(list);
    }
    out
}

/// One window's bounds (Quartz coordinates), if it is still on screen.
fn read_one(id: u32) -> Option<(f64, f64, f64, f64)> {
    // SAFETY: a plain CoreGraphics call; the array is released below.
    let list = unsafe { CGWindowListCopyWindowInfo(INCLUDING_WINDOW, id) };
    if list.is_null() {
        return None;
    }
    let mut out = None;
    // SAFETY: as in `read_raw`.
    unsafe {
        if CFArrayGetCount(list) > 0 {
            let dict = CFArrayGetValueAtIndex(list, 0);
            let shown = CFDictionaryGetValue(dict, kCGWindowIsOnscreen);
            let bounds = CFDictionaryGetValue(dict, kCGWindowBounds);
            let mut rect = CGRect::default();
            if !shown.is_null()
                && CFBooleanGetValue(shown)
                && !bounds.is_null()
                && CGRectMakeWithDictionaryRepresentation(bounds, &mut rect)
            {
                out = Some((
                    rect.origin.x,
                    rect.origin.y,
                    rect.size.width,
                    rect.size.height,
                ));
            }
        }
        CFRelease(list);
    }
    out
}

/// The primary display's height, which flips Quartz's y-down into AppKit's
/// y-up. Thread-safe.
fn primary_height() -> f64 {
    // SAFETY: plain CoreGraphics calls.
    unsafe { CGDisplayBounds(CGMainDisplayID()).size.height }
}

struct Shared {
    windows: Mutex<Arc<Vec<Win>>>,
    version: AtomicU64,
    /// The [`Watch`], packed: 0 resting, 1 moving, 2 + id perched.
    watch: AtomicU64,
    stop: AtomicBool,
    nudge: (Mutex<bool>, Condvar),
}

/// The window list, kept fresh by its own thread.
pub struct Desktop {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Desktop {
    /// Starts the thread. `changed` is called (from it) when the list
    /// changes: the UI wakes up for her to notice.
    pub fn start(changed: Arc<dyn Fn() + Send + Sync>) -> Self {
        let shared = Arc::new(Shared {
            windows: Mutex::new(Arc::new(Vec::new())),
            version: AtomicU64::new(0),
            watch: AtomicU64::new(1),
            stop: AtomicBool::new(false),
            nudge: (Mutex::new(false), Condvar::new()),
        });
        let inner = shared.clone();
        let thread = std::thread::Builder::new()
            .name("wawi-ventanas".into())
            .spawn(move || poll(&inner, &*changed))
            .ok();
        Self { shared, thread }
    }

    /// The version and the windows (front to back). Cheap: an atomic and,
    /// only when it changed, an `Arc` clone.
    pub fn version(&self) -> u64 {
        self.shared.version.load(Ordering::Acquire)
    }

    pub fn windows(&self) -> Arc<Vec<Win>> {
        self.shared
            .windows
            .lock()
            .map(|w| w.clone())
            .unwrap_or_default()
    }

    /// What her footing needs now. Starting to move reads the list at
    /// once.
    pub fn watch(&self, watch: Watch) {
        let packed = match watch {
            Watch::Resting => 0,
            Watch::Moving => 1,
            Watch::Perch(id) => 2 + u64::from(id),
        };
        let old = self.shared.watch.swap(packed, Ordering::Relaxed);
        if old != packed && packed == 1 {
            self.nudge();
        }
    }

    fn nudge(&self) {
        let (lock, cv) = &self.shared.nudge;
        if let Ok(mut flag) = lock.lock() {
            *flag = true;
            cv.notify_one();
        }
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.nudge();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn poll(shared: &Shared, changed: &(dyn Fn() + Send + Sync)) {
    let pid = i64::from(std::process::id());
    let mut last: Vec<Win> = Vec::new();
    let mut first = true;
    let timing = super::timing().is_some();
    let (mut polls, mut spent) = (0u32, Duration::ZERO);
    let mut last_full = Instant::now();
    while !shared.stop.load(Ordering::Relaxed) {
        let watch = shared.watch.load(Ordering::Relaxed);
        // Perched: only her window, unless it moved or went away, and
        // the whole list now and then (a window may have come over it).
        if watch >= 2 && !first && last_full.elapsed() < SLOW {
            let id = (watch - 2) as u32;
            let height = primary_height();
            let now = read_one(id).map(|(x, y, w, h)| (x, height - y - h, w, h));
            let before = last.iter().find(|w| w.id == id).map(|w| w.rect);
            if now.is_some() && now == before {
                if !wait(shared, FAST) {
                    return;
                }
                continue;
            }
        }
        let started = Instant::now();
        last_full = started;
        let windows = parse_windows(&read_raw(), pid, primary_height());
        if timing {
            polls += 1;
            spent += started.elapsed();
            if polls % 20 == 0 {
                eprintln!(
                    "TIMING window_list_ms={:.2} polls={polls}",
                    spent.as_secs_f64() * 1000.0 / f64::from(polls)
                );
            }
        }
        if first || windows != last {
            first = false;
            last = windows.clone();
            if let Ok(mut slot) = shared.windows.lock() {
                *slot = Arc::new(windows);
            }
            shared.version.fetch_add(1, Ordering::AcqRel);
            changed();
        }
        let pause = if shared.watch.load(Ordering::Relaxed) == 0 {
            SLOW
        } else {
            FAST
        };
        if !wait(shared, pause) {
            return;
        }
    }
}

/// Sleeps `pause`, or until nudged. False if the lock is broken.
fn wait(shared: &Shared, pause: Duration) -> bool {
    let (lock, cv) = &shared.nudge;
    let Ok(mut flag) = lock.lock() else {
        return false;
    };
    if !*flag {
        flag = match cv.wait_timeout(flag, pause) {
            Ok((flag, _)) => flag,
            Err(_) => return false,
        };
    }
    *flag = false;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a read costs on this Mac: `cargo test -p wawi-body --
    /// --ignored window_reads --nocapture`.
    #[test]
    #[ignore = "reads the real window list; prints timings"]
    fn window_reads() {
        let pid = i64::from(std::process::id());
        let started = Instant::now();
        let mut windows = Vec::new();
        for _ in 0..50 {
            windows = parse_windows(&read_raw(), pid, primary_height());
        }
        let full = started.elapsed().as_secs_f64() * 1000.0 / 50.0;
        let id = windows.first().map_or(0, |w| w.id);
        let started = Instant::now();
        for _ in 0..50 {
            let _ = read_one(id);
        }
        let one = started.elapsed().as_secs_f64() * 1000.0 / 50.0;
        println!(
            "whole list {full:.2} ms ({} windows), one window {one:.3} ms",
            windows.len()
        );
    }
}
