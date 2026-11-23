//! Local wall-clock time, for the time route, the tools and the log
//! timestamps. The core only knows milliseconds since start; the hour of day
//! lives here. The time zone is the system's: `TZ` if set, else
//! `/etc/localtime`, as the C library reads them (no zone data of our own).

use std::time::{SystemTime, UNIX_EPOCH};

use wawi_tools::{Civil, WallClock};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub millis: u16,
}

impl LocalTime {
    /// `2026-11-20 16:20:01.123`.
    pub fn stamp(&self) -> String {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
            self.year, self.month, self.day, self.hour, self.minute, self.second, self.millis
        )
    }
}

#[cfg(unix)]
pub fn local_now() -> LocalTime {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = since.as_secs() as libc::time_t;
    // SAFETY: `localtime_r` only writes the `tm` we own; an all-zero `tm` is
    // a valid value to start from.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&seconds, &mut tm).is_null() {
            return utc(since.as_secs(), since.subsec_millis());
        }
        tm
    };
    LocalTime {
        year: tm.tm_year + 1900,
        month: (tm.tm_mon + 1) as u8,
        day: tm.tm_mday as u8,
        hour: tm.tm_hour as u8,
        minute: tm.tm_min as u8,
        second: tm.tm_sec as u8,
        millis: since.subsec_millis() as u16,
    }
}

#[cfg(not(unix))]
pub fn local_now() -> LocalTime {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    utc(since.as_secs(), since.subsec_millis())
}

/// UTC from Unix seconds (Howard Hinnant's civil-from-days).
fn utc(seconds: u64, millis: u32) -> LocalTime {
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
    LocalTime {
        year,
        month,
        day,
        hour: (rest / 3600) as u8,
        minute: (rest % 3600 / 60) as u8,
        second: (rest % 60) as u8,
        millis: millis as u16,
    }
}

/// The system's clock and time zone.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl SystemClock {
    /// Reads `TZ` and `/etc/localtime` once, so every later conversion uses
    /// them.
    pub fn new() -> Self {
        #[cfg(unix)]
        // SAFETY: `tzset` only initializes the C library's zone globals.
        unsafe {
            tzset();
        }
        Self
    }
}

#[cfg(unix)]
unsafe extern "C" {
    fn tzset();
}

impl WallClock for SystemClock {
    fn now(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs() as i64)
    }

    #[cfg(unix)]
    fn local(&self, epoch: i64) -> Civil {
        let seconds = epoch as libc::time_t;
        // SAFETY: `localtime_r` only writes the `tm` we own; an all-zero `tm`
        // is a valid value to start from.
        let tm = unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            if libc::localtime_r(&seconds, &mut tm).is_null() {
                return utc_civil(epoch);
            }
            tm
        };
        Civil {
            year: tm.tm_year + 1900,
            month: (tm.tm_mon + 1) as u8,
            day: tm.tm_mday as u8,
            hour: tm.tm_hour as u8,
            minute: tm.tm_min as u8,
            second: tm.tm_sec as u8,
        }
    }

    #[cfg(not(unix))]
    fn local(&self, epoch: i64) -> Civil {
        utc_civil(epoch)
    }

    #[cfg(unix)]
    fn epoch_of(&self, local: Civil) -> Option<i64> {
        // SAFETY: `mktime` reads and normalizes the `tm` we own.
        let epoch = unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            tm.tm_year = local.year - 1900;
            tm.tm_mon = i32::from(local.month) - 1;
            tm.tm_mday = i32::from(local.day);
            tm.tm_hour = i32::from(local.hour);
            tm.tm_min = i32::from(local.minute);
            tm.tm_sec = i32::from(local.second);
            // Let the zone rules decide whether summer time applies.
            tm.tm_isdst = -1;
            libc::mktime(&mut tm)
        };
        // A time the clocks skip comes back normalized to another one; the
        // caller compares it with what it asked for.
        (epoch != -1).then_some(epoch as i64)
    }

    #[cfg(not(unix))]
    fn epoch_of(&self, local: Civil) -> Option<i64> {
        Some(
            wawi_tools::clock::days_from_civil(local.year, local.month, local.day) * 86_400
                + i64::from(local.hour) * 3_600
                + i64::from(local.minute) * 60
                + i64::from(local.second),
        )
    }
}

fn utc_civil(epoch: i64) -> Civil {
    let t = utc(epoch.max(0) as u64, 0);
    Civil {
        year: t.year,
        month: t.month,
        day: t.day,
        hour: t.hour,
        minute: t.minute,
        second: t.second,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_dates_are_right() {
        let t = utc(0, 0);
        assert_eq!((t.year, t.month, t.day, t.hour), (1970, 1, 1, 0));
        // 2026-11-20 19:14:38 UTC.
        let t = utc(1_795_202_078, 5);
        assert_eq!(t.stamp(), "2026-11-20 19:14:38.005");
    }

    #[test]
    fn the_system_clock_round_trips_local_times() {
        let clock = SystemClock::new();
        let now = clock.now();
        let local = clock.local(now);
        assert_eq!(clock.epoch_of(local), Some(now));
    }

    #[test]
    fn local_time_is_plausible() {
        let now = local_now();
        assert!(now.year >= 2024 && (1..=12).contains(&now.month));
        assert!(now.hour < 24 && now.minute < 60);
    }
}
