use super::{ArtifactKind, PreparedRelease, Result, http_client};

pub fn dry_run(release: &PreparedRelease) -> Result<Vec<String>> {
    let config = release
        .config
        .maven
        .as_ref()
        .ok_or_else(|| crate::Error::from("Maven is not configured"))?;
    if !config.repository.starts_with("https://") {
        return Err("publish.maven.repository must use HTTPS".into());
    }
    let group = &release.lock.pack.group;
    let artifact = &release.lock.pack.slug;
    let prefix = format!(
        "{}/{}/{}/",
        config.repository.trim_end_matches('/'),
        group.replace('.', "/"),
        artifact
    );
    let version_prefix = format!("{prefix}{}/", release.lock.pack.version);
    let mut output = Vec::new();
    for item in &release.artifacts {
        let url = match item.kind {
            ArtifactKind::Maven | ArtifactKind::Modrinth => {
                format!("{version_prefix}{}", item.name)
            }
            ArtifactKind::MavenMetadata => format!("{prefix}{}", item.name),
            _ => continue,
        };
        output.push(format!("DRY Maven {url}"));
        output.push(format!("DRY Maven {url}.sha512"));
    }
    Ok(output)
}

pub fn publish(release: &PreparedRelease) -> Result<Vec<String>> {
    let config = release
        .config
        .maven
        .as_ref()
        .ok_or_else(|| crate::Error::from("Maven is not configured"))?;
    if !config.repository.starts_with("https://") {
        return Err("publish.maven.repository must use HTTPS".into());
    }
    let username = std::env::var("MAVEN_PUBLISH_USERNAME")
        .map_err(|_| crate::Error::from("set MAVEN_PUBLISH_USERNAME"))?;
    let password = std::env::var("MAVEN_PUBLISH_PASSWORD")
        .map_err(|_| crate::Error::from("set MAVEN_PUBLISH_PASSWORD"))?;
    let group = &release.lock.pack.group;
    let artifact = &release.lock.pack.slug;
    let prefix = format!(
        "{}/{}/{}/{}",
        config.repository.trim_end_matches('/'),
        group.replace('.', "/"),
        artifact,
        release.lock.pack.version
    );
    let client = http_client()?;
    let mut uploaded = Vec::new();
    for kind in [
        ArtifactKind::Modrinth,
        ArtifactKind::Maven,
        ArtifactKind::MavenMetadata,
    ] {
        let Some(item) = release.artifacts.iter().find(|item| item.kind == kind) else {
            continue;
        };
        let url = match kind {
            ArtifactKind::MavenMetadata => format!(
                "{}/{}/{}/{}",
                config.repository.trim_end_matches('/'),
                group.replace('.', "/"),
                artifact,
                item.name
            ),
            _ => format!("{prefix}/{}", item.name),
        };
        put_if_needed(&client, &url, item, &username, &password)?;
        uploaded.push(format!("Maven {}", item.name));
        let checksum_url = format!("{url}.sha512");
        let checksum = checksum_bytes(item);
        put_bytes_if_needed(
            &client,
            &checksum_url,
            &checksum,
            &super::hash::sha512_hex(&checksum),
            &username,
            &password,
        )?;
    }
    Ok(uploaded)
}

fn checksum_bytes(artifact: &super::Artifact) -> Vec<u8> {
    artifact.sha512.as_bytes().to_vec()
}

fn put_if_needed(
    client: &reqwest::blocking::Client,
    url: &str,
    artifact: &super::Artifact,
    username: &str,
    password: &str,
) -> Result<()> {
    put_bytes_if_needed(
        client,
        url,
        &artifact.bytes,
        &artifact.sha512,
        username,
        password,
    )
}

fn put_bytes_if_needed(
    client: &reqwest::blocking::Client,
    url: &str,
    bytes: &[u8],
    sha512: &str,
    username: &str,
    password: &str,
) -> Result<()> {
    let existing = client
        .get(url)
        .basic_auth(username, Some(password))
        .send()?;
    if existing.status().is_success() {
        let bytes = existing.bytes()?;
        if super::hash::sha512_hex(&bytes) == sha512 {
            return Ok(());
        }
        return Err(format!("Maven artifact already exists with different bytes: {url}").into());
    }
    if existing.status() != reqwest::StatusCode::NOT_FOUND {
        return Err(format!("Maven lookup failed: {url}: {}", existing.status()).into());
    }
    let response = client
        .put(url)
        .basic_auth(username, Some(password))
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        .body(bytes.to_vec())
        .send()?;
    if !response.status().is_success() {
        return Err(format!("Maven upload failed: {url}: {}", response.status()).into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maven_sidecars_are_raw_digests() {
        let artifact = super::super::Artifact {
            name: "pack.mrpack".into(),
            kind: ArtifactKind::Modrinth,
            sha512: "a".repeat(128),
            bytes: Vec::new(),
        };
        assert_eq!(checksum_bytes(&artifact), "a".repeat(128).into_bytes());
    }
}
