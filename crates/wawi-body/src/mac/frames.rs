//! Decoding the pet's pictures without slowing anything down.
//!
//! - Lazily: a picture is decoded when a clip that shows it is about to
//!   play (the animator says which, [`crate::anim::Animator::wanted`]), and
//!   the look the dialogue goes to next while the current one shows
//!   (listening while idle, thinking while listening, …). Only idle is
//!   decoded before the first frame.
//! - Off the main thread and never on the voice thread: one loader thread
//!   reads the PNG and draws it once into a 1×1 bitmap, which makes ImageIO
//!   decode the pixels and cache them with the image; the main thread only
//!   wraps it. (Asking the rep for `bitmapData` would decode too, but keeps
//!   a second copy of the pixels once drawn: measured, +9 MB for Wawi.)
//! - Capped: decoded pictures are kept up to [`CAP_BYTES`]; past that the
//!   least recently shown go first (never one the current clips need).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use objc2::AnyThread;
use objc2::rc::{Retained, autoreleasepool};
use objc2_app_kit::{NSBitmapImageRep, NSImage};
use objc2_foundation::{NSData, NSSize};

/// Decoded pictures kept in memory, at most (Wawi's whole dialogue set plus
/// her idle life is about 10 MB).
pub(crate) const CAP_BYTES: usize = 48 * 1024 * 1024;

/// A decoded bitmap on its way from the loader to the main thread.
pub(crate) struct Decoded(Retained<NSBitmapImageRep>);

// SAFETY: an NSBitmapImageRep may be created on a secondary thread and then
// used on another one (AppKit's thread-safety rules for image reps). The
// loader creates it, never touches it again after sending, and the main
// thread is the only user from then on.
unsafe impl Send for Decoded {}

struct Job {
    generation: u64,
    image: usize,
    path: PathBuf,
}

pub(crate) struct Done {
    pub generation: u64,
    pub image: usize,
    pub result: Result<Decoded, String>,
}

/// Reads one PNG and, with `warm`, decodes it all the way to pixels in
/// memory. Without, the pixels are decoded at the first draw.
pub(crate) fn decode(path: &std::path::Path, warm_up: bool) -> Result<Decoded, String> {
    autoreleasepool(|_| {
        let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let data = NSData::with_bytes(&bytes);
        let rep = NSBitmapImageRep::imageRepWithData(&data)
            .ok_or_else(|| format!("{} no es una imagen que pueda leer", path.display()))?;
        // Drawn once, here, so the pixels are decoded (and cached with the
        // image) now, instead of on the main thread at the first real draw.
        if warm_up {
            warm(&rep);
        }
        Ok(Decoded(rep))
    })
}

impl Decoded {
    /// The picture as an image whose coordinates are its pixels, and the
    /// bytes its pixels take.
    pub(crate) fn into_image(self) -> (Retained<NSImage>, usize) {
        let rep = self.0;
        let (w, h) = (rep.pixelsWide().max(1), rep.pixelsHigh().max(1));
        let size = NSSize::new(w as f64, h as f64);
        rep.setSize(size);
        let image = NSImage::initWithSize(NSImage::alloc(), size);
        image.addRepresentation(&rep);
        let bytes = (rep.bytesPerRow().max(w * 4) * h) as usize;
        (image, bytes)
    }
}

struct Entry {
    bytes: usize,
    used: u64,
}

/// The main thread's side: what is decoded, what is on its way, what to
/// drop. The pictures themselves live in the mascot view.
pub(crate) struct Book {
    generation: u64,
    jobs: Sender<Job>,
    done: Receiver<Done>,
    entries: HashMap<usize, Entry>,
    pending: HashSet<usize>,
    failed: HashSet<usize>,
    total: usize,
    clock: u64,
    /// Peak decoded bytes (for `WAWI2_BODY_TIMING`).
    pub peak: usize,
}

impl Book {
    /// Starts the loader thread. `wake` runs on the loader after each
    /// picture, to get the main thread to collect it.
    pub(crate) fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        let (jobs, job_rx) = mpsc::channel::<Job>();
        let (done_tx, done) = mpsc::channel();
        let spawned = thread::Builder::new()
            .name("wawi-cuerpo-cuadros".into())
            .spawn(move || {
                for job in job_rx {
                    let result = decode(&job.path, true);
                    let sent = done_tx.send(Done {
                        generation: job.generation,
                        image: job.image,
                        result,
                    });
                    if sent.is_err() {
                        return;
                    }
                    wake();
                }
            });
        if let Err(error) = spawned {
            eprintln!("  cuerpo: no pude crear el hilo de los cuadros: {error}");
        }
        Self {
            generation: 0,
            jobs,
            done,
            entries: HashMap::new(),
            pending: HashSet::new(),
            failed: HashSet::new(),
            total: 0,
            clock: 0,
            peak: 0,
        }
    }

    /// A new pack: forget everything (results for the old one are dropped).
    pub(crate) fn reset(&mut self) {
        self.generation += 1;
        self.entries.clear();
        self.pending.clear();
        self.failed.clear();
        self.total = 0;
    }

    /// Asks the loader for `image` unless it is there, coming or broken.
    pub(crate) fn want(&mut self, image: usize, path: &std::path::Path) {
        self.clock += 1;
        if let Some(entry) = self.entries.get_mut(&image) {
            entry.used = self.clock;
            return;
        }
        if self.pending.contains(&image) || self.failed.contains(&image) {
            return;
        }
        self.pending.insert(image);
        let _ = self.jobs.send(Job {
            generation: self.generation,
            image,
            path: path.to_owned(),
        });
    }

    /// Records a picture the main thread decoded itself (idle, at startup).
    pub(crate) fn add(&mut self, image: usize, bytes: usize) {
        self.clock += 1;
        self.pending.remove(&image);
        if let Some(old) = self.entries.insert(
            image,
            Entry {
                bytes,
                used: self.clock,
            },
        ) {
            self.total -= old.bytes;
        }
        self.total += bytes;
        self.peak = self.peak.max(self.total);
    }

    pub(crate) fn fail(&mut self, image: usize) -> bool {
        self.pending.remove(&image);
        self.failed.insert(image)
    }

    /// The loader's results for the current pack.
    pub(crate) fn collect(&mut self) -> Vec<Done> {
        let mut out = Vec::new();
        while let Ok(done) = self.done.try_recv() {
            if done.generation == self.generation {
                out.push(done);
            }
        }
        out
    }

    /// What to drop to get back under the cap: the least recently wanted,
    /// never one in `keep`.
    pub(crate) fn evict(&mut self, keep: &HashSet<usize>) -> Vec<usize> {
        let mut dropped = Vec::new();
        while self.total > CAP_BYTES {
            let oldest = self
                .entries
                .iter()
                .filter(|(image, _)| !keep.contains(image))
                .min_by_key(|(_, e)| e.used)
                .map(|(image, _)| *image);
            let Some(image) = oldest else { break };
            if let Some(entry) = self.entries.remove(&image) {
                self.total -= entry.bytes;
            }
            dropped.push(image);
        }
        dropped
    }

    pub(crate) fn total(&self) -> usize {
        self.total
    }
}

/// Draws the rep once into a 1×1 bitmap, so its decoded pixels are cached
/// before the first real draw.
fn warm(rep: &NSBitmapImageRep) {
    // SAFETY: null planes: AppKit allocates the 1x1 buffer.
    let tiny = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            1,
            1,
            8,
            4,
            true,
            false,
            objc2_app_kit::NSDeviceRGBColorSpace,
            0,
            0,
        )
    };
    let Some(tiny) = tiny else { return };
    let Some(ctx) = objc2_app_kit::NSGraphicsContext::graphicsContextWithBitmapImageRep(&tiny)
    else {
        return;
    };
    objc2_app_kit::NSGraphicsContext::saveGraphicsState_class();
    objc2_app_kit::NSGraphicsContext::setCurrentContext(Some(&ctx));
    rep.drawInRect(objc2_foundation::NSRect::new(
        objc2_foundation::NSPoint::ZERO,
        NSSize::new(1.0, 1.0),
    ));
    objc2_app_kit::NSGraphicsContext::restoreGraphicsState_class();
}
