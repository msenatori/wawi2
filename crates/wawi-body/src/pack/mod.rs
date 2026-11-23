//! Pet packs: a pet's name, art and animations, in a folder, outside the
//! code. Wawi is the built-in pack (`assets/mascotas/wawi`); anyone can make
//! another one in `<home>/mascotas/<id>/` (see `docs/MASCOTAS.md`).
//!
//! A pack is a `mascota.toml` plus PNG art: single frames in a folder, whole
//! folders of frames (a sequence), or sprite sheets (a grid). Loading a pack
//! only reads the manifest and checks which files are there; no image is
//! decoded here (the backend decodes lazily, off the voice thread).
//!
//! Everything in this module is pure and tested on any OS.

mod catalog;
mod check;
mod cuerpo;
pub mod png;
mod scaffold;
pub mod toml;

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::anim::{Clip, FrameId, Layer, Motion, Step};
use toml::{Table, Value, get};

pub(crate) use catalog::load_logged;
pub use catalog::{Installed, active, builtin_dir, find, home_dir, installed, pets_dir};
pub use check::{Report, validate};
pub use cuerpo::Cuerpo;
pub use scaffold::scaffold;

/// The manifest's file name, inside the pack's folder.
pub const MANIFEST: &str = "mascota.toml";
/// The built-in pack, and the fallback for any pack that does not load.
pub const BUILTIN_ID: &str = "wawi";
/// Default on-screen width when a pack gives no `escala`: Wawi's, in points.
pub const DEFAULT_WIDTH_PT: f64 = 124.0;
/// Frames per second when neither the animation nor the pack says.
pub const DEFAULT_FPS: f64 = 8.0;

/// The looks the body shows. Each one is an animation of the pack (by
/// default the one with the same name) or, if the pack has none, the next
/// look down its fallback chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Look {
    Idle,
    Sleeping,
    Listening,
    Transcribing,
    Thinking,
    Speaking,
    Interrupted,
    Error,
    Muted,
}

impl Look {
    pub const ALL: [Look; 9] = [
        Look::Idle,
        Look::Sleeping,
        Look::Listening,
        Look::Transcribing,
        Look::Thinking,
        Look::Speaking,
        Look::Interrupted,
        Look::Error,
        Look::Muted,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Look::Idle => "idle",
            Look::Sleeping => "sleeping",
            Look::Listening => "listening",
            Look::Transcribing => "transcribing",
            Look::Thinking => "thinking",
            Look::Speaking => "speaking",
            Look::Interrupted => "interrupted",
            Look::Error => "error",
            Look::Muted => "muted",
        }
    }

    pub fn parse(name: &str) -> Option<Look> {
        Look::ALL.into_iter().find(|look| look.name() == name)
    }

    /// Where a missing look goes. The states end at idle; the reactions
    /// (interrupted, error) and muted end in nothing: no reaction at all.
    pub fn fallback(self) -> Option<Look> {
        match self {
            Look::Idle => None,
            Look::Sleeping | Look::Listening | Look::Thinking | Look::Speaking => Some(Look::Idle),
            Look::Transcribing => Some(Look::Thinking),
            Look::Interrupted => Some(Look::Error),
            Look::Error | Look::Muted => None,
        }
    }

    /// What it is, for artists (validator and docs).
    pub fn meaning(self) -> &'static str {
        match self {
            Look::Idle => "quieta, sin hacer nada (obligatorio)",
            Look::Sleeping => "dormida, después de un rato quieta",
            Look::Listening => "escuchando mientras le hablás",
            Look::Transcribing => "entendiendo lo que dijiste",
            Look::Thinking => "pensando la respuesta",
            Look::Speaking => "hablando",
            Look::Interrupted => "la interrumpiste (una vez)",
            Look::Error => "algo falló (una vez)",
            Look::Muted => "con el micrófono apagado",
        }
    }
}

/// How bad a problem is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// A hint: the pack works as it is.
    Warning,
    /// Something is broken but the pack still runs around it (a missing
    /// frame is skipped, a bad reference ignored).
    Error,
    /// The pack cannot run: Wawi falls back to the built-in pack.
    Fatal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub level: Level,
    pub message: String,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tag = match self.level {
            Level::Warning => "aviso",
            Level::Error => "error",
            Level::Fatal => "error grave",
        };
        write!(f, "{tag}: {}", self.message)
    }
}

/// Why a pack did not load: its fatal problems (and the rest, for context).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadError {
    pub dir: PathBuf,
    pub problems: Vec<Problem>,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "la mascota de {} no se puede usar", self.dir.display())?;
        for p in self.problems.iter().filter(|p| p.level == Level::Fatal) {
            write!(f, "; {}", p.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for LoadError {}

/// Who made it and how it may be used.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Info {
    pub id: String,
    pub nombre: String,
    pub autor: String,
    pub licencia: String,
    pub version: String,
    pub descripcion: String,
}

/// `[voz]`: the pet's own voice. Parsed and offered, not wired yet.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VoiceSeed {
    /// A Kokoro voice id, like `jf_alpha` or `ef_dora`.
    pub id: Option<String>,
    /// Speaking rate, 1.0 = normal.
    pub speed: Option<f64>,
    /// Pitch shift in semitones (Wawi: +3).
    pub pitch: Option<f64>,
}

/// `[alma]`: a default `SOUL.md` seed with the pet's personality. Parsed and
/// offered, not wired yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SoulSeed {
    File(PathBuf),
    Text(String),
}

/// `[vida]`: what she does on her own while idle.
#[derive(Clone, Debug, PartialEq)]
pub struct Life {
    pub blink: Option<String>,
    pub blink_gap: (f64, f64),
    pub gestures: Vec<String>,
    pub gesture_gap: (f64, f64),
    /// Seconds idle before she sleeps; 0 = never.
    pub sleep_after: f64,
    pub lie_down: Option<String>,
    pub wake: Option<String>,
    /// A click on her.
    pub poke: Option<String>,
    /// The code-driven sways and talking bob on top of the frames.
    pub sway: bool,
}

impl Default for Life {
    fn default() -> Self {
        Self {
            blink: None,
            blink_gap: (2.0, 6.0),
            gestures: Vec::new(),
            gesture_gap: (6.0, 20.0),
            sleep_after: 10.0,
            lie_down: None,
            wake: None,
            poke: None,
            sway: true,
        }
    }
}

/// A rectangle in an image, in pixels, origin top-left (as artists see it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cell {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Where a picture of another size goes inside the frame: its box, in
/// fractions of the frame (from the left and from the bottom; `y` may be
/// below 0, legs over a window's edge).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// One frame: an image file, or one cell of a sprite sheet.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// How the validator and the tests call it: the file stem, or
    /// `sheet#index`.
    pub name: String,
    pub image: usize,
    pub cell: Option<Cell>,
    /// Drawn in this box instead of stretched over the whole frame
    /// (`[cuadro] cajas` or `ajuste = "natural"`).
    pub place: Option<Place>,
}

/// One image file the art is cut from (the unit the backend decodes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub path: PathBuf,
    pub present: bool,
}

/// A loaded pack: the manifest, compiled into clips the animator plays.
#[derive(Clone, Debug)]
pub struct Pack {
    pub dir: PathBuf,
    pub builtin: bool,
    pub info: Info,
    /// The frame size in pixels.
    pub frame_size: (u32, u32),
    /// The point of the frame that stands on the ground, 0..1 (from the left,
    /// from the bottom).
    pub anchor: (f64, f64),
    /// Points on screen per pixel of art.
    pub scale: f64,
    /// Where the head is on the frame, 0..1 (the bubble points there).
    pub head: (f64, f64),
    pub fps: f64,
    pub menubar_icon: Option<PathBuf>,
    pub muted_icon: Option<PathBuf>,
    pub life: Life,
    /// `[cuerpo]`: she falls, walks and climbs. `None`: she stays put.
    pub body: Option<Cuerpo>,
    frames: Vec<Frame>,
    images: Vec<Image>,
    clips: HashMap<String, Clip>,
    /// Every animation the manifest names, playable or not (the validator).
    declared: Vec<String>,
    looks: HashMap<Look, String>,
    actions: Vec<(String, String)>,
    voice: Option<VoiceSeed>,
    soul: Option<SoulSeed>,
    /// Everything not fatal found while loading.
    pub problems: Vec<Problem>,
}

impl Pack {
    /// Reads `<dir>/mascota.toml` and checks which art files exist. Fails only
    /// on fatal problems; the rest is in [`Pack::problems`].
    pub fn load(dir: &Path) -> Result<Pack, LoadError> {
        let text = match fs::read_to_string(dir.join(MANIFEST)) {
            Ok(text) => text,
            Err(error) => {
                return Err(LoadError {
                    dir: dir.to_owned(),
                    problems: vec![fatal(format!(
                        "no puedo leer {}: {}",
                        dir.join(MANIFEST).display(),
                        spanish_io(&error)
                    ))],
                });
            }
        };
        Self::from_text(dir, &text)
    }

    pub fn from_text(dir: &Path, text: &str) -> Result<Pack, LoadError> {
        let mut problems = Vec::new();
        let pack = match toml::parse(text) {
            Ok(table) => build(dir, &table, &mut problems),
            Err(error) => {
                problems.push(fatal(format!("{MANIFEST}, {error}")));
                None
            }
        };
        match pack {
            Some(mut pack) if !problems.iter().any(|p| p.level == Level::Fatal) => {
                pack.problems = problems;
                Ok(pack)
            }
            _ => Err(LoadError {
                dir: dir.to_owned(),
                problems,
            }),
        }
    }

    /// A pack with nothing to show (no art found at all): the body keeps
    /// only its menu.
    pub fn empty(dir: PathBuf) -> Pack {
        Pack {
            dir,
            builtin: true,
            info: Info {
                id: BUILTIN_ID.into(),
                nombre: "Wawi".into(),
                ..Info::default()
            },
            frame_size: (1, 1),
            anchor: (0.5, 0.0),
            scale: 1.0,
            head: (0.5, 0.9),
            fps: DEFAULT_FPS,
            menubar_icon: None,
            muted_icon: None,
            life: Life::default(),
            body: None,
            frames: Vec::new(),
            images: Vec::new(),
            clips: HashMap::new(),
            declared: Vec::new(),
            looks: HashMap::new(),
            actions: Vec::new(),
            voice: None,
            soul: None,
            problems: Vec::new(),
        }
    }

    pub fn id(&self) -> &str {
        &self.info.id
    }

    /// The name the menu and the docs use ("Mostrar a Kitty").
    pub fn name(&self) -> &str {
        &self.info.nombre
    }

    pub fn frames(&self) -> &[Frame] {
        &self.frames
    }

    pub fn frame(&self, id: FrameId) -> Option<&Frame> {
        self.frames.get(usize::from(id))
    }

    pub fn frame_name(&self, id: FrameId) -> &str {
        self.frame(id).map(|f| f.name.as_str()).unwrap_or("?")
    }

    pub fn images(&self) -> &[Image] {
        &self.images
    }

    /// The image a frame is cut from.
    pub fn image_of(&self, id: FrameId) -> Option<usize> {
        self.frame(id).map(|f| f.image)
    }

    /// A playable animation (one with at least one frame on disk).
    pub fn clip(&self, name: &str) -> Option<&Clip> {
        self.clips.get(name)
    }

    pub fn clip_names(&self) -> impl Iterator<Item = &str> {
        self.clips.keys().map(String::as_str)
    }

    /// The animation a look uses, following the fallback chain.
    pub fn look_clip(&self, look: Look) -> Option<&str> {
        let mut at = Some(look);
        while let Some(look) = at {
            if let Some(name) = self.own_look(look) {
                return Some(name);
            }
            at = look.fallback();
        }
        None
    }

    /// The pack's own animation for a look, without falling back.
    pub fn own_look(&self, look: Look) -> Option<&str> {
        self.looks
            .get(&look)
            .map(String::as_str)
            .filter(|name| self.clips.contains_key(*name))
    }

    /// The custom actions, in manifest order: what a future `hacer(accion)`
    /// tool may ask for.
    pub fn actions(&self) -> Vec<&str> {
        self.actions
            .iter()
            .filter(|(_, clip)| self.clips.contains_key(clip))
            .map(|(name, _)| name.as_str())
            .collect()
    }

    /// The animation an action plays.
    pub fn action_clip(&self, action: &str) -> Option<&str> {
        self.actions
            .iter()
            .find(|(name, _)| name == action)
            .map(|(_, clip)| clip.as_str())
            .filter(|clip| self.clips.contains_key(*clip))
    }

    /// The action's description (its animation's `descripcion`), if any.
    pub fn action_description(&self, action: &str) -> Option<&str> {
        self.action_clip(action)
            .and_then(|clip| self.clips.get(clip))
            .and_then(|c| c.description.as_deref())
    }

    /// `[voz]`, for the voice to use one day.
    pub fn voice(&self) -> Option<&VoiceSeed> {
        self.voice.as_ref()
    }

    pub fn soul_seed(&self) -> Option<&SoulSeed> {
        self.soul.as_ref()
    }

    /// `[alma]`'s text: inline, or the file's contents.
    pub fn soul(&self) -> Option<String> {
        match self.soul.as_ref()? {
            SoulSeed::Text(text) => Some(text.clone()),
            SoulSeed::File(path) => fs::read_to_string(path).ok(),
        }
    }

    /// Every frame a clip may show: its steps, mouth, variants and what
    /// plays after it (one step down the chain). What the backend
    /// prefetches.
    pub fn clip_frames(&self, name: &str, out: &mut Vec<FrameId>) {
        let mut visit = |clip: &Clip| {
            out.extend(clip.steps.iter().map(|s| s.frame));
            if let Some(mouth) = clip.mouth {
                out.extend(mouth);
            }
            if let Some(layer) = clip.layer {
                out.push(layer.frame);
            }
        };
        let Some(clip) = self.clips.get(name) else {
            return;
        };
        visit(clip);
        for other in clip.variants.iter().chain(clip.next.iter()) {
            if let Some(c) = self.clips.get(other) {
                visit(c);
            }
        }
    }

    /// How far the art reaches around the frame, in fractions of it:
    /// `(left, right, below, above)` beyond its edges (boxes that stick out,
    /// the parachute's canopy). The window makes room for it.
    pub fn overhang(&self) -> (f64, f64, f64, f64) {
        let mut out = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
        let mut grow = |p: Place, (dx, dy): (f64, f64)| {
            out.0 = out.0.max(-(p.x + dx));
            out.1 = out.1.max(p.x + dx + p.w - 1.0);
            out.2 = out.2.max(-(p.y + dy));
            out.3 = out.3.max(p.y + dy + p.h - 1.0);
        };
        let whole = Place {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        };
        for clip in self.clips.values() {
            for step in &clip.steps {
                if let Some(p) = self.frame(step.frame).and_then(|f| f.place) {
                    grow(p, (0.0, 0.0));
                }
            }
            if let Some(layer) = clip.layer {
                let p = self
                    .frame(layer.frame)
                    .and_then(|f| f.place)
                    .unwrap_or(whole);
                grow(p, layer.at);
                // It sways and mirrors with her: room on both sides.
                grow(
                    Place {
                        x: 1.0 - p.x - p.w,
                        ..p
                    },
                    (-layer.at.0, layer.at.1),
                );
            }
        }
        out
    }

    /// The sprite on screen, in points.
    pub fn sprite_size(&self) -> (f64, f64) {
        (
            f64::from(self.frame_size.0) * self.scale,
            f64::from(self.frame_size.1) * self.scale,
        )
    }

    /// Decoded size of an image, in bytes, if its PNG header can be read.
    pub fn image_bytes(&self, image: usize) -> Option<usize> {
        let (w, h) = png::size(&self.images.get(image)?.path).ok()?;
        Some(w as usize * h as usize * 4)
    }
}

fn fatal(message: String) -> Problem {
    Problem {
        level: Level::Fatal,
        message,
    }
}

fn error(message: String) -> Problem {
    Problem {
        level: Level::Error,
        message,
    }
}

fn warning(message: String) -> Problem {
    Problem {
        level: Level::Warning,
        message,
    }
}

pub(crate) fn spanish_io(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound => "no existe".into(),
        std::io::ErrorKind::PermissionDenied => "no tengo permiso".into(),
        _ => error.to_string(),
    }
}

/// Reads typed fields from a table and complains, in Spanish, about the
/// wrong type and about keys nobody reads (usually a typo).
struct Fields<'a> {
    table: &'a Table,
    place: String,
    used: Vec<&'static str>,
}

impl<'a> Fields<'a> {
    fn new(table: &'a Table, place: impl Into<String>) -> Self {
        Self {
            table,
            place: place.into(),
            used: Vec::new(),
        }
    }

    fn raw(&mut self, key: &'static str) -> Option<&'a Value> {
        self.used.push(key);
        get(self.table, key)
    }

    fn wrong(&self, key: &str, want: &str, got: &Value, problems: &mut Vec<Problem>) {
        problems.push(error(format!(
            "{}`{key}` tiene que ser {want}, y es {}",
            self.place,
            got.kind()
        )));
    }

    fn string(&mut self, key: &'static str, problems: &mut Vec<Problem>) -> Option<String> {
        let value = self.raw(key)?;
        match value.as_str() {
            Some(s) => Some(s.to_owned()),
            None => {
                self.wrong(key, "un texto entre comillas", value, problems);
                None
            }
        }
    }

    fn number(&mut self, key: &'static str, problems: &mut Vec<Problem>) -> Option<f64> {
        let value = self.raw(key)?;
        match value.as_f64() {
            Some(n) => Some(n),
            None => {
                self.wrong(key, "un número", value, problems);
                None
            }
        }
    }

    fn boolean(&mut self, key: &'static str, problems: &mut Vec<Problem>) -> Option<bool> {
        let value = self.raw(key)?;
        match value.as_bool() {
            Some(b) => Some(b),
            None => {
                self.wrong(key, "true o false", value, problems);
                None
            }
        }
    }

    fn strings(&mut self, key: &'static str, problems: &mut Vec<Problem>) -> Option<Vec<String>> {
        let value = self.raw(key)?;
        let list = value.as_array().and_then(|items| {
            items
                .iter()
                .map(|v| v.as_str().map(str::to_owned))
                .collect()
        });
        if list.is_none() {
            self.wrong(
                key,
                "una lista de textos, como [\"a\", \"b\"]",
                value,
                problems,
            );
        }
        list
    }

    fn numbers(&mut self, key: &'static str, problems: &mut Vec<Problem>) -> Option<Vec<f64>> {
        let value = self.raw(key)?;
        let list = value
            .as_array()
            .and_then(|items| items.iter().map(Value::as_f64).collect());
        if list.is_none() {
            self.wrong(key, "una lista de números, como [1, 2]", value, problems);
        }
        list
    }

    fn pair(&mut self, key: &'static str, problems: &mut Vec<Problem>) -> Option<(f64, f64)> {
        let list = self.numbers(key, problems)?;
        match list.as_slice() {
            [a, b] => Some((*a, *b)),
            _ => {
                problems.push(error(format!(
                    "{}`{key}` lleva dos números, como [0.5, 0.0]",
                    self.place
                )));
                None
            }
        }
    }

    fn table(&mut self, key: &'static str, problems: &mut Vec<Problem>) -> Option<&'a Table> {
        let value = self.raw(key)?;
        match value.as_table() {
            Some(t) => Some(t),
            None => {
                self.wrong(key, "una tabla", value, problems);
                None
            }
        }
    }

    /// Keys nobody asked for: a typo, most of the time.
    fn finish(self, problems: &mut Vec<Problem>) {
        for (key, _) in self.table {
            if !self.used.contains(&key.as_str()) {
                let hint = self
                    .used
                    .iter()
                    .find(|known| close(known, key))
                    .map(|known| format!(" (¿quisiste decir `{known}`?)"))
                    .unwrap_or_default();
                problems.push(warning(format!(
                    "{}no conozco `{key}`, lo ignoro{hint}",
                    self.place
                )));
            }
        }
    }
}

/// Two names one typo apart (a swap, a missing, extra or wrong letter).
fn close(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a == b {
        return false;
    }
    if a.len() == b.len() {
        let diff: Vec<usize> = (0..a.len()).filter(|&i| a[i] != b[i]).collect();
        return diff.len() == 1
            || (diff.len() == 2
                && diff[1] == diff[0] + 1
                && a[diff[0]] == b[diff[1]]
                && a[diff[1]] == b[diff[0]]);
    }
    let (short, long) = if a.len() < b.len() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    if long.len() - short.len() != 1 {
        return false;
    }
    (0..long.len()).any(|skip| {
        long.iter()
            .enumerate()
            .filter(|(i, _)| *i != skip)
            .map(|(_, c)| c)
            .eq(short.iter())
    })
}

/// Builder state: frames and images, deduplicated.
struct Art<'a> {
    dir: &'a Path,
    folder: PathBuf,
    frame_size: Option<(u32, u32)>,
    frames: Vec<Frame>,
    images: Vec<Image>,
    by_key: HashMap<(usize, Option<Cell>), FrameId>,
}

impl Art<'_> {
    fn image(&mut self, path: PathBuf) -> usize {
        if let Some(i) = self.images.iter().position(|img| img.path == path) {
            return i;
        }
        let present = path.is_file();
        self.images.push(Image { path, present });
        self.images.len() - 1
    }

    fn frame(&mut self, name: String, image: usize, cell: Option<Cell>) -> Option<FrameId> {
        if let Some(id) = self.by_key.get(&(image, cell)) {
            return Some(*id);
        }
        let id = FrameId::try_from(self.frames.len()).ok()?;
        self.frames.push(Frame {
            name,
            image,
            cell,
            place: None,
        });
        self.by_key.insert((image, cell), id);
        Some(id)
    }

    /// `name` in the frames folder: `idle_a`, `idle_a.png` or `sub/idle_a`.
    fn file_frame(&mut self, name: &str) -> Option<FrameId> {
        let file = if name.to_ascii_lowercase().ends_with(".png") {
            name.to_owned()
        } else {
            format!("{name}.png")
        };
        let stem = file[..file.len() - 4].to_owned();
        let image = self.image(self.folder.join(&file));
        self.frame(stem, image, None)
    }
}

fn build(dir: &Path, root: &Table, problems: &mut Vec<Problem>) -> Option<Pack> {
    let mut top = Fields::new(root, "");
    let mut info = Info {
        id: top.string("id", problems).unwrap_or_default(),
        nombre: top.string("nombre", problems).unwrap_or_default(),
        autor: top.string("autor", problems).unwrap_or_default(),
        licencia: top.string("licencia", problems).unwrap_or_default(),
        version: top.string("version", problems).unwrap_or_default(),
        descripcion: top.string("descripcion", problems).unwrap_or_default(),
    };
    let folder_name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if info.id.trim().is_empty() {
        problems.push(error(format!(
            "falta `id` (el nombre corto de la carpeta, por ejemplo id = \"{folder_name}\")"
        )));
        info.id = folder_name.clone();
    } else if !valid_id(&info.id) {
        problems.push(error(format!(
            "`id = \"{}\"`: usá solo minúsculas, números, `-` o `_`",
            info.id
        )));
    }
    if info.nombre.trim().is_empty() {
        problems.push(warning(
            "falta `nombre`: el menú va a usar el id en su lugar".into(),
        ));
        info.nombre = info.id.clone();
    }
    let folder = top
        .string("carpeta", problems)
        .unwrap_or_else(|| "cuadros".into());

    // [cuadro]
    let mut frame_w = None;
    let mut frame_h = None;
    let mut anchor = (0.5, 0.0);
    let mut head = (0.5, 0.9);
    let mut scale = None;
    let mut fps = DEFAULT_FPS;
    let mut boxes = None;
    let mut natural = false;
    if let Some(table) = top.table("cuadro", problems) {
        let mut f = Fields::new(table, "[cuadro] ");
        frame_w = f.number("ancho", problems);
        frame_h = f.number("alto", problems);
        if let Some(a) = f.pair("ancla", problems) {
            anchor = unit_pair(a, "[cuadro] ancla", problems);
        }
        if let Some(h) = f.pair("cabeza", problems) {
            head = unit_pair(h, "[cuadro] cabeza", problems);
        }
        scale = f.number("escala", problems);
        if let Some(n) = f.number("fps", problems) {
            fps = checked_fps(n, "[cuadro] fps", problems).unwrap_or(DEFAULT_FPS);
        }
        boxes = f.string("cajas", problems);
        match f.string("ajuste", problems).as_deref() {
            None | Some("estirar") => {}
            Some("natural") => natural = true,
            Some(other) => problems.push(error(format!(
                "[cuadro] `ajuste = \"{other}\"`: puede ser \"estirar\" o \"natural\""
            ))),
        }
        f.finish(problems);
    }
    let mut art = Art {
        dir,
        folder: dir.join(&folder),
        frame_size: None,
        frames: Vec::new(),
        images: Vec::new(),
        by_key: HashMap::new(),
    };
    for (value, what) in [(frame_w, "ancho"), (frame_h, "alto")] {
        if let Some(n) = value
            && (!(1.0..=4096.0).contains(&n) || n.fract() != 0.0)
        {
            problems.push(error(format!(
                "[cuadro] `{what}` tiene que ser un número entero de píxeles entre 1 y 4096"
            )));
        }
    }
    let size_ok = |n: f64| (1.0..=4096.0).contains(&n) && n.fract() == 0.0;
    art.frame_size = match (frame_w, frame_h) {
        (Some(w), Some(h)) if size_ok(w) && size_ok(h) => Some((w as u32, h as u32)),
        _ => None,
    };

    // [iconos]
    let mut menubar_icon = None;
    let mut muted_icon = None;
    if let Some(table) = top.table("iconos", problems) {
        let mut f = Fields::new(table, "[iconos] ");
        for (key, slot) in [("barra", &mut menubar_icon), ("silencio", &mut muted_icon)] {
            if let Some(file) = f.string(key, problems) {
                let path = dir.join(&file);
                if !path.is_file() {
                    problems.push(error(format!("[iconos] `{key}`: no encuentro {file}")));
                } else {
                    *slot = Some(path);
                }
            }
        }
        f.finish(problems);
    }

    // [animaciones.*]
    let mut clips = HashMap::new();
    let mut declared = Vec::new();
    let mut needs_size = Vec::new();
    if let Some(table) = top.table("animaciones", problems) {
        for (name, value) in table {
            declared.push(name.clone());
            let Some(anim) = value.as_table() else {
                problems.push(error(format!(
                    "`animaciones.{name}` tiene que ser una tabla: [animaciones.{name}]"
                )));
                continue;
            };
            if let Some(clip) = animation(name, anim, fps, &mut art, &mut needs_size, problems) {
                clips.insert(name.clone(), clip);
            }
        }
    }
    if declared.is_empty() {
        problems.push(fatal(
            "no hay ninguna animación: hace falta al menos [animaciones.idle]".into(),
        ));
    }

    // The frame size: from [cuadro], or from the first frame on disk.
    let frame_size = match art.frame_size {
        Some(size) => size,
        None => {
            let first = clips
                .get("idle")
                .or_else(|| clips.values().next())
                .and_then(|c| c.steps.first())
                .and_then(|s| art.frames.get(usize::from(s.frame)))
                .filter(|f| f.cell.is_none())
                .and_then(|f| png::size(&art.images[f.image].path).ok());
            match first {
                Some(size) => {
                    if frame_w.is_none() && frame_h.is_none() {
                        problems.push(warning(format!(
                            "falta [cuadro] ancho y alto: uso el tamaño del primer cuadro, {}×{}",
                            size.0, size.1
                        )));
                    }
                    size
                }
                None => {
                    problems.push(fatal(
                        "falta [cuadro] ancho y alto (en píxeles), y no pude leerlos de un cuadro"
                            .into(),
                    ));
                    (1, 1)
                }
            }
        }
    };
    if !needs_size.is_empty() && art.frame_size.is_none() {
        problems.push(fatal(format!(
            "las hojas ({}) necesitan [cuadro] ancho y alto para saber dónde cortar",
            needs_size.join(", ")
        )));
    }
    place_frames(
        dir,
        &mut art,
        frame_size,
        boxes.as_deref(),
        natural,
        problems,
    );
    // Sheet cells were (column, row) until the frame size was known.
    for frame in &mut art.frames {
        if let Some(cell) = &mut frame.cell {
            cell.x *= frame_size.0;
            cell.y *= frame_size.1;
            cell.w = frame_size.0;
            cell.h = frame_size.1;
        }
    }
    let scale = match scale {
        Some(s) if s > 0.0 && s <= 20.0 => s,
        Some(_) => {
            problems.push(error(
                "[cuadro] `escala` tiene que ser un número mayor que 0 (1 = un punto por píxel)"
                    .into(),
            ));
            DEFAULT_WIDTH_PT / f64::from(frame_size.0)
        }
        None => DEFAULT_WIDTH_PT / f64::from(frame_size.0),
    };

    // [estados]
    let mut looks: HashMap<Look, String> = HashMap::new();
    for look in Look::ALL {
        if declared.iter().any(|n| n == look.name()) {
            looks.insert(look, look.name().to_owned());
        }
    }
    if let Some(table) = top.table("estados", problems) {
        for (key, value) in table {
            let Some(look) = Look::parse(key) else {
                let names: Vec<&str> = Look::ALL.iter().map(|l| l.name()).collect();
                problems.push(warning(format!(
                    "[estados] no conozco el estado `{key}`; los estados son: {}",
                    names.join(", ")
                )));
                continue;
            };
            match value.as_str() {
                Some(anim) => {
                    if !declared.iter().any(|n| n == anim) {
                        problems.push(error(format!(
                            "[estados] {key} = \"{anim}\", pero no hay [animaciones.{anim}]"
                        )));
                    }
                    looks.insert(look, anim.to_owned());
                }
                None => problems.push(error(format!(
                    "[estados] `{key}` tiene que ser el nombre de una animación, entre comillas"
                ))),
            }
        }
    }

    // [vida]
    let mut life = Life::default();
    if let Some(table) = top.table("vida", problems) {
        let mut f = Fields::new(table, "[vida] ");
        life.blink = f.string("parpadeo", problems);
        if let Some(gap) = f.pair("cada_parpadeo", problems) {
            life.blink_gap = gap_pair(gap, "cada_parpadeo", life.blink_gap, problems);
        }
        life.gestures = f.strings("gestos", problems).unwrap_or_default();
        if let Some(gap) = f.pair("cada_gesto", problems) {
            life.gesture_gap = gap_pair(gap, "cada_gesto", life.gesture_gap, problems);
        }
        if let Some(secs) = f.number("dormir_tras", problems) {
            if secs < 0.0 {
                problems.push(error(
                    "[vida] `dormir_tras` son segundos: 0 o más (0 = nunca se duerme)".into(),
                ));
            } else {
                life.sleep_after = secs;
            }
        }
        life.lie_down = f.string("dormirse", problems);
        life.wake = f.string("despertarse", problems);
        life.poke = f.string("toque", problems);
        if let Some(sway) = f.boolean("balanceo", problems) {
            life.sway = sway;
        }
        f.finish(problems);
        let named = life
            .blink
            .iter()
            .map(|n| ("parpadeo", n))
            .chain(life.gestures.iter().map(|n| ("gestos", n)))
            .chain(life.lie_down.iter().map(|n| ("dormirse", n)))
            .chain(life.wake.iter().map(|n| ("despertarse", n)))
            .chain(life.poke.iter().map(|n| ("toque", n)));
        for (key, anim) in named {
            if !declared.iter().any(|n| n == anim) {
                problems.push(error(format!(
                    "[vida] {key}: \"{anim}\" no es ninguna animación (falta [animaciones.{anim}])"
                )));
            }
        }
    }

    // [acciones]
    let mut actions = Vec::new();
    if let Some(table) = top.table("acciones", problems) {
        for (name, value) in table {
            let Some(anim) = value.as_str() else {
                problems.push(error(format!(
                    "[acciones] `{name}` tiene que ser el nombre de una animación, entre comillas"
                )));
                continue;
            };
            if !valid_action(name) {
                problems.push(warning(format!(
                    "[acciones] `{name}`: mejor un nombre en minúsculas y sin espacios"
                )));
            }
            if !declared.iter().any(|n| n == anim) {
                problems.push(error(format!(
                    "[acciones] {name} = \"{anim}\", pero no hay [animaciones.{anim}]"
                )));
            }
            actions.push((name.clone(), anim.to_owned()));
        }
    }

    // [cuerpo]
    let mut body = top
        .table("cuerpo", problems)
        .map(|table| cuerpo::parse(table, problems));
    if let Some(body) = &mut body {
        cuerpo::finish(body, &declared, &clips, problems);
    }

    // [voz] and [alma]: parsed, offered, not wired.
    let mut voice = None;
    if let Some(table) = top.table("voz", problems) {
        let mut f = Fields::new(table, "[voz] ");
        voice = Some(VoiceSeed {
            id: f.string("id", problems),
            speed: f.number("velocidad", problems),
            pitch: f.number("tono", problems),
        });
        f.finish(problems);
    }
    let mut soul = None;
    if let Some(table) = top.table("alma", problems) {
        let mut f = Fields::new(table, "[alma] ");
        let file = f.string("archivo", problems);
        let text = f.string("texto", problems);
        f.finish(problems);
        soul = match (file, text) {
            (Some(file), _) => {
                let path = dir.join(&file);
                if !path.is_file() {
                    problems.push(error(format!("[alma] `archivo`: no encuentro {file}")));
                }
                Some(SoulSeed::File(path))
            }
            (None, Some(text)) => Some(SoulSeed::Text(text)),
            (None, None) => None,
        };
    }
    top.finish(problems);

    // References between animations.
    for (name, clip) in &clips {
        for other in clip.next.iter().chain(clip.variants.iter()) {
            if !declared.iter().any(|n| n == other) {
                problems.push(error(format!(
                    "[animaciones.{name}] nombra a \"{other}\", que no existe"
                )));
            }
        }
    }
    if looks
        .get(&Look::Idle)
        .is_none_or(|name| !clips.contains_key(name))
    {
        let why = if looks.contains_key(&Look::Idle) {
            "la animación de `idle` no tiene ningún cuadro que exista"
        } else {
            "falta la animación `idle` (es la única obligatoria: todo lo demás cae en ella)"
        };
        problems.push(fatal(why.into()));
    }
    Some(Pack {
        dir: dir.to_owned(),
        builtin: false,
        info,
        frame_size,
        anchor,
        scale,
        head,
        fps,
        menubar_icon,
        muted_icon,
        life,
        body,
        frames: art.frames,
        images: art.images,
        clips,
        declared,
        looks,
        actions,
        voice,
        soul,
        problems: Vec::new(),
    })
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn valid_action(name: &str) -> bool {
    !name.is_empty() && !name.chars().any(|c| c.is_whitespace() || c.is_uppercase())
}

fn unit_pair((a, b): (f64, f64), what: &str, problems: &mut Vec<Problem>) -> (f64, f64) {
    if !(0.0..=1.0).contains(&a) || !(0.0..=1.0).contains(&b) {
        problems.push(warning(format!(
            "{what}: los dos números van de 0 a 1 (0.5 es el medio); los recorto"
        )));
    }
    (a.clamp(0.0, 1.0), b.clamp(0.0, 1.0))
}

fn gap_pair(
    (a, b): (f64, f64),
    what: &str,
    default: (f64, f64),
    problems: &mut Vec<Problem>,
) -> (f64, f64) {
    if a <= 0.0 || b < a {
        problems.push(error(format!(
            "[vida] `{what}` son segundos, [mínimo, máximo], como [2, 6]"
        )));
        return default;
    }
    (a, b)
}

fn checked_fps(fps: f64, what: &str, problems: &mut Vec<Problem>) -> Option<f64> {
    if fps > 0.0 && fps <= 60.0 {
        Some(fps)
    } else {
        problems.push(error(format!(
            "{what}: los cuadros por segundo van de más de 0 a 60 (por ejemplo 8 o 12)"
        )));
        None
    }
}

/// The words for `movimiento`, in the order the docs list them.
pub const MOTIONS: &str = "\"ninguno\", \"salto\", \"retroceso\", \"aplastar\", \"tumbo\", \"paso\", \"galope\", \"trepar\", \"vaiven_izq\", \"vaiven_der\", \"mareo\", \"sacudida\" u \"orgullo\"";

fn motion(word: &str) -> Option<Motion> {
    match word {
        "" | "ninguno" | "quieta" => Some(Motion::Still),
        "salto" => Some(Motion::Hop),
        "retroceso" => Some(Motion::Recoil),
        "aplastar" => Some(Motion::Squash),
        "tumbo" => Some(Motion::Tumble),
        "paso" => Some(Motion::Walk),
        "galope" => Some(Motion::Gallop),
        "trepar" => Some(Motion::Climb),
        "vaiven_izq" => Some(Motion::SwayLeft),
        "vaiven_der" => Some(Motion::SwayRight),
        "mareo" => Some(Motion::Dizzy),
        "sacudida" => Some(Motion::Shake),
        "orgullo" => Some(Motion::Proud),
        _ => None,
    }
}

/// Pictures of another size than the frame: placed by `cajas` (a JSON file
/// of boxes, in frame pixels from the bottom-left, `{"nombre": [x, y, ancho,
/// alto]}`), or with `ajuste = "natural"` at their own size, standing
/// bottom-centre. The rest stretch over the frame, as always.
fn place_frames(
    dir: &Path,
    art: &mut Art,
    frame_size: (u32, u32),
    boxes: Option<&str>,
    natural: bool,
    problems: &mut Vec<Problem>,
) {
    let (fw, fh) = (f64::from(frame_size.0), f64::from(frame_size.1));
    let mut table: HashMap<String, Place> = HashMap::new();
    if let Some(file) = boxes {
        let path = dir.join(file);
        let parsed = fs::read_to_string(&path)
            .map_err(|e| spanish_io(&e))
            .and_then(|text| wawi_core::json::parse(&text).map_err(|e| format!("{e:?}")));
        match parsed {
            Ok(wawi_core::json::Json::Object(members)) => {
                for (name, value) in members {
                    let nums: Option<Vec<f64>> = match &value {
                        wawi_core::json::Json::Array(items) => items
                            .iter()
                            .map(|v| match v {
                                wawi_core::json::Json::Number(n) => Some(*n),
                                _ => None,
                            })
                            .collect(),
                        _ => None,
                    };
                    match nums.as_deref() {
                        Some([x, y, w, h]) if *w > 0.0 && *h > 0.0 => {
                            table.insert(
                                name,
                                Place {
                                    x: x / fw,
                                    y: y / fh,
                                    w: w / fw,
                                    h: h / fh,
                                },
                            );
                        }
                        _ => problems.push(error(format!(
                            "[cuadro] cajas: «{name}» tiene que ser [x, y, ancho, alto]"
                        ))),
                    }
                }
            }
            Ok(_) => problems.push(error(format!(
                "[cuadro] cajas: {file} tiene que ser un objeto JSON, {{\"nombre\": [x, y, ancho, alto]}}"
            ))),
            Err(e) => problems.push(error(format!("[cuadro] cajas: no puedo leer {file}: {e}"))),
        }
    }
    for frame in &mut art.frames {
        if frame.cell.is_some() {
            continue;
        }
        let stem = frame.name.rsplit('/').next().unwrap_or(&frame.name);
        if let Some(place) = table.get(&frame.name).or_else(|| table.get(stem)) {
            frame.place = Some(*place);
        } else if natural
            && art.images[frame.image].present
            && let Ok((w, h)) = png::size(&art.images[frame.image].path)
            && (w, h) != frame_size
        {
            let (w, h) = (f64::from(w) / fw, f64::from(h) / fh);
            frame.place = Some(Place {
                x: (1.0 - w) / 2.0,
                y: 0.0,
                w,
                h,
            });
        }
    }
}

/// One `[animaciones.<name>]`, compiled. Missing frames are left out (and
/// reported); an animation with no frame left is `None`.
fn animation(
    name: &str,
    table: &Table,
    pack_fps: f64,
    art: &mut Art,
    needs_size: &mut Vec<String>,
    problems: &mut Vec<Problem>,
) -> Option<Clip> {
    let place = format!("[animaciones.{name}] ");
    let mut f = Fields::new(table, place.clone());
    let list = f.strings("cuadros", problems);
    let sequence = f.string("secuencia", problems);
    let sheet = f.string("hoja", problems);
    let columns = f.number("columnas", problems);
    let rows = f.number("filas", problems);
    let total = f.number("total", problems);
    let order = f.numbers("orden", problems);
    let fps = f
        .number("fps", problems)
        .and_then(|n| checked_fps(n, &format!("{place}fps"), problems))
        .unwrap_or(pack_fps);
    let durations = f.numbers("duraciones", problems);
    let looped = f.boolean("repetir", problems).unwrap_or(true);
    let next = f.string("siguiente", problems);
    let variants = f.strings("variantes", problems).unwrap_or_default();
    let xfade = f.number("fundido", problems).unwrap_or(0.0);
    let one_motion = f.string("movimiento", problems);
    let motions = f.strings("movimientos", problems);
    let mouth = f.strings("boca", problems);
    let description = f.string("descripcion", problems);
    let layer_name = f.string("capa", problems);
    let layer_at = f.pair("capa_en", problems);
    f.finish(problems);
    let layer = layer_name.and_then(|name| {
        let id = art.file_frame(&name);
        match id.filter(|id| art.images[art.frames[usize::from(*id)].image].present) {
            Some(frame) => Some(Layer {
                frame,
                at: layer_at.unwrap_or((0.0, 0.0)),
            }),
            None => {
                problems.push(error(format!("{place}`capa`: falta el cuadro {name}.png")));
                None
            }
        }
    });

    let sources = [list.is_some(), sequence.is_some(), sheet.is_some()]
        .iter()
        .filter(|x| **x)
        .count();
    if sources > 1 {
        problems.push(error(format!(
            "{place}usá una sola fuente de cuadros: `cuadros`, `secuencia` u `hoja`"
        )));
    }
    if sources == 0 && mouth.is_none() {
        problems.push(error(format!(
            "{place}no tiene cuadros: poné `cuadros = [...]`, `secuencia = \"carpeta\"` u `hoja = \"archivo.png\"`"
        )));
        return None;
    }
    if !(0.0..=5.0).contains(&xfade) {
        problems.push(error(format!("{place}`fundido` son segundos, entre 0 y 5")));
    }

    // The frames, in order. `None` marks a missing one (still counted, so
    // `duraciones` and `movimientos` keep lining up).
    let mut frames: Vec<Option<FrameId>> = Vec::new();
    let mut missing = Vec::new();
    if let Some(list) = &list {
        if list.is_empty() {
            problems.push(error(format!("{place}`cuadros` está vacía")));
        }
        for frame in list {
            let id = art.file_frame(frame);
            let present =
                id.is_some_and(|id| art.images[art.frames[usize::from(id)].image].present);
            if !present {
                missing.push(frame.clone());
            }
            frames.push(id.filter(|_| present));
        }
    } else if let Some(seq) = &sequence {
        let dir = art.folder.join(seq);
        match fs::read_dir(&dir) {
            Ok(entries) => {
                let mut files: Vec<String> = entries
                    .filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.to_ascii_lowercase().ends_with(".png") && !n.starts_with('.'))
                    .collect();
                files.sort_by(|a, b| natural_cmp(a, b));
                if files.is_empty() {
                    problems.push(error(format!(
                        "{place}la carpeta `{seq}` no tiene ningún .png"
                    )));
                }
                for file in files {
                    frames.push(art.file_frame(&format!("{seq}/{file}")));
                }
            }
            Err(e) => problems.push(error(format!(
                "{place}`secuencia`: la carpeta {} {}",
                dir.display(),
                spanish_io(&e)
            ))),
        }
    } else if let Some(sheet) = &sheet {
        needs_size.push(sheet.clone());
        let image = art.image(art.folder.join(sheet));
        if !art.images[image].present {
            problems.push(error(format!(
                "{place}no encuentro la hoja {}",
                art.folder
                    .join(sheet)
                    .strip_prefix(art.dir)
                    .unwrap_or(Path::new(sheet))
                    .display()
            )));
        }
        let cols = whole(columns, "columnas", &place, problems).unwrap_or(1);
        let rows = whole(rows, "filas", &place, problems).unwrap_or(1);
        let cells = cols * rows;
        let total = whole(total, "total", &place, problems).unwrap_or(cells);
        if total > cells {
            problems.push(error(format!(
                "{place}`total = {total}` es más que columnas × filas ({cells})"
            )));
        }
        let total = total.min(cells);
        let indices: Vec<u32> = match &order {
            Some(order) => order
                .iter()
                .filter_map(|n| {
                    let ok = n.fract() == 0.0 && *n >= 0.0 && (*n as u32) < total;
                    if !ok {
                        problems.push(error(format!(
                            "{place}`orden` tiene {n}, y los cuadros de la hoja van de 0 a {}",
                            total.saturating_sub(1)
                        )));
                    }
                    ok.then_some(*n as u32)
                })
                .collect(),
            None => (0..total).collect(),
        };
        let stem = sheet.trim_end_matches(".png").to_owned();
        for i in indices {
            // (column, row) for now; scaled to pixels once the size is known.
            let cell = Cell {
                x: i % cols,
                y: i / cols,
                w: 0,
                h: 0,
            };
            let id = art.frame(format!("{stem}#{i}"), image, Some(cell));
            frames.push(id.filter(|_| art.images[image].present));
        }
    }
    let mouth_ids = mouth.as_ref().and_then(|names| {
        if names.len() != 3 {
            problems.push(error(format!(
                "{place}`boca` lleva tres cuadros: cerrada, media y abierta"
            )));
            return None;
        }
        let ids: Vec<Option<FrameId>> = names
            .iter()
            .map(|n| {
                let id = art.file_frame(n);
                let ok = id.is_some_and(|id| art.images[art.frames[usize::from(id)].image].present);
                if !ok {
                    missing.push(n.clone());
                }
                id.filter(|_| ok)
            })
            .collect();
        match ids.as_slice() {
            [Some(a), Some(b), Some(c)] => Some([*a, *b, *c]),
            _ => None,
        }
    });
    if frames.is_empty()
        && let Some(m) = mouth_ids
    {
        frames = m.iter().map(|id| Some(*id)).collect();
    }
    for frame in &missing {
        problems.push(error(format!(
            "{place}falta el cuadro {}",
            Path::new(&art.folder.file_name().unwrap_or_default())
                .join(if frame.ends_with(".png") {
                    frame.clone()
                } else {
                    format!("{frame}.png")
                })
                .display()
        )));
    }

    if let Some(d) = &durations {
        if d.len() != frames.len() {
            problems.push(error(format!(
                "{place}`duraciones` tiene {} números y hay {} cuadros: tienen que ser iguales",
                d.len(),
                frames.len()
            )));
        }
        if d.iter().any(|s| *s <= 0.0 || *s > 60.0) {
            problems.push(error(format!(
                "{place}`duraciones` son segundos por cuadro, más que 0 (por ejemplo 0.15)"
            )));
        }
    }
    let default_motion = match one_motion.as_deref().map(|w| (w, motion(w))) {
        Some((_, Some(m))) => m,
        Some((w, None)) => {
            problems.push(error(format!(
                "{place}`movimiento = \"{w}\"`: puede ser {MOTIONS}"
            )));
            Motion::Still
        }
        None => Motion::Still,
    };
    if let Some(m) = &motions
        && m.len() != frames.len()
    {
        problems.push(error(format!(
            "{place}`movimientos` tiene {} valores y hay {} cuadros",
            m.len(),
            frames.len()
        )));
    }
    let steps: Vec<Step> = frames
        .iter()
        .enumerate()
        .filter_map(|(i, id)| {
            let frame = (*id)?;
            let secs = durations
                .as_ref()
                .and_then(|d| d.get(i))
                .copied()
                .filter(|s| *s > 0.0 && *s <= 60.0)
                .unwrap_or(1.0 / fps);
            let motion = motions
                .as_ref()
                .and_then(|m| m.get(i))
                .and_then(|w| {
                    let m = motion(w);
                    if m.is_none() {
                        problems.push(error(format!(
                            "{place}`movimientos`: \"{w}\" no es ninguno de {MOTIONS}"
                        )));
                    }
                    m
                })
                .unwrap_or(default_motion);
            Some(Step {
                frame,
                secs,
                motion,
            })
        })
        .collect();
    if steps.is_empty() {
        return None;
    }
    Some(Clip {
        name: name.to_owned(),
        steps,
        looped,
        xfade: xfade.clamp(0.0, 5.0),
        next,
        variants,
        mouth: mouth_ids,
        description,
        layer,
    })
}

fn whole(n: Option<f64>, what: &str, place: &str, problems: &mut Vec<Problem>) -> Option<u32> {
    let n = n?;
    if (1.0..=1024.0).contains(&n) && n.fract() == 0.0 {
        Some(n as u32)
    } else {
        problems.push(error(format!(
            "{place}`{what}` tiene que ser un número entero, 1 o más"
        )));
        None
    }
}

/// `frame2` before `frame10`, as people number files.
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    fn chunks(s: &str) -> Vec<(bool, String)> {
        let mut out: Vec<(bool, String)> = Vec::new();
        for c in s.chars() {
            let digit = c.is_ascii_digit();
            match out.last_mut() {
                Some((d, chunk)) if *d == digit => chunk.push(c),
                _ => out.push((digit, c.to_string())),
            }
        }
        out
    }
    let (ca, cb) = (chunks(a), chunks(b));
    for (x, y) in ca.iter().zip(cb.iter()) {
        let ord = match (x, y) {
            ((true, x), (true, y)) => x
                .trim_start_matches('0')
                .len()
                .cmp(&y.trim_start_matches('0').len())
                .then_with(|| x.trim_start_matches('0').cmp(y.trim_start_matches('0'))),
            ((_, x), (_, y)) => x.cmp(y),
        };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    ca.len().cmp(&cb.len())
}

impl Pack {
    /// Every animation the manifest declares (validator).
    pub(crate) fn declared(&self) -> &[String] {
        &self.declared
    }

    /// Looks mapped to an animation that the manifest declares.
    pub(crate) fn look_names(&self) -> &HashMap<Look, String> {
        &self.looks
    }

    pub(crate) fn action_pairs(&self) -> &[(String, String)] {
        &self.actions
    }
}

#[cfg(test)]
mod tests;
