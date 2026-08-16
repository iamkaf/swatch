use super::{ArtifactKind, PreparedRelease, Result, http_client};
use serde::{Deserialize, Serialize};

const API_BASE: &str = "https://minecraft.curseforge.com/api/projects";

fn upload_url(project: u64) -> String {
    format!("{API_BASE}/{project}/upload-file")
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UploadMetadata {
    changelog: String,
    changelog_type: String,
    display_name: String,
    game_version_names: Vec<String>,
    release_type: String,
}

#[derive(Debug, Deserialize)]
struct UploadResponse {
    id: u64,
}

pub fn dry_run(release: &PreparedRelease) -> Result<Vec<String>> {
    let config = release
        .config
        .curseforge
        .as_ref()
        .ok_or_else(|| crate::Error::from("CurseForge is not configured"))?;
    let artifact = release.artifact(ArtifactKind::CurseForge)?;
    if config.project == 0 {
        return Err("publish.curseforge.project must be a positive project ID".into());
    }
    Ok(vec![format!(
        "DRY CurseForge {} <- {} ({})",
        upload_url(config.project),
        artifact.name,
        artifact.sha512
    )])
}

pub fn publish(release: &PreparedRelease, root: &crate::PackRoot) -> Result<Vec<String>> {
    let config = release
        .config
        .curseforge
        .as_ref()
        .ok_or_else(|| crate::Error::from("CurseForge is not configured"))?;
    let token = std::env::var("CURSEFORGE_TOKEN")
        .map_err(|_| crate::Error::from("set CURSEFORGE_TOKEN"))?;
    let artifact = release.artifact(ArtifactKind::CurseForge)?;
    if config.project == 0 {
        return Err("publish.curseforge.project must be a positive project ID".into());
    }
    let metadata = serde_json::to_string(&UploadMetadata {
        changelog: release.changelog(root)?,
        changelog_type: "markdown".into(),
        display_name: format!("{} {}", release.lock.pack.name, release.lock.pack.version),
        game_version_names: vec![
            loader_display_name(&release.lock.pack.loader),
            release.lock.pack.minecraft.clone(),
        ],
        release_type: "release".into(),
    })?;
    let url = upload_url(config.project);
    let form = reqwest::blocking::multipart::Form::new()
        .text("metadata", metadata)
        .part(
            "file",
            reqwest::blocking::multipart::Part::bytes(artifact.bytes.clone())
                .file_name(artifact.name.clone()),
        );
    let response = http_client()?
        .post(url)
        .header("X-Api-Token", token)
        .multipart(form)
        .send()?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "CurseForge upload failed: {status}: {}",
            response.text().unwrap_or_default().trim()
        )
        .into());
    }
    let uploaded: UploadResponse = response.json()?;
    Ok(vec![format!(
        "uploaded CurseForge {} as file {}",
        artifact.name, uploaded.id
    )])
}

fn loader_display_name(loader: &str) -> String {
    match loader {
        "fabric" => "Fabric".into(),
        "forge" => "Forge".into(),
        "neoforge" => "NeoForge".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_the_curseforge_author_upload_endpoint() {
        assert_eq!(
            upload_url(123),
            "https://minecraft.curseforge.com/api/projects/123/upload-file"
        );
    }

    #[test]
    fn sends_game_version_names_to_curseforge() {
        let metadata = UploadMetadata {
            changelog: String::new(),
            changelog_type: "markdown".into(),
            display_name: "Pack 1.0.0".into(),
            game_version_names: vec!["Fabric".into(), "26.2".into()],
            release_type: "release".into(),
        };
        let json = serde_json::to_value(metadata).expect("metadata JSON");
        assert_eq!(json["gameVersionNames"][1], "26.2");
        assert!(json.get("gameVersions").is_none());
    }

    #[test]
    fn names_the_selected_loader() {
        assert_eq!(loader_display_name("fabric"), "Fabric");
        assert_eq!(loader_display_name("neoforge"), "NeoForge");
        assert_eq!(loader_display_name("custom"), "custom");
    }
}
