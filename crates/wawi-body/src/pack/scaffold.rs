//! `mascota nueva`: a starter pack with placeholder art (a round little
//! creature drawn by code) and a commented `mascota.toml`, so an artist can
//! replace the drawings one by one and see each change right away.

use std::fs;
use std::path::{Path, PathBuf};

use super::{MANIFEST, png};

/// Placeholder frame size, in pixels.
const W: u32 = 160;
const H: u32 = 160;

type Rgba = [u8; 4];

struct Canvas {
    px: Vec<f32>,
}

impl Canvas {
    fn new() -> Self {
        Self {
            px: vec![0.0; (W * H * 4) as usize],
        }
    }

    /// Paints every pixel `inside` says is in the shape (4×4 samples per
    /// pixel, so the edges are smooth). Coordinates: pixels, y down.
    fn fill(&mut self, color: Rgba, inside: impl Fn(f32, f32) -> bool) {
        let a = f32::from(color[3]) / 255.0;
        for y in 0..H {
            for x in 0..W {
                let mut hits = 0;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let (fx, fy) = (
                            x as f32 + (sx as f32 + 0.5) / 4.0,
                            y as f32 + (sy as f32 + 0.5) / 4.0,
                        );
                        if inside(fx, fy) {
                            hits += 1;
                        }
                    }
                }
                if hits == 0 {
                    continue;
                }
                let cover = a * hits as f32 / 16.0;
                let i = ((y * W + x) * 4) as usize;
                for (c, channel) in color.iter().take(3).enumerate() {
                    let src = f32::from(*channel) / 255.0;
                    self.px[i + c] = src * cover + self.px[i + c] * (1.0 - cover);
                }
                self.px[i + 3] = cover + self.px[i + 3] * (1.0 - cover);
            }
        }
    }

    fn disc(&mut self, color: Rgba, (cx, cy): (f32, f32), (rx, ry): (f32, f32)) {
        self.fill(color, |x, y| {
            let (dx, dy) = ((x - cx) / rx, (y - cy) / ry);
            dx * dx + dy * dy <= 1.0
        });
    }

    fn triangle(&mut self, color: Rgba, a: (f32, f32), b: (f32, f32), c: (f32, f32)) {
        let side = |p: (f32, f32), q: (f32, f32), x: f32, y: f32| {
            (q.0 - p.0) * (y - p.1) - (q.1 - p.1) * (x - p.0)
        };
        self.fill(color, |x, y| {
            let (d1, d2, d3) = (side(a, b, x, y), side(b, c, x, y), side(c, a, x, y));
            let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
            let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
            !(neg && pos)
        });
    }

    fn line(&mut self, color: Rgba, a: (f32, f32), b: (f32, f32), width: f32) {
        self.fill(color, |x, y| {
            let (vx, vy) = (b.0 - a.0, b.1 - a.1);
            let len2 = (vx * vx + vy * vy).max(1e-6);
            let t = (((x - a.0) * vx + (y - a.1) * vy) / len2).clamp(0.0, 1.0);
            let (px, py) = (a.0 + t * vx - x, a.1 + t * vy - y);
            px * px + py * py <= (width / 2.0) * (width / 2.0)
        });
    }

    fn png(&self) -> Vec<u8> {
        let bytes: Vec<u8> = self
            .px
            .iter()
            .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
            .collect();
        png::encode_rgba(W, H, &bytes)
    }
}

#[derive(Clone, Copy, Default)]
enum Eyes {
    #[default]
    Open,
    Closed,
    Up,
    Wide,
    Cross,
}

#[derive(Clone, Copy, Default)]
struct Pose {
    /// Pixels up from her resting place.
    lift: f32,
    /// 1 = normal; < 1 squashed.
    squash: f32,
    eyes: Eyes,
    /// 0 closed … 1 wide open.
    mouth: f32,
    ears_up: bool,
    /// A paw raised beside her: none, low or high.
    paw: u8,
    zzz: bool,
}

const INK: Rgba = [40, 36, 52, 255];
const CHEEK: Rgba = [255, 150, 160, 160];
const WHITE: Rgba = [255, 255, 255, 255];

fn draw(body: Rgba, pose: Pose) -> Vec<u8> {
    let mut c = Canvas::new();
    let squash = if pose.squash == 0.0 { 1.0 } else { pose.squash };
    let (rx, ry) = (52.0 / squash.sqrt(), 50.0 * squash);
    let cx = 80.0;
    let cy = H as f32 - 4.0 - ry - pose.lift;
    // Shadow on the ground.
    c.disc([0, 0, 0, 40], (cx, H as f32 - 5.0), (40.0, 4.0));
    // Ears.
    let ear = if pose.ears_up { 26.0 } else { 16.0 };
    let top = cy - ry * 0.75;
    c.triangle(
        body,
        (cx - 46.0, top + 14.0),
        (cx - 38.0, top - ear),
        (cx - 12.0, top),
    );
    c.triangle(
        body,
        (cx + 46.0, top + 14.0),
        (cx + 38.0, top - ear),
        (cx + 12.0, top),
    );
    // Body.
    c.disc(body, (cx, cy), (rx, ry));
    if pose.paw > 0 {
        let py = if pose.paw == 1 { cy - 8.0 } else { cy - 34.0 };
        c.disc(body, (cx + rx + 4.0, py), (11.0, 11.0));
        c.disc(INK, (cx + rx + 4.0, py), (3.0, 3.0));
    }
    // Face.
    let (ex, ey) = (20.0, cy - 8.0);
    for side in [-1.0f32, 1.0] {
        let e = (cx + side * ex, ey);
        match pose.eyes {
            Eyes::Open => c.disc(INK, e, (5.0, 6.5)),
            Eyes::Up => {
                c.disc(WHITE, e, (7.0, 8.0));
                c.disc(INK, (e.0, e.1 - 3.5), (4.0, 4.0));
            }
            Eyes::Wide => {
                c.disc(WHITE, e, (8.5, 9.5));
                c.disc(INK, e, (4.5, 4.5));
            }
            Eyes::Closed => c.line(INK, (e.0 - 7.0, e.1 + 1.0), (e.0 + 7.0, e.1 + 1.0), 3.0),
            Eyes::Cross => {
                c.line(INK, (e.0 - 6.0, e.1 - 6.0), (e.0 + 6.0, e.1 + 6.0), 3.0);
                c.line(INK, (e.0 - 6.0, e.1 + 6.0), (e.0 + 6.0, e.1 - 6.0), 3.0);
            }
        }
        c.disc(CHEEK, (cx + side * 34.0, cy + 6.0), (7.0, 4.5));
    }
    let my = cy + 12.0;
    if pose.mouth <= 0.05 {
        c.line(INK, (cx - 7.0, my), (cx, my + 3.0), 2.5);
        c.line(INK, (cx, my + 3.0), (cx + 7.0, my), 2.5);
    } else {
        c.disc(INK, (cx, my + 3.0), (7.0, 2.0 + 8.0 * pose.mouth));
    }
    if pose.zzz {
        for (i, s) in [(0.0f32, 5.0f32), (1.0, 7.0)] {
            let (zx, zy) = (cx + 44.0 + i * 12.0, cy - ry - 4.0 - i * 16.0);
            c.line(INK, (zx - s, zy - s), (zx + s, zy - s), 2.5);
            c.line(INK, (zx + s, zy - s), (zx - s, zy + s), 2.5);
            c.line(INK, (zx - s, zy + s), (zx + s, zy + s), 2.5);
        }
    }
    c.png()
}

/// A body colour from the id, so two new pets do not look the same.
fn colour_for(id: &str) -> Rgba {
    const PALETTE: [Rgba; 6] = [
        [244, 182, 96, 255],
        [150, 196, 240, 255],
        [176, 214, 150, 255],
        [222, 170, 230, 255],
        [240, 160, 140, 255],
        [200, 200, 210, 255],
    ];
    let h = id
        .bytes()
        .fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
    PALETTE[h as usize % PALETTE.len()]
}

fn manifest(id: &str, nombre: &str) -> String {
    format!(
        r##"# La ficha de tu mascota. Las líneas con # son comentarios: Wawi las ignora.
# Guía completa: docs/MASCOTAS.md. Para revisarla: mascota validar <esta carpeta>

id = "{id}"                # igual que el nombre de la carpeta
nombre = "{nombre}"            # así aparece en el menú: «Mostrar a {nombre}»
autor = "Tu nombre"
licencia = "CC0-1.0"       # los dibujos de ejemplo son libres; cambiala por la de tu arte
version = "0.1"
descripcion = "Una mascota nueva, con dibujos de ejemplo para reemplazar."

[cuadro]
ancho = {W}                # cada cuadro mide esto, en píxeles
alto = {H}
ancla = [0.5, 0.0]         # el punto que pisa el suelo: medio, abajo
escala = 0.8               # puntos en pantalla por píxel ({W} × 0.8 = 128 de ancho)
fps = 8                    # cuadros por segundo si una animación no dice otra cosa
cabeza = [0.5, 0.7]        # adónde apunta el globo de texto

[iconos]
barra = "barra.png"        # el ícono de la barra de menú (opcional)

# Lo que hace sola mientras está quieta.
[vida]
parpadeo = "parpadeo"
cada_parpadeo = [2, 6]     # segundos, mínimo y máximo
gestos = ["saludar"]       # cada tanto hace uno de estos
cada_gesto = [8, 20]
dormir_tras = 20           # segundos quieta antes de dormirse (0 = nunca)
toque = "festejar"         # lo que hace si le hacés clic

# --- Los estados. Solo `idle` es obligatorio: los que faltan usan otro. ---

[animaciones.idle]
cuadros = ["idle_a", "idle_b"]   # archivos en cuadros/ (sin el .png)
duraciones = [1.6, 1.4]          # segundos de cada cuadro (o usá fps)
fundido = 0.3                    # se funden un poco entre cuadro y cuadro

[animaciones.sleeping]
cuadros = ["dormida_a", "dormida_b"]
fps = 0.5

[animaciones.listening]
cuadros = ["escucha"]

[animaciones.thinking]
cuadros = ["piensa"]
# transcribing no está: usa thinking.

[animaciones.speaking]
boca = ["habla_a", "habla_b", "habla_c"]   # boca cerrada, media y abierta

[animaciones.interrupted]
cuadros = ["sorpresa"]
repetir = false
duraciones = [0.7]
movimiento = "retroceso"

[animaciones.error]
cuadros = ["error"]
repetir = false
duraciones = [1.5]

# --- Gestos y acciones ---

[animaciones.parpadeo]
cuadros = ["parpadeo"]
duraciones = [0.15]

[animaciones.saludar]
cuadros = ["saludo_a", "saludo_b", "saludo_a", "saludo_b"]
fps = 4
descripcion = "Saluda con la pata."

[animaciones.festejar]
hoja = "festejar.png"      # una tira: 4 cuadros uno al lado del otro
columnas = 4
filas = 1
fps = 8
movimiento = "salto"
descripcion = "Salta de alegría."

# Nombres que otras partes de Wawi pueden pedir: «saludar», «festejar».
[acciones]
saludar = "saludar"
festejar = "festejar"

# Para que sienta la gravedad, camine y trepe, sacale el # a estas líneas
# y nombrá tus animaciones (ver docs/MASCOTAS.md, «Darle un cuerpo»):
# [cuerpo]
# mira = "izquierda"       # hacia dónde mira tu dibujo
# caer = "idle"            # mientras cae (mejor un dibujo propio)
# caminar = "idle"         # mientras camina (mejor un dibujo propio)
# cuanto_camina = [8, 25]  # minutos entre paseos

# Para más adelante (todavía no se usan):
# [voz]
# id = "ef_dora"           # una voz de Kokoro
# velocidad = 1.0
# tono = 0                 # semitonos
#
# [alma]
# archivo = "alma.md"      # su personalidad, para empezar su SOUL.md
"##
    )
}

/// Writes a new pack `id` inside `parent` and returns its folder. Never
/// overwrites: an existing folder is an error.
pub fn scaffold(parent: &Path, id: &str, nombre: &str) -> Result<PathBuf, String> {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(format!(
            "«{id}» no sirve como id: usá minúsculas, números, `-` o `_` (por ejemplo «kitty»)"
        ));
    }
    if id == super::BUILTIN_ID {
        return Err("«wawi» es la mascota que viene con Wawi: elegí otro id".into());
    }
    let dir = parent.join(id);
    if dir.exists() {
        return Err(format!("{} ya existe: no la piso", dir.display()));
    }
    let frames = dir.join("cuadros");
    fs::create_dir_all(&frames).map_err(|e| format!("no pude crear {}: {e}", frames.display()))?;
    let body = colour_for(id);
    let p = Pose::default;
    let art: Vec<(&str, Pose)> = vec![
        ("idle_a", p()),
        (
            "idle_b",
            Pose {
                squash: 0.97,
                ..p()
            },
        ),
        (
            "parpadeo",
            Pose {
                eyes: Eyes::Closed,
                ..p()
            },
        ),
        (
            "dormida_a",
            Pose {
                eyes: Eyes::Closed,
                squash: 0.9,
                zzz: true,
                ..p()
            },
        ),
        (
            "dormida_b",
            Pose {
                eyes: Eyes::Closed,
                squash: 0.92,
                ..p()
            },
        ),
        (
            "escucha",
            Pose {
                ears_up: true,
                eyes: Eyes::Wide,
                ..p()
            },
        ),
        (
            "piensa",
            Pose {
                eyes: Eyes::Up,
                ..p()
            },
        ),
        ("habla_a", Pose { mouth: 0.0, ..p() }),
        ("habla_b", Pose { mouth: 0.4, ..p() }),
        ("habla_c", Pose { mouth: 1.0, ..p() }),
        (
            "sorpresa",
            Pose {
                eyes: Eyes::Wide,
                mouth: 0.7,
                ears_up: true,
                ..p()
            },
        ),
        (
            "error",
            Pose {
                eyes: Eyes::Cross,
                mouth: 0.3,
                ..p()
            },
        ),
        ("saludo_a", Pose { paw: 1, ..p() }),
        (
            "saludo_b",
            Pose {
                paw: 2,
                mouth: 0.4,
                ..p()
            },
        ),
    ];
    let write = |path: PathBuf, bytes: &[u8]| {
        fs::write(&path, bytes).map_err(|e| format!("no pude escribir {}: {e}", path.display()))
    };
    for (name, pose) in art {
        write(frames.join(format!("{name}.png")), &draw(body, pose))?;
    }
    // The sprite sheet: four jumps side by side.
    let lifts = [0.0, 10.0, 18.0, 8.0];
    let cells: Vec<Vec<u8>> = lifts
        .iter()
        .map(|lift| {
            let png_bytes = draw(
                body,
                Pose {
                    lift: *lift,
                    ears_up: true,
                    mouth: 0.6,
                    paw: 2,
                    ..p()
                },
            );
            raw_rgba(&png_bytes)
        })
        .collect();
    let mut sheet = vec![0u8; (W * 4 * H * 4) as usize];
    for (i, cell) in cells.iter().enumerate() {
        for y in 0..H as usize {
            let src = &cell[y * W as usize * 4..(y + 1) * W as usize * 4];
            let at = (y * (W as usize * 4) + i * W as usize) * 4;
            sheet[at..at + W as usize * 4].copy_from_slice(src);
        }
    }
    write(
        frames.join("festejar.png"),
        &png::encode_rgba(W * 4, H, &sheet),
    )?;
    write(dir.join("barra.png"), &draw(body, p()))?;
    write(dir.join(MANIFEST), manifest(id, nombre).as_bytes())?;
    write(
        dir.join("LICENCIA-arte.md"),
        format!(
            "# Licencia del arte de {nombre}\n\nLos dibujos de ejemplo los generó `mascota nueva` y son de dominio público (CC0-1.0).\nCuando pongas tus propios dibujos, escribí acá quién los hizo y bajo qué licencia, y\ncambiá `licencia` en mascota.toml.\n"
        )
        .as_bytes(),
    )?;
    Ok(dir)
}

/// The pixels back out of a PNG this module wrote (stored, unfiltered).
fn raw_rgba(png_bytes: &[u8]) -> Vec<u8> {
    // Signature 8 + IHDR 25 + IDAT length 4 + "IDAT" 4 + zlib header 2.
    let mut at = 8 + 25 + 8 + 2;
    let mut raw = Vec::new();
    loop {
        let last = png_bytes[at] & 1 == 1;
        let len = usize::from(u16::from_le_bytes([png_bytes[at + 1], png_bytes[at + 2]]));
        at += 5;
        raw.extend_from_slice(&png_bytes[at..at + len]);
        at += len;
        if last {
            break;
        }
    }
    let row = W as usize * 4;
    raw.chunks(row + 1)
        .flat_map(|line| line[1..].to_vec())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_pack_is_valid_and_never_overwrites() {
        let parent = std::env::temp_dir().join(format!("wawi-scaffold-{}", std::process::id()));
        let _ = fs::remove_dir_all(&parent);
        let dir = scaffold(&parent, "kitty", "Kitty").unwrap();
        let report = super::super::validate(&dir);
        assert!(report.ok(), "{report}");
        let pack = report.pack.as_ref().unwrap();
        assert_eq!(pack.name(), "Kitty");
        assert_eq!(pack.actions(), vec!["saludar", "festejar"]);
        assert_eq!(
            pack.frames().iter().filter(|f| f.cell.is_some()).count(),
            4,
            "the sheet is cut in four"
        );
        assert_eq!(
            png::size(&dir.join("cuadros/festejar.png")).unwrap(),
            (W * 4, H)
        );
        assert!(scaffold(&parent, "kitty", "Kitty").is_err());
        assert!(scaffold(&parent, "Kitty Cat", "Kitty").is_err());
        assert!(scaffold(&parent, "wawi", "Wawi").is_err());
        let _ = fs::remove_dir_all(parent);
    }
}
