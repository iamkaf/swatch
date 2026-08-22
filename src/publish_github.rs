use super::{Artifact, PreparedRelease, Result, http_client};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};

const API_BASE: &str = "https://api.github.com";

#[derive(Debug, Serialize)]
struct NewRelease<'a> {
    tag_name: &'a str,
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

pub fn publish(release: &PreparedRelease) -> Result<Vec<String>> {
    let config = release
        .config
        .github
        .as_ref()
        .ok_or_else(|| crate::Error::from("GitHub is not configured"))?;
    let repository = repository(config)?;
    let token = std::env::var("GITHUB_TOKEN")
        .or_else(|_| std::env::var("GH_TOKEN"))
        .map_err(|_| crate::Error::from("set GITHUB_TOKEN (or GH_TOKEN)"))?;
    let client = http_client()?;
    let github_release = find_or_create_release(&client, &token, &repository, release)?;
    let mut output = Vec::new();
    for artifact in release.artifacts.iter().filter(|artifact| {
        matches!(
            artifact.kind,
            super::ArtifactKind::Modrinth
                | super::ArtifactKind::Server
                | super::ArtifactKind::CurseForge
        )
    }) {
        upload_if_needed(&client, &token, &github_release, artifact, &mut output)?;
    }
    Ok(output)
}

fn find_or_create_release(
    client: &reqwest::blocking::Client,
    token: &str,
    repository: &str,
    prepared: &PreparedRelease,
) -> Result<Release> {
    let tag = release_tag(&prepared.lock.pack.version);
    let url = format!("{API_BASE}/repos/{}/releases/tags/{}", repository, tag);
    let response = client.get(&url).bearer_auth(token).send()?;
    if response.status() != reqwest::StatusCode::NOT_FOUND {
        return Ok(response.error_for_status()?.json()?);
    }
    let body = prepared.changelog()?;
    let response = client
        .post(format!("{API_BASE}/repos/{repository}/releases"))
        .bearer_auth(token)
        .json(&NewRelease {
            tag_name: &tag,
            name: &format!("{} {}", prepared.lock.pack.name, prepared.lock.pack.version),
            body,
            draft: false,
            prerelease: false,
        })
        .send()?;
    Ok(response.error_for_status()?.json()?)
}

fn release_tag(version: &str) -> String {
    format!("v{version}")
}

fn upload_if_needed(
    client: &reqwest::blocking::Client,
    token: &str,
    release: &Release,
    artifact: &Artifact,
    output: &mut Vec<String>,
) -> Result<()> {
    if let Some(existing) = release
        .assets
        .iter()
        .find(|asset| asset.name == artifact.name)
    {
        if existing.size == artifact.bytes.len() as u64 {
            let sha256 = format!("sha256:{}", hex::encode(Sha256::digest(&artifact.bytes)));
            if existing.digest.as_deref() == Some(sha256.as_str()) {
                output.push(format!("GitHub already has {}", artifact.name));
                return Ok(());
            }
            let response = client
                .get(&existing.url)
                .bearer_auth(token)
                .header("Accept", "application/octet-stream")
                .send()?;
            if response.status().is_success() {
                let bytes = response.bytes()?;
                let mut hasher = Sha512::new();
                hasher.update(&bytes);
                if hex::encode(hasher.finalize()) == artifact.sha512 {
                    output.push(format!("GitHub already has {}", artifact.name));
                    return Ok(());
                }
            }
        }
        return Err(format!(
            "GitHub release already has {} with different bytes",
            artifact.name
        )
        .into());
    }
    let upload_url = release
        .upload_url
        .split('{')
        .next()
        .unwrap_or(&release.upload_url);
    let url = format!("{upload_url}?name={}", urlencoding(&artifact.name));
    let response = client
        .post(url)
        .bearer_auth(token)
        .header("Content-Type", "application/octet-stream")
        .body(artifact.bytes.clone())
        .send()?;
    if !response.status().is_success() {
        return Err(format!("GitHub asset upload failed: {}", response.status()).into());
    }
    output.push(format!("uploaded GitHub {}", artifact.name));
    Ok(())
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
}
