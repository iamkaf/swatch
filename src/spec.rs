use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SideRequirement {
    Required,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvSpec {
    pub client: SideRequirement,
    pub server: SideRequirement,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackMeta {
    pub name: String,
    pub slug: String,
    pub version: String,
    pub group: String,
    pub minecraft: String,
    pub loader: String,
    pub loader_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileSpec {
    pub id: String,
    pub requested_version: String,
    pub path: String,
    pub file_size: u64,
    pub sha1: String,
    pub sha512: String,
    pub env: EnvSpec,
    pub downloads: Vec<String>,
}

impl FileSpec {
    pub fn validate(&self) -> Result<()> {
        check_content_id(&self.id)?;
        if self.requested_version.trim().is_empty() {
            return Err(format!("{} has no requested_version", self.path).into());
        }
        check_pack_path(&self.path)?;
        if self.downloads.len() != 1 || !self.downloads[0].starts_with("https://") {
            return Err(format!("{} must have one HTTPS download", self.path).into());
        }
        if self.sha1.len() != 40 || self.sha512.len() != 128 {
            return Err(format!("{} is missing a full sha1/sha512 pin", self.path).into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentSide {
    Both,
    Client,
    Server,
}

impl ContentSide {
    pub fn env(self) -> EnvSpec {
        match self {
            Self::Both => EnvSpec {
                client: SideRequirement::Required,
                server: SideRequirement::Required,
            },
            Self::Client => EnvSpec {
                client: SideRequirement::Required,
                server: SideRequirement::Unsupported,
            },
            Self::Server => EnvSpec {
                client: SideRequirement::Unsupported,
                server: SideRequirement::Required,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    Mod,
    Shader,
}

impl ContentKind {
    pub fn folder(self) -> &'static str {
        match self {
            Self::Mod => "mods",
            Self::Shader => "shaderpacks",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentSpec {
    pub id: String,
    pub version: String,
    pub kind: ContentKind,
    pub side: ContentSide,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackSpec {
    pub format: u32,
    pub pack: PackMeta,
    content: Vec<ContentSpec>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PackDocument {
    format: u32,
    pack: PackMeta,
    #[serde(default)]
    mods: BTreeMap<String, String>,
    #[serde(default)]
    client_mods: BTreeMap<String, String>,
    #[serde(default)]
    server_mods: BTreeMap<String, String>,
    #[serde(default)]
    shaders: BTreeMap<String, String>,
    #[serde(default, rename = "publish")]
    _publish: Option<toml::Value>,
}

impl PackSpec {
    pub fn parse(text: &str) -> Result<Self> {
        let document: PackDocument =
            toml::from_str(text).map_err(|error| Error::from(format!("pack.toml: {error}")))?;
        let mut content = Vec::with_capacity(
            document.mods.len()
                + document.client_mods.len()
                + document.server_mods.len()
                + document.shaders.len(),
        );
        append_content(
            &mut content,
            document.mods,
            ContentKind::Mod,
            ContentSide::Both,
        );
        append_content(
            &mut content,
            document.client_mods,
            ContentKind::Mod,
            ContentSide::Client,
        );
        append_content(
            &mut content,
            document.server_mods,
            ContentKind::Mod,
            ContentSide::Server,
        );
        append_content(
            &mut content,
            document.shaders,
            ContentKind::Shader,
            ContentSide::Client,
        );
        let spec = Self {
            format: document.format,
            pack: document.pack,
            content,
        };
        spec.validate()?;
        Ok(spec)
    }

    fn validate(&self) -> Result<()> {
        if self.format != 1 {
            return Err(format!("unsupported pack.toml format {}", self.format).into());
        }
        validate_pack_meta(&self.pack)?;
        if self.content.is_empty() {
            return Err("pack.toml has no mods, client mods, server mods, or shaders".into());
        }
        let mut ids = BTreeSet::new();
        for content in &self.content {
            check_content_id(&content.id)?;
            if content.version.trim().is_empty() {
                return Err(format!("{} version is required", content.id).into());
            }
            if !ids.insert(content.id.as_str()) {
                return Err(format!("duplicate content ID {}", content.id).into());
            }
        }
        Ok(())
    }

    pub fn content(&self) -> impl Iterator<Item = &ContentSpec> {
        self.content.iter()
    }

    pub fn content_count(&self) -> usize {
        self.content.len()
    }
}

fn append_content(
    content: &mut Vec<ContentSpec>,
    entries: BTreeMap<String, String>,
    kind: ContentKind,
    side: ContentSide,
) {
    content.extend(entries.into_iter().map(|(id, version)| ContentSpec {
        id,
        version,
        kind,
        side,
    }));
}

fn validate_pack_meta(pack: &PackMeta) -> Result<()> {
    if pack.name.trim().is_empty() {
        return Err("pack.name is required".into());
    }
    check_coordinate("pack.slug", &pack.slug, false)?;
    check_coordinate("pack.version", &pack.version, true)?;
    check_coordinate("pack.group", &pack.group, true)?;
    if pack.minecraft.trim().is_empty() {
        return Err("pack.minecraft is required".into());
    }
    if pack.loader_version.trim().is_empty() {
        return Err("pack.loader_version is required".into());
    }
    if !matches!(pack.loader.as_str(), "fabric" | "forge" | "neoforge") {
        return Err(format!(
            "pack.loader `{}` is not supported; use fabric, forge, or neoforge",
            pack.loader
        )
        .into());
    }
    Ok(())
}

fn check_coordinate(name: &str, value: &str, allow_dots: bool) -> Result<()> {
    let valid = !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'-' | b'_')
                || (allow_dots && byte == b'.')
        })
        && (!allow_dots || value.split('.').all(|part| !part.is_empty()));
    if !valid {
        return Err(format!("invalid {name} `{value}`").into());
    }
    Ok(())
}

fn check_content_id(id: &str) -> Result<()> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(format!("invalid content ID `{id}`").into());
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lockfile {
    pub version: u32,
    pub pack: PackMeta,
    pub file: Vec<FileSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub curseforge: Vec<CurseForgeFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurseForgeFile {
    pub path: String,
    pub sha1: String,
    pub project_id: u32,
    pub file_id: u32,
}

impl Lockfile {
    pub fn new(pack: PackMeta, file: Vec<FileSpec>) -> Self {
        Self {
            version: 2,
            pack,
            file,
            curseforge: Vec::new(),
        }
    }

    pub fn retain_curseforge_from(&mut self, previous: &Self) {
        let pins: BTreeSet<_> = self
            .file
            .iter()
            .map(|file| (file.path.as_str(), file.sha1.as_str()))
            .collect();
        self.curseforge = previous
            .curseforge
            .iter()
            .filter(|file| pins.contains(&(file.path.as_str(), file.sha1.as_str())))
            .cloned()
            .collect();
    }

    pub fn parse(text: &str) -> Result<Self> {
        let lock: Self = toml::from_str(text)
            .map_err(|error| Error::from(format!("pack.lock.toml: {error}")))?;
        if lock.version != 2 {
            return Err(format!("unsupported lock version {}", lock.version).into());
        }
        validate_pack_meta(&lock.pack)?;
        if lock.file.is_empty() {
            return Err("pack.lock.toml has no [[file]] entries".into());
        }
        let mut ids = BTreeSet::new();
        let mut paths = BTreeSet::new();
        for file in &lock.file {
            file.validate()?;
            if !ids.insert(file.id.as_str()) {
                return Err(format!("duplicate locked content ID {}", file.id).into());
            }
            if !paths.insert(file.path.as_str()) {
                return Err(format!("duplicate pack path {}", file.path).into());
            }
        }
        let pins: BTreeSet<_> = lock
            .file
            .iter()
            .map(|file| (file.path.as_str(), file.sha1.as_str()))
            .collect();
        let mut mapped = BTreeSet::new();
        for file in &lock.curseforge {
            check_pack_path(&file.path)?;
            if file.project_id == 0 || file.file_id == 0 {
                return Err(format!("{} has an invalid CurseForge ID", file.path).into());
            }
            if !pins.contains(&(file.path.as_str(), file.sha1.as_str())) {
                return Err(format!("{} has a stale CurseForge mapping", file.path).into());
            }
            if !mapped.insert(file.path.as_str()) {
                return Err(format!("duplicate CurseForge mapping for {}", file.path).into());
            }
        }
        Ok(lock)
    }

    pub fn to_toml(&self) -> Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }
}

pub fn check_pack_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains('\0')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(format!("invalid relative pack path `{path}`").into());
    }
    if path.split('/').next() == Some("world") {
        return Err(format!("pack path `{path}` must not replace a server world").into());
    }
    Ok(())
}

pub fn server_file(file: &FileSpec) -> bool {
    file.env.server != SideRequirement::Unsupported
}

pub fn client_file(file: &FileSpec) -> bool {
    file.env.client != SideRequirement::Unsupported
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_pack_paths_and_coordinates() {
        for path in [
            "world/level.dat",
            "../mods/x.jar",
            "/mods/x.jar",
            "mods\\x.jar",
            "mods//x.jar",
            "mods/./x.jar",
            "mods/x.jar/",
        ] {
            assert!(check_pack_path(path).is_err(), "accepted {path}");
        }
        assert!(check_pack_path("mods/sodium.jar").is_ok());
        assert!(check_coordinate("pack.slug", "example-pack", false).is_ok());
        assert!(check_coordinate("pack.version", "1.2.0", true).is_ok());
        assert!(check_coordinate("pack.group", "org.example.packs", true).is_ok());
        assert!(check_coordinate("pack.slug", "../elsewhere", false).is_err());
        assert!(check_coordinate("pack.version", "../1.2.0", true).is_err());
        assert!(check_coordinate("pack.group", "com..modpacks", true).is_err());
    }

    #[test]
    fn source_config_rejects_unknown_or_structured_content() {
        let unknown = r#"
format = 1
sdie = "client"

[pack]
name = "Example"
slug = "example"
version = "1.0.0"
group = "org.example.packs"
minecraft = "26.2"
loader = "fabric"
loader_version = "0.19.3"

[mods]
sodium = "1"
"#;
        assert!(PackSpec::parse(unknown).is_err());

        let structured = unknown.replace("sdie = \"client\"\n", "").replace(
            "sodium = \"1\"",
            "sodium = { version = \"1\", provider = \"direct\" }",
        );
        assert!(PackSpec::parse(&structured).is_err());
    }

    #[test]
    fn supports_all_loaders_and_server_only_content() {
        for loader in ["fabric", "forge", "neoforge"] {
            let text = format!(
                r#"
format = 1

[pack]
name = "Example"
slug = "example"
version = "1.0.0"
group = "org.example.packs"
minecraft = "26.2"
loader = "{loader}"
loader_version = "1.0.0"

[server_mods]
dedicated = "1.2.3"
"#
            );
            let spec = PackSpec::parse(&text).expect("supported loader");
            let content = spec.content().next().expect("server mod");
            assert_eq!(content.side, ContentSide::Server);
            assert_eq!(content.side.env().client, SideRequirement::Unsupported);
            assert_eq!(content.side.env().server, SideRequirement::Required);
        }
    }

    #[test]
    fn rejects_unknown_loaders_with_the_supported_values() {
        let text = r#"
format = 1

[pack]
name = "Example"
slug = "example"
version = "1.0.0"
group = "org.example.packs"
minecraft = "26.2"
loader = "quilt"
loader_version = "1.0.0"

[mods]
example = "1.2.3"
"#;
        let error = PackSpec::parse(text).expect_err("unknown loader");
        assert!(error.to_string().contains("fabric, forge, or neoforge"));
    }
}
