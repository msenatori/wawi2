//! `<home>/logs/plugins.log`: one line per plugin call and per connection
//! change. Never arguments, results or secrets: who, which tool, from where,
//! how it went, how long it took and how big the answer was. Error texts
//! pass through `Secrets::scrub`.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use wawi_tools::clock::civil_from_days;

pub const FILE: &str = "plugins.log";

#[derive(Clone, Default)]
pub struct CallLog {
    file: Arc<Mutex<Option<File>>>,
}

impl CallLog {
    /// Appends to `<logs_dir>/plugins.log`. Without a folder, logs nowhere.
    pub fn open(logs_dir: Option<&Path>) -> Self {
        let file = logs_dir.and_then(|dir| {
            std::fs::create_dir_all(dir).ok()?;
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join(FILE))
                .ok()
        });
        Self {
            file: Arc::new(Mutex::new(file)),
        }
    }

    /// Writes `line` after a UTC timestamp. Newlines are flattened.
    pub fn line(&self, line: &str) {
        if let Ok(mut file) = self.file.lock()
            && let Some(file) = file.as_mut()
        {
            let line = line.replace(['\n', '\r'], " ");
            let _ = writeln!(file, "{} {line}", utc_stamp());
        }
    }
}

/// `2026-11-21T09:00:05Z`.
pub fn utc_stamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (year, month, day) = civil_from_days(seconds.div_euclid(86_400));
    let rest = seconds.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}
