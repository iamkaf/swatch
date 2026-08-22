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
    let text = std::fs::read_to_string(&path)
        .map_err(|_| format!("missing {}; run `swatch install` first", path.display()))?;
    spec::Lockfile::parse(&text)
}

pub fn build(root: &PackRoot, side: BuildSide) -> Result<PathBuf> {
    let spec = load_spec(root)?;
    let lock = load_lock(root)?;
    if !resolve::lock_matches_spec(&spec, &lock) {
        return Err("pack.toml changed since the last install; run `swatch install` first".into());
    }
    export::export_from_lock(root, &lock, side)
}
