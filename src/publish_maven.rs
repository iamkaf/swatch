use super::{ArtifactKind, PreparedRelease, Result};

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

pub fn reject_live_publish(release: &PreparedRelease) -> Result<()> {
    let config = release
        .config
        .maven
        .as_ref()
        .ok_or_else(|| crate::Error::from("Maven is not configured"))?;
    if !config.repository.starts_with("https://") {
        return Err("publish.maven.repository must use HTTPS".into());
    }
    Err(
        "Maven publication requires an atomic repository transaction for versioned artifacts and maven-metadata.xml; raw HTTPS repositories do not provide one, so no files were uploaded"
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{Loader, Lockfile, PackMeta};

    fn artifact(name: &str, kind: ArtifactKind, bytes: &[u8]) -> super::super::Artifact {
        super::super::Artifact {
            name: name.into(),
            kind,
            sha256: super::super::hash::sha256_hex(bytes),
            sha512: super::super::hash::sha512_hex(bytes),
            bytes: bytes.into(),
        }
    }

    #[test]
    fn live_publication_fails_before_repository_access() {
        let release = PreparedRelease {
            lock: Lockfile::new(
                PackMeta {
                    name: "Example Pack".into(),
                    slug: "example-pack".into(),
                    version: "1.0.0".into(),
                    group: "org.example.packs".into(),
                    minecraft: "26.2".into(),
                    loader: Loader::Fabric,
                    loader_version: "0.19.3".into(),
                },
                Vec::new(),
            ),
            config: super::super::PublishConfig {
                maven: Some(super::super::MavenConfig {
                    repository: "https://example.invalid/maven".into(),
                }),
                ..Default::default()
            },
            artifacts: vec![
                artifact("example-pack-1.0.0-client.mrpack", ArtifactKind::Modrinth, b"client"),
                artifact("example-pack-1.0.0.pom", ArtifactKind::Maven, b"pom"),
                artifact(
                    "maven-metadata.xml",
                    ArtifactKind::MavenMetadata,
                    b"<metadata><versioning><versions><version>0.9.0</version><version>1.0.0</version></versions></versioning></metadata>",
                ),
            ],
            changelog: None,
        };
        let error = reject_live_publish(&release)
            .expect_err("non-atomic Maven publication")
            .to_string();
        assert!(error.contains("no files were uploaded"));
    }
}
