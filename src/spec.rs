use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const PACK_MANIFEST_FORMAT: u32 = 1;
const LOCKFILE_VERSION: u32 = 1;

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
    pub loader: Loader,
    pub loader_version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Loader {
    Fabric,
    Forge,
    NeoForge,
}

impl Loader {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fabric => "fabric",
            Self::Forge => "forge",
            Self::NeoForge => "neoforge",
        }
    }
}

impl std::fmt::Display for Loader {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
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
        check_digest(&self.sha1, 40, "sha1", &self.path)?;
        check_digest(&self.sha512, 128, "sha512", &self.path)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentPlacement {
    SharedMod,
    ClientMod,
    ServerMod,
    Shader,
    ResourcePack,
    DataPack,
}

impl ContentPlacement {
    pub fn manifest_table(self) -> &'static str {
        match self {
            Self::SharedMod => "mods",
            Self::ClientMod => "client_mods",
            Self::ServerMod => "server_mods",
            Self::Shader => "shaders",
            Self::ResourcePack => "resource_packs",
            Self::DataPack => "datapacks",
        }
    }

    pub fn folder(self) -> &'static str {
        match self {
            Self::SharedMod | Self::ClientMod | Self::ServerMod => "mods",
            Self::Shader => "shaderpacks",
            Self::ResourcePack => "resourcepacks",
            Self::DataPack => "datapacks",
        }
    }

    pub fn modrinth_kind(self) -> &'static str {
        match self {
            Self::SharedMod | Self::ClientMod | Self::ServerMod => "mod",
            Self::Shader => "shader",
            Self::ResourcePack => "resourcepack",
            Self::DataPack => "datapack",
        }
    }

    pub fn env(self) -> EnvSpec {
        match self {
            Self::SharedMod => EnvSpec {
                client: SideRequirement::Required,
                server: SideRequirement::Required,
            },
            Self::ClientMod | Self::Shader | Self::ResourcePack => EnvSpec {
                client: SideRequirement::Required,
                server: SideRequirement::Unsupported,
            },
            Self::ServerMod => EnvSpec {
                client: SideRequirement::Unsupported,
                server: SideRequirement::Required,
            },
            Self::DataPack => EnvSpec {
                client: SideRequirement::Required,
                server: SideRequirement::Required,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentSpec {
    pub id: String,
    pub version: String,
    pub placement: ContentPlacement,
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
    #[serde(default)]
    resource_packs: BTreeMap<String, String>,
    #[serde(default)]
    datapacks: BTreeMap<String, String>,
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
                + document.shaders.len()
                + document.resource_packs.len()
                + document.datapacks.len(),
        );
        append_content(&mut content, document.mods, ContentPlacement::SharedMod);
        append_content(
            &mut content,
            document.client_mods,
            ContentPlacement::ClientMod,
        );
        append_content(
            &mut content,
            document.server_mods,
            ContentPlacement::ServerMod,
        );
        append_content(&mut content, document.shaders, ContentPlacement::Shader);
        append_content(
            &mut content,
            document.resource_packs,
            ContentPlacement::ResourcePack,
        );
        append_content(&mut content, document.datapacks, ContentPlacement::DataPack);
        let spec = Self {
            format: document.format,
            pack: document.pack,
            content,
        };
        spec.validate()?;
        Ok(spec)
    }

    fn validate(&self) -> Result<()> {
        if self.format != PACK_MANIFEST_FORMAT {
            return Err(format!("unsupported pack.toml format {}", self.format).into());
        }
        validate_pack_meta(&self.pack)?;
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
    placement: ContentPlacement,
) {
    content.extend(entries.into_iter().map(|(id, version)| ContentSpec {
        id,
        version,
        placement,
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
    Ok(())
}

fn check_digest(value: &str, width: usize, name: &str, path: &str) -> Result<()> {
    if value.len() != width
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(format!(
            "{path} must have a {width}-character lowercase hexadecimal {name} pin"
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
    pub authored: Vec<AuthoredFile>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthoredRoot {
    Shared,
    Client,
    Server,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthoredFile {
    pub root: AuthoredRoot,
    pub path: String,
    pub file_size: u64,
    pub sha1: String,
    pub sha512: String,
}

impl AuthoredFile {
    pub fn validate(&self) -> Result<()> {
        check_pack_path(&self.path)?;
        check_digest(&self.sha1, 40, "sha1", &self.path)?;
        check_digest(&self.sha512, 128, "sha512", &self.path)?;
        Ok(())
    }
}

impl Lockfile {
    pub fn new(pack: PackMeta, file: Vec<FileSpec>) -> Self {
        Self {
            version: LOCKFILE_VERSION,
            pack,
            file,
            authored: Vec::new(),
            curseforge: Vec::new(),
        }
    }

    pub fn set_authored(&mut self, authored: Vec<AuthoredFile>) {
        self.authored = authored;
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
        lock.validate()?;
        Ok(lock)
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != LOCKFILE_VERSION {
            return Err(format!("unsupported lock version {}", self.version).into());
        }
        validate_pack_meta(&self.pack)?;
        let mut ids = BTreeSet::new();
        let mut paths = BTreeSet::new();
        let mut client_paths = BTreeMap::new();
        let mut server_paths = BTreeMap::new();
        for file in &self.file {
            file.validate()?;
            if !ids.insert(file.id.as_str()) {
                return Err(format!("duplicate locked content ID {}", file.id).into());
            }
            if !paths.insert(file.path.as_str()) {
                return Err(format!("duplicate pack path {}", file.path).into());
            }
            if client_file(file) {
                insert_output_path(&mut client_paths, &file.path, "client")?;
            }
            if server_file(file) {
                insert_output_path(&mut server_paths, &file.path, "server")?;
            }
        }
        let mut authored = BTreeSet::new();
        for file in &self.authored {
            file.validate()?;
            if !authored.insert((file.root, file.path.as_str())) {
                return Err(
                    format!("duplicate authored file {:?}/{}", file.root, file.path).into(),
                );
            }
            if matches!(file.root, AuthoredRoot::Shared | AuthoredRoot::Client) {
                insert_output_path(&mut client_paths, &file.path, "client")?;
            }
            if matches!(file.root, AuthoredRoot::Shared | AuthoredRoot::Server) {
                insert_output_path(&mut server_paths, &file.path, "server")?;
            }
        }
        let pins: BTreeSet<_> = self
            .file
            .iter()
            .map(|file| (file.path.as_str(), file.sha1.as_str()))
            .collect();
        let mut mapped = BTreeSet::new();
        for file in &self.curseforge {
            check_pack_path(&file.path)?;
            check_digest(&file.sha1, 40, "sha1", &file.path)?;
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
        Ok(())
    }

    pub fn to_toml(&self) -> Result<String> {
        self.validate()?;
        Ok(toml::to_string_pretty(self)?)
    }
}

pub fn check_pack_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == ".." || !portable_part(part))
    {
        return Err(format!("invalid relative pack path `{path}`").into());
    }
    if let Some(part) = path.split('/').find(|part| resembles_short_name(part)) {
        return Err(format!(
            "pack path `{path}` contains DOS 8.3-style component `{part}`; rename it without a `~<digits>` suffix"
        )
        .into());
    }
    Ok(())
}

fn portable_part(part: &str) -> bool {
    if part.ends_with(['.', ' '])
        || part.chars().any(|character| {
            character <= '\u{1f}' || matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*')
        })
    {
        return false;
    }
    let stem = part
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    !matches!(
        stem.as_str(),
        "con" | "prn" | "aux" | "nul" | "conin$" | "conout$"
    ) && !reserved_numbered_device(&stem, "com")
        && !reserved_numbered_device(&stem, "lpt")
}

fn reserved_numbered_device(stem: &str, prefix: &str) -> bool {
    stem.strip_prefix(prefix).is_some_and(|number| {
        matches!(
            number,
            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
        )
    })
}

fn resembles_short_name(part: &str) -> bool {
    let stem = part.rsplit_once('.').map_or(part, |(stem, _)| stem);
    stem.rsplit_once('~').is_some_and(|(_, suffix)| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn insert_output_path<'a>(
    paths: &mut BTreeMap<String, &'a str>,
    path: &'a str,
    side: &str,
) -> Result<()> {
    let folded = path.to_lowercase();
    if let Some(existing) = paths.get(&folded) {
        if *existing == path {
            return Err(format!("duplicate {side} output path {path}").into());
        }
        return Err(format!(
            "case-insensitive {side} output path collision: {existing} and {path}"
        )
        .into());
    }
    if let Some(existing) = paths.iter().find_map(|(existing_folded, existing)| {
        (output_path_is_ancestor(existing_folded, &folded)
            || output_path_is_ancestor(&folded, existing_folded))
        .then_some(existing)
    }) {
        return Err(
            format!("{side} output file/directory path collision: {existing} and {path}").into(),
        );
    }
    paths.insert(folded, path);
    Ok(())
}

fn output_path_is_ancestor(ancestor: &str, descendant: &str) -> bool {
    descendant
        .strip_prefix(ancestor)
        .is_some_and(|suffix| suffix.starts_with('/'))
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

    fn valid_file() -> FileSpec {
        FileSpec {
            id: "example".into(),
            requested_version: "1.0.0".into(),
            path: "mods/example.jar".into(),
            file_size: 0,
            sha1: "a".repeat(40),
            sha512: "b".repeat(128),
            env: ContentPlacement::SharedMod.env(),
            downloads: vec!["https://example.invalid/example.jar".into()],
        }
    }

    fn valid_lock(loader: Loader) -> Lockfile {
        Lockfile::new(
            PackMeta {
                name: "Example Pack".into(),
                slug: "example-pack".into(),
                version: "1.0.0".into(),
                group: "org.example.packs".into(),
                minecraft: "26.2".into(),
                loader,
                loader_version: "1.0.0".into(),
            },
            vec![valid_file()],
        )
    }

    #[test]
    fn rejects_unsafe_pack_paths_and_coordinates() {
        for path in [
            "../mods/x.jar",
            "/mods/x.jar",
            "mods\\x.jar",
            "mods//x.jar",
            "mods/./x.jar",
            "mods/x.jar/",
            "mods/trailing. ",
            "mods/trailing.",
            "mods/CON",
            "mods/nul.txt",
            "mods/Com1.jar",
            "mods/COM¹.jar",
            "mods/lpt9/config.txt",
            "config/a:b.txt",
            "config/question?.txt",
            "config/control\u{1f}.txt",
        ] {
            assert!(check_pack_path(path).is_err(), "accepted {path}");
        }
        assert!(check_pack_path("mods/sodium.jar").is_ok());
        assert!(check_pack_path("world/level.dat").is_ok());
        assert!(check_pack_path("config/.gitkeep").is_ok());
        assert!(check_pack_path("mods/com10.jar").is_ok());
        assert!(check_coordinate("pack.slug", "example-pack", false).is_ok());
        assert!(check_coordinate("pack.version", "1.2.0", true).is_ok());
        assert!(check_coordinate("pack.group", "org.example.packs", true).is_ok());
        assert!(check_coordinate("pack.slug", "../elsewhere", false).is_err());
        assert!(check_coordinate("pack.version", "../1.2.0", true).is_err());
        assert!(check_coordinate("pack.group", "com..modpacks", true).is_err());
    }

    #[test]
    fn rejects_dos_short_name_aliases() {
        for path in [
            "mods/LONGFI~1.JAR",
            "config/longfi~12/settings.json",
            "resourcepacks/PACK~2.zip",
        ] {
            let error = check_pack_path(path)
                .expect_err("DOS short-name alias")
                .to_string();
            assert!(error.contains("DOS 8.3-style component"));
            assert!(error.contains("rename it without a `~<digits>` suffix"));
        }
        for path in [
            "mods/long~name.jar",
            "mods/long~.jar",
            "mods/long~1beta.jar",
        ] {
            assert!(check_pack_path(path).is_ok(), "rejected {path}");
        }
    }

    #[test]
    fn rejects_console_device_paths() {
        for path in [
            "CONIN$",
            "mods/conin$",
            "config/ConOuT$.txt",
            "logs/CONIN$.txt.old",
        ] {
            assert!(check_pack_path(path).is_err(), "accepted {path}");
        }
        for path in [
            "mods/conin.txt",
            "config/conout.json",
            "logs/conin$extra.txt",
        ] {
            assert!(check_pack_path(path).is_ok(), "rejected {path}");
        }
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
            assert_eq!(content.placement, ContentPlacement::ServerMod);
            assert_eq!(content.placement.env().client, SideRequirement::Unsupported);
            assert_eq!(content.placement.env().server, SideRequirement::Required);
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
        let message = error.to_string();
        assert!(message.contains("fabric"));
        assert!(message.contains("forge"));
        assert!(message.contains("neoforge"));
    }

    #[test]
    fn placements_own_manifest_folder_and_environment() {
        let cases = [
            (ContentPlacement::SharedMod, "mods", "mods", true, true),
            (
                ContentPlacement::ClientMod,
                "client_mods",
                "mods",
                true,
                false,
            ),
            (
                ContentPlacement::ServerMod,
                "server_mods",
                "mods",
                false,
                true,
            ),
            (
                ContentPlacement::Shader,
                "shaders",
                "shaderpacks",
                true,
                false,
            ),
            (
                ContentPlacement::ResourcePack,
                "resource_packs",
                "resourcepacks",
                true,
                false,
            ),
            (
                ContentPlacement::DataPack,
                "datapacks",
                "datapacks",
                true,
                true,
            ),
        ];
        for (placement, table, folder, client, server) in cases {
            assert_eq!(placement.manifest_table(), table);
            assert_eq!(placement.folder(), folder);
            assert_eq!(placement.env().client == SideRequirement::Required, client);
            assert_eq!(placement.env().server == SideRequirement::Required, server);
        }
    }

    #[test]
    fn parses_each_manifest_table_as_one_legal_placement() {
        let spec = PackSpec::parse(
            r#"format = 1

[pack]
name = "Example Pack"
slug = "example-pack"
version = "1.0.0"
group = "org.example.packs"
minecraft = "26.2"
loader = "fabric"
loader_version = "0.19.3"

[mods]
shared = "1"

[client_mods]
client = "1"

[server_mods]
server = "1"

[shaders]
shader = "1"

[resource_packs]
resources = "1"

[datapacks]
data = "1"
"#,
        )
        .expect("all placements");
        let placements: BTreeMap<_, _> = spec
            .content()
            .map(|content| (content.id.as_str(), content.placement))
            .collect();
        assert_eq!(placements["shared"], ContentPlacement::SharedMod);
        assert_eq!(placements["client"], ContentPlacement::ClientMod);
        assert_eq!(placements["server"], ContentPlacement::ServerMod);
        assert_eq!(placements["shader"], ContentPlacement::Shader);
        assert_eq!(placements["resources"], ContentPlacement::ResourcePack);
        assert_eq!(placements["data"], ContentPlacement::DataPack);
    }

    #[test]
    fn rejects_noncanonical_digests_before_they_become_paths() {
        let mut file = valid_file();
        for digest in [
            "b".repeat(127),
            "B".repeat(128),
            format!("{}g", "b".repeat(127)),
            format!("{}/{}", "b".repeat(63), "b".repeat(64)),
            format!("{}..", "b".repeat(126)),
            format!("/{}", "b".repeat(127)),
        ] {
            file.sha512 = digest;
            assert!(file.validate().is_err(), "accepted {}", file.sha512);
        }
    }

    #[test]
    fn lock_serialization_reuses_parse_validation() {
        let mut lock = valid_lock(Loader::Fabric);
        lock.file.push(lock.file[0].clone());
        let encode_error = lock
            .to_toml()
            .expect_err("duplicate lock entry")
            .to_string();
        assert!(encode_error.contains("duplicate locked content ID"));

        lock.file.pop();
        lock.file[0].path = "../outside.jar".into();
        let encode_error = lock.to_toml().expect_err("unsafe path").to_string();
        assert!(encode_error.contains("invalid relative pack path"));
    }

    #[test]
    fn rejects_authored_output_collisions_per_built_side() {
        let authored = |root, path: &str| AuthoredFile {
            root,
            path: path.into(),
            file_size: 0,
            sha1: "c".repeat(40),
            sha512: "d".repeat(128),
        };

        let mut shared_and_client = valid_lock(Loader::Fabric);
        shared_and_client.set_authored(vec![
            authored(AuthoredRoot::Shared, "config/example.json"),
            authored(AuthoredRoot::Client, "config/example.json"),
        ]);
        let error = shared_and_client
            .to_toml()
            .expect_err("shared and client collision")
            .to_string();
        assert!(error.contains("duplicate client output path config/example.json"));

        let mut shared_and_server = valid_lock(Loader::Fabric);
        shared_and_server.set_authored(vec![
            authored(AuthoredRoot::Shared, "config/example.json"),
            authored(AuthoredRoot::Server, "config/example.json"),
        ]);
        let error = shared_and_server
            .to_toml()
            .expect_err("shared and server collision")
            .to_string();
        assert!(error.contains("duplicate server output path config/example.json"));

        let mut dependency_and_authored = valid_lock(Loader::Fabric);
        dependency_and_authored
            .set_authored(vec![authored(AuthoredRoot::Client, "mods/example.jar")]);
        let error = dependency_and_authored
            .to_toml()
            .expect_err("dependency and authored collision")
            .to_string();
        assert!(error.contains("duplicate client output path mods/example.jar"));

        let mut case_alias = valid_lock(Loader::Fabric);
        case_alias.file[0].path = "mods/Example.jar".into();
        case_alias.set_authored(vec![authored(AuthoredRoot::Client, "mods/example.jar")]);
        let error = case_alias
            .to_toml()
            .expect_err("case-insensitive dependency and authored collision")
            .to_string();
        assert!(error.contains("case-insensitive client output path collision"));
        assert!(error.contains("mods/Example.jar"));
        assert!(error.contains("mods/example.jar"));
    }

    #[test]
    fn rejects_case_insensitive_dependency_collisions_per_side() {
        let mut lock = valid_lock(Loader::Fabric);
        lock.file[0].path = "mods/Example.jar".into();
        let mut alias = lock.file[0].clone();
        alias.id = "alias".into();
        alias.path = "mods/example.jar".into();
        lock.file.push(alias);

        let error = lock
            .to_toml()
            .expect_err("case-insensitive client collision")
            .to_string();
        assert!(error.contains("case-insensitive client output path collision"));

        lock.file[0].env = ContentPlacement::ClientMod.env();
        lock.file[1].env = ContentPlacement::ServerMod.env();
        lock.to_toml()
            .expect("case aliases on disjoint sides do not collide");
    }

    #[test]
    fn rejects_file_directory_output_collisions_per_built_side() {
        let authored = |path: &str| AuthoredFile {
            root: AuthoredRoot::Client,
            path: path.into(),
            file_size: 0,
            sha1: "c".repeat(40),
            sha512: "d".repeat(128),
        };

        let mut dependency_is_ancestor = valid_lock(Loader::Fabric);
        dependency_is_ancestor.file[0].path = "config/Options".into();
        dependency_is_ancestor.set_authored(vec![authored("config/options/settings.json")]);
        let error = dependency_is_ancestor
            .to_toml()
            .expect_err("dependency file blocks authored descendant")
            .to_string();
        assert!(error.contains("client output file/directory path collision"));
        assert!(error.contains("config/Options"));
        assert!(error.contains("config/options/settings.json"));

        let mut authored_is_ancestor = valid_lock(Loader::Fabric);
        authored_is_ancestor.file[0].path = "config/Options/settings.json".into();
        authored_is_ancestor.set_authored(vec![authored("config/options")]);
        let error = authored_is_ancestor
            .to_toml()
            .expect_err("authored file blocks dependency descendant")
            .to_string();
        assert!(error.contains("client output file/directory path collision"));
        assert!(error.contains("config/Options/settings.json"));
        assert!(error.contains("config/options"));
    }

    #[test]
    fn mutually_exclusive_sides_may_use_related_output_paths() {
        let mut lock = valid_lock(Loader::Fabric);
        lock.file[0].env = ContentPlacement::ClientMod.env();
        lock.file[0].path = "config/Options".into();
        lock.set_authored(vec![AuthoredFile {
            root: AuthoredRoot::Server,
            path: "config/options/settings.json".into(),
            file_size: 0,
            sha1: "c".repeat(40),
            sha512: "d".repeat(128),
        }]);

        lock.to_toml()
            .expect("related paths on disjoint sides do not collide");
    }

    #[test]
    fn client_and_server_authored_roots_may_share_an_output_path() {
        let mut lock = valid_lock(Loader::Fabric);
        lock.set_authored(vec![
            AuthoredFile {
                root: AuthoredRoot::Client,
                path: "config/example.json".into(),
                file_size: 1,
                sha1: "c".repeat(40),
                sha512: "d".repeat(128),
            },
            AuthoredFile {
                root: AuthoredRoot::Server,
                path: "config/example.json".into(),
                file_size: 1,
                sha1: "e".repeat(40),
                sha512: "f".repeat(128),
            },
        ]);

        let encoded = lock.to_toml().expect("side-specific paths are disjoint");
        let decoded = Lockfile::parse(&encoded).expect("round-trip lock");
        assert_eq!(decoded.authored, lock.authored);
    }

    #[test]
    fn every_loader_round_trips_through_lock_toml() {
        for loader in [Loader::Fabric, Loader::Forge, Loader::NeoForge] {
            let lock = valid_lock(loader);
            let text = lock.to_toml().expect("encode lock");
            assert!(text.contains(&format!("loader = \"{}\"", loader.as_str())));
            assert_eq!(
                Lockfile::parse(&text).expect("parse lock").pack.loader,
                loader
            );
        }
    }

    #[test]
    fn lockfile_contract_is_version_one_only() {
        let mut lock = valid_lock(Loader::Fabric);
        assert!(
            lock.to_toml()
                .expect("version 1 lock")
                .starts_with("version = 1")
        );

        lock.version = 2;
        assert!(lock.to_toml().is_err());
    }
}
