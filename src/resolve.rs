use crate::spec::{ContentPlacement, ContentSpec, FileSpec, Lockfile, PackMeta};
use crate::{Result, USER_AGENT};
use reqwest::blocking::Client;
use serde::Deserialize;
use std::time::Duration;

const MODRINTH_API: &str = "https://api.modrinth.com/v2";

pub struct Resolver {
    client: Client,
}

impl Resolver {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .user_agent(USER_AGENT)
                .timeout(Duration::from_secs(60))
                .build()?,
        })
    }

    pub fn resolve(&self, pack: &PackMeta, content: &ContentSpec) -> Result<FileSpec> {
        self.resolve_modrinth(pack, content)
    }

    /// Resolve a project name to a Modrinth slug. Exact slugs win; a search
    /// result is accepted only when it is unambiguous.
    pub fn find_project(&self, query: &str) -> Result<String> {
        let query = query.trim();
        if query.is_empty() {
            return Err("project name is required".into());
        }

        if valid_project_id(query) {
            let response = self
                .client
                .get(format!("{MODRINTH_API}/project/{query}"))
                .send()?;
            if response.status().is_success() {
                return Ok(response.json::<ModrinthProject>()?.slug);
            }
            if response.status() != reqwest::StatusCode::NOT_FOUND {
                return Err(format!(
                    "could not look up Modrinth project `{query}`: {}",
                    response.status()
                )
                .into());
            }
        }

        let response: ModrinthSearch = self
            .client
            .get(format!("{MODRINTH_API}/search"))
            .query(&[("query", query), ("limit", "10")])
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)?
            .json()?;
        let exact: Vec<_> = response
            .hits
            .iter()
            .filter(|hit| {
                hit.slug.eq_ignore_ascii_case(query) || hit.title.eq_ignore_ascii_case(query)
            })
            .collect();
        let candidates = if exact.is_empty() {
            response.hits.iter().collect()
        } else {
            exact
        };
        match candidates.as_slice() {
            [hit] => Ok(hit.slug.clone()),
            [] => Err(format!("Modrinth has no project matching `{query}`").into()),
            hits => {
                let names = hits
                    .iter()
                    .take(5)
                    .map(|hit| hit.slug.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                Err(format!(
                    "Modrinth found several projects for `{query}`: {names}; use a project slug"
                )
                .into())
            }
        }
    }

    pub fn project_placement(
        &self,
        project: &str,
        requested: ContentPlacement,
    ) -> Result<ContentPlacement> {
        let project: ModrinthProject = self
            .client
            .get(format!("{MODRINTH_API}/project/{project}"))
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)?
            .json()?;
        let expected_type = requested.modrinth_kind();
        if project.project_type != expected_type {
            return Err(format!(
                "Modrinth project {} is a {}, not a {expected_type}",
                project.slug, project.project_type
            )
            .into());
        }
        if requested == ContentPlacement::Shader {
            return Ok(ContentPlacement::Shader);
        }
        match (project.client_side.as_str(), project.server_side.as_str()) {
            (_, "unsupported") => Ok(ContentPlacement::ClientMod),
            ("unsupported", _) => Ok(ContentPlacement::ServerMod),
            _ => Ok(ContentPlacement::SharedMod),
        }
    }

    pub fn latest_version(
        &self,
        pack: &PackMeta,
        placement: ContentPlacement,
        project: &str,
    ) -> Result<String> {
        let url = format!("{MODRINTH_API}/project/{project}/version");
        let request = self.client.get(url);
        let request = match placement {
            ContentPlacement::SharedMod
            | ContentPlacement::ClientMod
            | ContentPlacement::ServerMod => request.query(&[
                ("loaders", serde_json::to_string(&[pack.loader.as_str()])?),
                (
                    "game_versions",
                    serde_json::to_string(&[pack.minecraft.as_str()])?,
                ),
            ]),
            ContentPlacement::Shader => request.query(&[(
                "game_versions",
                serde_json::to_string(&[pack.minecraft.as_str()])?,
            )]),
        };
        let versions: Vec<ModrinthVersion> = request
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)?
            .json()?;
        versions
            .first()
            .map(|version| version.version_number.clone())
            .ok_or_else(|| {
                format!(
                    "Modrinth project {project} has no compatible version for Minecraft {}",
                    pack.minecraft
                )
                .into()
            })
    }

    fn resolve_modrinth(&self, pack: &PackMeta, content: &ContentSpec) -> Result<FileSpec> {
        let project = &content.id;
        let requested_version = &content.version;
        let url = format!("{MODRINTH_API}/project/{project}/version");
        let request = self.client.get(&url);
        let request = match content.placement {
            ContentPlacement::SharedMod
            | ContentPlacement::ClientMod
            | ContentPlacement::ServerMod => request.query(&[
                ("loaders", serde_json::to_string(&[pack.loader.as_str()])?),
                (
                    "game_versions",
                    serde_json::to_string(&[pack.minecraft.as_str()])?,
                ),
            ]),
            ContentPlacement::Shader => request.query(&[(
                "game_versions",
                serde_json::to_string(&[pack.minecraft.as_str()])?,
            )]),
        };
        let response = request
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|error| format!("could not resolve Modrinth project {project}: {error}"))?;
        let versions: Vec<ModrinthVersion> =
            serde_json::from_str(&response.text().map_err(|error| {
                format!("could not read Modrinth response for {project}: {error}")
            })?)
            .map_err(|error| format!("invalid Modrinth response for {project}: {error}"))?;
        let version = exact_version(project, requested_version, &versions)?;
        let file = primary_file(project, requested_version, &version.files)?;
        Ok(FileSpec {
            id: project.to_string(),
            requested_version: requested_version.to_string(),
            path: format!("{}/{}", content.placement.folder(), file.filename),
            file_size: file.size,
            sha1: file.hashes.sha1.clone(),
            sha512: file.hashes.sha512.clone(),
            env: content.placement.env(),
            downloads: vec![file.url.clone()],
        })
    }
}

fn valid_project_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

#[derive(Debug, Deserialize)]
struct ModrinthProject {
    slug: String,
    project_type: String,
    #[serde(default)]
    client_side: String,
    #[serde(default)]
    server_side: String,
}

#[derive(Debug, Deserialize)]
struct ModrinthSearch {
    hits: Vec<ModrinthSearchHit>,
}

#[derive(Debug, Deserialize)]
struct ModrinthSearchHit {
    slug: String,
    title: String,
}

pub fn resolve_candidate(
    spec: &crate::spec::PackSpec,
    previous: Option<&Lockfile>,
) -> Result<Lockfile> {
    let resolver = Resolver::new()?;
    let total = spec.content_count();
    let mut files = Vec::with_capacity(total);
    for (index, content) in spec.content().enumerate() {
        eprintln!("[{}/{}] {}", index + 1, total, content.id);
        let file = resolver.resolve(&spec.pack, content)?;
        file.validate()?;
        files.push(file);
    }
    let mut lock = Lockfile::new(spec.pack.clone(), files);
    if let Some(previous) = previous {
        lock.retain_curseforge_from(previous);
    }
    Ok(lock)
}

pub fn lock_matches_spec(spec: &crate::spec::PackSpec, lock: &crate::spec::Lockfile) -> bool {
    if spec.pack != lock.pack || spec.content_count() != lock.file.len() {
        return false;
    }
    spec.content().all(|content| {
        let Some(file) = lock.file.iter().find(|file| file.id == content.id) else {
            return false;
        };
        file.requested_version == content.version
            && file.env == content.placement.env()
            && file
                .path
                .starts_with(&format!("{}/", content.placement.folder()))
    })
}

#[derive(Debug, Deserialize)]
struct ModrinthVersion {
    id: String,
    version_number: String,
    files: Vec<ModrinthFile>,
}

#[derive(Debug, Deserialize)]
struct ModrinthFile {
    hashes: ModrinthHashes,
    url: String,
    filename: String,
    primary: bool,
    size: u64,
}

#[derive(Debug, Deserialize)]
struct ModrinthHashes {
    sha1: String,
    sha512: String,
}

fn exact_version<'a>(
    project: &str,
    requested: &str,
    versions: &'a [ModrinthVersion],
) -> Result<&'a ModrinthVersion> {
    let matches: Vec<_> = versions
        .iter()
        .filter(|version| version.version_number == requested)
        .collect();
    match matches.as_slice() {
        [version] => Ok(version),
        [] => Err(
            format!("Modrinth project {project} has no compatible version `{requested}`").into(),
        ),
        versions => {
            let first = versions[0];
            let first_file = primary_file(project, requested, &first.files)?;
            let same_file = versions.iter().skip(1).all(|version| {
                primary_file(project, requested, &version.files)
                    .is_ok_and(|file| file.hashes.sha512 == first_file.hashes.sha512)
            });
            if same_file {
                return Ok(first);
            }
            let ids = versions
                .iter()
                .map(|version| version.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            Err(format!(
                "Modrinth project {project} has multiple compatible versions named `{requested}`: {ids}"
            )
            .into())
        }
    }
}

fn primary_file<'a>(
    project: &str,
    version: &str,
    files: &'a [ModrinthFile],
) -> Result<&'a ModrinthFile> {
    let primary: Vec<_> = files.iter().filter(|file| file.primary).collect();
    match primary.as_slice() {
        [file] => Ok(file),
        [] if files.len() == 1 => Ok(&files[0]),
        [] => Err(
            format!("Modrinth project {project} version `{version}` has no primary file").into(),
        ),
        _ => Err(format!(
            "Modrinth project {project} version `{version}` has multiple primary files"
        )
        .into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, primary: bool) -> ModrinthFile {
        ModrinthFile {
            hashes: ModrinthHashes {
                sha1: "a".repeat(40),
                sha512: "b".repeat(128),
            },
            url: format!("https://example.invalid/{name}"),
            filename: name.into(),
            primary,
            size: 1,
        }
    }

    #[test]
    fn exact_versions_and_primary_files_must_be_unambiguous() {
        let versions = vec![ModrinthVersion {
            id: "version-id".into(),
            version_number: "1.2.3".into(),
            files: vec![file("main.jar", true), file("sources.jar", false)],
        }];
        let version = exact_version("example", "1.2.3", &versions).expect("exact version");
        assert_eq!(
            primary_file("example", "1.2.3", &version.files)
                .expect("primary file")
                .filename,
            "main.jar"
        );
        assert!(exact_version("example", "latest", &versions).is_err());

        let duplicate = ModrinthVersion {
            id: "duplicate-version-id".into(),
            version_number: "1.2.3".into(),
            files: vec![file("main.jar", true)],
        };
        let mut versions = vec![versions.into_iter().next().expect("version"), duplicate];
        assert_eq!(
            exact_version("example", "1.2.3", &versions)
                .expect("duplicate metadata for the same file")
                .id,
            "version-id"
        );

        let mut conflicting = file("other.jar", true);
        conflicting.hashes.sha512 = "c".repeat(128);
        let conflict = ModrinthVersion {
            id: "conflicting-version-id".into(),
            version_number: "1.2.3".into(),
            files: vec![conflicting],
        };
        assert!(exact_version("example", "1.2.3", &[versions.remove(0), conflict]).is_err());
    }

    #[test]
    fn project_names_are_searched_instead_of_embedded_in_urls() {
        assert!(valid_project_id("sodium"));
        assert!(valid_project_id("AANobbMI"));
        assert!(!valid_project_id("Sodium Extra"));
        assert!(!valid_project_id("../version"));
    }
}
