//! How `wawi2` was launched, and the environment it needs either way.
//!
//! An app opened from the Finder (Wawi.app) gets no shell: no arguments, stdin
//! on /dev/null, nobody reading stdout, and launchd's short PATH
//! (`/usr/bin:/bin:/usr/sbin:/sbin`). So `claude` and plugin commands like
//! `~/.cargo/bin/wawi-instinto` would not be found. [`extend_path`] appends
//! the usual install folders, plus the ones listed in `<home>/path`, before
//! anything starts; every child (the brain, plugins) inherits it.

use std::path::{Path, PathBuf};

/// The file with extra folders for the PATH, one per line, in the Wawi home.
pub const PATH_FILE: &str = "path";

/// The folder of the running binary when it lives in `X.app/Contents/MacOS`.
pub fn bundle_contents() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    contents_of(&exe)
}

/// `X.app/Contents` for a binary at `X.app/Contents/MacOS/<bin>`.
fn contents_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    let is_bundle = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && app.extension().is_some_and(|ext| ext == "app");
    is_bundle.then(|| contents.to_path_buf())
}

/// Running from Wawi.app (opened from the Finder, the Dock or at login).
pub fn from_bundle() -> bool {
    bundle_contents().is_some()
}

/// The arguments that matter: old macOS versions add `-psn_…` when the
/// Finder opens an app.
pub fn real_args(args: impl Iterator<Item = String>) -> Vec<String> {
    args.filter(|arg| !arg.starts_with("-psn_")).collect()
}

/// Where `claude` and plugin commands usually live, for an app that did not
/// get the shell's PATH. Only folders that exist are added.
fn usual_dirs(user_home: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = [
        ".local/bin",
        ".claude/local",
        ".cargo/bin",
        ".bun/bin",
        ".volta/bin",
        ".npm-global/bin",
    ]
    .iter()
    .map(|dir| user_home.join(dir))
    .collect();
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    dirs
}

/// The folders listed in `<home>/path`: one per line, `~/` allowed, `#`
/// starts a comment.
pub fn listed_dirs(text: &str, user_home: Option<&Path>) -> Vec<PathBuf> {
    text.lines()
        .map(|line| line.split('#').next().unwrap_or("").trim())
        .filter(|line| !line.is_empty())
        .filter_map(|line| match (line.strip_prefix("~/"), user_home) {
            (Some(rest), Some(home)) => Some(home.join(rest)),
            (Some(_), None) => None,
            (None, _) => Some(PathBuf::from(line)),
        })
        .collect()
}

/// `current`, then the listed folders, then the usual ones, without
/// repeating any. The shell's own order always wins.
pub fn joined_path(current: &str, listed: &[PathBuf], usual: &[PathBuf]) -> String {
    let mut parts: Vec<String> = current
        .split(':')
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect();
    for dir in listed.iter().chain(usual) {
        let dir = dir.to_string_lossy().into_owned();
        if !parts.contains(&dir) {
            parts.push(dir);
        }
    }
    parts.join(":")
}

/// Appends the listed and usual folders that exist to `PATH`. A handful of
/// stats, once, at the very start of `main`.
pub fn extend_path(wawi_home: Option<&Path>) {
    let user_home = std::env::var_os("HOME").map(PathBuf::from);
    let listed = wawi_home
        .and_then(|dir| std::fs::read_to_string(dir.join(PATH_FILE)).ok())
        .map(|text| listed_dirs(&text, user_home.as_deref()))
        .unwrap_or_default();
    let usual: Vec<PathBuf> = user_home
        .as_deref()
        .map(usual_dirs)
        .unwrap_or_default()
        .into_iter()
        .filter(|dir| dir.is_dir())
        .collect();
    let current = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin:/usr/sbin:/sbin".into());
    let path = joined_path(&current, &listed, &usual);
    if path != current {
        // SAFETY: called first thing in `main`, before any thread exists.
        unsafe { std::env::set_var("PATH", path) };
    }
}

/// The first `name` on `PATH` that is a file.
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Sends stdout and stderr to `<logs>/wawi-app.log` (the previous run's goes
/// to `wawi-app.log.1`), for an app opened from the Finder, where nobody
/// reads them.
#[cfg(unix)]
pub fn log_output_to(logs: &Path) {
    use std::os::fd::AsRawFd;
    if std::fs::create_dir_all(logs).is_err() {
        return;
    }
    let file = logs.join("wawi-app.log");
    let _ = std::fs::rename(&file, logs.join("wawi-app.log.1"));
    let Ok(out) = std::fs::File::create(&file) else {
        return;
    };
    // SAFETY: duplicates a valid descriptor onto 1 and 2; `out` stays open
    // until both copies exist.
    unsafe {
        libc::dup2(out.as_raw_fd(), 1);
        libc::dup2(out.as_raw_fd(), 2);
    }
}

/// Shows a message box (for the app, which has no terminal). Waits until the
/// owner closes it.
pub fn alert(title: &str, message: &str) {
    let quote = |text: &str| text.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        "display alert \"{}\" message \"{}\" as warning buttons {{\"OK\"}} default button \"OK\"",
        quote(title),
        quote(message)
    );
    let _ = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// The System Settings panes for each permission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    Microphone,
    InputMonitoring,
    ScreenRecording,
}

impl Pane {
    pub fn url(self) -> &'static str {
        match self {
            Pane::Microphone => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
            }
            Pane::InputMonitoring => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent"
            }
            Pane::ScreenRecording => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
            }
        }
    }

    /// Its name as System Settings shows it in Spanish.
    pub fn name(self) -> &'static str {
        match self {
            Pane::Microphone => "Micrófono",
            Pane::InputMonitoring => "Monitoreo de entrada",
            Pane::ScreenRecording => "Grabación de pantalla",
        }
    }

    /// Opens the pane in System Settings.
    pub fn open(self) {
        let _ = std::process::Command::new("/usr/bin/open")
            .arg(self.url())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_binary_inside_an_app_is_a_bundle() {
        assert_eq!(
            contents_of(Path::new("/Applications/Wawi.app/Contents/MacOS/wawi2")),
            Some(PathBuf::from("/Applications/Wawi.app/Contents"))
        );
        assert_eq!(
            contents_of(Path::new("/x/wawi2/target/release/wawi2")),
            None
        );
        assert_eq!(contents_of(Path::new("/x/Contents/MacOS/wawi2")), None);
    }

    #[test]
    fn the_finders_process_serial_number_is_not_an_argument() {
        let args = ["-psn_0_12345".to_owned(), "--mute".to_owned()];
        assert_eq!(real_args(args.into_iter()), vec!["--mute".to_owned()]);
    }

    #[test]
    fn the_path_keeps_the_shells_order_and_adds_the_rest_once() {
        let listed = vec![PathBuf::from("/opt/nvm/bin"), PathBuf::from("/usr/bin")];
        let usual = vec![
            PathBuf::from("/h/.local/bin"),
            PathBuf::from("/opt/nvm/bin"),
        ];
        assert_eq!(
            joined_path("/usr/bin:/bin", &listed, &usual),
            "/usr/bin:/bin:/opt/nvm/bin:/h/.local/bin"
        );
        assert_eq!(joined_path("", &[], &usual[..1]), "/h/.local/bin");
    }

    #[test]
    fn the_path_file_takes_folders_comments_and_the_home() {
        let text = "# extra folders\n~/.nvm/versions/node/v22/bin\n\n/opt/tools/bin  # mine\n";
        assert_eq!(
            listed_dirs(text, Some(Path::new("/Users/x"))),
            vec![
                PathBuf::from("/Users/x/.nvm/versions/node/v22/bin"),
                PathBuf::from("/opt/tools/bin")
            ]
        );
        assert_eq!(listed_dirs("~/bin", None), Vec::<PathBuf>::new());
    }
}
