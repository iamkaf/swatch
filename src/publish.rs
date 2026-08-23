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
use serde::{Deserialize, Deserializer, Serialize, de};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const DIST_PATH: &str = "build/dist";
const DIST_PREFIX: &str = "build/dist/";

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
    curseforge: Option<crate::curseforge::Config>,
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
struct GitHubConfig {
    #[serde(default)]
    repository: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MavenConfig {
    repository: String,
}

fn deserialize_curseforge<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<crate::curseforge::Config>, D::Error>
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
    Server,
    CurseForge,
    Maven,
    MavenMetadata,
    ReleaseNotes,
}

#[derive(Debug, PartialEq, Eq)]
struct Artifact {
    name: String,
    kind: ArtifactKind,
    sha256: String,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseManifest {
    pub schema_version: u32,
    pub pack_version: String,
    pub preparation_mode: ReleasePreparation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    pub targets: ReleaseTargets,
    pub artifacts: Vec<ReleaseArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseTargets {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub github: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modrinth: Option<ReleaseModrinthTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub curseforge: Option<ReleaseCurseForgeTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maven: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseCurseForgeTarget {
    pub project: u64,
    pub author: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseModrinthTarget {
    pub project: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseArtifact {
    pub role: String,
    pub path: String,
    pub media_type: String,
    pub sha256: String,
    pub sha512: String,
    pub destinations: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReleasePreparation {
    Strict,
    Preview,
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
fn prepare(root: &PackRoot, mode: ReleasePreparation) -> Result<PreparedRelease> {
    let github_repository = std::env::var("GITHUB_REPOSITORY").ok();
    let github_revision = std::env::var("GITHUB_SHA").ok();
    prepare_with_ci_environment(
        root,
        mode,
        github_repository.as_deref(),
        github_revision.as_deref(),
    )
}

fn prepare_with_ci_environment(
    root: &PackRoot,
    mode: ReleasePreparation,
    github_repository: Option<&str>,
    github_revision: Option<&str>,
) -> Result<PreparedRelease> {
    let lock = crate::load_lock(root)?;
    let manifest = fs::read_to_string(root.pack_toml())?;
    let spec = crate::spec::PackSpec::parse(&manifest)?;
    if !crate::resolve::lock_matches_spec(&spec, &lock) {
        return Err(
            "pack.toml changed since the last install; run `swatch install` before publishing"
                .into(),
        );
    }
    crate::authored::verify(root, &lock.authored)?;
    let mut config = load_config(&manifest)?;
    resolve_publish_targets(&mut config, mode, github_repository)?;
    if mode == ReleasePreparation::Strict {
        require_clean_repository(root)?;
        require_matching_github_revision(root, github_revision)?;
    }
    let wants_changelog = config.changelog.is_some()
        || config.modrinth.is_some()
        || config.curseforge.is_some()
        || config.github.is_some();
    let changelog = if wants_changelog {
        match load_changelog(root, &config) {
            Ok(changelog) => Some(changelog),
            Err(_) if mode == ReleasePreparation::Preview => None,
            Err(error) => return Err(error),
        }
    } else {
        None
    };
    let output_dir = match mode {
        ReleasePreparation::Strict => root.dist_dir(),
        ReleasePreparation::Preview => root.dist_dir().join("preview"),
    };
    fs::create_dir_all(&output_dir)?;

    let mrpack = crate::export::export_from_lock_to(
        root,
        &lock,
        crate::export::BuildSide::Client,
        &output_dir,
    )?;
    let mut artifacts = vec![artifact(&mrpack, ArtifactKind::Modrinth)?];
    let server = crate::export::export_from_lock_to(
        root,
        &lock,
        crate::export::BuildSide::Server,
        &output_dir,
    )?;
    artifacts.push(artifact(&server, ArtifactKind::Server)?);
    if let Some(curseforge_config) = &config.curseforge {
        let curseforge =
            crate::curseforge::export_from_lock_to(root, &lock, curseforge_config, &output_dir)?;
        artifacts.push(artifact(&curseforge, ArtifactKind::CurseForge)?);
    }
    if let Some(maven) = &config.maven {
        if !maven.repository.starts_with("https://") {
            return Err("publish.maven.repository must use HTTPS".into());
        }
        let pom_name = format!("{}-{}.pom", lock.pack.slug, lock.pack.version);
        let pom = output_dir.join(&pom_name);
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

        let metadata = output_dir.join("maven-metadata.xml");
        fs::write(
            &metadata,
            prepare_maven_metadata(&lock, &maven.repository, mode)?,
        )?;
        artifacts.push(artifact(&metadata, ArtifactKind::MavenMetadata)?);
    }
    if let Some(changelog) = &changelog {
        let notes = output_dir.join("release-notes.md");
        fs::write(&notes, changelog)?;
        artifacts.push(artifact(&notes, ArtifactKind::ReleaseNotes)?);
    }
    artifacts.sort_by(|left, right| left.name.cmp(&right.name));

    Ok(PreparedRelease {
        lock,
        config,
        artifacts,
        changelog,
    })
}

pub fn prepare_release(root: &PackRoot) -> Result<PathBuf> {
    let release = prepare(root, ReleasePreparation::Strict)?;
    let manifest = manifest_from_release(root, &release, ReleasePreparation::Strict)?;
    let path = root.dist_dir().join("release.json");
    let mut bytes = serde_json::to_vec_pretty(&manifest)?;
    bytes.push(b'\n');
    fs::write(&path, bytes)?;
    Ok(path)
}

pub fn verify_release(root: &PackRoot) -> Result<ReleaseManifest> {
    let (manifest, _) = load_prepared(root)?;
    Ok(manifest)
}

/// Prepare once, then publish the same artifact bytes to every configured target.
pub fn publish(root: &PackRoot, mode: PublishMode) -> Result<Vec<String>> {
    let (manifest, release) = if mode == PublishMode::DryRun {
        let release = prepare(root, ReleasePreparation::Preview)?;
        let manifest = manifest_from_release(root, &release, ReleasePreparation::Preview)?;
        let path = root.dist_dir().join("release.preview.json");
        let mut bytes = serde_json::to_vec_pretty(&manifest)?;
        bytes.push(b'\n');
        fs::write(path, bytes)?;
        (manifest, release)
    } else {
        load_prepared(root)?
    };
    let mut output = dispatch_publish_targets(
        &release.config,
        mode,
        |name| {
            std::env::var(name)
                .ok()
                .is_some_and(|value| !value.is_empty())
        },
        |target| match (mode, target) {
            (PublishMode::DryRun, PublishTarget::GitHub) => github_adapter::dry_run(&release),
            (PublishMode::DryRun, PublishTarget::Maven) => maven_adapter::dry_run(&release),
            (PublishMode::DryRun, PublishTarget::Modrinth) => modrinth_adapter::dry_run(&release),
            (PublishMode::DryRun, PublishTarget::CurseForge) => {
                curseforge_adapter::dry_run(&release)
            }
            (PublishMode::Publish, PublishTarget::GitHub) => {
                let input = github_adapter::preflight(root, manifest.source_revision.as_deref())?;
                github_adapter::publish(&release, &input)
            }
            (PublishMode::Publish, PublishTarget::Maven) => maven_adapter::publish(&release),
            (PublishMode::Publish, PublishTarget::Modrinth) => modrinth_adapter::publish(&release),
            (PublishMode::Publish, PublishTarget::CurseForge) => {
                curseforge_adapter::publish(&release)
            }
        },
    )?;
    if output.is_empty() {
        output.push("prepared release locally; no publish targets are configured".into());
    }
    Ok(output)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublishTarget {
    GitHub,
    Maven,
    Modrinth,
    CurseForge,
}

const PUBLISH_ORDER: [PublishTarget; 4] = [
    PublishTarget::GitHub,
    PublishTarget::Maven,
    PublishTarget::Modrinth,
    PublishTarget::CurseForge,
];

fn dispatch_publish_targets(
    config: &PublishConfig,
    mode: PublishMode,
    credential_is_set: impl Fn(&str) -> bool,
    mut publish_target: impl FnMut(PublishTarget) -> Result<Vec<String>>,
) -> Result<Vec<String>> {
    if mode == PublishMode::Publish {
        validate_publish_credentials(config, credential_is_set)?;
    }

    let mut output = Vec::new();
    for target in configured_targets(config) {
        output.extend(publish_target(target)?);
    }
    Ok(output)
}

fn configured_targets(config: &PublishConfig) -> impl Iterator<Item = PublishTarget> + '_ {
    PUBLISH_ORDER.into_iter().filter(|target| match target {
        PublishTarget::GitHub => config.github.is_some(),
        PublishTarget::Maven => config.maven.is_some(),
        PublishTarget::Modrinth => config.modrinth.is_some(),
        PublishTarget::CurseForge => config.curseforge.is_some(),
    })
}

fn validate_publish_credentials(
    config: &PublishConfig,
    credential_is_set: impl Fn(&str) -> bool,
) -> Result<()> {
    let mut missing = Vec::new();
    if config.github.is_some()
        && !credential_is_set("GITHUB_TOKEN")
        && !credential_is_set("GH_TOKEN")
    {
        missing.push("GitHub: set GITHUB_TOKEN (or GH_TOKEN)");
    }
    if config.maven.is_some() {
        if !credential_is_set("MAVEN_PUBLISH_USERNAME") {
            missing.push("Maven: set MAVEN_PUBLISH_USERNAME");
        }
        if !credential_is_set("MAVEN_PUBLISH_PASSWORD") {
            missing.push("Maven: set MAVEN_PUBLISH_PASSWORD");
        }
    }
    if config.modrinth.is_some() && !credential_is_set("MODRINTH_TOKEN") {
        missing.push("Modrinth: set MODRINTH_TOKEN");
    }
    if config.curseforge.is_some() && !credential_is_set("CURSEFORGE_TOKEN") {
        missing.push("CurseForge: set CURSEFORGE_TOKEN");
    }
    if missing.is_empty() {
        return Ok(());
    }

    Err(format!(
        "cannot publish because configured target credentials are missing:\n  - {}\nno files were uploaded",
        missing.join("\n  - ")
    )
    .into())
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

fn manifest_from_release(
    root: &PackRoot,
    release: &PreparedRelease,
    preparation: ReleasePreparation,
) -> Result<ReleaseManifest> {
    let artifact_root = match preparation {
        ReleasePreparation::Strict => DIST_PATH,
        ReleasePreparation::Preview => "build/dist/preview",
    };
    let mut artifacts = Vec::with_capacity(release.artifacts.len());
    for artifact in &release.artifacts {
        artifacts.push(ReleaseArtifact {
            role: artifact_role(artifact.kind).into(),
            path: format!("{artifact_root}/{}", artifact.name),
            media_type: artifact_media_type(artifact.kind).into(),
            sha256: artifact.sha256.clone(),
            sha512: artifact.sha512.clone(),
            destinations: destinations_for(artifact.kind, &release.config),
        });
    }
    artifacts.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(ReleaseManifest {
        schema_version: 1,
        pack_version: release.lock.pack.version.clone(),
        preparation_mode: preparation,
        source_revision: source_revision(root),
        targets: release_targets(&release.config, preparation),
        artifacts,
    })
}

fn load_prepared(root: &PackRoot) -> Result<(ReleaseManifest, PreparedRelease)> {
    let github_repository = std::env::var("GITHUB_REPOSITORY").ok();
    load_prepared_with_github_repository(root, github_repository.as_deref())
}

fn load_prepared_with_github_repository(
    root: &PackRoot,
    github_repository: Option<&str>,
) -> Result<(ReleaseManifest, PreparedRelease)> {
    let lock = crate::load_lock(root)?;
    let manifest_text = fs::read_to_string(root.pack_toml())?;
    let spec = crate::spec::PackSpec::parse(&manifest_text)?;
    if !crate::resolve::lock_matches_spec(&spec, &lock) {
        return Err(
            "pack.toml changed since the last install; run `swatch install` and prepare again"
                .into(),
        );
    }
    crate::authored::verify(root, &lock.authored)?;
    let mut config = load_config(&manifest_text)?;
    resolve_publish_targets(&mut config, ReleasePreparation::Strict, github_repository)?;
    let path = root.dist_dir().join("release.json");
    let bytes = fs::read(&path).map_err(|error| {
        crate::Error::from(format!(
            "cannot read {}: {error}; run `swatch prepare` first",
            path.display()
        ))
    })?;
    let manifest: ReleaseManifest = serde_json::from_slice(&bytes)?;
    if manifest.schema_version != 1 {
        return Err(format!(
            "unsupported release.json schema version {}",
            manifest.schema_version
        )
        .into());
    }
    if manifest.preparation_mode != ReleasePreparation::Strict {
        return Err(
            "release.json is a preview and cannot be verified or published; run `swatch prepare`"
                .into(),
        );
    }
    if manifest.pack_version != lock.pack.version {
        return Err(format!(
            "release.json pack version {} does not match pack.lock.toml {}",
            manifest.pack_version, lock.pack.version
        )
        .into());
    }
    let current_targets = release_targets(&config, ReleasePreparation::Strict);
    if manifest.targets != current_targets {
        return Err(
            "release.json publication targets no longer match pack.toml or the environment; prepare again"
                .into(),
        );
    }
    if manifest
        .source_revision
        .as_deref()
        .is_some_and(|revision| !valid_revision(revision))
    {
        return Err("release.json has an invalid source revision".into());
    }
    if let (Some(prepared), Some(current)) = (&manifest.source_revision, source_revision(root))
        && prepared != &current
    {
        return Err(format!(
            "release.json was prepared from source revision {prepared}, current revision is {current}"
        )
        .into());
    }

    let mut artifacts = Vec::with_capacity(manifest.artifacts.len());
    let mut paths = BTreeSet::new();
    let mut roles = BTreeSet::new();
    let mut changelog = None;
    for record in &manifest.artifacts {
        crate::spec::check_pack_path(&record.path)?;
        if !record.path.starts_with(DIST_PREFIX) || record.path[DIST_PREFIX.len()..].contains('/') {
            return Err(format!(
                "release artifact must be directly under build/dist/: {}",
                record.path
            )
            .into());
        }
        if !paths.insert(record.path.as_str()) || !roles.insert(record.role.as_str()) {
            return Err(format!("duplicate release artifact {}", record.path).into());
        }
        let kind = artifact_kind(&record.role)?;
        let expected_name = expected_artifact_name(kind, &lock);
        if record.path[DIST_PREFIX.len()..] != expected_name {
            return Err(format!("{} role must use build/dist/{expected_name}", record.role).into());
        }
        if record.media_type != artifact_media_type(kind) {
            return Err(format!("{} has an unexpected media type", record.path).into());
        }
        if record.destinations != destinations_for(kind, &config) {
            return Err(format!("{} destinations no longer match pack.toml", record.path).into());
        }
        let artifact_path = root.path.join(&record.path);
        let artifact = artifact(&artifact_path, kind)?;
        if artifact.sha256 != record.sha256 || artifact.sha512 != record.sha512 {
            return Err(format!("{} does not match release.json", record.path).into());
        }
        if kind == ArtifactKind::ReleaseNotes {
            changelog = Some(
                String::from_utf8(artifact.bytes.clone())
                    .map_err(|_| crate::Error::from(format!("{} is not UTF-8", record.path)))?,
            );
        }
        artifacts.push(artifact);
    }
    let mut required = vec![ArtifactKind::Modrinth, ArtifactKind::Server];
    if config.curseforge.is_some() {
        required.push(ArtifactKind::CurseForge);
    }
    if config.maven.is_some() {
        required.extend([ArtifactKind::Maven, ArtifactKind::MavenMetadata]);
    }
    if config.changelog.is_some()
        || config.modrinth.is_some()
        || config.curseforge.is_some()
        || config.github.is_some()
    {
        required.push(ArtifactKind::ReleaseNotes);
    }
    for required in required {
        if !artifacts.iter().any(|artifact| artifact.kind == required) {
            return Err(format!(
                "release.json is missing the {} artifact",
                artifact_role(required)
            )
            .into());
        }
    }
    if (config.changelog.is_some()
        || config.modrinth.is_some()
        || config.curseforge.is_some()
        || config.github.is_some())
        && changelog.is_none()
    {
        return Err("release.json is missing release notes required by a publish target".into());
    }
    Ok((
        manifest,
        PreparedRelease {
            lock,
            config,
            artifacts,
            changelog,
        },
    ))
}

fn artifact_role(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Modrinth => "client",
        ArtifactKind::Server => "server",
        ArtifactKind::CurseForge => "curseforge",
        ArtifactKind::Maven => "maven-pom",
        ArtifactKind::MavenMetadata => "maven-metadata",
        ArtifactKind::ReleaseNotes => "release-notes",
    }
}

fn artifact_kind(role: &str) -> Result<ArtifactKind> {
    match role {
        "client" => Ok(ArtifactKind::Modrinth),
        "server" => Ok(ArtifactKind::Server),
        "curseforge" => Ok(ArtifactKind::CurseForge),
        "maven-pom" => Ok(ArtifactKind::Maven),
        "maven-metadata" => Ok(ArtifactKind::MavenMetadata),
        "release-notes" => Ok(ArtifactKind::ReleaseNotes),
        other => Err(format!("unknown release artifact role `{other}`").into()),
    }
}

fn artifact_media_type(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Modrinth | ArtifactKind::Server => "application/x-modrinth-modpack+zip",
        ArtifactKind::CurseForge => "application/zip",
        ArtifactKind::Maven | ArtifactKind::MavenMetadata => "application/xml",
        ArtifactKind::ReleaseNotes => "text/markdown; charset=utf-8",
    }
}

fn resolve_publish_targets(
    config: &mut PublishConfig,
    preparation: ReleasePreparation,
    github_repository: Option<&str>,
) -> Result<()> {
    if let Some(github) = &mut config.github {
        match github.repository.as_deref().or(github_repository) {
            Some(repository) => {
                validate_github_repository(repository)?;
                github.repository = Some(repository.to_string());
            }
            None if preparation == ReleasePreparation::Preview => {}
            None => {
                return Err(
                    "publish.github.repository is required outside GitHub Actions; GITHUB_REPOSITORY was not set"
                        .into(),
                );
            }
        }
    }
    if let Some(modrinth) = &config.modrinth
        && modrinth.project.trim().is_empty()
    {
        return Err("publish.modrinth.project is required".into());
    }
    if let Some(curseforge) = &config.curseforge {
        if curseforge.project == 0 {
            return Err("publish.curseforge.project must be a positive project ID".into());
        }
        if curseforge.author.trim().is_empty() {
            return Err("publish.curseforge.author is required".into());
        }
    }
    if let Some(maven) = &mut config.maven {
        if !maven.repository.starts_with("https://") {
            return Err("publish.maven.repository must use HTTPS".into());
        }
        let repository = maven.repository.trim_end_matches('/');
        if repository.len() == "https:".len() {
            return Err("publish.maven.repository must name an HTTPS repository".into());
        }
        maven.repository = repository.to_string();
    }
    Ok(())
}

fn release_targets(config: &PublishConfig, preparation: ReleasePreparation) -> ReleaseTargets {
    ReleaseTargets {
        github: config.github.as_ref().map(|github| {
            github.repository.clone().unwrap_or_else(|| {
                debug_assert_eq!(preparation, ReleasePreparation::Preview);
                "<GITHUB_REPOSITORY>".into()
            })
        }),
        modrinth: config
            .modrinth
            .as_ref()
            .map(|modrinth| ReleaseModrinthTarget {
                project: modrinth.project.clone(),
            }),
        curseforge: config
            .curseforge
            .as_ref()
            .map(|curseforge| ReleaseCurseForgeTarget {
                project: curseforge.project,
                author: curseforge.author.clone(),
            }),
        maven: config.maven.as_ref().map(|maven| maven.repository.clone()),
    }
}

fn validate_github_repository(repository: &str) -> Result<()> {
    let mut parts = repository.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if owner.is_empty() || name.is_empty() || parts.next().is_some() {
        return Err(format!("publish.github.repository must be owner/name: {repository}").into());
    }
    Ok(())
}

fn expected_artifact_name(kind: ArtifactKind, lock: &Lockfile) -> String {
    match kind {
        ArtifactKind::Modrinth => format!("{}-{}-client.mrpack", lock.pack.slug, lock.pack.version),
        ArtifactKind::Server => format!("{}-{}-server.mrpack", lock.pack.slug, lock.pack.version),
        ArtifactKind::CurseForge => {
            format!("{}-{}-curseforge.zip", lock.pack.slug, lock.pack.version)
        }
        ArtifactKind::Maven => format!("{}-{}.pom", lock.pack.slug, lock.pack.version),
        ArtifactKind::MavenMetadata => "maven-metadata.xml".into(),
        ArtifactKind::ReleaseNotes => "release-notes.md".into(),
    }
}

fn destinations_for(kind: ArtifactKind, config: &PublishConfig) -> Vec<String> {
    let mut destinations = Vec::new();
    match kind {
        ArtifactKind::Modrinth => {
            if config.github.is_some() {
                destinations.push("github".into());
            }
            if config.maven.is_some() {
                destinations.push("maven".into());
            }
            if config.modrinth.is_some() {
                destinations.push("modrinth".into());
            }
        }
        ArtifactKind::Server => {
            if config.github.is_some() {
                destinations.push("github".into());
            }
        }
        ArtifactKind::CurseForge => {
            if config.curseforge.is_some() {
                destinations.push("curseforge".into());
            }
            if config.github.is_some() {
                destinations.push("github".into());
            }
        }
        ArtifactKind::Maven | ArtifactKind::MavenMetadata => {
            if config.maven.is_some() {
                destinations.push("maven".into());
            }
        }
        ArtifactKind::ReleaseNotes => {}
    }
    destinations.sort();
    destinations
}

fn source_revision(root: &PackRoot) -> Option<String> {
    git_revision(root)
}

fn git_revision(root: &PackRoot) -> Option<String> {
    let output = Command::new("git")
        .current_dir(&root.path)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let revision = String::from_utf8(output.stdout).ok()?.trim().to_string();
    valid_revision(&revision).then_some(revision)
}

fn require_matching_github_revision(root: &PackRoot, github_revision: Option<&str>) -> Result<()> {
    let (Some(github_revision), Some(head)) = (github_revision, git_revision(root)) else {
        return Ok(());
    };
    if !valid_revision(github_revision) || !github_revision.eq_ignore_ascii_case(&head) {
        return Err(format!(
            "GITHUB_SHA {github_revision} does not match the checked-out HEAD {head}"
        )
        .into());
    }
    Ok(())
}

fn require_clean_repository(root: &PackRoot) -> Result<()> {
    let repository = Command::new("git")
        .current_dir(&root.path)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output();
    let Ok(repository) = repository else {
        return Ok(());
    };
    if !repository.status.success() || String::from_utf8_lossy(&repository.stdout).trim() != "true"
    {
        return Ok(());
    }

    let status = Command::new("git")
        .current_dir(&root.path)
        .args(["status", "--porcelain=v1", "--untracked-files=normal"])
        .output()
        .map_err(|error| {
            crate::Error::from(format!("cannot inspect repository status: {error}"))
        })?;
    if !status.status.success() {
        return Err("cannot inspect repository status before release preparation".into());
    }
    if !status.stdout.is_empty() {
        return Err(
            "strict release preparation requires a clean repository, including no untracked non-ignored files"
                .into(),
        );
    }
    Ok(())
}

fn valid_revision(revision: &str) -> bool {
    matches!(revision.len(), 40 | 64) && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
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
        sha256: hash::sha256_hex(&bytes),
        sha512: hash::sha512_hex(&bytes),
        bytes,
    };
    Ok(artifact)
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

fn prepare_maven_metadata(
    lock: &Lockfile,
    repository: &str,
    mode: ReleasePreparation,
) -> Result<String> {
    let group_path = lock.pack.group.replace('.', "/");
    let url = format!(
        "{}/{}/{}/maven-metadata.xml",
        repository.trim_end_matches('/'),
        group_path,
        lock.pack.slug
    );
    let mut versions = BTreeSet::new();
    if mode == ReleasePreparation::Strict {
        let response = http_client()?.get(&url).send()?;
        if response.status().is_success() {
            let existing: ExistingMetadata =
                quick_xml::de::from_str(&response.text()?).map_err(crate::Error::from_display)?;
            versions.extend(existing.versioning.versions.version);
        } else if response.status() != reqwest::StatusCode::NOT_FOUND {
            return Err(format!(
                "cannot prepare exact Maven metadata because {url} is not publicly readable: {}",
                response.status()
            )
            .into());
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
    fn pom_is_metadata_only() {
        let pom = minimal_pom("com.example", "pack", "1.0.0", "Pack");
        assert!(pom.contains("Minecraft modpack"));
        assert!(pom.contains("<packaging>pom</packaging>"));
        assert!(!pom.contains("<dependencies>"));
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
    fn release_targets_bind_each_configured_destination() {
        let mut config = load_config(
            r#"[publish.modrinth]
project = "modrinth-project"

[publish.curseforge]
project = 123
author = "Example Author"

[publish.github]
repository = "example/example-pack"

[publish.maven]
repository = "https://maven.example.invalid/releases///"
"#,
        )
        .expect("publish config");
        resolve_publish_targets(&mut config, ReleasePreparation::Strict, None)
            .expect("resolved targets");

        assert_eq!(
            release_targets(&config, ReleasePreparation::Strict),
            ReleaseTargets {
                github: Some("example/example-pack".into()),
                modrinth: Some(ReleaseModrinthTarget {
                    project: "modrinth-project".into(),
                }),
                curseforge: Some(ReleaseCurseForgeTarget {
                    project: 123,
                    author: "Example Author".into(),
                }),
                maven: Some("https://maven.example.invalid/releases".into()),
            }
        );
        assert_eq!(
            artifact_media_type(ArtifactKind::CurseForge),
            "application/zip"
        );
    }

    #[test]
    fn live_publish_checks_every_credential_before_running_an_adapter() {
        let config = load_config(
            r#"[publish.github]
repository = "example/example-pack"

[publish.maven]
repository = "https://maven.example.invalid/releases"

[publish.modrinth]
project = "modrinth-project"

[publish.curseforge]
project = 123
author = "Example Author"
"#,
        )
        .expect("publish config");
        let mut adapter_calls = Vec::new();
        let error = dispatch_publish_targets(
            &config,
            PublishMode::Publish,
            |name| name == "MAVEN_PUBLISH_USERNAME",
            |target| {
                adapter_calls.push(target);
                Ok(Vec::new())
            },
        )
        .expect_err("missing credentials")
        .to_string();

        assert!(adapter_calls.is_empty());
        assert!(error.contains("GitHub: set GITHUB_TOKEN (or GH_TOKEN)"));
        assert!(error.contains("Maven: set MAVEN_PUBLISH_PASSWORD"));
        assert!(error.contains("Modrinth: set MODRINTH_TOKEN"));
        assert!(error.contains("CurseForge: set CURSEFORGE_TOKEN"));
        assert!(error.contains("no files were uploaded"));
        assert!(!error.contains("MAVEN_PUBLISH_USERNAME"));
    }

    #[test]
    fn dry_run_skips_credentials_and_uses_safe_publish_order() {
        let config = load_config(
            r#"[publish.github]
repository = "example/example-pack"

[publish.maven]
repository = "https://maven.example.invalid/releases"

[publish.modrinth]
project = "modrinth-project"

[publish.curseforge]
project = 123
author = "Example Author"
"#,
        )
        .expect("publish config");
        let mut adapter_calls = Vec::new();
        dispatch_publish_targets(
            &config,
            PublishMode::DryRun,
            |_| panic!("dry-run must not inspect credentials"),
            |target| {
                adapter_calls.push(target);
                Ok(Vec::new())
            },
        )
        .expect("dry-run dispatch");

        assert_eq!(
            adapter_calls,
            [
                PublishTarget::GitHub,
                PublishTarget::Maven,
                PublishTarget::Modrinth,
                PublishTarget::CurseForge,
            ]
        );
        assert_eq!(PUBLISH_ORDER.last(), Some(&PublishTarget::CurseForge));
    }

    #[test]
    fn empty_github_target_uses_actions_repository_or_preview_placeholder() {
        let (_directory, root, _lock) = release_root();
        let manifest = fs::read_to_string(root.pack_toml())
            .expect("read manifest")
            .replace("repository = \"example/example-pack\"\n", "");
        fs::write(root.pack_toml(), manifest).expect("write generated GitHub target");

        let error = prepare_with_ci_environment(&root, ReleasePreparation::Strict, None, None)
            .expect_err("unbound strict GitHub target")
            .to_string();
        assert!(error.contains("GITHUB_REPOSITORY was not set"));

        let strict = prepare_with_ci_environment(
            &root,
            ReleasePreparation::Strict,
            Some("example/generated-pack"),
            None,
        )
        .expect("Actions release");
        let strict_manifest = manifest_from_release(&root, &strict, ReleasePreparation::Strict)
            .expect("strict manifest");
        assert_eq!(
            strict_manifest.targets.github.as_deref(),
            Some("example/generated-pack")
        );
        assert_eq!(
            strict
                .config
                .github
                .as_ref()
                .and_then(|github| github.repository.as_deref()),
            Some("example/generated-pack")
        );

        let preview = prepare_with_ci_environment(&root, ReleasePreparation::Preview, None, None)
            .expect("local preview");
        let preview_manifest = manifest_from_release(&root, &preview, ReleasePreparation::Preview)
            .expect("preview manifest");
        assert_eq!(
            preview_manifest.targets.github.as_deref(),
            Some("<GITHUB_REPOSITORY>")
        );
    }

    #[test]
    fn preparation_retains_one_lock_and_changelog_snapshot() {
        let (_directory, root, lock) = release_root();
        let release = prepare(&root, ReleasePreparation::Strict).expect("prepared release");

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
        let release = prepare(&root, ReleasePreparation::Preview).expect("dry-run release");
        assert!(release.changelog.is_none());
    }

    #[test]
    fn strict_preparation_rejects_a_dirty_repository() {
        let (_directory, root, _lock) = release_root();
        fs::write(root.path.join(".gitignore"), "build/\n").expect("gitignore");
        for arguments in [
            &["init"][..],
            &["config", "user.name", "Test Author"],
            &["config", "user.email", "test@example.invalid"],
            &["add", "."],
            &["commit", "-m", "Create test pack"],
        ] {
            let status = Command::new("git")
                .current_dir(&root.path)
                .args(arguments)
                .status()
                .expect("run git");
            assert!(status.success(), "git {arguments:?}");
        }
        let head = git_revision(&root).expect("Git HEAD");
        let mismatch = "a".repeat(40);
        let error =
            prepare_with_ci_environment(&root, ReleasePreparation::Strict, None, Some(&mismatch))
                .expect_err("mismatched Actions revision")
                .to_string();
        assert!(error.contains("does not match the checked-out HEAD"));

        let clean =
            prepare_with_ci_environment(&root, ReleasePreparation::Strict, None, Some(&head))
                .expect("clean strict preparation");
        let clean_manifest = manifest_from_release(&root, &clean, ReleasePreparation::Strict)
            .expect("clean manifest");
        assert!(clean_manifest.source_revision.is_some());

        fs::write(root.path.join("CHANGELOG.md"), "Changed notes\n").expect("tracked change");
        prepare(&root, ReleasePreparation::Preview).expect("tracked dirty preview");
        let error = prepare(&root, ReleasePreparation::Strict)
            .expect_err("tracked dirty strict preparation")
            .to_string();
        assert!(error.contains("requires a clean repository"));

        fs::write(root.path.join("CHANGELOG.md"), "Original notes\n").expect("restore changelog");
        fs::write(root.path.join("untracked.txt"), "dirty\n").expect("untracked file");

        prepare(&root, ReleasePreparation::Preview).expect("untracked dirty preview");
        let error = prepare(&root, ReleasePreparation::Strict)
            .expect_err("untracked dirty strict preparation")
            .to_string();
        assert!(error.contains("requires a clean repository"));
    }

    #[test]
    fn non_git_preparation_does_not_claim_the_actions_revision() {
        let (_directory, root, _lock) = release_root();
        let release = prepare_with_ci_environment(
            &root,
            ReleasePreparation::Strict,
            None,
            Some(&"b".repeat(40)),
        )
        .expect("non-Git preparation");
        let manifest = manifest_from_release(&root, &release, ReleasePreparation::Strict)
            .expect("release manifest");
        assert_eq!(manifest.source_revision, None);
    }

    #[test]
    fn maven_preview_preserves_prior_strict_metadata() {
        let (_directory, root, _lock) = release_root();
        let manifest = fs::read_to_string(root.pack_toml())
            .expect("read manifest")
            .replace(
                "[publish.github]\nrepository = \"example/example-pack\"",
                "[publish.maven]\nrepository = \"https://example.invalid/maven\"",
            );
        fs::write(root.pack_toml(), manifest).expect("write Maven publish target");
        fs::create_dir_all(root.dist_dir()).expect("create dist directory");
        let metadata_path = root.dist_dir().join("maven-metadata.xml");
        fs::write(
            &metadata_path,
            metadata_xml(
                "org.example.packs",
                "example-pack",
                "0.9.0",
                &["0.9.0".into()],
            ),
        )
        .expect("write prior Maven metadata");

        publish(&root, PublishMode::DryRun).expect("Maven preview");
        let preview: ReleaseManifest = serde_json::from_slice(
            &fs::read(root.dist_dir().join("release.preview.json")).expect("preview manifest"),
        )
        .expect("parse preview manifest");
        assert_eq!(preview.preparation_mode, ReleasePreparation::Preview);
        assert!(
            fs::read_to_string(metadata_path)
                .expect("strict metadata")
                .contains("0.9.0")
        );
        assert!(
            preview
                .artifacts
                .iter()
                .all(|artifact| artifact.path.starts_with("build/dist/preview/"))
        );
        assert!(
            !fs::read_to_string(root.dist_dir().join("preview/maven-metadata.xml"))
                .expect("preview metadata")
                .contains("0.9.0")
        );

        let verify_error = verify_release(&root)
            .expect_err("preview verification")
            .to_string();
        assert!(verify_error.contains("run `swatch prepare` first"));
        let publish_error = publish(&root, PublishMode::Publish)
            .expect_err("preview publication")
            .to_string();
        assert!(publish_error.contains("run `swatch prepare` first"));
    }

    #[test]
    fn dry_run_preserves_a_strict_release_snapshot() {
        let (_directory, root, _lock) = release_root();
        let strict_path = prepare_release(&root).expect("prepare strict release");
        let strict_bytes = fs::read(&strict_path).expect("strict release JSON");

        publish(&root, PublishMode::DryRun).expect("publication preview");

        assert_eq!(
            fs::read(&strict_path).expect("strict release JSON after preview"),
            strict_bytes
        );
        let preview: ReleaseManifest = serde_json::from_slice(
            &fs::read(root.dist_dir().join("release.preview.json")).expect("preview release JSON"),
        )
        .expect("parse preview release JSON");
        assert_eq!(preview.preparation_mode, ReleasePreparation::Preview);
        assert!(
            preview
                .artifacts
                .iter()
                .all(|artifact| artifact.path.starts_with("build/dist/preview/"))
        );
        verify_release(&root).expect("strict snapshot still verifies");
    }

    #[test]
    fn release_manifest_verifies_exact_prepared_bytes() {
        let (_directory, root, _lock) = release_root();
        let path = prepare_release(&root).expect("prepare release");
        let manifest: ReleaseManifest =
            serde_json::from_slice(&fs::read(path).expect("release JSON"))
                .expect("parse release JSON");
        assert_eq!(manifest.schema_version, 1);
        assert_eq!(manifest.pack_version, "1.0.0");
        assert_eq!(manifest.preparation_mode, ReleasePreparation::Strict);
        assert!(manifest.artifacts.iter().any(|artifact| {
            artifact.role == "client"
                && artifact.destinations == ["github"]
                && artifact.media_type == "application/x-modrinth-modpack+zip"
                && artifact.sha256.len() == 64
                && artifact.sha512.len() == 128
        }));
        assert!(manifest.artifacts.iter().any(|artifact| {
            artifact.role == "server"
                && artifact.destinations == ["github"]
                && artifact.media_type == "application/x-modrinth-modpack+zip"
        }));
        assert_eq!(
            manifest.targets.github.as_deref(),
            Some("example/example-pack")
        );
        verify_release(&root).expect("verify release");

        let client = manifest
            .artifacts
            .iter()
            .find(|artifact| artifact.role == "client")
            .expect("client artifact");
        fs::write(root.path.join(&client.path), b"changed").expect("change artifact");
        let error = verify_release(&root)
            .expect_err("changed artifact")
            .to_string();
        assert!(error.contains("does not match release.json"));
    }

    #[test]
    fn verification_rejects_a_changed_same_provider_destination() {
        let (_directory, root, _lock) = release_root();
        prepare_release(&root).expect("prepare release");
        let manifest = fs::read_to_string(root.pack_toml())
            .expect("read manifest")
            .replace("example/example-pack", "example/other-pack");
        fs::write(root.pack_toml(), manifest).expect("change GitHub repository");

        let error = verify_release(&root)
            .expect_err("changed GitHub target")
            .to_string();
        assert!(error.contains("publication targets no longer match"));
    }
}
