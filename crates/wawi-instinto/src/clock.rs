//! The system's wall clock: `TZ` if set, else `/etc/localtime`, as the C
//! library reads them. The same approach as the voice app's clock, kept
//! here so this crate does not depend on the app.

use std::time::{SystemTime, UNIX_EPOCH};

use wawi_tools::{Civil, WallClock};

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl SystemClock {
    pub fn new() -> Self {
        // SAFETY: `tzset` only initializes the C library's zone globals.
        unsafe { tzset() };
        Self
    }
}

unsafe extern "C" {
    fn tzset();
}

impl WallClock for SystemClock {
    fn now(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs() as i64)
    }

    fn local(&self, epoch: i64) -> Civil {
        let seconds = epoch as libc::time_t;
        // SAFETY: `localtime_r` only writes the `tm` we own; an all-zero
        // `tm` is a valid value to start from.
        let tm = unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            if libc::localtime_r(&seconds, &mut tm).is_null() {
                return utc(epoch);
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
            tm.tm_isdst = -1;
            libc::mktime(&mut tm)
        };
        (epoch != -1).then_some(epoch as i64)
    }
}

fn utc(epoch: i64) -> Civil {
    let (year, month, day) = wawi_tools::clock::civil_from_days(epoch.div_euclid(86_400));
    let rest = epoch.rem_euclid(86_400);
    Civil::date(year, month, day).at(
        (rest / 3600) as u8,
        (rest % 3600 / 60) as u8,
        (rest % 60) as u8,
    )
}

/// `2026-11-23T09:05`, the local time format of the queues.
pub fn stamp(clock: &dyn WallClock, epoch: i64) -> String {
    clock.local(epoch).iso_minute()
}

/// A local `AAAA-MM-DDTHH:MM[:SS]` as an epoch, or why not.
pub fn parse_epoch(clock: &dyn WallClock, text: &str) -> Result<i64, String> {
    let local = wawi_tools::clock::parse_local(text)
        .ok_or_else(|| format!("«{text}» no es una hora local AAAA-MM-DDTHH:MM"))?;
    clock
        .epoch_of(local)
        .ok_or_else(|| format!("«{text}» no existe en este huso"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_clock_round_trips() {
        let clock = SystemClock::new();
        let now = clock.now();
        assert_eq!(clock.epoch_of(clock.local(now)), Some(now));
    }
}
