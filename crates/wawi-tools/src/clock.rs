//! Wall-clock time for the tools: what time it is, in the owner's time zone,
//! and how to say it in Spanish. The zone itself is behind [`WallClock`], so
//! the real one (the system's local time) lives in the program and the tests
//! use [`FakeClock`].

use std::cell::Cell;
use std::rc::Rc;

/// A local date and time, to the second.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Civil {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl Civil {
    pub fn date(year: i32, month: u8, day: u8) -> Self {
        Self {
            year,
            month,
            day,
            hour: 0,
            minute: 0,
            second: 0,
        }
    }

    pub fn at(self, hour: u8, minute: u8, second: u8) -> Self {
        Self {
            hour,
            minute,
            second,
            ..self
        }
    }

    /// Days since 1970-01-01 of this date.
    pub fn days(&self) -> i64 {
        days_from_civil(self.year, self.month, self.day)
    }

    /// 0 = domingo … 6 = sábado.
    pub fn weekday(&self) -> u8 {
        (self.days() + 4).rem_euclid(7) as u8
    }

    /// `2026-11-20`.
    pub fn iso_date(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// `2026-11-20T16:23`.
    pub fn iso_minute(&self) -> String {
        format!("{}T{:02}:{:02}", self.iso_date(), self.hour, self.minute)
    }

    /// Seconds since the epoch if this civil time were UTC.
    fn as_if_utc(&self) -> i64 {
        self.days() * 86_400
            + i64::from(self.hour) * 3_600
            + i64::from(self.minute) * 60
            + i64::from(self.second)
    }

    fn from_utc_seconds(seconds: i64) -> Self {
        let (year, month, day) = civil_from_days(seconds.div_euclid(86_400));
        let rest = seconds.rem_euclid(86_400);
        Self {
            year,
            month,
            day,
            hour: (rest / 3_600) as u8,
            minute: (rest % 3_600 / 60) as u8,
            second: (rest % 60) as u8,
        }
    }
}

/// The owner's clock.
pub trait WallClock {
    /// Seconds since the Unix epoch.
    fn now(&self) -> i64;
    /// The local date and time at `epoch`.
    fn local(&self, epoch: i64) -> Civil;
    /// The epoch of a local date and time, or `None` when that time does not
    /// exist there (the hour skipped when the clocks go forward).
    fn epoch_of(&self, local: Civil) -> Option<i64>;

    fn local_now(&self) -> Civil {
        self.local(self.now())
    }
}

/// A clock for tests and the simulator: a fixed UTC offset and a time that
/// only moves when told to.
#[derive(Debug)]
pub struct FakeClock {
    now: Cell<i64>,
    offset_s: i64,
}

impl FakeClock {
    /// At `local` in a zone `offset_hours` from UTC (`-3` for Buenos Aires).
    pub fn at(local: Civil, offset_hours: i64) -> Rc<Self> {
        let offset_s = offset_hours * 3_600;
        Rc::new(Self {
            now: Cell::new(local.as_if_utc() - offset_s),
            offset_s,
        })
    }

    pub fn set(&self, epoch: i64) {
        self.now.set(epoch);
    }

    pub fn advance(&self, seconds: i64) {
        self.now.set(self.now.get() + seconds);
    }
}

impl WallClock for FakeClock {
    fn now(&self) -> i64 {
        self.now.get()
    }

    fn local(&self, epoch: i64) -> Civil {
        Civil::from_utc_seconds(epoch + self.offset_s)
    }

    fn epoch_of(&self, local: Civil) -> Option<i64> {
        Some(local.as_if_utc() - self.offset_s)
    }
}

/// The hour for the core's `TimeRoute`, from a wall clock.
pub struct RouteClock(pub Rc<dyn WallClock>);

impl wawi_core::Clock for RouteClock {
    fn now_hm(&self) -> (u8, u8) {
        let now = self.0.local_now();
        (now.hour, now.minute)
    }
}

/// Days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
pub fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let month = i64::from(month);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date `days` after 1970-01-01 (Howard Hinnant's `civil_from_days`).
pub fn civil_from_days(days: i64) -> (i32, u8, u8) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    ((yoe + era * 400 + i64::from(month <= 2)) as i32, month, day)
}

fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        _ => 28,
    }
}

/// A local date and time as the model writes it: `YYYY-MM-DDTHH:MM`, with
/// optional `:SS`, and a space allowed instead of the `T`. Nothing else: no
/// zone, no fractions, no words. Every field must be in range.
pub fn parse_local(text: &str) -> Option<Civil> {
    let text = text.trim();
    let bytes = text.as_bytes();
    if !matches!(bytes.len(), 16 | 19)
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !matches!(bytes[10], b'T' | b' ')
        || bytes[13] != b':'
        || (bytes.len() == 19 && bytes[16] != b':')
    {
        return None;
    }
    let number = |from: usize, to: usize| -> Option<u32> {
        let part = &text[from..to];
        part.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| part.parse().ok())
            .flatten()
    };
    let year = number(0, 4)? as i32;
    let month = number(5, 7)? as u8;
    let day = number(8, 10)? as u8;
    let hour = number(11, 13)? as u8;
    let minute = number(14, 16)? as u8;
    let second = if bytes.len() == 19 {
        number(17, 19)? as u8
    } else {
        0
    };
    let valid = (1..=12).contains(&month)
        && day >= 1
        && day <= days_in_month(year, month)
        && hour < 24
        && minute < 60
        && second < 60;
    valid.then_some(Civil {
        year,
        month,
        day,
        hour,
        minute,
        second,
    })
}

const WEEKDAYS: [&str; 7] = [
    "domingo",
    "lunes",
    "martes",
    "miércoles",
    "jueves",
    "viernes",
    "sábado",
];

const MONTHS: [&str; 12] = [
    "enero",
    "febrero",
    "marzo",
    "abril",
    "mayo",
    "junio",
    "julio",
    "agosto",
    "septiembre",
    "octubre",
    "noviembre",
    "diciembre",
];

/// `16:23`, `9:05`.
pub fn clock_time(time: &Civil) -> String {
    format!("{}:{:02}", time.hour, time.minute)
}

/// `a las 16:23`, `a la 1:05`.
fn at_time(time: &Civil) -> String {
    let article = if time.hour == 1 { "a la" } else { "a las" };
    format!("{article} {}", clock_time(time))
}

/// The line the brain gets on every turn:
/// `viernes 20 de noviembre de 2026, 16:23 (2026-11-20T16:23, hora local)`.
pub fn describe_now(now: &Civil) -> String {
    format!(
        "{} {} de {} de {}, {} ({}, hora local)",
        WEEKDAYS[usize::from(now.weekday())],
        now.day,
        MONTHS[usize::from(now.month - 1)],
        now.year,
        clock_time(now),
        now.iso_minute()
    )
}

/// When something happens, as said from `now`: `hoy a las 15:30`,
/// `mañana a las 8:00`, `ayer a las 20:00`, `el jueves 26 de noviembre a las
/// 9:00`, or with the year when it is not this one.
pub fn when(time: &Civil, now: &Civil) -> String {
    let at = at_time(time);
    match time.days() - now.days() {
        0 => format!("hoy {at}"),
        1 => format!("mañana {at}"),
        -1 => format!("ayer {at}"),
        _ => {
            let year = if time.year == now.year {
                String::new()
            } else {
                format!(" de {}", time.year)
            };
            format!(
                "el {} {} de {}{year} {at}",
                WEEKDAYS[usize::from(time.weekday())],
                time.day,
                MONTHS[usize::from(time.month - 1)]
            )
        }
    }
}

/// Until when, said after «hasta»: `las 18:00` today, `la 1:05`, `mañana
/// a las 8:00`, `el jueves 26 de noviembre a las 9:00`.
pub fn until(time: &Civil, now: &Civil) -> String {
    if time.days() == now.days() {
        at_time(time).trim_start_matches("a ").to_owned()
    } else {
        when(time, now)
    }
}

/// A length of time in words, with its two largest units: `1 minuto`,
/// `1 hora y 5 minutos`, `45 segundos`, `2 días y 3 horas`.
pub fn duration(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let units = [
        (seconds / 86_400, "día", "días"),
        (seconds % 86_400 / 3_600, "hora", "horas"),
        (seconds % 3_600 / 60, "minuto", "minutos"),
        (seconds % 60, "segundo", "segundos"),
    ];
    let Some(first) = units.iter().position(|(n, _, _)| *n > 0) else {
        return "0 segundos".into();
    };
    let word =
        |(n, one, many): (i64, &str, &str)| format!("{n} {}", if n == 1 { one } else { many });
    let mut out = word(units[first]);
    if let Some(&next) = units.get(first + 1)
        && next.0 > 0
    {
        out.push_str(" y ");
        out.push_str(&word(next));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn friday() -> Civil {
        Civil::date(2026, 11, 20).at(16, 23, 5)
    }

    #[test]
    fn dates_round_trip_through_days() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2024, 2, 29), 19_782);
        for days in [-1_000, 0, 19_782, 20_412, 60_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(friday().weekday(), 5, "2026-11-20 is a Friday");
    }

    #[test]
    fn the_fake_clock_keeps_its_offset() {
        let clock = FakeClock::at(friday(), -3);
        assert_eq!(clock.local_now(), friday());
        assert_eq!(clock.epoch_of(friday()), Some(clock.now()));
        clock.advance(3_600 * 8);
        assert_eq!(clock.local_now(), Civil::date(2026, 11, 21).at(0, 23, 5));
    }

    #[test]
    fn only_strict_local_datetimes_parse() {
        assert_eq!(
            parse_local("2026-11-20T15:30"),
            Some(Civil::date(2026, 11, 20).at(15, 30, 0))
        );
        assert_eq!(
            parse_local("2026-11-20 15:30:45"),
            Some(Civil::date(2026, 11, 20).at(15, 30, 45))
        );
        assert_eq!(
            parse_local("2028-02-29T08:00"),
            Some(Civil::date(2028, 2, 29).at(8, 0, 0))
        );
        for bad in [
            "",
            "15:30",
            "mañana a las 8",
            "2026-11-20",
            "2026-11-20T15:30Z",
            "2026-11-20T15:30-03:00",
            "2026-11-20T15:30:00.5",
            "2026-11-31T10:00",
            "2027-02-29T10:00",
            "2026-13-01T10:00",
            "2026-11-20T24:00",
            "2026-11-20T10:60",
            "2026-1-20T10:00",
            "2026/11/20T10:00",
        ] {
            assert_eq!(parse_local(bad), None, "{bad}");
        }
    }

    #[test]
    fn until_reads_after_hasta() {
        let now = friday();
        let at = |d: u8, h: u8, m: u8| Civil::date(2026, 11, d).at(h, m, 0);
        assert_eq!(until(&at(20, 18, 0), &now), "las 18:00");
        assert_eq!(until(&at(20, 1, 5), &now), "la 1:05");
        assert_eq!(until(&at(21, 8, 0), &now), "mañana a las 8:00");
        assert!(until(&at(26, 9, 0), &now).starts_with("el jueves 26 de noviembre"));
    }

    #[test]
    fn times_are_said_in_spanish() {
        let now = friday();
        assert_eq!(
            describe_now(&now),
            "viernes 20 de noviembre de 2026, 16:23 (2026-11-20T16:23, hora local)"
        );
        assert_eq!(
            when(&Civil::date(2026, 11, 20).at(17, 0, 0), &now),
            "hoy a las 17:00"
        );
        assert_eq!(
            when(&Civil::date(2026, 11, 21).at(1, 5, 0), &now),
            "mañana a la 1:05"
        );
        assert_eq!(
            when(&Civil::date(2026, 11, 19).at(20, 0, 0), &now),
            "ayer a las 20:00"
        );
        assert_eq!(
            when(&Civil::date(2026, 11, 26).at(9, 0, 0), &now),
            "el jueves 26 de noviembre a las 9:00"
        );
        assert_eq!(
            when(&Civil::date(2027, 1, 4).at(9, 0, 0), &now),
            "el lunes 4 de enero de 2027 a las 9:00"
        );
    }

    #[test]
    fn durations_keep_two_units() {
        assert_eq!(duration(60), "1 minuto");
        assert_eq!(duration(45), "45 segundos");
        assert_eq!(duration(90), "1 minuto y 30 segundos");
        assert_eq!(duration(3_900), "1 hora y 5 minutos");
        assert_eq!(duration(3_600 * 5 + 7), "5 horas");
        assert_eq!(duration(86_400 * 2 + 3_600 * 3 + 60), "2 días y 3 horas");
        assert_eq!(duration(0), "0 segundos");
    }
}
