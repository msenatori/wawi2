//! When Wawi may look on her own. Pure: the clock, the dice and the probes
//! of the Mac come in from outside, so every rule is tested with fakes.
//!
//! The host's `[[watch]]` asks `momento` every 20 minutes. It says yes only
//! when ALL of this holds, cheap checks first:
//!
//! 1. The owner did not pause it («no mires la pantalla»).
//! 2. Not in quiet hours (23 to 8).
//! 3. Fewer than [`Reglas::max_dia`] looks today (2).
//! 4. At least [`Reglas::enfriamiento`] since the last look (3 h).
//! 5. At least [`Reglas::tras_dueno`] since the owner's last turn (10 min).
//! 6. The dice say so ([`Reglas::probabilidad`] per check): random timing.
//! 7. The Mac says it is a good moment ([`Senales`]): Screen Recording is
//!    allowed, the screen is not locked, asleep or under the screensaver,
//!    no full-screen app or meeting app is in front, no camera is in use,
//!    no Zoom call, and someone touched the keyboard or mouse lately.
//!
//! Then the core's instinct gate still decides (quiet hours, gaps, never
//! over the owner). A yes counts as a look even if the gate says no later.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq)]
pub struct Reglas {
    pub max_dia: usize,
    pub enfriamiento: i64,
    pub tras_dueno: i64,
    pub probabilidad: f64,
    /// Local minutes: quiet from..until (wraps past midnight).
    pub silencio: (u16, u16),
    /// Someone touched the keyboard or mouse in this many seconds.
    pub presente: u64,
}

impl Default for Reglas {
    fn default() -> Self {
        Self {
            max_dia: 2,
            enfriamiento: 3 * 3600,
            tras_dueno: 10 * 60,
            probabilidad: 0.12,
            silencio: (23 * 60, 8 * 60),
            presente: 5 * 60,
        }
    }
}

/// What the plugin remembers: `<home>/curiosidad/estado.json`. Never an
/// image: only when it looked.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Estado {
    /// Unix seconds of each look (the last few days).
    #[serde(default)]
    pub miradas: Vec<i64>,
    /// Paused until then. `i64::MAX` is «until the owner says so».
    #[serde(default)]
    pub pausa_hasta: Option<i64>,
}

impl Estado {
    pub fn pausada(&self, ahora: i64) -> bool {
        self.pausa_hasta.is_some_and(|until| ahora < until)
    }
}

/// What the Mac says right now. `None` when a probe could not tell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Senales {
    pub permiso: bool,
    pub bloqueada: bool,
    pub salvapantallas: bool,
    pub dormida: bool,
    pub pantalla_completa: bool,
    pub reunion: bool,
    pub camara: bool,
    /// Seconds since the last keyboard or mouse event.
    pub inactivo: Option<u64>,
}

impl Senales {
    /// Why the screen cannot be looked at at all, even when asked.
    pub fn impide_mirar(&self) -> Option<&'static str> {
        if !self.permiso {
            return Some("sin permiso de Grabación de pantalla");
        }
        if self.bloqueada {
            return Some("la pantalla está bloqueada");
        }
        if self.salvapantallas {
            return Some("está el protector de pantalla");
        }
        if self.dormida {
            return Some("la pantalla está apagada");
        }
        None
    }
}

fn quiet(silencio: (u16, u16), minute: u16) -> bool {
    let (from, until) = silencio;
    if from == until {
        false
    } else if from < until {
        (from..until).contains(&minute)
    } else {
        minute >= from || minute < until
    }
}

/// Whether to look now. `Ok` records the look in `estado`; `Err` says why
/// not. `minuto` is the local minute of the day, `hoy` the Unix second of
/// local midnight, `dado` a number in 0..1, and `senales` is only asked
/// once every cheap rule passed.
#[allow(clippy::too_many_arguments)]
pub fn decidir(
    reglas: &Reglas,
    estado: &mut Estado,
    ahora: i64,
    minuto: u16,
    hoy: i64,
    ultimo_turno: i64,
    dado: f64,
    senales: impl FnOnce() -> Senales,
) -> Result<(), &'static str> {
    estado.miradas.retain(|t| ahora - t < 7 * 86_400);
    if estado.pausada(ahora) {
        return Err("el dueño la pausó");
    }
    if quiet(reglas.silencio, minuto) {
        return Err("horas de silencio");
    }
    if estado.miradas.iter().filter(|t| **t >= hoy).count() >= reglas.max_dia {
        return Err("ya miró lo suyo hoy");
    }
    if estado
        .miradas
        .iter()
        .any(|t| ahora - t < reglas.enfriamiento)
    {
        return Err("miró hace poco");
    }
    if ahora - ultimo_turno < reglas.tras_dueno {
        return Err("el dueño habló hace poco");
    }
    if dado >= reglas.probabilidad {
        return Err("no tocó");
    }
    let s = senales();
    if let Some(why) = s.impide_mirar() {
        return Err(why);
    }
    if s.pantalla_completa {
        return Err("hay una app a pantalla completa");
    }
    if s.reunion || s.camara {
        return Err("parece una reunión");
    }
    if s.inactivo.is_none_or(|idle| idle > reglas.presente) {
        return Err("el dueño no está");
    }
    estado.miradas.push(ahora);
    Ok(())
}

/// A small xorshift, seeded from the clock and the process: the dice.
pub struct Dado(u64);

impl Dado {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    pub fn tirar(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIA: i64 = 1_800_000_000;
    const H: i64 = 3600;

    fn buenas() -> Senales {
        Senales {
            permiso: true,
            inactivo: Some(10),
            ..Senales::default()
        }
    }

    fn mirar(
        estado: &mut Estado,
        hora: i64,
        ultimo_turno: i64,
        dado: f64,
    ) -> Result<(), &'static str> {
        decidir(
            &Reglas::default(),
            estado,
            DIA + hora,
            (hora / 60) as u16,
            DIA,
            DIA + ultimo_turno,
            dado,
            buenas,
        )
    }

    #[test]
    fn at_most_two_a_day_three_hours_apart() {
        let mut e = Estado::default();
        assert_eq!(mirar(&mut e, 10 * H, 0, 0.0), Ok(()));
        assert_eq!(mirar(&mut e, 12 * H, 0, 0.0), Err("miró hace poco"));
        assert_eq!(mirar(&mut e, 13 * H, 0, 0.0), Ok(()));
        assert_eq!(mirar(&mut e, 20 * H, 0, 0.0), Err("ya miró lo suyo hoy"));
        // Tomorrow is another day.
        let tomorrow = decidir(
            &Reglas::default(),
            &mut e,
            DIA + 34 * H,
            600,
            DIA + 24 * H,
            DIA,
            0.0,
            buenas,
        );
        assert_eq!(tomorrow, Ok(()));
    }

    #[test]
    fn quiet_hours_the_owner_and_the_dice() {
        let mut e = Estado::default();
        assert_eq!(
            mirar(&mut e, 23 * H + 5 * 60, 0, 0.0),
            Err("horas de silencio")
        );
        assert_eq!(mirar(&mut e, 7 * H, 0, 0.0), Err("horas de silencio"));
        assert_eq!(
            mirar(&mut e, 10 * H, 10 * H - 60, 0.0),
            Err("el dueño habló hace poco")
        );
        assert_eq!(mirar(&mut e, 10 * H, 0, 0.5), Err("no tocó"));
        assert!(e.miradas.is_empty());
        e.pausa_hasta = Some(i64::MAX);
        assert_eq!(mirar(&mut e, 10 * H, 0, 0.0), Err("el dueño la pausó"));
    }

    #[test]
    fn the_mac_must_say_it_is_a_good_moment() {
        type Tweak = fn(&mut Senales);
        let cases: [(Tweak, &str); 7] = [
            (
                |s| s.permiso = false,
                "sin permiso de Grabación de pantalla",
            ),
            (|s| s.bloqueada = true, "la pantalla está bloqueada"),
            (|s| s.salvapantallas = true, "está el protector de pantalla"),
            (
                |s| s.pantalla_completa = true,
                "hay una app a pantalla completa",
            ),
            (|s| s.reunion = true, "parece una reunión"),
            (|s| s.camara = true, "parece una reunión"),
            (|s| s.inactivo = Some(3600), "el dueño no está"),
        ];
        for (tweak, why) in cases {
            let mut s = buenas();
            tweak(&mut s);
            let mut e = Estado::default();
            let got = decidir(
                &Reglas::default(),
                &mut e,
                DIA + 10 * H,
                600,
                DIA,
                DIA,
                0.0,
                || s,
            );
            assert_eq!(got, Err(why));
            assert!(e.miradas.is_empty());
        }
    }

    #[test]
    fn the_probes_run_only_after_the_cheap_rules() {
        let mut e = Estado::default();
        let got = decidir(
            &Reglas::default(),
            &mut e,
            DIA + 10 * H,
            600,
            DIA,
            DIA,
            0.9,
            || panic!("probed the Mac for nothing"),
        );
        assert_eq!(got, Err("no tocó"));
    }

    #[test]
    fn the_dice_average_about_right() {
        let mut d = Dado::new(42);
        let hits = (0..10_000).filter(|_| d.tirar() < 0.12).count();
        assert!((1000..1400).contains(&hits), "{hits}");
    }
}
