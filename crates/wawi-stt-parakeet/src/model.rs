//! Where the model lives on disk, and how it gets there.
//!
//! Model files are never part of the repository. They are downloaded once to
//! `$WAWI2_HOME/models/<name>/` (default `~/.wawi2/models/<name>/`) from a
//! pinned revision, and each file's size and SHA-256 are checked. A
//! `.verified` marker records a successful hash check, so later starts only
//! compare sizes instead of re-hashing 650 MB.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug)]
pub struct ModelFile {
    pub name: &'static str,
    pub size: u64,
    /// Hex SHA-256 of the whole file.
    pub sha256: &'static str,
}

#[derive(Clone, Copy, Debug)]
pub struct ModelSpec {
    /// Folder name under `models/`.
    pub dir_name: &'static str,
    /// Download base, pinned to one revision so the checksums stay valid.
    pub base_url: &'static str,
    pub license: &'static str,
    pub files: &'static [ModelFile],
}

impl ModelSpec {
    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

/// NVIDIA Parakeet TDT 0.6B v3 (25 European languages, Spanish included),
/// int8 ONNX export by istupakov for onnx-asr. The same export Handy ships.
/// Original model: <https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3>.
pub const PARAKEET_TDT_V3_INT8: ModelSpec = ModelSpec {
    dir_name: "parakeet-tdt-0.6b-v3-int8",
    base_url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce",
    license: "CC-BY-4.0",
    files: &[
        ModelFile {
            name: "vocab.txt",
            size: 93_939,
            sha256: "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d",
        },
        ModelFile {
            name: "nemo128.onnx",
            size: 139_764,
            sha256: "a9fde1486ebfcc08f328d75ad4610c67835fea58c73ba57e3209a6f6cf019e9f",
        },
        ModelFile {
            name: "decoder_joint-model.int8.onnx",
            size: 18_202_004,
            sha256: "eea7483ee3d1a30375daedc8ed83e3960c91b098812127a0d99d1c8977667a70",
        },
        ModelFile {
            name: "encoder-model.int8.onnx",
            size: 652_183_999,
            sha256: "6139d2fa7e1b086097b277c7149725edbab89cc7c7ae64b23c741be4055aff09",
        },
    ],
};

#[derive(Debug)]
pub enum ModelError {
    NoHome,
    Missing(Vec<&'static str>),
    WrongSize {
        file: &'static str,
        expected: u64,
        found: u64,
    },
    WrongChecksum {
        file: &'static str,
    },
    Download {
        file: &'static str,
        reason: String,
    },
    Io(io::Error),
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelError::NoHome => write!(f, "no home folder: set $WAWI2_HOME"),
            ModelError::Missing(files) => write!(f, "missing model files: {}", files.join(", ")),
            ModelError::WrongSize {
                file,
                expected,
                found,
            } => write!(f, "{file}: expected {expected} bytes, found {found}"),
            ModelError::WrongChecksum { file } => write!(f, "{file}: SHA-256 does not match"),
            ModelError::Download { file, reason } => write!(f, "{file}: download failed: {reason}"),
            ModelError::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for ModelError {}

impl From<io::Error> for ModelError {
    fn from(error: io::Error) -> Self {
        ModelError::Io(error)
    }
}

/// `$WAWI2_HOME`, or `~/.wawi2` (`%USERPROFILE%\.wawi2` on Windows).
pub fn wawi_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("WAWI2_HOME") {
        return Some(dir.into());
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".wawi2"))
}

/// `<home>/models/<spec.dir_name>`.
pub fn model_dir(spec: &ModelSpec) -> Result<PathBuf, ModelError> {
    wawi_home()
        .map(|home| home.join("models").join(spec.dir_name))
        .ok_or(ModelError::NoHome)
}

const VERIFIED_MARKER: &str = ".verified";

/// Checks that every file is present with the right size. Hashes too, unless
/// an earlier full check left the marker and the sizes still match.
pub fn verify(spec: &ModelSpec, dir: &Path) -> Result<(), ModelError> {
    let missing: Vec<_> = spec
        .files
        .iter()
        .filter(|f| !dir.join(f.name).is_file())
        .map(|f| f.name)
        .collect();
    if !missing.is_empty() {
        return Err(ModelError::Missing(missing));
    }
    for file in spec.files {
        check_size(file, &dir.join(file.name))?;
    }
    if dir.join(VERIFIED_MARKER).is_file() {
        return Ok(());
    }
    for file in spec.files {
        if sha256_file(&dir.join(file.name))? != file.sha256 {
            return Err(ModelError::WrongChecksum { file: file.name });
        }
    }
    fs::write(dir.join(VERIFIED_MARKER), "sha256 ok\n")?;
    Ok(())
}

fn check_size(file: &ModelFile, path: &Path) -> Result<(), ModelError> {
    let found = fs::metadata(path)?.len();
    if found != file.size {
        return Err(ModelError::WrongSize {
            file: file.name,
            expected: file.size,
            found,
        });
    }
    Ok(())
}

/// Makes sure the model is in `dir`, downloading what is missing or wrong.
/// `say` gets human-readable progress lines.
pub fn ensure(spec: &ModelSpec, dir: &Path, say: &mut dyn FnMut(&str)) -> Result<(), ModelError> {
    if verify(spec, dir).is_ok() {
        return Ok(());
    }
    fs::create_dir_all(dir)?;
    let _ = fs::remove_file(dir.join(VERIFIED_MARKER));
    let needed: Vec<&ModelFile> = spec
        .files
        .iter()
        .filter(|f| !matches_exactly(f, &dir.join(f.name)))
        .collect();
    let bytes: u64 = needed.iter().map(|f| f.size).sum();
    say(&format!(
        "Downloading the speech model {} ({} MB, license {}) to {}. Only this once.",
        spec.dir_name,
        bytes / 1_000_000,
        spec.license,
        dir.display()
    ));
    for file in needed {
        download(spec, file, dir, say)?;
    }
    verify(spec, dir)
}

fn matches_exactly(file: &ModelFile, path: &Path) -> bool {
    check_size(file, path).is_ok() && sha256_file(path).is_ok_and(|sum| sum == file.sha256)
}

fn download(
    spec: &ModelSpec,
    file: &ModelFile,
    dir: &Path,
    say: &mut dyn FnMut(&str),
) -> Result<(), ModelError> {
    let url = format!("{}/{}", spec.base_url, file.name);
    let fail = |reason: String| ModelError::Download {
        file: file.name,
        reason,
    };
    let response = ureq::get(&url).call().map_err(|e| fail(e.to_string()))?;
    let mut body = response.into_body().into_reader();
    let partial = dir.join(format!("{}.part", file.name));
    let mut out = File::create(&partial)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1 << 16];
    let mut written = 0_u64;
    let mut next_report = 0_u64;
    loop {
        let read = body.read(&mut buffer).map_err(|e| fail(e.to_string()))?;
        if read == 0 {
            break;
        }
        out.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        written += read as u64;
        if written >= next_report && file.size > 50_000_000 {
            say(&format!(
                "  {}: {}%",
                file.name,
                written * 100 / file.size.max(1)
            ));
            next_report += file.size / 10;
        }
    }
    out.sync_all()?;
    drop(out);
    if written != file.size {
        let _ = fs::remove_file(&partial);
        return Err(ModelError::WrongSize {
            file: file.name,
            expected: file.size,
            found: written,
        });
    }
    if hex(&hasher.finalize()) != file.sha256 {
        let _ = fs::remove_file(&partial);
        return Err(ModelError::WrongChecksum { file: file.name });
    }
    fs::rename(&partial, dir.join(file.name))?;
    say(&format!("  {}: ok", file.name));
    Ok(())
}

pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(hex(&hasher.finalize()));
        }
        hasher.update(&buffer[..read]);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELLO_SHA: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    const TINY: ModelSpec = ModelSpec {
        dir_name: "tiny",
        base_url: "http://127.0.0.1:9/unused",
        license: "test",
        files: &[ModelFile {
            name: "a.bin",
            size: 5,
            sha256: HELLO_SHA,
        }],
    };

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("wawi-stt-model-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn hashes_like_sha256sum() {
        let dir = scratch("hash");
        fs::write(dir.join("a.bin"), "hello").unwrap();
        assert_eq!(sha256_file(&dir.join("a.bin")).unwrap(), HELLO_SHA);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn verify_reports_missing_wrong_and_good_files() {
        let dir = scratch("verify");
        assert!(matches!(verify(&TINY, &dir), Err(ModelError::Missing(_))));
        fs::write(dir.join("a.bin"), "hullo").unwrap();
        assert!(matches!(
            verify(&TINY, &dir),
            Err(ModelError::WrongChecksum { .. })
        ));
        fs::write(dir.join("a.bin"), "hello!").unwrap();
        assert!(matches!(
            verify(&TINY, &dir),
            Err(ModelError::WrongSize { .. })
        ));
        fs::write(dir.join("a.bin"), "hello").unwrap();
        verify(&TINY, &dir).unwrap();
        assert!(dir.join(VERIFIED_MARKER).is_file());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ensure_does_not_download_a_good_model() {
        let dir = scratch("ensure");
        fs::write(dir.join("a.bin"), "hello").unwrap();
        let mut said = Vec::new();
        ensure(&TINY, &dir, &mut |line| said.push(line.to_owned())).unwrap();
        assert!(said.is_empty(), "{said:?}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_parakeet_files_are_pinned() {
        assert!(PARAKEET_TDT_V3_INT8.base_url.contains("/resolve/8f23f0c0"));
        assert!(
            PARAKEET_TDT_V3_INT8
                .files
                .iter()
                .all(|f| f.sha256.len() == 64)
        );
        assert_eq!(PARAKEET_TDT_V3_INT8.total_size(), 670_619_706);
    }
}
