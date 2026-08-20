//! Prepare one release and hand the exact prepared bytes to each publisher.
//!
//! Resolution and archive creation stay local. Publisher modules only know how to
//! send files from [`PreparedRelease`]. This keeps a dry run useful and prevents
//! a platform adapter from quietly producing a different pack.

#[path = "publish_curseforge.rs"]
mod curseforge_adapter;
#[path = "publish_github.rs"]
mod github_adapter;
#[path = "publish_maven.rs"]
mod maven_adapter;
#[path = "publish_modrinth.rs"]
mod modrinth_adapter;

use crate::hash;
use crate::spec::Lockfile;
use crate::{PackRoot, Result, USER_AGENT};
use serde::{Deserialize, Deserializer, de};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishMode {
    DryRun,
    Publish,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PublishConfig {
    #[serde(default)]
    changelog: Option<String>,
    #[serde(default)]
    modrinth: Option<ModrinthConfig>,
    #[serde(default, deserialize_with = "deserialize_curseforge")]
    curseforge: Option<CurseForgeConfig>,
    #[serde(default)]
    github: Option<GitHubConfig>,
    #[serde(default)]
    maven: Option<MavenConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModrinthConfig {
    project: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CurseForgeConfig {
    project: u64,
    author: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GitHubConfig {
    repository: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MavenConfig {
    repository: String,
}

fn deserialize_curseforge<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<CurseForgeConfig>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = toml::Value::deserialize(deserializer)?;
    match value {
        toml::Value::Boolean(false) => Ok(None),
        toml::Value::Table(_) => value.try_into().map(Some).map_err(de::Error::custom),
        toml::Value::Boolean(true) => Err(de::Error::custom(
            "publish.curseforge must be false or a table with project and author",
        )),
        _ => Err(de::Error::custom(
            "publish.curseforge must be false or a table with project and author",
        )),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArtifactKind {
    Modrinth,
    CurseForge,
    Maven,
    MavenMetadata,
}

#[derive(Debug, PartialEq, Eq)]
struct Artifact {
    name: String,
    kind: ArtifactKind,
    sha512: String,
    bytes: Vec<u8>,
}

#[derive(Debug)]
struct PreparedRelease {
    lock: Lockfile,
    config: PublishConfig,
    artifacts: Vec<Artifact>,
    changelog: Option<String>,
}

impl PreparedRelease {
    fn artifact(&self, kind: ArtifactKind) -> Result<&Artifact> {
        self.artifacts
            .iter()
            .find(|artifact| artifact.kind == kind)
            .ok_or_else(|| format!("prepared release is missing a {kind:?} artifact").into())
    }

    fn changelog(&self) -> Result<&str> {
        self.changelog
            .as_deref()
            .ok_or_else(|| "prepared release has no changelog".into())
    }
}

/// Resolve every local release artifact once.
fn prepare(root: &PackRoot, mode: PublishMode) -> Result<PreparedRelease> {
    let lock = crate::load_lock(root)?;
    let manifest = fs::read_to_string(root.pack_toml())?;
    let spec = crate::spec::PackSpec::parse(&manifest)?;
    if !crate::resolve::lock_matches_spec(&spec, &lock) {
        return Err(
            "pack.toml changed since the last install; run `swatch install` before publishing"
                .into(),
        );
    }
    let config = load_config(&manifest)?;
    let changelog = if mode == PublishMode::Publish
        && (config.modrinth.is_some() || config.curseforge.is_some() || config.github.is_some())
    {
        Some(load_changelog(root, &config)?)
    } else {
        None
    };
    fs::create_dir_all(root.dist_dir())?;

    let mrpack = crate::export::export_from_lock(root, &lock)?;
    let mut artifacts = vec![artifact(&mrpack, ArtifactKind::Modrinth)?];
    if let Some(curseforge_config) = &config.curseforge {
        let overrides = crate::curseforge::load_config(root)?;
        let curseforge = crate::curseforge::export_from_lock(
            root,
            &curseforge_config.author,
            &lock,
            &overrides,
        )?;
        artifacts.push(artifact(&curseforge, ArtifactKind::CurseForge)?);
    }
    if let Some(maven) = &config.maven {
        if !maven.repository.starts_with("https://") {
            return Err("publish.maven.repository must use HTTPS".into());
        }
        let pom_name = format!("{}-{}.pom", lock.pack.slug, lock.pack.version);
        let pom = root.dist_dir().join(&pom_name);
        fs::write(
            &pom,
            minimal_pom(
                &lock.pack.group,
                &lock.pack.slug,
                &lock.pack.version,
                &lock.pack.name,
            ),
        )?;
        artifacts.push(artifact(&pom, ArtifactKind::Maven)?);

        let metadata = root.dist_dir().join("maven-metadata.xml");
        fs::write(
            &metadata,
            prepare_maven_metadata(&lock, &maven.repository, mode)?,
        )?;
        artifacts.push(artifact(&metadata, ArtifactKind::MavenMetadata)?);
    }
    artifacts.sort_by(|left, right| left.name.cmp(&right.name));

    Ok(PreparedRelease {
        lock,
        config,
        artifacts,
        changelog,
    })
}

/// Prepare once, then publish the same artifact bytes to every configured target.
pub fn publish(root: &PackRoot, mode: PublishMode) -> Result<Vec<String>> {
    let release = prepare(root, mode)?;
    let mut output = Vec::new();
    if release.config.modrinth.is_some() {
        output.extend(if mode == PublishMode::DryRun {
            modrinth_adapter::dry_run(&release)?
        } else {
            modrinth_adapter::publish(&release)?
        });
    }
    if release.config.curseforge.is_some() {
        output.extend(if mode == PublishMode::DryRun {
            curseforge_adapter::dry_run(&release)?
        } else {
            curseforge_adapter::publish(&release)?
        });
    }
    if release.config.github.is_some() {
        output.extend(if mode == PublishMode::DryRun {
            github_adapter::dry_run(&release)?
        } else {
            github_adapter::publish(&release)?
        });
    }
    if release.config.maven.is_some() {
        output.extend(if mode == PublishMode::DryRun {
            maven_adapter::dry_run(&release)?
        } else {
            maven_adapter::publish(&release)?
        });
    }
    if output.is_empty() {
        output.push("prepared release locally; no publish targets are configured".into());
    }
    Ok(output)
}

fn load_config(text: &str) -> Result<PublishConfig> {
    let value: toml::Value =
        toml::from_str(text).map_err(|error| crate::Error::from(format!("pack.toml: {error}")))?;
    let Some(table) = value.get("publish") else {
        return Ok(PublishConfig::default());
    };
    let config: PublishConfig = table
        .clone()
        .try_into()
        .map_err(|error| crate::Error::from(format!("pack.toml [publish]: {error}")))?;
    Ok(config)
}

fn load_changelog(root: &PackRoot, config: &PublishConfig) -> Result<String> {
    let relative = config.changelog.as_deref().unwrap_or("CHANGELOG.md");
    crate::spec::check_pack_path(relative)?;
    let path = root.path.join(relative);
    fs::read_to_string(&path).map_err(|error| {
        format!("cannot read publish changelog {}: {error}", path.display()).into()
    })
}

fn artifact(path: &Path, kind: ArtifactKind) -> Result<Artifact> {
    let bytes = fs::read(path)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            crate::Error::from(format!("invalid artifact filename: {}", path.display()))
        })?;
    let artifact = Artifact {
        name: name.into(),
        kind,
        sha512: hash::sha512_hex(&bytes),
        bytes,
    };
    fs::write(
        path.with_file_name(format!("{}.sha512", artifact.name)),
        checksum_bytes(&artifact),
    )?;
    Ok(artifact)
}

fn artifact_checksum(artifact: &Artifact) -> Artifact {
    let bytes = checksum_bytes(artifact);
    Artifact {
        name: format!("{}.sha512", artifact.name),
        kind: artifact.kind,
        sha512: hash::sha512_hex(&bytes),
        bytes,
    }
}

fn checksum_bytes(artifact: &Artifact) -> Vec<u8> {
    format!("{}  {}\n", artifact.sha512, artifact.name).into_bytes()
}

fn minimal_pom(group: &str, artifact: &str, version: &str, name: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<project xmlns="http://maven.apache.org/POM/4.0.0"
         xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
         xsi:schemaLocation="http://maven.apache.org/POM/4.0.0 https://maven.apache.org/xsd/maven-4.0.0.xsd">
  <modelVersion>4.0.0</modelVersion>
  <groupId>{}</groupId>
  <artifactId>{}</artifactId>
  <version>{}</version>
  <packaging>pom</packaging>
  <name>{}</name>
  <description>Minecraft modpack (.mrpack)</description>
</project>
"#,
        xml(group),
        xml(artifact),
        xml(version),
        xml(name)
    )
}

pub(crate) fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub(crate) fn http_client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(300))
        .build()?)
}

#[derive(Debug, Default, Deserialize)]
struct ExistingMetadata {
    #[serde(default)]
    versioning: ExistingVersioning,
}

#[derive(Debug, Default, Deserialize)]
struct ExistingVersioning {
    #[serde(default)]
    versions: ExistingVersions,
}

#[derive(Debug, Default, Deserialize)]
struct ExistingVersions {
    #[serde(default)]
    version: Vec<String>,
}

fn prepare_maven_metadata(lock: &Lockfile, repository: &str, mode: PublishMode) -> Result<String> {
    let group_path = lock.pack.group.replace('.', "/");
    let url = format!(
        "{}/{}/{}/maven-metadata.xml",
        repository.trim_end_matches('/'),
        group_path,
        lock.pack.slug
    );
    let mut versions = BTreeSet::new();
    if mode == PublishMode::Publish {
        let username = std::env::var("MAVEN_PUBLISH_USERNAME")
            .map_err(|_| crate::Error::from("set MAVEN_PUBLISH_USERNAME"))?;
        let password = std::env::var("MAVEN_PUBLISH_PASSWORD")
            .map_err(|_| crate::Error::from("set MAVEN_PUBLISH_PASSWORD"))?;
        let response = http_client()?
            .get(&url)
            .basic_auth(username, Some(password))
            .send()?;
        if response.status().is_success() {
            let existing: ExistingMetadata =
                quick_xml::de::from_str(&response.text()?).map_err(crate::Error::from_display)?;
            versions.extend(existing.versioning.versions.version);
        } else if response.status() != reqwest::StatusCode::NOT_FOUND {
            return Err(
                format!("Maven metadata lookup failed: {url}: {}", response.status()).into(),
            );
        }
    }
    versions.insert(lock.pack.version.clone());
    let latest = versions
        .iter()
        .max_by(|left, right| compare_pack_versions(left, right))
        .cloned()
        .unwrap_or_else(|| lock.pack.version.clone());
    Ok(metadata_xml(
        &lock.pack.group,
        &lock.pack.slug,
        &latest,
        &versions.into_iter().collect::<Vec<_>>(),
    ))
}

fn compare_pack_versions(left: &str, right: &str) -> std::cmp::Ordering {
    let numbers = |value: &str| {
        let mut parts = value.split('.');
        let parsed = [
            parts.next().and_then(|part| part.parse::<u64>().ok()),
            parts.next().and_then(|part| part.parse::<u64>().ok()),
            parts.next().and_then(|part| part.parse::<u64>().ok()),
        ];
        (parts.next().is_none() && parsed.iter().all(Option::is_some))
            .then(|| parsed.map(Option::unwrap))
    };
    match (numbers(left), numbers(right)) {
        (Some(left), Some(right)) => left.cmp(&right),
        _ => left.cmp(right),
    }
}

fn metadata_xml(group: &str, artifact: &str, version: &str, versions: &[String]) -> String {
    let version_rows = versions
        .iter()
        .map(|value| format!("      <version>{}</version>\n", xml(value)))
        .collect::<String>();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<metadata>\n\
  <groupId>{}</groupId>\n\
  <artifactId>{}</artifactId>\n\
  <versioning>\n\
    <latest>{}</latest>\n\
    <release>{}</release>\n\
    <versions>\n\
{}\
    </versions>\n\
  </versioning>\n\
</metadata>\n",
        xml(group),
        xml(artifact),
        xml(version),
        xml(version),
        version_rows
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{ContentPlacement, FileSpec, Loader, PackMeta};
    use std::io::{Cursor, Read};

    fn release_root() -> (tempfile::TempDir, PackRoot, Lockfile) {
        let directory = tempfile::tempdir().expect("temporary pack");
        let root = PackRoot {
            path: directory.path().to_path_buf(),
        };
        fs::write(
            root.pack_toml(),
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
example = "1.0.0"

[publish.github]
repository = "example/example-pack"
"#,
        )
        .expect("manifest");
        fs::write(root.path.join("CHANGELOG.md"), "Original notes\n").expect("changelog");
        let lock = Lockfile::new(
            PackMeta {
                name: "Example Pack".into(),
                slug: "example-pack".into(),
                version: "1.0.0".into(),
                group: "org.example.packs".into(),
                minecraft: "26.2".into(),
                loader: Loader::Fabric,
                loader_version: "0.19.3".into(),
            },
            vec![FileSpec {
                id: "example".into(),
                requested_version: "1.0.0".into(),
                path: "mods/example.jar".into(),
                file_size: 0,
                sha1: "a".repeat(40),
                sha512: "b".repeat(128),
                env: ContentPlacement::SharedMod.env(),
                downloads: vec!["https://example.invalid/example.jar".into()],
            }],
        );
        fs::write(root.lock_toml(), lock.to_toml().expect("lock TOML")).expect("lockfile");
        (directory, root, lock)
    }

    #[test]
    fn pom_does_not_mention_the_server_launcher() {
        let pom = minimal_pom("com.example", "pack", "1.0.0", "Pack");
        assert!(pom.contains("Minecraft modpack"));
        assert!(!pom.contains("Pastel"));
    }

    #[test]
    fn maven_metadata_keeps_existing_versions() {
        let metadata = metadata_xml(
            "com.example",
            "pack",
            "1.2.0",
            &["1.1.1".into(), "1.2.0".into()],
        );
        assert!(metadata.contains("<version>1.1.1</version>"));
        assert!(metadata.contains("<latest>1.2.0</latest>"));
        assert_eq!(
            compare_pack_versions("1.10.0", "1.9.0"),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn curseforge_can_be_explicitly_unconfigured() {
        let disabled: PublishConfig =
            toml::from_str("curseforge = false\n").expect("disabled CurseForge target");
        assert!(disabled.curseforge.is_none());

        let enabled: PublishConfig =
            toml::from_str("[curseforge]\nproject = 123\nauthor = \"Example Author\"\n")
                .expect("configured CurseForge target");
        assert_eq!(
            enabled.curseforge.as_ref().map(|config| config.project),
            Some(123)
        );
    }

    #[test]
    fn preparation_retains_one_lock_and_changelog_snapshot() {
        let (_directory, root, lock) = release_root();
        let release = prepare(&root, PublishMode::Publish).expect("prepared release");

        let mut replacement = lock;
        replacement.pack.version = "2.0.0".into();
        fs::write(
            root.lock_toml(),
            replacement.to_toml().expect("replacement lock"),
        )
        .expect("replace lock");
        fs::remove_file(root.path.join("CHANGELOG.md")).expect("remove changelog");

        assert_eq!(release.lock.pack.version, "1.0.0");
        assert_eq!(
            release.changelog().expect("captured changelog"),
            "Original notes\n"
        );
        let artifact = release.artifact(ArtifactKind::Modrinth).expect("mrpack");
        let mut archive = zip::ZipArchive::new(Cursor::new(&artifact.bytes)).expect("mrpack zip");
        let mut index = String::new();
        archive
            .by_name("modrinth.index.json")
            .expect("index")
            .read_to_string(&mut index)
            .expect("index text");
        let index: serde_json::Value = serde_json::from_str(&index).expect("index JSON");
        assert_eq!(index["versionId"], "1.0.0");
    }

    #[test]
    fn dry_run_does_not_require_release_notes() {
        let (_directory, root, _lock) = release_root();
        fs::remove_file(root.path.join("CHANGELOG.md")).expect("remove changelog");
        let release = prepare(&root, PublishMode::DryRun).expect("dry-run release");
        assert!(release.changelog.is_none());
    }
}
