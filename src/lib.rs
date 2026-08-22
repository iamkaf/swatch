use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

mod archive;
mod authored;
pub mod authoring;
mod curseforge;
mod export;
mod fetch;
mod hash;
pub mod init;
pub mod publish;
mod resolve;
pub mod spec;

pub use export::BuildSide;

/// The stable name used in user-facing messages and HTTP requests.
pub const TOOL_NAME: &str = "swatch";
pub const TOOL_DISPLAY_NAME: &str = "Swatch";
pub const USER_AGENT: &str = concat!(
    "swatch/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/iamkaf/swatch)"
);

#[derive(Debug)]
pub struct Error(String);

impl Error {
    pub fn from_display(value: impl fmt::Display) -> Self {
        Self(value.to_string())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for Error {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self(value.to_string())
    }
}

impl From<toml::de::Error> for Error {
    fn from(value: toml::de::Error) -> Self {
        Self(format!("pack.toml: {value}"))
    }
}

impl From<toml::ser::Error> for Error {
    fn from(value: toml::ser::Error) -> Self {
        Self(format!("toml: {value}"))
    }
}

impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self(format!("json: {value}"))
    }
}

impl From<zip::result::ZipError> for Error {
    fn from(value: zip::result::ZipError) -> Self {
        Self(format!("zip: {value}"))
    }
}

impl From<reqwest::Error> for Error {
    fn from(value: reqwest::Error) -> Self {
        Self(format!("http: {value}"))
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone)]
pub struct PackRoot {
    pub path: PathBuf,
}

impl PackRoot {
    pub fn discover(start: &Path) -> Result<Self> {
        let mut current = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
        loop {
            if current.join("pack.toml").is_file() {
                return Ok(Self { path: current });
            }
            if !current.pop() {
                return Err("could not find pack.toml in this directory or its parents".into());
            }
        }
    }

    pub fn pack_toml(&self) -> PathBuf {
        self.path.join("pack.toml")
    }

    pub fn lock_toml(&self) -> PathBuf {
        self.path.join("pack.lock.toml")
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.path.join(".cache")
    }

    pub fn dist_dir(&self) -> PathBuf {
        self.path.join("dist")
    }

    pub fn generated_dir(&self) -> PathBuf {
        self.path.join("generated")
    }

    pub fn overrides_dir(&self) -> PathBuf {
        self.path.join("overrides")
    }

    pub fn client_overrides_dir(&self) -> PathBuf {
        self.path.join("client-overrides")
    }

    pub fn server_overrides_dir(&self) -> PathBuf {
        self.path.join("server-overrides")
    }
}

pub fn load_spec(root: &PackRoot) -> Result<spec::PackSpec> {
    let text = std::fs::read_to_string(root.pack_toml())?;
    spec::PackSpec::parse(&text)
}

pub fn load_lock(root: &PackRoot) -> Result<spec::Lockfile> {
    let path = root.lock_toml();
    load_lock_if_present(root)?
        .ok_or_else(|| format!("missing {}; run `swatch install` first", path.display()).into())
}

pub(crate) fn load_lock_if_present(root: &PackRoot) -> Result<Option<spec::Lockfile>> {
    let path = root.lock_toml();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match std::fs::symlink_metadata(&path) {
                Ok(_) => return Err(format!("could not read {}: {error}", path.display()).into()),
                Err(metadata_error) if metadata_error.kind() == io::ErrorKind::NotFound => {
                    return Ok(None);
                }
                Err(metadata_error) => {
                    return Err(format!(
                        "could not inspect {} after a read error: {metadata_error}",
                        path.display()
                    )
                    .into());
                }
            }
        }
        Err(error) => return Err(format!("could not read {}: {error}", path.display()).into()),
    };
    spec::Lockfile::parse(&text).map(Some)
}

pub fn build(root: &PackRoot, side: BuildSide) -> Result<PathBuf> {
    let spec = load_spec(root)?;
    let lock = load_lock(root)?;
    if !resolve::lock_matches_spec(&spec, &lock) {
        return Err("pack.toml changed since the last install; run `swatch install` first".into());
    }
    export::export_from_lock(root, &lock, side)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_lock_reports_an_absent_file_as_missing() {
        let directory = tempfile::tempdir().expect("temporary pack");
        let root = PackRoot {
            path: directory.path().to_path_buf(),
        };

        let error = load_lock(&root).expect_err("missing lockfile").to_string();

        assert!(error.contains("missing"));
        assert!(error.contains("run `swatch install` first"));
    }

    #[test]
    fn load_lock_does_not_report_other_read_errors_as_missing() {
        let directory = tempfile::tempdir().expect("temporary pack");
        let root = PackRoot {
            path: directory.path().to_path_buf(),
        };
        std::fs::create_dir(root.lock_toml()).expect("lockfile directory");

        let error = load_lock(&root)
            .expect_err("unreadable lockfile")
            .to_string();

        assert!(error.contains("could not read"));
        assert!(!error.contains("missing"));
    }

    #[cfg(unix)]
    #[test]
    fn load_lock_does_not_treat_a_dangling_symlink_as_absent() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temporary pack");
        let root = PackRoot {
            path: directory.path().to_path_buf(),
        };
        symlink("missing-target", root.lock_toml()).expect("dangling lockfile symlink");

        let error = load_lock(&root)
            .expect_err("unreadable lockfile symlink")
            .to_string();

        assert!(error.contains("could not read"));
        assert!(!error.contains("missing"));
        assert!(root.lock_toml().is_symlink());
    }
}
