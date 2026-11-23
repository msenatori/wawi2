//! What the Mac says right now, cheaply and without new permissions, and
//! the screenshot itself. Every probe degrades to «could not tell» and
//! never panics.
//!
//! - Screen Recording: `CGPreflightScreenCaptureAccess` (asks nothing).
//! - Locked screen: `CGSessionCopyCurrentDictionary`; asleep display:
//!   `CGDisplayIsAsleep`; screensaver: a `ScreenSaverEngine` process.
//! - The front window: `CGWindowListCopyWindowInfo`, only the owner's app
//!   name and the bounds (no titles, so no permission): full screen when it
//!   covers a whole display; a meeting when the app is FaceTime, Zoom,
//!   Teams or Webex. A Zoom call also has its `CptHost` process.
//! - The camera: CoreMediaIO, «is any camera running somewhere».
//! - Someone is there: `HIDIdleTime` from `ioreg`.
//!
//! The microphone is not a signal: Wawi herself keeps it open.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::momento::Senales;

/// Apps whose window in front means a call.
const MEETING_APPS: &[&str] = &[
    "facetime",
    "zoom.us",
    "microsoft teams",
    "microsoft teams (work or school)",
    "webex",
    "cisco webex meetings",
];

fn process_running(name: &str) -> bool {
    Command::new("/usr/bin/pgrep")
        .args(["-x", name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Seconds since the last keyboard or mouse event, from `ioreg`.
fn idle_seconds() -> Option<u64> {
    let out = Command::new("/usr/sbin/ioreg")
        .args(["-c", "IOHIDSystem", "-d", "4"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| l.contains("\"HIDIdleTime\""))?;
    let ns: u64 = line.rsplit('=').next()?.trim().parse().ok()?;
    Some(ns / 1_000_000_000)
}

pub fn senales() -> Senales {
    let front = ffi::front_window();
    Senales {
        permiso: ffi::screen_permission(),
        bloqueada: ffi::session_locked(),
        salvapantallas: process_running("ScreenSaverEngine"),
        dormida: ffi::display_asleep(),
        pantalla_completa: front.as_ref().is_some_and(|f| f.1),
        reunion: front
            .as_ref()
            .is_some_and(|f| MEETING_APPS.contains(&f.0.to_lowercase().as_str()))
            || process_running("CptHost"),
        camara: ffi::camera_running(),
        inactivo: idle_seconds(),
    }
}

/// Only what `mirar_pantalla` needs before capturing: fast.
pub fn senales_para_mirar() -> Senales {
    Senales {
        permiso: ffi::screen_permission(),
        bloqueada: ffi::session_locked(),
        salvapantallas: process_running("ScreenSaverEngine"),
        dormida: ffi::display_asleep(),
        ..Senales::default()
    }
}

fn private_folder(dir: &Path) -> Result<std::path::PathBuf, String> {
    let folder = dir.join(format!("wawi-curiosidad-{}", std::process::id()));
    std::fs::create_dir_all(&folder).map_err(|e| format!("no pude crear la carpeta: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700));
    }
    Ok(folder)
}

/// `src` shrunk to at most `max_edge` pixels on the long side, as a JPEG
/// of `quality`, written to `out`.
fn shrink(src: &Path, out: &Path, max_edge: u32, quality: u32) -> Result<(), String> {
    let status = Command::new("/usr/bin/sips")
        .args(["-Z", &max_edge.to_string(), "-s", "format", "jpeg"])
        .args(["-s", "formatOptions", &quality.to_string()])
        .arg(src)
        .arg("--out")
        .arg(out)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("no pude correr sips: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("sips no achicó la captura".into())
    }
}

/// Takes the main display, shrinks it to at most `max_edge` pixels on the
/// long side as a JPEG of `quality`, and returns the bytes. The files live
/// in a private folder under `dir` only for the few hundred milliseconds
/// this takes, and are deleted on every path.
pub fn capturar(dir: &Path, max_edge: u32, quality: u32) -> Result<Vec<u8>, String> {
    let folder = private_folder(dir)?;
    let raw = folder.join("pantalla.jpg");
    let small = folder.join("chica.jpg");
    let result = (|| {
        let shot = Command::new("/usr/sbin/screencapture")
            // -x: no sound; -m: the main display only; -t: the format.
            .args(["-x", "-m", "-t", "jpg"])
            .arg(&raw)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("no pude correr screencapture: {e}"))?;
        if !shot.success() || !raw.exists() {
            return Err("screencapture no sacó la captura".to_owned());
        }
        shrink(&raw, &small, max_edge, quality)?;
        std::fs::read(&small).map_err(|e| format!("no pude leer la captura: {e}"))
    })();
    let _ = std::fs::remove_file(&raw);
    let _ = std::fs::remove_file(&small);
    let _ = std::fs::remove_dir(&folder);
    result
}

/// The same shrinking for a file that is not the screen (to measure).
pub fn achicar(src: &Path, dir: &Path, max_edge: u32, quality: u32) -> Result<Vec<u8>, String> {
    let folder = private_folder(dir)?;
    let small = folder.join("chica.jpg");
    let result = shrink(src, &small, max_edge, quality)
        .and_then(|()| std::fs::read(&small).map_err(|e| format!("no pude leer: {e}")));
    let _ = std::fs::remove_file(&small);
    let _ = std::fs::remove_dir(&folder);
    result
}

#[cfg(target_os = "macos")]
mod ffi {
    use std::ffi::{CStr, CString, c_char, c_void};

    type CFTypeRef = *const c_void;
    type CFIndex = isize;

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

    #[repr(C)]
    struct CmioAddress {
        selector: u32,
        scope: u32,
        element: u32,
    }

    const UTF8: u32 = 0x0800_0100;
    const SINT32: CFIndex = 3;
    const ON_SCREEN_ONLY: u32 = 1;
    const EXCLUDE_DESKTOP: u32 = 1 << 4;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: CFTypeRef);
        fn CFArrayGetCount(array: CFTypeRef) -> CFIndex;
        fn CFArrayGetValueAtIndex(array: CFTypeRef, index: CFIndex) -> CFTypeRef;
        fn CFDictionaryGetValue(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
        fn CFNumberGetValue(number: CFTypeRef, kind: CFIndex, out: *mut c_void) -> bool;
        fn CFBooleanGetValue(boolean: CFTypeRef) -> bool;
        fn CFGetTypeID(cf: CFTypeRef) -> usize;
        fn CFBooleanGetTypeID() -> usize;
        fn CFStringCreateWithCString(alloc: CFTypeRef, text: *const c_char, enc: u32) -> CFTypeRef;
        fn CFStringGetCString(s: CFTypeRef, buf: *mut c_char, size: CFIndex, enc: u32) -> bool;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
        fn CGSessionCopyCurrentDictionary() -> CFTypeRef;
        fn CGMainDisplayID() -> u32;
        fn CGDisplayIsAsleep(display: u32) -> u32;
        fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CFTypeRef;
        fn CGRectMakeWithDictionaryRepresentation(dict: CFTypeRef, rect: *mut CGRect) -> bool;
        fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
        fn CGDisplayBounds(display: u32) -> CGRect;
        static kCGWindowLayer: CFTypeRef;
        static kCGWindowBounds: CFTypeRef;
        static kCGWindowOwnerName: CFTypeRef;
    }

    #[link(name = "CoreMediaIO", kind = "framework")]
    unsafe extern "C" {
        fn CMIOObjectGetPropertyDataSize(
            object: u32,
            address: *const CmioAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: *mut u32,
        ) -> i32;
        fn CMIOObjectGetPropertyData(
            object: u32,
            address: *const CmioAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: u32,
            used: *mut u32,
            data: *mut c_void,
        ) -> i32;
    }

    fn fourcc(code: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*code)
    }

    pub fn screen_permission() -> bool {
        // SAFETY: no arguments; it only reads the TCC state.
        unsafe { CGPreflightScreenCaptureAccess() }
    }

    pub fn display_asleep() -> bool {
        // SAFETY: plain calls with a display id.
        unsafe { CGDisplayIsAsleep(CGMainDisplayID()) != 0 }
    }

    pub fn session_locked() -> bool {
        // SAFETY: the dictionary is ours (Copy) and released; the key is a
        // CFString we create and release; the value is borrowed from it.
        unsafe {
            let session = CGSessionCopyCurrentDictionary();
            if session.is_null() {
                return false;
            }
            let key = CString::new("CGSSessionScreenIsLocked").unwrap_or_default();
            let key = CFStringCreateWithCString(std::ptr::null(), key.as_ptr(), UTF8);
            let value = if key.is_null() {
                std::ptr::null()
            } else {
                CFDictionaryGetValue(session, key)
            };
            let locked = !value.is_null()
                && CFGetTypeID(value) == CFBooleanGetTypeID()
                && CFBooleanGetValue(value);
            if !key.is_null() {
                CFRelease(key);
            }
            CFRelease(session);
            locked
        }
    }

    fn cf_string(s: CFTypeRef) -> String {
        if s.is_null() {
            return String::new();
        }
        let mut buf = [0 as c_char; 256];
        // SAFETY: the buffer is ours and its size is passed.
        unsafe {
            if CFStringGetCString(s, buf.as_mut_ptr(), buf.len() as CFIndex, UTF8) {
                CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
            } else {
                String::new()
            }
        }
    }

    fn displays() -> Vec<CGRect> {
        let mut ids = [0u32; 16];
        let mut count = 0u32;
        // SAFETY: the array and the count are ours, sized as passed.
        unsafe {
            if CGGetActiveDisplayList(16, ids.as_mut_ptr(), &mut count) != 0 {
                return Vec::new();
            }
            ids[..count as usize]
                .iter()
                .map(|id| CGDisplayBounds(*id))
                .collect()
        }
    }

    /// The app name of the front normal window, and whether it covers a
    /// whole display.
    pub fn front_window() -> Option<(String, bool)> {
        let screens = displays();
        // SAFETY: the list is ours (Copy) and released at the end; every
        // value read from it is borrowed and checked for null.
        unsafe {
            let list = CGWindowListCopyWindowInfo(ON_SCREEN_ONLY | EXCLUDE_DESKTOP, 0);
            if list.is_null() {
                return None;
            }
            let mut found = None;
            for i in 0..CFArrayGetCount(list) {
                let window = CFArrayGetValueAtIndex(list, i);
                if window.is_null() {
                    continue;
                }
                let layer = CFDictionaryGetValue(window, kCGWindowLayer);
                let mut level: i32 = -1;
                if layer.is_null()
                    || !CFNumberGetValue(layer, SINT32, (&mut level as *mut i32).cast())
                    || level != 0
                {
                    continue;
                }
                let mut rect = CGRect::default();
                let bounds = CFDictionaryGetValue(window, kCGWindowBounds);
                let has_rect =
                    !bounds.is_null() && CGRectMakeWithDictionaryRepresentation(bounds, &mut rect);
                let full = has_rect
                    && screens.iter().any(|s| {
                        (s.origin.x - rect.origin.x).abs() < 1.0
                            && (s.origin.y - rect.origin.y).abs() < 1.0
                            && (s.size.width - rect.size.width).abs() < 1.0
                            && (s.size.height - rect.size.height).abs() < 1.0
                    });
                let name = cf_string(CFDictionaryGetValue(window, kCGWindowOwnerName));
                found = Some((name, full));
                break;
            }
            CFRelease(list);
            found
        }
    }

    /// Whether any camera is running in any app.
    pub fn camera_running() -> bool {
        let devices = CmioAddress {
            selector: fourcc(b"dev#"),
            scope: fourcc(b"glob"),
            element: 0,
        };
        // SAFETY: sizes are asked first and the buffers sized from them.
        unsafe {
            let mut size = 0u32;
            if CMIOObjectGetPropertyDataSize(1, &devices, 0, std::ptr::null(), &mut size) != 0 {
                return false;
            }
            let n = size as usize / std::mem::size_of::<u32>();
            let mut ids = vec![0u32; n];
            let mut used = 0u32;
            if n == 0
                || CMIOObjectGetPropertyData(
                    1,
                    &devices,
                    0,
                    std::ptr::null(),
                    size,
                    &mut used,
                    ids.as_mut_ptr().cast(),
                ) != 0
            {
                return false;
            }
            let running = CmioAddress {
                selector: fourcc(b"gone"),
                scope: fourcc(b"glob"),
                element: 0,
            };
            ids.iter().any(|id| {
                let mut on = 0u32;
                let mut used = 0u32;
                CMIOObjectGetPropertyData(
                    *id,
                    &running,
                    0,
                    std::ptr::null(),
                    4,
                    &mut used,
                    (&mut on as *mut u32).cast(),
                ) == 0
                    && on != 0
            })
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod ffi {
    pub fn screen_permission() -> bool {
        false
    }
    pub fn display_asleep() -> bool {
        false
    }
    pub fn session_locked() -> bool {
        false
    }
    pub fn front_window() -> Option<(String, bool)> {
        None
    }
    pub fn camera_running() -> bool {
        false
    }
}
