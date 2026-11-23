//! When watches and routines run: `every = "15m"` or `at = "lun-vie 09:00"`,
//! on an injected wall clock, plus the argument templates (`{hoy}`, `{dia}`,
//! `{dia_o_finde}`). The scheduler only says what is due; the host runs it.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;
use wawi_tools::Civil;

/// An `at` that is late by more than this is skipped for the day: starting
/// Wawi at 9:20 still runs the 9:00 watch, starting her at 15:00 does not.
pub const AT_GRACE_MINUTES: u16 = 60;

/// `90s`, `15m`, `2h`, `1d`, or several together (`1h30m`).
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let bad = || format!("«{text}» no es una duración (por ejemplo 90s, 15m, 2h, 1d)");
    if text.is_empty() {
        return Err(bad());
    }
    let mut total: u64 = 0;
    let mut number = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() {
            number.push(c);
            continue;
        }
        let unit = match c {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            ' ' if number.is_empty() => continue,
            _ => return Err(bad()),
        };
        let n: u64 = number.parse().map_err(|_| bad())?;
        total = total.saturating_add(n.saturating_mul(unit));
        number.clear();
    }
    if !number.is_empty() || total == 0 {
        return Err(bad());
    }
    Ok(Duration::from_secs(total))
}

/// A time of day on some weekdays. `days[0]` is Sunday, like
/// `Civil::weekday`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct At {
    pub days: [bool; 7],
    pub minute: u16,
}

const DAY_NAMES: [(&str, u8); 18] = [
    ("dom", 0),
    ("domingo", 0),
    ("lun", 1),
    ("lunes", 1),
    ("mar", 2),
    ("martes", 2),
    ("mie", 3),
    ("mié", 3),
    ("miercoles", 3),
    ("miércoles", 3),
    ("jue", 4),
    ("jueves", 4),
    ("vie", 5),
    ("viernes", 5),
    ("sab", 6),
    ("sáb", 6),
    ("sabado", 6),
    ("sábado", 6),
];

fn day(name: &str) -> Option<u8> {
    DAY_NAMES
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, day)| *day)
}

/// `09:00` (every day), `lun-vie 09:00`, `vie 18:30`, `sab,dom 10:00`,
/// `finde 11:00`, `todos 08:00`.
pub fn parse_at(text: &str) -> Result<At, String> {
    let bad = |why: &str| format!("«{text}» no es un horario ({why}); por ejemplo «lun-vie 09:00»");
    let lower = text.trim().to_lowercase();
    let (days_part, time_part) = match lower.rsplit_once(' ') {
        Some((days, time)) => (days.trim(), time.trim()),
        None => ("", lower.as_str()),
    };
    let (hour, minute) = time_part
        .split_once(':')
        .ok_or_else(|| bad("falta la hora"))?;
    let hour: u16 = hour.parse().map_err(|_| bad("hora"))?;
    let minute: u16 = minute.parse().map_err(|_| bad("minutos"))?;
    if hour > 23 || minute > 59 {
        return Err(bad("hora fuera de rango"));
    }
    let mut days = [false; 7];
    if days_part.is_empty() || matches!(days_part, "todos" | "diario" | "todos los dias") {
        days = [true; 7];
    }
    for piece in days_part
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        if matches!(piece, "todos" | "diario" | "todos los dias") {
            continue;
        }
        if piece == "finde" {
            days[0] = true;
            days[6] = true;
            continue;
        }
        match piece.split_once('-') {
            Some((from, to)) => {
                let from = day(from.trim()).ok_or_else(|| bad("día"))?;
                let to = day(to.trim()).ok_or_else(|| bad("día"))?;
                let mut d = from;
                loop {
                    days[d as usize] = true;
                    if d == to {
                        break;
                    }
                    d = (d + 1) % 7;
                }
            }
            None => days[day(piece).ok_or_else(|| bad("día"))? as usize] = true,
        }
    }
    Ok(At {
        days,
        minute: hour * 60 + minute,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum When {
    Every(Duration),
    At(At),
}

pub fn minute_of_day(time: &Civil) -> u16 {
    u16::from(time.hour) * 60 + u16::from(time.minute)
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

/// The Spanish weekday, lowercase: `lunes`, `miércoles`.
pub fn weekday_name(time: &Civil) -> &'static str {
    WEEKDAYS[time.weekday() as usize]
}

/// `Finde` on Saturday and Sunday, otherwise the weekday capitalized:
/// `Lunes`, `Martes`, `Miércoles`, `Jueves`, `Viernes`.
pub fn weekday_or_weekend(time: &Civil) -> String {
    match time.weekday() {
        0 | 6 => "Finde".to_owned(),
        _ => {
            let name = weekday_name(time);
            let mut chars = name.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        }
    }
}

/// Replaces `{hoy}`, `{dia}` and `{dia_o_finde}` in every string inside
/// `value`, keys left alone.
pub fn expand(value: &Value, now: &Civil) -> Value {
    match value {
        Value::String(text) => Value::String(
            text.replace("{hoy}", &now.iso_date())
                .replace("{dia_o_finde}", &weekday_or_weekend(now))
                .replace("{dia}", weekday_name(now)),
        ),
        Value::Array(items) => Value::Array(items.iter().map(|v| expand(v, now)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), expand(v, now)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// One scheduled thing. `key` is `plugin/name`.
#[derive(Clone, Debug)]
pub struct Entry {
    pub key: String,
    pub plugin: String,
    pub when: When,
}

/// Decides what is due. `at` entries remember the day they last ran in a
/// small JSON file, so a restart does not run the morning watch twice.
pub struct Scheduler {
    entries: Vec<Entry>,
    /// `every` entries: the Unix second they are due next.
    next_every: BTreeMap<String, i64>,
    /// `at` entries: the local date they last ran.
    ran_on: BTreeMap<String, String>,
    state_file: Option<PathBuf>,
}

impl Scheduler {
    pub fn new(entries: Vec<Entry>, state_file: Option<PathBuf>) -> Self {
        let ran_on = state_file
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self {
            entries,
            next_every: BTreeMap::new(),
            ran_on,
            state_file,
        }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The keys due now. `ready` says whether a plugin can take calls; an
    /// entry of a plugin that is not ready stays due. `every` entries run
    /// first as soon as their plugin is ready.
    pub fn due(&mut self, now: &Civil, epoch: i64, ready: impl Fn(&str) -> bool) -> Vec<String> {
        let mut due = Vec::new();
        let today = now.iso_date();
        let minute = minute_of_day(now);
        let mut changed = false;
        for entry in &self.entries {
            if !ready(&entry.plugin) {
                continue;
            }
            match entry.when {
                When::Every(every) => {
                    let next = self.next_every.entry(entry.key.clone()).or_insert(epoch);
                    if epoch >= *next {
                        *next = epoch + every.as_secs() as i64;
                        due.push(entry.key.clone());
                    }
                }
                When::At(at) => {
                    let on_day = at.days[now.weekday() as usize];
                    let in_window = minute >= at.minute && minute < at.minute + AT_GRACE_MINUTES;
                    let already = self.ran_on.get(&entry.key) == Some(&today);
                    if on_day && in_window && !already {
                        self.ran_on.insert(entry.key.clone(), today.clone());
                        changed = true;
                        due.push(entry.key.clone());
                    }
                }
            }
        }
        if changed {
            self.save();
        }
        due
    }

    /// Written to a temporary file and renamed. A failure only means a
    /// restart may repeat today's run.
    fn save(&self) {
        let Some(path) = &self.state_file else {
            return;
        };
        let Ok(text) = serde_json::to_string_pretty(&self.ran_on) else {
            return;
        };
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn monday() -> Civil {
        // 2026-11-23 is a Monday.
        Civil::date(2026, 11, 23).at(9, 0, 0)
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("15m"), Ok(Duration::from_secs(900)));
        assert_eq!(parse_duration("1h30m"), Ok(Duration::from_secs(5400)));
        assert_eq!(parse_duration("2d"), Ok(Duration::from_secs(172_800)));
        assert_eq!(parse_duration("2h 30m"), Ok(Duration::from_secs(9000)));
        for bad in ["", "15", "m", "0m", "15x", "-1m"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn times_of_day_on_weekdays() {
        let at = parse_at("lun-vie 09:00").unwrap();
        assert_eq!(at.minute, 540);
        assert_eq!(at.days, [false, true, true, true, true, true, false]);
        assert_eq!(parse_at("18:30").unwrap().days, [true; 7]);
        assert_eq!(
            parse_at("Vie 18:30").unwrap().days,
            [false, false, false, false, false, true, false]
        );
        assert_eq!(
            parse_at("finde 10:00").unwrap().days,
            parse_at("sab,dom 10:00").unwrap().days
        );
        assert_eq!(
            parse_at("vie-lun 07:05").unwrap().days,
            [true, true, false, false, false, true, true]
        );
        for bad in ["lun-vie", "25:00", "lun 9", "xyz 09:00"] {
            assert!(parse_at(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn weekday_templates() {
        let days: Vec<String> = (0..7)
            .map(|i| weekday_or_weekend(&Civil::date(2026, 11, 22 + i)))
            .collect();
        assert_eq!(
            days,
            [
                "Finde",
                "Lunes",
                "Martes",
                "Miércoles",
                "Jueves",
                "Viernes",
                "Finde"
            ]
        );
        let args = json!({"team": "Life", "state": "{dia_o_finde}", "q": ["{hoy} {dia}"], "n": 3});
        assert_eq!(
            expand(&args, &monday()),
            json!({"team": "Life", "state": "Lunes", "q": ["2026-11-23 lunes"], "n": 3})
        );
    }

    #[test]
    fn at_runs_once_a_day_within_its_window_and_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("wawi-plugins-sched-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let state = dir.join("estado.json");
        let _ = std::fs::remove_file(&state);
        let entries = vec![Entry {
            key: "p/hoy".into(),
            plugin: "p".into(),
            when: When::At(parse_at("lun-vie 09:00").unwrap()),
        }];
        let mut s = Scheduler::new(entries.clone(), Some(state.clone()));
        let early = Civil::date(2026, 11, 23).at(8, 59, 0);
        assert!(s.due(&early, 0, |_| true).is_empty());
        // Not ready yet: stays due.
        assert!(s.due(&monday(), 0, |_| false).is_empty());
        assert_eq!(s.due(&monday(), 0, |_| true), ["p/hoy"]);
        assert!(s.due(&monday().at(9, 30, 0), 0, |_| true).is_empty());
        let mut again = Scheduler::new(entries.clone(), Some(state.clone()));
        assert!(again.due(&monday().at(9, 30, 0), 0, |_| true).is_empty());
        // Too late on Tuesday; Saturday is not a weekday.
        let tuesday_late = Civil::date(2026, 11, 24).at(10, 0, 0);
        assert!(again.due(&tuesday_late, 0, |_| true).is_empty());
        let saturday = Civil::date(2026, 11, 28).at(9, 0, 0);
        assert!(again.due(&saturday, 0, |_| true).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_runs_when_ready_then_each_period() {
        let mut s = Scheduler::new(
            vec![Entry {
                key: "p/w".into(),
                plugin: "p".into(),
                when: When::Every(Duration::from_secs(900)),
            }],
            None,
        );
        assert_eq!(s.due(&monday(), 1000, |_| true), ["p/w"]);
        assert!(s.due(&monday(), 1899, |_| true).is_empty());
        assert_eq!(s.due(&monday(), 1900, |_| true), ["p/w"]);
    }
}
