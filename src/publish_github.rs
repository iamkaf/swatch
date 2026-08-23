use super::{Artifact, PreparedRelease, Result, http_client};
use crate::{PackRoot, hash};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;

const API_BASE: &str = "https://api.github.com";
const RELEASE_MANIFEST_NAME: &str = "release.json";
const SIGSTORE_BUNDLE_NAME: &str = "release.json.sigstore.json";

#[derive(Debug, Serialize)]
struct NewRelease<'a> {
    tag_name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_commitish: Option<&'a str>,
    name: &'a str,
    body: &'a str,
    draft: bool,
    prerelease: bool,
}

#[derive(Debug, Deserialize)]
struct Release {
    upload_url: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
struct Asset {
    url: String,
    name: String,
    size: u64,
    #[serde(default)]
    digest: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitObjectResponse {
    object: GitObject,
}

#[derive(Debug, Deserialize)]
struct GitObject {
    sha: String,
    #[serde(rename = "type")]
    kind: GitObjectKind,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum GitObjectKind {
    Commit,
    Tag,
    #[serde(other)]
    Unsupported,
}

#[derive(Debug)]
pub(super) struct PublishInput {
    source_revision: String,
    release_manifest: ProofAsset,
    sigstore_bundle: ProofAsset,
}

impl PublishInput {
    fn iter(&self) -> impl Iterator<Item = UploadAsset<'_>> {
        [&self.release_manifest, &self.sigstore_bundle]
            .into_iter()
            .map(UploadAsset::from)
    }
}

#[derive(Debug)]
struct ProofAsset {
    name: &'static str,
    bytes: Vec<u8>,
    sha256: String,
    sha512: String,
}

#[derive(Debug, Clone, Copy)]
struct UploadAsset<'a> {
    name: &'a str,
    bytes: &'a [u8],
    sha256: &'a str,
    sha512: &'a str,
}

impl<'a> From<&'a ProofAsset> for UploadAsset<'a> {
    fn from(asset: &'a ProofAsset) -> Self {
        Self {
            name: asset.name,
            bytes: &asset.bytes,
            sha256: &asset.sha256,
            sha512: &asset.sha512,
        }
    }
}

impl<'a> From<&'a Artifact> for UploadAsset<'a> {
    fn from(artifact: &'a Artifact) -> Self {
        Self {
            name: &artifact.name,
            bytes: &artifact.bytes,
            sha256: &artifact.sha256,
            sha512: &artifact.sha512,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExistingAssetComparison {
    Identical,
    NeedsDownload,
    Different,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssetAction {
    AlreadyExists,
    Upload,
}

#[derive(Debug, Clone, Copy)]
struct PlannedAsset<'a> {
    asset: UploadAsset<'a>,
    action: AssetAction,
}

#[derive(Debug, PartialEq, Eq)]
enum RemoteTag {
    Missing,
    Commit(String),
}

pub fn dry_run(release: &PreparedRelease) -> Result<Vec<String>> {
    let config = release
        .config
        .github
        .as_ref()
        .ok_or_else(|| crate::Error::from("GitHub is not configured"))?;
    let repository = preview_repository(config)?;
    let mut output = Vec::new();
    for artifact in release.artifacts.iter().filter(|artifact| {
        matches!(
            artifact.kind,
            super::ArtifactKind::Modrinth
                | super::ArtifactKind::Server
                | super::ArtifactKind::CurseForge
        )
    }) {
        output.push(format!(
            "DRY GitHub {API_BASE}/repos/{}/releases/{}/assets <- {} ({})",
            repository, release.lock.pack.version, artifact.name, artifact.sha512
        ));
    }
    Ok(output)
}

pub(super) fn preflight(root: &PackRoot, source_revision: Option<&str>) -> Result<PublishInput> {
    let source_revision = source_revision.ok_or_else(|| {
        crate::Error::from(
            "GitHub publication requires release.json.sourceRevision; run `swatch prepare` from a Git checkout before publishing",
        )
    })?;

    Ok(PublishInput {
        source_revision: source_revision.into(),
        release_manifest: read_proof_asset(root, RELEASE_MANIFEST_NAME)?,
        sigstore_bundle: read_proof_asset(root, SIGSTORE_BUNDLE_NAME).map_err(|error| {
            crate::Error::from(format!(
                "cannot load GitHub proof asset {SIGSTORE_BUNDLE_NAME}: {error}; sign build/dist/{RELEASE_MANIFEST_NAME} before publishing"
            ))
        })?,
    })
}

pub fn publish(release: &PreparedRelease, input: &PublishInput) -> Result<Vec<String>> {
    let config = release
        .config
        .github
        .as_ref()
        .ok_or_else(|| crate::Error::from("GitHub is not configured"))?;
    let repository = repository(config)?;
    let token = github_token()?;
    let client = http_client()?;
    let github_release = find_or_create_release(
        &client,
        &token,
        &repository,
        release,
        &input.source_revision,
    )?;
    let assets = input.iter().chain(
        release
            .artifacts
            .iter()
            .filter(|artifact| {
                matches!(
                    artifact.kind,
                    super::ArtifactKind::Modrinth
                        | super::ArtifactKind::Server
                        | super::ArtifactKind::CurseForge
                )
            })
            .map(UploadAsset::from),
    );
    let upload_plan = preflight_assets(&github_release, assets, |existing| {
        download_asset(&client, &token, existing)
    })?;
    let mut output = Vec::new();
    for planned in upload_plan {
        match planned.action {
            AssetAction::AlreadyExists => {
                output.push(format!("GitHub already has {}", planned.asset.name));
            }
            AssetAction::Upload => {
                upload_asset(&client, &token, &github_release, planned.asset, &mut output)?;
            }
        }
    }
    Ok(output)
}

fn find_or_create_release(
    client: &reqwest::blocking::Client,
    token: &str,
    repository: &str,
    prepared: &PreparedRelease,
    source_revision: &str,
) -> Result<Release> {
    let tag = release_tag(&prepared.lock.pack.version);
    let url = format!("{API_BASE}/repos/{}/releases/tags/{}", repository, tag);
    let response = client.get(&url).bearer_auth(token).send()?;
    if response.status() != reqwest::StatusCode::NOT_FOUND {
        let release = response.error_for_status()?.json()?;
        require_tag_source_revision(client, token, repository, &tag, source_revision)?;
        return Ok(release);
    }
    let remote_tag = resolve_remote_tag(client, token, repository, &tag)?;
    let target_commitish = target_commitish_for_new_release(&tag, &remote_tag, source_revision)?;
    let body = prepared.changelog()?;
    let response = client
        .post(format!("{API_BASE}/repos/{repository}/releases"))
        .bearer_auth(token)
        .json(&NewRelease {
            tag_name: &tag,
            target_commitish,
            name: &format!("{} {}", prepared.lock.pack.name, prepared.lock.pack.version),
            body,
            draft: false,
            prerelease: false,
        })
        .send()?;
    let release = response.error_for_status()?.json()?;
    require_tag_source_revision(client, token, repository, &tag, source_revision)?;
    Ok(release)
}

fn require_tag_source_revision(
    client: &reqwest::blocking::Client,
    token: &str,
    repository: &str,
    tag: &str,
    source_revision: &str,
) -> Result<()> {
    match resolve_remote_tag(client, token, repository, tag)? {
        RemoteTag::Commit(commit) => require_matching_revision(tag, &commit, source_revision),
        RemoteTag::Missing => Err(format!("GitHub release tag {tag} does not exist").into()),
    }
}

fn resolve_remote_tag(
    client: &reqwest::blocking::Client,
    token: &str,
    repository: &str,
    tag: &str,
) -> Result<RemoteTag> {
    let response = client
        .get(format!(
            "{API_BASE}/repos/{repository}/git/ref/tags/{}",
            urlencoding(tag)
        ))
        .bearer_auth(token)
        .send()?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(RemoteTag::Missing);
    }
    let object = response
        .error_for_status()?
        .json::<GitObjectResponse>()?
        .object;
    let commit = peel_tag_to_commit(object, |tag_sha| {
        fetch_git_object(
            client,
            token,
            format!("{API_BASE}/repos/{repository}/git/tags/{tag_sha}"),
        )
    })
    .map_err(|error| {
        crate::Error::from(format!("cannot resolve GitHub release tag {tag}: {error}"))
    })?;
    Ok(RemoteTag::Commit(commit))
}

fn peel_tag_to_commit(
    mut object: GitObject,
    mut fetch_tag: impl FnMut(&str) -> Result<GitObject>,
) -> Result<String> {
    let mut visited_tags = HashSet::new();
    loop {
        match object.kind {
            GitObjectKind::Commit => return Ok(object.sha),
            GitObjectKind::Tag => {
                if !visited_tags.insert(object.sha.clone()) {
                    return Err("annotated tag cycle".into());
                }
                object = fetch_tag(&object.sha)?;
            }
            GitObjectKind::Unsupported => {
                return Err("tag does not resolve to a commit".into());
            }
        }
    }
}

fn fetch_git_object(
    client: &reqwest::blocking::Client,
    token: &str,
    url: String,
) -> Result<GitObject> {
    let response = client.get(url).bearer_auth(token).send()?;
    Ok(response
        .error_for_status()?
        .json::<GitObjectResponse>()?
        .object)
}

fn require_matching_revision(tag: &str, actual: &str, expected: &str) -> Result<()> {
    if actual != expected {
        return Err(format!(
            "GitHub release tag {tag} resolves to {actual}, but release.json.sourceRevision is {expected}"
        )
        .into());
    }
    Ok(())
}

fn target_commitish_for_new_release<'a>(
    tag: &str,
    remote_tag: &RemoteTag,
    source_revision: &'a str,
) -> Result<Option<&'a str>> {
    match remote_tag {
        RemoteTag::Missing => Ok(Some(source_revision)),
        RemoteTag::Commit(commit) => {
            require_matching_revision(tag, commit, source_revision)?;
            Ok(None)
        }
    }
}

fn release_tag(version: &str) -> String {
    format!("v{version}")
}

fn preflight_assets<'a>(
    release: &Release,
    assets: impl IntoIterator<Item = UploadAsset<'a>>,
    mut download: impl FnMut(&Asset) -> Result<Vec<u8>>,
) -> Result<Vec<PlannedAsset<'a>>> {
    assets
        .into_iter()
        .map(|asset| {
            let Some(existing) = release
                .assets
                .iter()
                .find(|existing| existing.name == asset.name)
            else {
                return Ok(PlannedAsset {
                    asset,
                    action: AssetAction::Upload,
                });
            };
            let comparison = match compare_existing_asset(existing, asset, None) {
                ExistingAssetComparison::NeedsDownload => {
                    let bytes = download(existing)?;
                    compare_existing_asset(existing, asset, Some(&bytes))
                }
                comparison => comparison,
            };
            match comparison {
                ExistingAssetComparison::Identical => Ok(PlannedAsset {
                    asset,
                    action: AssetAction::AlreadyExists,
                }),
                ExistingAssetComparison::NeedsDownload => unreachable!("download was supplied"),
                ExistingAssetComparison::Different => Err(format!(
                    "GitHub release already has {} with different bytes",
                    asset.name
                )
                .into()),
            }
        })
        .collect()
}

fn download_asset(
    client: &reqwest::blocking::Client,
    token: &str,
    existing: &Asset,
) -> Result<Vec<u8>> {
    let response = client
        .get(&existing.url)
        .bearer_auth(token)
        .header("Accept", "application/octet-stream")
        .send()?;
    Ok(response.error_for_status()?.bytes()?.to_vec())
}

fn upload_asset(
    client: &reqwest::blocking::Client,
    token: &str,
    release: &Release,
    artifact: UploadAsset<'_>,
    output: &mut Vec<String>,
) -> Result<()> {
    let upload_url = release
        .upload_url
        .split('{')
        .next()
        .unwrap_or(&release.upload_url);
    let url = format!("{upload_url}?name={}", urlencoding(artifact.name));
    let response = client
        .post(url)
        .bearer_auth(token)
        .header("Content-Type", "application/octet-stream")
        .body(artifact.bytes.to_vec())
        .send()?;
    if !response.status().is_success() {
        return Err(format!("GitHub asset upload failed: {}", response.status()).into());
    }
    output.push(format!("uploaded GitHub {}", artifact.name));
    Ok(())
}

fn compare_existing_asset(
    existing: &Asset,
    expected: UploadAsset<'_>,
    downloaded: Option<&[u8]>,
) -> ExistingAssetComparison {
    if existing.size != expected.bytes.len() as u64 {
        return ExistingAssetComparison::Different;
    }
    match existing.digest.as_deref() {
        Some(digest) if digest == format!("sha256:{}", expected.sha256) => {
            ExistingAssetComparison::Identical
        }
        Some(_) => ExistingAssetComparison::Different,
        None => match downloaded {
            Some(bytes) if hash::sha512_hex(bytes) == expected.sha512 => {
                ExistingAssetComparison::Identical
            }
            Some(_) => ExistingAssetComparison::Different,
            None => ExistingAssetComparison::NeedsDownload,
        },
    }
}

fn read_proof_asset(root: &PackRoot, name: &'static str) -> Result<ProofAsset> {
    let path = root.dist_dir().join(name);
    let bytes = fs::read(&path)
        .map_err(|error| crate::Error::from(format!("cannot read {}: {error}", path.display())))?;
    Ok(ProofAsset {
        name,
        sha256: hash::sha256_hex(&bytes),
        sha512: hash::sha512_hex(&bytes),
        bytes,
    })
}

fn validate_repository(repository: &str) -> Result<()> {
    let mut parts = repository.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if owner.is_empty() || name.is_empty() || parts.next().is_some() {
        return Err(format!("publish.github.repository must be owner/name: {repository}").into());
    }
    Ok(())
}

fn github_token() -> Result<String> {
    ["GITHUB_TOKEN", "GH_TOKEN"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
        .ok_or_else(|| crate::Error::from("set GITHUB_TOKEN (or GH_TOKEN)"))
}

fn repository(config: &super::GitHubConfig) -> Result<String> {
    let repository = match config.repository.as_deref() {
        Some(repository) => repository.to_string(),
        None => std::env::var("GITHUB_REPOSITORY").map_err(|_| {
            crate::Error::from("publish.github.repository is required outside GitHub Actions")
        })?,
    };
    validate_repository(&repository)?;
    Ok(repository)
}

fn preview_repository(config: &super::GitHubConfig) -> Result<String> {
    match config
        .repository
        .clone()
        .or_else(|| std::env::var("GITHUB_REPOSITORY").ok())
    {
        Some(repository) => {
            validate_repository(&repository)?;
            Ok(repository)
        }
        None => Ok("<GITHUB_REPOSITORY>".into()),
    }
}

fn urlencoding(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_tags_are_version_prefixed() {
        assert_eq!(release_tag("1.2.0"), "v1.2.0");
    }

    #[test]
    fn new_releases_target_the_prepared_source_revision() {
        let revision = "a".repeat(40);
        let release = NewRelease {
            tag_name: "v1.2.0",
            target_commitish: Some(&revision),
            name: "Example Pack 1.2.0",
            body: "Notes",
            draft: false,
            prerelease: false,
        };

        let json = serde_json::to_value(release).expect("release JSON");
        assert_eq!(json["target_commitish"], revision);
    }

    #[test]
    fn existing_tags_are_verified_and_not_retargeted() {
        let revision = "a".repeat(40);
        let existing = RemoteTag::Commit(revision.clone());
        assert_eq!(
            target_commitish_for_new_release("v1.2.0", &existing, &revision)
                .expect("matching existing tag"),
            None
        );

        let release = NewRelease {
            tag_name: "v1.2.0",
            target_commitish: None,
            name: "Example Pack 1.2.0",
            body: "Notes",
            draft: false,
            prerelease: false,
        };
        let json = serde_json::to_value(release).expect("release JSON");
        assert!(json.get("target_commitish").is_none());

        let error = target_commitish_for_new_release(
            "v1.2.0",
            &RemoteTag::Commit("b".repeat(40)),
            &revision,
        )
        .expect_err("mismatched existing tag")
        .to_string();
        assert!(error.contains("release.json.sourceRevision"));
    }

    #[test]
    fn missing_tags_are_created_at_the_prepared_revision() {
        let revision = "a".repeat(40);
        assert_eq!(
            target_commitish_for_new_release("v1.2.0", &RemoteTag::Missing, &revision)
                .expect("missing tag target"),
            Some(revision.as_str())
        );
    }

    #[test]
    fn github_git_objects_peel_lightweight_and_annotated_tags() {
        let lightweight: GitObjectResponse = serde_json::from_value(serde_json::json!({
            "object": { "type": "commit", "sha": "a".repeat(40) }
        }))
        .expect("lightweight tag response");
        assert_eq!(
            peel_tag_to_commit(lightweight.object, |_| unreachable!())
                .expect("lightweight tag commit"),
            "a".repeat(40)
        );

        let annotated: GitObjectResponse = serde_json::from_value(serde_json::json!({
            "object": { "type": "tag", "sha": "b".repeat(40) }
        }))
        .expect("annotated tag response");
        let commit = peel_tag_to_commit(annotated.object, |sha| {
            assert_eq!(sha, "b".repeat(40));
            Ok(GitObject {
                sha: "c".repeat(40),
                kind: GitObjectKind::Commit,
            })
        })
        .expect("annotated tag commit");
        assert_eq!(commit, "c".repeat(40));
    }

    #[test]
    fn nested_annotated_tag_cycles_are_rejected() {
        let tag = GitObject {
            sha: "b".repeat(40),
            kind: GitObjectKind::Tag,
        };
        let error = peel_tag_to_commit(tag, |sha| {
            Ok(GitObject {
                sha: sha.into(),
                kind: GitObjectKind::Tag,
            })
        })
        .expect_err("annotated tag cycle")
        .to_string();
        assert!(error.contains("cycle"));
    }

    #[test]
    fn existing_release_requires_an_exact_source_revision_match() {
        let revision = "a".repeat(40);
        require_matching_revision("v1.2.0", &revision, &revision).expect("matching revision");

        let error = require_matching_revision("v1.2.0", &revision.to_uppercase(), &revision)
            .expect_err("case-changed revision")
            .to_string();
        assert!(error.contains("release.json.sourceRevision"));
        assert!(error.contains(&revision.to_uppercase()));
    }

    #[test]
    fn proof_assets_use_the_dist_names_and_exact_bytes() {
        let directory = tempfile::tempdir().expect("temporary pack");
        let root = PackRoot {
            path: directory.path().to_path_buf(),
        };
        fs::create_dir_all(root.dist_dir()).expect("dist directory");
        fs::write(root.dist_dir().join(RELEASE_MANIFEST_NAME), b"manifest")
            .expect("release manifest");
        fs::write(root.dist_dir().join(SIGSTORE_BUNDLE_NAME), b"bundle").expect("Sigstore bundle");

        let proofs = preflight(&root, Some(&"a".repeat(40))).expect("GitHub preflight");
        let assets = proofs.iter().collect::<Vec<_>>();
        assert_eq!(
            assets.iter().map(|asset| asset.name).collect::<Vec<_>>(),
            [RELEASE_MANIFEST_NAME, SIGSTORE_BUNDLE_NAME]
        );
        assert_eq!(assets[0].bytes, b"manifest");
        assert_eq!(assets[1].bytes, b"bundle");
    }

    #[test]
    fn github_preflight_requires_a_source_revision_before_proof_files() {
        let directory = tempfile::tempdir().expect("temporary pack");
        let root = PackRoot {
            path: directory.path().to_path_buf(),
        };

        let error = preflight(&root, None)
            .expect_err("missing source revision")
            .to_string();
        assert!(error.contains("release.json.sourceRevision"));
    }

    #[test]
    fn github_preflight_requires_the_sigstore_bundle() {
        let directory = tempfile::tempdir().expect("temporary pack");
        let root = PackRoot {
            path: directory.path().to_path_buf(),
        };
        fs::create_dir_all(root.dist_dir()).expect("dist directory");
        fs::write(root.dist_dir().join(RELEASE_MANIFEST_NAME), b"manifest")
            .expect("release manifest");

        let error = preflight(&root, Some(&"a".repeat(40)))
            .expect_err("missing Sigstore bundle")
            .to_string();
        assert!(error.contains(SIGSTORE_BUNDLE_NAME));
        assert!(error.contains("sign build/dist/release.json before publishing"));
    }

    #[test]
    fn existing_assets_are_idempotent_only_when_bytes_match() {
        let bytes = b"proof";
        let sha256 = hash::sha256_hex(bytes);
        let sha512 = hash::sha512_hex(bytes);
        let expected = UploadAsset {
            name: RELEASE_MANIFEST_NAME,
            bytes,
            sha256: &sha256,
            sha512: &sha512,
        };
        let matching_digest = Asset {
            url: "https://api.github.invalid/asset/1".into(),
            name: RELEASE_MANIFEST_NAME.into(),
            size: bytes.len() as u64,
            digest: Some(format!("sha256:{sha256}")),
        };
        assert_eq!(
            compare_existing_asset(&matching_digest, expected, None),
            ExistingAssetComparison::Identical
        );

        let legacy = Asset {
            digest: None,
            ..matching_digest
        };
        assert_eq!(
            compare_existing_asset(&legacy, expected, None),
            ExistingAssetComparison::NeedsDownload
        );
        assert_eq!(
            compare_existing_asset(&legacy, expected, Some(bytes)),
            ExistingAssetComparison::Identical
        );
        assert_eq!(
            compare_existing_asset(&legacy, expected, Some(b"other")),
            ExistingAssetComparison::Different
        );

        let wrong_digest = Asset {
            digest: Some(format!("sha256:{}", "0".repeat(64))),
            ..legacy
        };
        assert_eq!(
            compare_existing_asset(&wrong_digest, expected, None),
            ExistingAssetComparison::Different
        );
    }

    #[test]
    fn asset_preflight_checks_proofs_and_packs_before_returning_an_upload_plan() {
        let manifest_bytes = b"manifest";
        let bundle_bytes = b"bundle";
        let pack_bytes = b"pack";
        let manifest_sha256 = hash::sha256_hex(manifest_bytes);
        let manifest_sha512 = hash::sha512_hex(manifest_bytes);
        let bundle_sha256 = hash::sha256_hex(bundle_bytes);
        let bundle_sha512 = hash::sha512_hex(bundle_bytes);
        let pack_sha256 = hash::sha256_hex(pack_bytes);
        let pack_sha512 = hash::sha512_hex(pack_bytes);
        let assets = [
            UploadAsset {
                name: RELEASE_MANIFEST_NAME,
                bytes: manifest_bytes,
                sha256: &manifest_sha256,
                sha512: &manifest_sha512,
            },
            UploadAsset {
                name: SIGSTORE_BUNDLE_NAME,
                bytes: bundle_bytes,
                sha256: &bundle_sha256,
                sha512: &bundle_sha512,
            },
            UploadAsset {
                name: "example.mrpack",
                bytes: pack_bytes,
                sha256: &pack_sha256,
                sha512: &pack_sha512,
            },
        ];
        let release = Release {
            upload_url: "https://uploads.github.invalid/releases/1/assets{?name,label}".into(),
            assets: vec![
                Asset {
                    url: "https://api.github.invalid/assets/1".into(),
                    name: RELEASE_MANIFEST_NAME.into(),
                    size: manifest_bytes.len() as u64,
                    digest: Some(format!("sha256:{manifest_sha256}")),
                },
                Asset {
                    url: "https://api.github.invalid/assets/2".into(),
                    name: SIGSTORE_BUNDLE_NAME.into(),
                    size: bundle_bytes.len() as u64,
                    digest: None,
                },
            ],
        };
        let mut downloaded = Vec::new();

        let plan = preflight_assets(&release, assets, |asset| {
            downloaded.push(asset.name.clone());
            Ok(bundle_bytes.to_vec())
        })
        .expect("asset preflight");

        assert_eq!(downloaded, [SIGSTORE_BUNDLE_NAME]);
        assert_eq!(
            plan.iter()
                .map(|planned| (planned.asset.name, planned.action))
                .collect::<Vec<_>>(),
            [
                (RELEASE_MANIFEST_NAME, AssetAction::AlreadyExists),
                (SIGSTORE_BUNDLE_NAME, AssetAction::AlreadyExists),
                ("example.mrpack", AssetAction::Upload),
            ]
        );
    }

    #[test]
    fn a_late_asset_conflict_rejects_the_whole_upload_plan() {
        let proof_bytes = b"proof";
        let pack_bytes = b"pack";
        let proof_sha256 = hash::sha256_hex(proof_bytes);
        let proof_sha512 = hash::sha512_hex(proof_bytes);
        let pack_sha256 = hash::sha256_hex(pack_bytes);
        let pack_sha512 = hash::sha512_hex(pack_bytes);
        let assets = [
            UploadAsset {
                name: RELEASE_MANIFEST_NAME,
                bytes: proof_bytes,
                sha256: &proof_sha256,
                sha512: &proof_sha512,
            },
            UploadAsset {
                name: "example.mrpack",
                bytes: pack_bytes,
                sha256: &pack_sha256,
                sha512: &pack_sha512,
            },
        ];
        let release = Release {
            upload_url: "https://uploads.github.invalid/releases/1/assets{?name,label}".into(),
            assets: vec![Asset {
                url: "https://api.github.invalid/assets/1".into(),
                name: "example.mrpack".into(),
                size: pack_bytes.len() as u64,
                digest: Some(format!("sha256:{}", "0".repeat(64))),
            }],
        };

        let error = preflight_assets(&release, assets, |_| {
            panic!("digest-bearing assets do not need a download")
        })
        .expect_err("pack conflict")
        .to_string();
        assert!(error.contains("example.mrpack"));
    }
}
