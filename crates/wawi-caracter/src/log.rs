//! `<home>/logs/caracter.log`: one line per thing that happened. Ops,
//! reasons and counts; never what the owner said (the evidence goes to
//! `<home>/caracter/cambios.jsonl`, his private file).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};

pub const FILE: &str = "logs/caracter.log";

#[derive(Clone, Default)]
pub struct Log {
    file: Arc<Mutex<Option<File>>>,
    echo: bool,
}

impl Log {
    pub fn open(home: &Path, echo: bool) -> Self {
        let path = home.join(FILE);
        let file = path
            .parent()
            .and_then(|dir| std::fs::create_dir_all(dir).ok())
            .and_then(|()| {
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .ok()
            });
        Self {
            file: Arc::new(Mutex::new(file)),
            echo,
        }
    }

    /// Logs nowhere (tests).
    pub fn none() -> Self {
        Self::default()
    }

    pub fn line(&self, line: &str) {
        let line = line.replace(['\n', '\r'], " ");
        let stamp = wawi_plugins::log::utc_stamp();
        if self.echo {
            eprintln!("{stamp} {line}");
        }
        if let Ok(mut file) = self.file.lock()
            && let Some(file) = file.as_mut()
        {
            let _ = writeln!(file, "{stamp} {line}");
        }
    }
}
