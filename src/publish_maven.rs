use super::{Artifact, ArtifactKind, PreparedRelease, Result};
use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use reqwest::header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_MATCH, IF_NONE_MATCH, PRAGMA};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_METADATA_BYTES: usize = 1024 * 1024;
static CACHE_BUSTER: AtomicU64 = AtomicU64::new(0);

pub fn dry_run(release: &PreparedRelease) -> Result<Vec<String>> {
    let locations = Locations::from_release(release)?;
    let mut output = Vec::new();
    for item in immutable_artifacts(release) {
        let url = locations.version_file(&release.lock.pack.version, &item.name);
        output.push(format!("DRY Maven {url}"));
        output.push(format!("DRY Maven {url}.sha512"));
    }
    let metadata = metadata_artifact(release)?;
    output.push(format!("DRY Maven {}", locations.metadata(&metadata.name)));
    Ok(output)
}

pub fn publish(release: &PreparedRelease) -> Result<Vec<String>> {
    let locations = Locations::from_release(release)?;
    let username = required_env("MAVEN_PUBLISH_USERNAME")?;
    let password = required_env("MAVEN_PUBLISH_PASSWORD")?;
    let client = super::http_client()?;
    let metadata = metadata_artifact(release)?;
    let metadata_url = locations.metadata(&metadata.name);
    let metadata_update = prepare_metadata_update(
        &client,
        &metadata_url,
        metadata,
        &release.lock.pack.group,
        &release.lock.pack.slug,
        &release.lock.pack.version,
    )
    .map_err(|error| crate::Error::from(format!("{error}; no files were uploaded")))?;

    let mut output = Vec::new();
    for item in immutable_artifacts(release) {
        let url = locations.version_file(&release.lock.pack.version, &item.name);
        publish_immutable(&client, &url, &item.name, &item.bytes, &username, &password)?;
        output.push(format!("published Maven {}", item.name));

        let sidecar_name = format!("{}.sha512", item.name);
        let sidecar_url = format!("{url}.sha512");
        publish_immutable(
            &client,
            &sidecar_url,
            &sidecar_name,
            item.sha512.as_bytes(),
            &username,
            &password,
        )?;
        output.push(format!("published Maven {sidecar_name}"));
    }

    match metadata_update {
        MetadataUpdate::Unchanged => {
            output.push("Maven metadata is already published".into());
        }
        MetadataUpdate::Create => {
            put_metadata(
                &client,
                &metadata_url,
                &metadata.bytes,
                MetadataPrecondition::Missing,
                &username,
                &password,
            )?;
            output.push("published Maven metadata".into());
        }
        MetadataUpdate::Replace(etag) => {
            put_metadata(
                &client,
                &metadata_url,
                &metadata.bytes,
                MetadataPrecondition::Matching(&etag),
                &username,
                &password,
            )?;
            output.push("published Maven metadata".into());
        }
    }
    Ok(output)
}

struct Locations {
    base: String,
}

impl Locations {
    fn from_release(release: &PreparedRelease) -> Result<Self> {
        let config = release
            .config
            .maven
            .as_ref()
            .ok_or_else(|| crate::Error::from("Maven is not configured"))?;
        if !config.repository.starts_with("https://") {
            return Err("publish.maven.repository must use HTTPS".into());
        }
        Ok(Self {
            base: format!(
                "{}/{}/{}",
                config.repository.trim_end_matches('/'),
                release.lock.pack.group.replace('.', "/"),
                release.lock.pack.slug
            ),
        })
    }

    fn version_file(&self, version: &str, name: &str) -> String {
        format!("{}/{version}/{name}", self.base)
    }

    fn metadata(&self, name: &str) -> String {
        format!("{}/{name}", self.base)
    }
}

fn immutable_artifacts(release: &PreparedRelease) -> impl Iterator<Item = &Artifact> {
    release
        .artifacts
        .iter()
        .filter(|item| matches!(item.kind, ArtifactKind::Maven | ArtifactKind::Modrinth))
}

fn metadata_artifact(release: &PreparedRelease) -> Result<&Artifact> {
    let mut matches = release
        .artifacts
        .iter()
        .filter(|item| item.kind == ArtifactKind::MavenMetadata);
    let metadata = matches
        .next()
        .ok_or_else(|| crate::Error::from("prepared release is missing Maven metadata"))?;
    if matches.next().is_some() {
        return Err("prepared release has more than one Maven metadata artifact".into());
    }
    Ok(metadata)
}

fn required_env(name: &str) -> Result<String> {
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => Ok(value),
        _ => Err(format!("Maven publication requires {name}; no files were uploaded").into()),
    }
}

fn publish_immutable(
    client: &Client,
    url: &str,
    name: &str,
    bytes: &[u8],
    username: &str,
    password: &str,
) -> Result<()> {
    match get_public(client, url, bytes.len())? {
        PublicFile::Present(remote) if remote == bytes => return Ok(()),
        PublicFile::Present(_) => {
            return Err(format!("Maven already has different bytes for {name}").into());
        }
        PublicFile::Missing => {}
    }

    let response = client
        .put(url)
        .basic_auth(username, Some(password))
        .header(CONTENT_TYPE, "application/octet-stream")
        .body(bytes.to_vec())
        .send()?;
    match response.status() {
        StatusCode::OK => Ok(()),
        StatusCode::CONFLICT => match get_public(client, url, bytes.len())? {
            PublicFile::Present(remote) if remote == bytes => Ok(()),
            PublicFile::Present(_) => Err(format!(
                "Maven rejected duplicate {name}, and the published bytes differ"
            )
            .into()),
            PublicFile::Missing => Err(format!(
                "Maven rejected duplicate {name}, but the file is still not publicly readable"
            )
            .into()),
        },
        status => Err(format!("Maven upload for {name} failed with HTTP {status}").into()),
    }
}

enum PublicFile {
    Missing,
    Present(Vec<u8>),
}

fn get_public(client: &Client, url: &str, expected_len: usize) -> Result<PublicFile> {
    let response = client
        .get(cache_busted_url(url)?)
        .header(CACHE_CONTROL, "no-cache")
        .header(PRAGMA, "no-cache")
        .send()?;
    match response.status() {
        StatusCode::OK => Ok(PublicFile::Present(read_limited(
            response,
            expected_len,
            "published Maven file",
        )?)),
        StatusCode::NOT_FOUND => Ok(PublicFile::Missing),
        status => Err(format!("Maven lookup for {url} failed with HTTP {status}").into()),
    }
}

enum MetadataUpdate {
    Unchanged,
    Create,
    Replace(String),
}

fn prepare_metadata_update(
    client: &Client,
    url: &str,
    prepared: &Artifact,
    group: &str,
    artifact: &str,
    version: &str,
) -> Result<MetadataUpdate> {
    let response = client
        .get(cache_busted_url(url)?)
        .header(CACHE_CONTROL, "no-cache")
        .header(PRAGMA, "no-cache")
        .send()?;
    match response.status() {
        StatusCode::NOT_FOUND => {
            validate_metadata(&prepared.bytes, None, group, artifact, version)?;
            Ok(MetadataUpdate::Create)
        }
        StatusCode::OK => {
            let mut etags = response.headers().get_all(ETAG).iter();
            let etag = etags.next().cloned().filter(|_| etags.next().is_none());
            let current = read_limited(response, MAX_METADATA_BYTES, "published Maven metadata")?;
            validate_metadata(&prepared.bytes, Some(&current), group, artifact, version)?;
            if current == prepared.bytes {
                return Ok(MetadataUpdate::Unchanged);
            }
            let etag = strong_etag(etag.as_ref())?;
            Ok(MetadataUpdate::Replace(etag.into()))
        }
        status => Err(format!("Maven metadata lookup failed with HTTP {status}").into()),
    }
}

enum MetadataPrecondition<'a> {
    Missing,
    Matching(&'a str),
}

fn put_metadata(
    client: &Client,
    url: &str,
    bytes: &[u8],
    precondition: MetadataPrecondition<'_>,
    username: &str,
    password: &str,
) -> Result<()> {
    let request = client
        .put(url)
        .basic_auth(username, Some(password))
        .header(CACHE_CONTROL, "no-cache")
        .header(CONTENT_TYPE, "application/xml");
    let request = match precondition {
        MetadataPrecondition::Missing => request.header(IF_NONE_MATCH, "*"),
        MetadataPrecondition::Matching(etag) => request.header(IF_MATCH, etag),
    };
    let response = request.body(bytes.to_vec()).send()?;
    match response.status() {
        StatusCode::OK => Ok(()),
        StatusCode::PRECONDITION_FAILED => Err(
            "Maven metadata changed after preparation; run `swatch prepare` again and retry".into(),
        ),
        status => {
            Err(format!("conditional Maven metadata upload failed with HTTP {status}").into())
        }
    }
}

fn cache_busted_url(url: &str) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(url).map_err(crate::Error::from_display)?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = CACHE_BUSTER.fetch_add(1, Ordering::Relaxed);
    url.query_pairs_mut().append_pair(
        "swatch_release_check",
        &format!("{timestamp:x}-{sequence:x}"),
    );
    Ok(url)
}

fn read_limited(mut response: Response, limit: usize, subject: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(limit.saturating_add(1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(format!("{subject} is unexpectedly large").into());
    }
    Ok(bytes)
}

fn strong_etag(value: Option<&reqwest::header::HeaderValue>) -> Result<&str> {
    value
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            value.len() >= 2
                && value.starts_with('"')
                && value.ends_with('"')
                && !value[1..value.len() - 1].contains('"')
        })
        .ok_or_else(|| {
            crate::Error::from(
                "Maven metadata GET did not return one strong ETag; conditional publication is disabled",
            )
        })
}

#[derive(Debug, Default, Deserialize)]
struct MavenMetadata {
    #[serde(default, rename = "groupId")]
    group_id: String,
    #[serde(default, rename = "artifactId")]
    artifact_id: String,
    #[serde(default)]
    versioning: MavenVersioning,
}

#[derive(Debug, Default, Deserialize)]
struct MavenVersioning {
    #[serde(default)]
    latest: Option<String>,
    #[serde(default)]
    release: Option<String>,
    #[serde(default)]
    versions: MavenVersions,
}

#[derive(Debug, Default, Deserialize)]
struct MavenVersions {
    #[serde(default)]
    version: Vec<String>,
}

fn validate_metadata(
    prepared: &[u8],
    current: Option<&[u8]>,
    expected_group: &str,
    expected_artifact: &str,
    release_version: &str,
) -> Result<()> {
    let prepared = parse_metadata(prepared, "prepared")?;
    validate_metadata_identity(&prepared, expected_group, expected_artifact, "prepared")?;
    let prepared_versions = versions(&prepared, "prepared")?;
    if !prepared_versions.contains(release_version) {
        return Err(format!(
            "prepared Maven metadata does not contain release version {release_version}"
        )
        .into());
    }
    validate_version_pointers(&prepared, &prepared_versions, "prepared")?;

    let Some(current) = current else {
        return Ok(());
    };
    let current = parse_metadata(current, "published")?;
    validate_metadata_identity(&current, expected_group, expected_artifact, "published")?;
    let current_versions = versions(&current, "published")?;
    if !current_versions.is_subset(&prepared_versions) {
        let missing = current_versions
            .difference(&prepared_versions)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "prepared Maven metadata would remove published versions: {missing}; run `swatch prepare` again"
        )
        .into());
    }
    validate_version_pointers(&current, &prepared_versions, "published")
}

fn parse_metadata(bytes: &[u8], subject: &str) -> Result<MavenMetadata> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| crate::Error::from(format!("{subject} Maven metadata is not UTF-8")))?;
    quick_xml::de::from_str(text).map_err(|error| {
        crate::Error::from(format!("cannot parse {subject} Maven metadata: {error}"))
    })
}

fn validate_metadata_identity(
    metadata: &MavenMetadata,
    group: &str,
    artifact: &str,
    subject: &str,
) -> Result<()> {
    if metadata.group_id != group {
        return Err(format!("{subject} Maven metadata has the wrong groupId").into());
    }
    if metadata.artifact_id != artifact {
        return Err(format!("{subject} Maven metadata has the wrong artifactId").into());
    }
    Ok(())
}

fn versions(metadata: &MavenMetadata, subject: &str) -> Result<BTreeSet<String>> {
    let mut versions = BTreeSet::new();
    for version in &metadata.versioning.versions.version {
        if version.is_empty() {
            return Err(format!("{subject} Maven metadata contains an empty version").into());
        }
        if !versions.insert(version.clone()) {
            return Err(
                format!("{subject} Maven metadata contains duplicate version {version}").into(),
            );
        }
    }
    Ok(versions)
}

fn validate_version_pointers(
    metadata: &MavenMetadata,
    allowed_versions: &BTreeSet<String>,
    subject: &str,
) -> Result<()> {
    for (name, value) in [
        ("latest", metadata.versioning.latest.as_deref()),
        ("release", metadata.versioning.release.as_deref()),
    ] {
        if let Some(value) = value
            && !allowed_versions.contains(value)
        {
            return Err(
                format!("{subject} Maven metadata has an unknown {name} version: {value}").into(),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{Loader, Lockfile, PackMeta};
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    fn artifact(name: &str, kind: ArtifactKind, bytes: &[u8]) -> super::super::Artifact {
        super::super::Artifact {
            name: name.into(),
            kind,
            sha256: super::super::hash::sha256_hex(bytes),
            sha512: super::super::hash::sha512_hex(bytes),
            bytes: bytes.into(),
        }
    }

    fn release() -> PreparedRelease {
        PreparedRelease {
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
                artifact(
                    "example-pack-1.0.0-client.mrpack",
                    ArtifactKind::Modrinth,
                    b"client",
                ),
                artifact("example-pack-1.0.0.pom", ArtifactKind::Maven, b"pom"),
                artifact(
                    "maven-metadata.xml",
                    ArtifactKind::MavenMetadata,
                    metadata(&["0.9.0", "1.0.0"]).as_bytes(),
                ),
            ],
            changelog: None,
        }
    }

    fn metadata(versions: &[&str]) -> String {
        let latest = versions.last().copied().unwrap_or_default();
        let rows = versions
            .iter()
            .map(|version| format!("      <version>{version}</version>\n"))
            .collect::<String>();
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<metadata>\n\
  <groupId>org.example.packs</groupId>\n\
  <artifactId>example-pack</artifactId>\n\
  <versioning>\n\
    <latest>{latest}</latest>\n\
    <release>{latest}</release>\n\
    <versions>\n\
{rows}\
    </versions>\n\
  </versioning>\n\
</metadata>\n"
        )
    }

    #[test]
    fn dry_run_lists_only_versioned_sidecars() {
        let output = dry_run(&release()).expect("Maven dry run");
        assert_eq!(output.len(), 5);
        assert!(
            output
                .iter()
                .any(|line| line.ends_with("maven-metadata.xml"))
        );
        assert!(
            output
                .iter()
                .all(|line| !line.ends_with("maven-metadata.xml.sha512"))
        );
    }

    #[test]
    fn metadata_update_cannot_remove_published_versions() {
        let prepared = metadata(&["0.9.0", "1.0.0"]);
        let current = metadata(&["0.9.0", "0.9.1"]);
        let error = validate_metadata(
            prepared.as_bytes(),
            Some(current.as_bytes()),
            "org.example.packs",
            "example-pack",
            "1.0.0",
        )
        .expect_err("destructive metadata")
        .to_string();
        assert!(error.contains("remove published versions: 0.9.1"));
    }

    #[test]
    fn metadata_update_keeps_published_versions() {
        validate_metadata(
            metadata(&["0.9.0", "1.0.0"]).as_bytes(),
            Some(metadata(&["0.9.0"]).as_bytes()),
            "org.example.packs",
            "example-pack",
            "1.0.0",
        )
        .expect("non-destructive metadata");
    }

    #[test]
    fn weak_or_missing_etags_are_rejected() {
        assert!(strong_etag(None).is_err());
        assert!(strong_etag(Some(&"W/\"weak\"".parse().expect("header"))).is_err());
        assert_eq!(
            strong_etag(Some(&"\"strong\"".parse().expect("header"))).expect("strong ETag"),
            "\"strong\""
        );
    }

    #[test]
    fn immutable_conflict_is_accepted_only_after_exact_public_read() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind server");
        let address = listener.local_addr().expect("server address");
        let server = thread::spawn(move || {
            let first = read_request(listener.accept().expect("first request").0);
            assert_eq!(first.method, "GET");
            assert_eq!(first.header("authorization"), None);
            respond(first.stream, 404, &[], &[]);

            let second = read_request(listener.accept().expect("second request").0);
            assert_eq!(second.method, "PUT");
            assert!(second.header("authorization").is_some());
            assert_eq!(second.body, b"release bytes");
            respond(second.stream, 409, &[], &[]);

            let third = read_request(listener.accept().expect("third request").0);
            assert_eq!(third.method, "GET");
            assert_eq!(third.header("authorization"), None);
            respond(third.stream, 200, &[], b"release bytes");
        });

        publish_immutable(
            &Client::new(),
            &format!("http://{address}/pack.mrpack"),
            "pack.mrpack",
            b"release bytes",
            "release-user",
            "release-password",
        )
        .expect("idempotent conflict");
        server.join().expect("server thread");
    }

    #[test]
    fn stale_metadata_reports_reprepare_action() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind server");
        let address = listener.local_addr().expect("server address");
        let prepared = metadata(&["0.9.0", "1.0.0"]);
        let current = metadata(&["0.9.0"]);
        let server = thread::spawn(move || {
            let get = read_request(listener.accept().expect("GET request").0);
            assert_eq!(get.method, "GET");
            assert_eq!(get.header("authorization"), None);
            assert_eq!(get.header("cache-control"), Some("no-cache"));
            assert!(get.path.contains("swatch_release_check="));
            respond(
                get.stream,
                200,
                &[("ETag", "\"initial\"")],
                current.as_bytes(),
            );

            let put = read_request(listener.accept().expect("PUT request").0);
            assert_eq!(put.method, "PUT");
            assert_eq!(put.header("if-match"), Some("\"initial\""));
            assert!(put.header("authorization").is_some());
            respond(put.stream, 412, &[], &[]);
        });
        let client = Client::new();
        let url = format!("http://{address}/maven-metadata.xml");
        let update = prepare_metadata_update(
            &client,
            &url,
            &artifact(
                "maven-metadata.xml",
                ArtifactKind::MavenMetadata,
                prepared.as_bytes(),
            ),
            "org.example.packs",
            "example-pack",
            "1.0.0",
        )
        .expect("metadata preflight");
        let MetadataUpdate::Replace(etag) = update else {
            panic!("expected replacement");
        };
        let error = put_metadata(
            &client,
            &url,
            prepared.as_bytes(),
            MetadataPrecondition::Matching(&etag),
            "release-user",
            "release-password",
        )
        .expect_err("stale metadata")
        .to_string();
        assert!(error.contains("run `swatch prepare` again and retry"));
        server.join().expect("server thread");
    }

    struct Request {
        method: String,
        path: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
        stream: TcpStream,
    }

    impl Request {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        }
    }

    fn read_request(stream: TcpStream) -> Request {
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
        let mut request_line = String::new();
        reader.read_line(&mut request_line).expect("request line");
        let mut parts = request_line.split_whitespace();
        let method = parts.next().expect("method").into();
        let path = parts.next().expect("path").into();
        let mut headers: Vec<(String, String)> = Vec::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).expect("header");
            if line == "\r\n" {
                break;
            }
            let (name, value) = line.split_once(':').expect("header separator");
            headers.push((name.to_ascii_lowercase(), value.trim().into()));
        }
        let content_length = headers
            .iter()
            .find(|(name, _)| name == "content-length")
            .and_then(|(_, value)| value.parse().ok())
            .unwrap_or(0);
        let mut body = vec![0; content_length];
        reader.read_exact(&mut body).expect("request body");
        Request {
            method,
            path,
            headers,
            body,
            stream,
        }
    }

    fn respond(mut stream: TcpStream, status: u16, headers: &[(&str, &str)], body: &[u8]) {
        let reason = match status {
            200 => "OK",
            404 => "Not Found",
            409 => "Conflict",
            412 => "Precondition Failed",
            _ => "Test",
        };
        write!(stream, "HTTP/1.1 {status} {reason}\r\n").expect("status");
        for (name, value) in headers {
            write!(stream, "{name}: {value}\r\n").expect("header");
        }
        write!(
            stream,
            "Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .expect("response headers");
        stream.write_all(body).expect("response body");
    }
}
