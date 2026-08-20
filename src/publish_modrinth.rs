use super::{ArtifactKind, PreparedRelease, Result, http_client};
use serde::{Deserialize, Serialize};

const API_BASE: &str = "https://api.modrinth.com/v2";

fn create_version_url() -> String {
    format!("{API_BASE}/version")
}

#[derive(Debug, Serialize)]
struct VersionData<'a> {
    name: &'a str,
    version_number: &'a str,
    changelog: &'a str,
    version_type: &'a str,
    loaders: &'a [String],
    game_versions: &'a [String],
    featured: bool,
    status: &'static str,
    project_id: &'a str,
    file_parts: Vec<String>,
    primary_file: String,
}

#[derive(Debug, Deserialize)]
struct ExistingVersion {
    version_number: String,
    files: Vec<ExistingFile>,
}

#[derive(Debug, Deserialize)]
struct ExistingFile {
    filename: String,
    hashes: ExistingHashes,
}

#[derive(Debug, Deserialize)]
struct ExistingHashes {
    #[serde(default)]
    sha512: Option<String>,
}

pub fn dry_run(release: &PreparedRelease) -> Result<Vec<String>> {
    let config = release
        .config
        .modrinth
        .as_ref()
        .ok_or_else(|| crate::Error::from("Modrinth is not configured"))?;
    let artifact = release.artifact(ArtifactKind::Modrinth)?;
    if config.project.trim().is_empty() {
        return Err("publish.modrinth.project is required".into());
    }
    Ok(vec![format!(
        "DRY Modrinth {} for {} <- {} ({})",
        create_version_url(),
        config.project,
        artifact.name,
        artifact.sha512
    )])
}

pub fn publish(release: &PreparedRelease) -> Result<Vec<String>> {
    let config = release
        .config
        .modrinth
        .as_ref()
        .ok_or_else(|| crate::Error::from("Modrinth is not configured"))?;
    let artifact = release.artifact(ArtifactKind::Modrinth)?;
    let token =
        std::env::var("MODRINTH_TOKEN").map_err(|_| crate::Error::from("set MODRINTH_TOKEN"))?;
    let client = http_client()?;
    if let Some(message) = already_published(&client, &token, config, release, artifact)? {
        return Ok(vec![message]);
    }

    let loaders = vec![release.lock.pack.loader.as_str().to_string()];
    let game_versions = vec![release.lock.pack.minecraft.clone()];
    let changelog = release.changelog()?;
    let data = serde_json::to_string(&VersionData {
        name: &format!("{} {}", release.lock.pack.name, release.lock.pack.version),
        version_number: &release.lock.pack.version,
        changelog,
        version_type: "release",
        loaders: &loaders,
        game_versions: &game_versions,
        featured: true,
        status: "listed",
        project_id: &config.project,
        file_parts: vec!["file".into()],
        primary_file: "file".into(),
    })?;
    let form = reqwest::blocking::multipart::Form::new()
        .text("data", data)
        .part(
            "file",
            reqwest::blocking::multipart::Part::bytes(artifact.bytes.clone())
                .file_name(artifact.name.clone()),
        );
    let response = client
        .post(create_version_url())
        .header("Authorization", token)
        .multipart(form)
        .send()?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "Modrinth upload failed: {status}: {}",
            response.text().unwrap_or_default().trim()
        )
        .into());
    }
    Ok(vec![format!("uploaded Modrinth {}", artifact.name)])
}

fn already_published(
    client: &reqwest::blocking::Client,
    token: &str,
    config: &super::ModrinthConfig,
    release: &PreparedRelease,
    artifact: &super::Artifact,
) -> Result<Option<String>> {
    let url = format!("{API_BASE}/project/{}/version", config.project);
    let response = client
        .get(url)
        .header("Authorization", token)
        .query(&[
            (
                "game_versions",
                serde_json::to_string(&[release.lock.pack.minecraft.as_str()])?,
            ),
            ("include_changelog", "false".to_string()),
        ])
        .send()?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let versions: Vec<ExistingVersion> = response.error_for_status()?.json()?;
    for version in versions {
        if version.version_number != release.lock.pack.version {
            continue;
        }
        if version.files.iter().any(|file| {
            file.filename == artifact.name
                && file.hashes.sha512.as_deref() == Some(artifact.sha512.as_str())
        }) {
            return Ok(Some(format!("Modrinth already has {}", artifact.name)));
        }
        return Err(format!(
            "Modrinth already has version {} with different bytes",
            release.lock.pack.version
        )
        .into());
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_versions_at_the_modrinth_version_endpoint() {
        assert_eq!(create_version_url(), "https://api.modrinth.com/v2/version");
    }
}
