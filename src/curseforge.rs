use crate::fetch;
use crate::spec::{CurseForgeFile, Lockfile, SideRequirement, client_file};
use crate::{PackRoot, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

const CURSEFORGE_MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Overrides {
    curseforge: Config,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    #[serde(default)]
    add: Vec<ExplicitFile>,
    #[serde(default)]
    exclude: Vec<ExcludedFile>,
}

#[derive(Debug, Deserialize)]
struct ExplicitFile {
    id: String,
    project_id: u32,
    file_id: u32,
}

#[derive(Debug, Deserialize)]
struct ExcludedFile {
    id: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
struct PackwizMeta {
    filename: String,
    download: PackwizDownload,
    update: PackwizUpdate,
}

#[derive(Debug, Deserialize)]
struct PackwizDownload {
    #[serde(rename = "hash-format")]
    hash_format: String,
    hash: String,
}

#[derive(Debug, Deserialize)]
struct PackwizUpdate {
    curseforge: PackwizCurseForge,
}

#[derive(Debug, Deserialize)]
struct PackwizCurseForge {
    #[serde(rename = "project-id")]
    project_id: u32,
    #[serde(rename = "file-id")]
    file_id: u32,
}

pub fn ensure_mappings(
    root: &PackRoot,
    lock: Lockfile,
    verified: &fetch::VerifiedFiles,
) -> Result<Lockfile> {
    let config = load_config(root)?;
    let excluded = validate_config(&config, &lock)?;
    let mapped: BTreeSet<_> = lock
        .curseforge
        .iter()
        .map(|file| (file.path.as_str(), file.sha1.as_str()))
        .collect();
    let complete = lock
        .file
        .iter()
        .filter(|file| client_file(file))
        .all(|file| {
            excluded.contains(&file.path)
                || mapped.contains(&(file.path.as_str(), file.sha1.as_str()))
        });
    if complete {
        return Ok(lock);
    }

    let packwiz = std::env::var_os("PACKWIZ_BIN").unwrap_or_else(|| "packwiz".into());
    let temp = tempfile::tempdir()?;
    initialise_packwiz(temp.path(), &lock)?;

    for file in lock.file.iter().filter(|file| {
        client_file(file)
            && !excluded.contains(&file.path)
            && file.path.starts_with("mods/")
            && file.path.ends_with(".jar")
    }) {
        let source = verified.path(file)?;
        let destination = temp.path().join(&file.path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, destination)?;
    }

    run_packwiz(&packwiz, temp.path(), &["--yes", "curseforge", "detect"])?;
    for file in &config.add {
        let locked = lock
            .file
            .iter()
            .find(|candidate| candidate.id == file.id)
            .ok_or_else(|| format!("{} is not in pack.lock.toml", file.id))?;
        if !client_file(locked) {
            return Err(format!("{} is not a client file", file.id).into());
        }
        let folder = Path::new(&locked.path)
            .parent()
            .and_then(Path::to_str)
            .ok_or_else(|| format!("{} has no metadata folder", locked.path))?;
        let project_id = file.project_id.to_string();
        let file_id = file.file_id.to_string();
        run_packwiz(
            &packwiz,
            temp.path(),
            &[
                "--meta-folder",
                folder,
                "--yes",
                "curseforge",
                "add",
                "--addon-id",
                &project_id,
                "--file-id",
                &file_id,
            ],
        )?;
    }

    let mappings = mappings_from_packwiz(temp.path(), &lock)?;
    finish_mappings(lock, mappings, &excluded)
}

fn finish_mappings(
    mut lock: Lockfile,
    mappings: Vec<CurseForgeFile>,
    excluded: &BTreeSet<String>,
) -> Result<Lockfile> {
    lock.curseforge = mappings;
    lock.curseforge
        .sort_by(|left, right| left.path.cmp(&right.path));
    let mapped: BTreeSet<_> = lock
        .curseforge
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    let unresolved: Vec<_> = lock
        .file
        .iter()
        .filter(|file| {
            client_file(file)
                && !mapped.contains(file.path.as_str())
                && !excluded.contains(&file.path)
        })
        .map(|file| file.path.clone())
        .collect();
    if !unresolved.is_empty() {
        return Err(format!(
            "Packwiz could not map these CurseForge files: {}",
            unresolved.join(", ")
        )
        .into());
    }
    lock.validate()?;
    Ok(lock)
}

fn validate_config(config: &Config, lock: &Lockfile) -> Result<BTreeSet<String>> {
    let client_files: BTreeMap<_, _> = lock
        .file
        .iter()
        .filter(|file| client_file(file))
        .map(|file| (file.id.as_str(), file))
        .collect();
    let mut additions = BTreeSet::new();
    for file in &config.add {
        if !client_files.contains_key(file.id.as_str()) {
            return Err(format!(
                "overrides.toml [[curseforge.add]] ID is not a locked client file: {}",
                file.id
            )
            .into());
        }
        if file.project_id == 0 || file.file_id == 0 {
            return Err(format!(
                "overrides.toml [[curseforge.add]] has an invalid ID: {}",
                file.id
            )
            .into());
        }
        if !additions.insert(file.id.as_str()) {
            return Err(format!(
                "duplicate overrides.toml [[curseforge.add]] ID: {}",
                file.id
            )
            .into());
        }
    }

    let mut exclusions = BTreeSet::new();
    for file in &config.exclude {
        let Some(locked) = client_files.get(file.id.as_str()) else {
            return Err(format!(
                "overrides.toml [[curseforge.exclude]] ID is not a locked client file: {}",
                file.id
            )
            .into());
        };
        if locked.env.server != SideRequirement::Unsupported {
            return Err(format!(
                "overrides.toml may exclude only client-only files: {}",
                file.id
            )
            .into());
        }
        if file.reason.trim().is_empty() {
            return Err(format!(
                "overrides.toml [[curseforge.exclude]] reason is required: {}",
                file.id
            )
            .into());
        }
        if additions.contains(file.id.as_str()) {
            return Err(format!(
                "overrides.toml cannot add and exclude the same file: {}",
                file.id
            )
            .into());
        }
        if !exclusions.insert(locked.path.clone()) {
            return Err(format!(
                "duplicate overrides.toml [[curseforge.exclude]] ID: {}",
                file.id
            )
            .into());
        }
    }
    Ok(exclusions)
}

fn initialise_packwiz(dir: &Path, lock: &Lockfile) -> Result<()> {
    let pack = format!(
        "name = {}\nauthor = {}\nversion = {}\npack-format = \"packwiz:1.1.0\"\n\n[index]\nfile = \"index.toml\"\nhash-format = \"sha256\"\nhash = \"\"\n\n[versions]\n{} = {}\nminecraft = {}\n",
        toml_string(&lock.pack.name),
        toml_string(crate::TOOL_DISPLAY_NAME),
        toml_string(&lock.pack.version),
        lock.pack.loader,
        toml_string(&lock.pack.loader_version),
        toml_string(&lock.pack.minecraft),
    );
    fs::write(dir.join("pack.toml"), pack)?;
    fs::write(dir.join("index.toml"), "hash-format = \"sha256\"\n")?;
    Ok(())
}

fn run_packwiz(binary: &std::ffi::OsStr, dir: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new(binary)
        .current_dir(dir)
        .args(args)
        .status()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                format!(
                    "Packwiz is required to refresh CurseForge mappings, but `{}` was not found; install Packwiz or set PACKWIZ_BIN to its executable path",
                    binary.to_string_lossy()
                )
            } else {
                format!("could not run Packwiz: {error}")
            }
        })?;
    if !status.success() {
        return Err(format!("Packwiz exited with {status}").into());
    }
    Ok(())
}

fn mappings_from_packwiz(dir: &Path, lock: &Lockfile) -> Result<Vec<CurseForgeFile>> {
    let mut metadata = Vec::new();
    collect_metadata(dir, &mut metadata)?;
    let mut mappings = Vec::new();
    let mut seen = BTreeSet::new();
    for path in metadata {
        let text = fs::read_to_string(&path)?;
        let meta: PackwizMeta = toml::from_str(&text).map_err(crate::Error::from_display)?;
        if meta.download.hash_format != "sha1" {
            return Err(format!(
                "{} used unsupported Packwiz hash format {}",
                path.display(),
                meta.download.hash_format
            )
            .into());
        }
        let matches: Vec<_> = lock
            .file
            .iter()
            .filter(|file| {
                Path::new(&file.path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    == Some(meta.filename.as_str())
            })
            .collect();
        let [file] = matches.as_slice() else {
            return Err(format!(
                "{} did not identify exactly one locked file named {}",
                path.display(),
                meta.filename
            )
            .into());
        };
        if file.sha1 != meta.download.hash {
            return Err(format!("Packwiz hash did not match the pin for {}", file.path).into());
        }
        if !seen.insert(file.path.as_str()) {
            return Err(format!("Packwiz mapped {} more than once", file.path).into());
        }
        mappings.push(CurseForgeFile {
            path: file.path.clone(),
            sha1: file.sha1.clone(),
            project_id: meta.update.curseforge.project_id,
            file_id: meta.update.curseforge.file_id,
        });
    }
    Ok(mappings)
}

fn collect_metadata(dir: &Path, output: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.collect();
    entries.sort_by_key(|entry| {
        entry
            .as_ref()
            .map(|entry| entry.file_name())
            .unwrap_or_default()
    });
    for entry in entries {
        let path = entry?.path();
        if path.is_dir() {
            collect_metadata(&path, output)?;
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("toml")
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".pw.toml"))
        {
            output.push(path);
        }
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub minecraft: Minecraft,
    pub manifest_type: String,
    pub manifest_version: u32,
    pub name: String,
    pub version: String,
    pub author: String,
    pub files: Vec<ManifestFile>,
    pub overrides: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Minecraft {
    pub version: String,
    #[serde(rename = "modLoaders")]
    pub mod_loaders: Vec<ModLoader>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModLoader {
    pub id: String,
    pub primary: bool,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestFile {
    #[serde(rename = "projectID")]
    pub project_id: u32,
    #[serde(rename = "fileID")]
    pub file_id: u32,
    pub required: bool,
}

fn manifest_from_lock(
    lock: &Lockfile,
    author: &str,
    excluded: &BTreeSet<String>,
) -> Result<Manifest> {
    let mappings: BTreeMap<_, _> = lock
        .curseforge
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    let missing: Vec<_> = lock
        .file
        .iter()
        .filter(|file| {
            client_file(file)
                && !excluded.contains(&file.path)
                && !mappings.contains_key(file.path.as_str())
        })
        .map(|file| file.path.as_str())
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "CurseForge has no locked file for: {}; run `swatch install` with PACKWIZ_BIN set before publishing",
            missing.join(", ")
        )
        .into());
    }
    let mut files: Vec<_> = lock
        .file
        .iter()
        .filter(|file| client_file(file) && !excluded.contains(&file.path))
        .map(|file| {
            let mapped = mappings[&file.path.as_str()];
            ManifestFile {
                project_id: mapped.project_id,
                file_id: mapped.file_id,
                required: true,
            }
        })
        .collect();
    files.sort_by_key(|file| (file.project_id, file.file_id));
    Ok(Manifest {
        minecraft: Minecraft {
            version: lock.pack.minecraft.clone(),
            mod_loaders: vec![ModLoader {
                id: format!("{}-{}", lock.pack.loader, lock.pack.loader_version),
                primary: true,
            }],
        },
        manifest_type: "minecraftModpack".into(),
        manifest_version: CURSEFORGE_MANIFEST_VERSION,
        name: lock.pack.name.clone(),
        version: lock.pack.version.clone(),
        author: author.into(),
        files,
        overrides: "overrides".into(),
    })
}

pub(crate) fn export_from_lock(
    root: &PackRoot,
    author: &str,
    lock: &Lockfile,
    config: &Config,
) -> Result<PathBuf> {
    let excluded = validate_config(config, lock)?;
    let manifest = manifest_from_lock(lock, author, &excluded)?;
    let mut manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    manifest_bytes.push(b'\n');
    let name = format!("{}-{}-curseforge.zip", lock.pack.slug, lock.pack.version);
    fs::create_dir_all(root.dist_dir())?;
    let destination = root.dist_dir().join(&name);
    write_archive(root, &destination, &manifest_bytes)?;
    Ok(destination)
}

fn write_archive(root: &PackRoot, destination: &Path, manifest: &[u8]) -> Result<()> {
    let mut entries = BTreeMap::new();
    crate::archive::collect_tree(root.overrides_dir(), "overrides", &mut entries)?;
    crate::archive::collect_tree(root.client_overrides_dir(), "overrides", &mut entries)?;
    let file = File::create(destination)?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("manifest.json", options)?;
    zip.write_all(manifest)?;
    for (path, bytes) in entries {
        zip.start_file(path, options)?;
        zip.write_all(&bytes)?;
    }
    zip.finish()?;
    Ok(())
}

pub(crate) fn load_config(root: &PackRoot) -> Result<Config> {
    let path = root.path.join("overrides.toml");
    let text = fs::read_to_string(&path)
        .map_err(|error| crate::Error::from(format!("{}: {error}", path.display())))?;
    let overrides: Overrides = toml::from_str(&text)
        .map_err(|error| crate::Error::from(format!("overrides.toml: {error}")))?;
    Ok(overrides.curseforge)
}

fn toml_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{EnvSpec, FileSpec, Loader, PackMeta, SideRequirement};
    use std::io::Read;

    fn lock(mapped: bool) -> Lockfile {
        let file = FileSpec {
            id: "example".into(),
            requested_version: "1.0.0".into(),
            path: "mods/example.jar".into(),
            file_size: 1,
            sha1: "a".repeat(40),
            sha512: "b".repeat(128),
            env: EnvSpec {
                client: SideRequirement::Required,
                server: SideRequirement::Required,
            },
            downloads: vec!["https://example.invalid/example.jar".into()],
        };
        Lockfile {
            version: 2,
            pack: PackMeta {
                name: "Example Pack".into(),
                slug: "example-pack".into(),
                version: "1.2.0".into(),
                group: "com.example.packs".into(),
                minecraft: "26.2".into(),
                loader: Loader::Fabric,
                loader_version: "0.19.3".into(),
            },
            file: vec![file.clone()],
            curseforge: mapped
                .then_some(CurseForgeFile {
                    path: file.path,
                    sha1: file.sha1,
                    project_id: 123,
                    file_id: 456,
                })
                .into_iter()
                .collect(),
        }
    }

    fn no_exclusions() -> BTreeSet<String> {
        BTreeSet::new()
    }

    #[test]
    fn manifest_uses_locked_ids_and_loader() {
        let manifest =
            manifest_from_lock(&lock(true), "Example Author", &no_exclusions()).expect("manifest");
        assert_eq!(manifest.files[0].project_id, 123);
        assert_eq!(manifest.files[0].file_id, 456);
        assert_eq!(manifest.minecraft.mod_loaders[0].id, "fabric-0.19.3");
        let json = serde_json::to_value(manifest).expect("manifest JSON");
        assert_eq!(json["files"][0]["projectID"], 123);
        assert_eq!(json["files"][0]["fileID"], 456);
        assert!(json["files"][0].get("projectId").is_none());
    }

    #[test]
    fn manifest_rejects_an_unresolved_client_file() {
        let error = manifest_from_lock(&lock(false), "Example Author", &no_exclusions())
            .expect_err("unresolved mapping")
            .to_string();
        assert!(error.contains("mods/example.jar"));
    }

    #[test]
    fn manifest_omits_an_explicitly_excluded_file() {
        let excluded = BTreeSet::from(["mods/example.jar".to_string()]);
        let manifest = manifest_from_lock(&lock(false), "Example Author", &excluded)
            .expect("manifest with exclusion");
        assert!(manifest.files.is_empty());
    }

    #[test]
    fn config_rejects_a_stale_exclusion() {
        let config = Config {
            add: Vec::new(),
            exclude: vec![ExcludedFile {
                id: "missing".into(),
                reason: "Unavailable".into(),
            }],
        };
        let error = validate_config(&config, &lock(false))
            .expect_err("stale exclusion")
            .to_string();
        assert!(error.contains("missing"));
    }

    #[test]
    fn config_rejects_excluding_a_server_file() {
        let config = Config {
            add: Vec::new(),
            exclude: vec![ExcludedFile {
                id: "example".into(),
                reason: "Unavailable".into(),
            }],
        };
        let error = validate_config(&config, &lock(false))
            .expect_err("server file exclusion")
            .to_string();
        assert!(error.contains("client-only"));
    }

    #[test]
    fn packwiz_metadata_uses_pack_identity_and_selected_loader() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let mut pack = lock(true);
        pack.pack.name = "Other Pack".into();
        pack.pack.loader = Loader::NeoForge;
        pack.pack.loader_version = "21.0.0".into();
        initialise_packwiz(temp.path(), &pack).expect("Packwiz metadata");

        let metadata = fs::read_to_string(temp.path().join("pack.toml")).expect("pack.toml");
        assert!(metadata.contains("name = \"Other Pack\""));
        assert!(metadata.contains("author = \"Swatch\""));
        assert!(metadata.contains("neoforge = \"21.0.0\""));
        assert!(!metadata.contains("fabric ="));
    }

    #[test]
    fn every_loader_has_a_canonical_packwiz_name() {
        for (loader, wire) in [
            (Loader::Fabric, "fabric"),
            (Loader::Forge, "forge"),
            (Loader::NeoForge, "neoforge"),
        ] {
            let temp = tempfile::tempdir().expect("temporary directory");
            let mut pack = lock(true);
            pack.pack.loader = loader;
            initialise_packwiz(temp.path(), &pack).expect("Packwiz metadata");
            let metadata = fs::read_to_string(temp.path().join("pack.toml")).expect("pack.toml");
            assert!(metadata.contains(&format!("{wire} = \"0.19.3\"")));
        }
    }

    #[test]
    fn unresolved_mappings_do_not_produce_a_candidate_lock() {
        let error = finish_mappings(lock(false), Vec::new(), &no_exclusions())
            .expect_err("unresolved mapping")
            .to_string();
        assert!(error.contains("mods/example.jar"));
    }

    #[test]
    fn missing_packwiz_error_explains_how_to_configure_it() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let error = run_packwiz(
            std::ffi::OsStr::new("packwiz-does-not-exist"),
            temp.path(),
            &[],
        )
        .expect_err("missing Packwiz");
        let message = error.to_string();
        assert!(message.contains("Packwiz"));
        assert!(message.contains("PACKWIZ_BIN"));
    }

    #[test]
    fn archive_merges_only_common_and_client_overrides() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = PackRoot {
            path: temp.path().into(),
        };
        fs::create_dir_all(root.overrides_dir().join("config")).expect("common overrides");
        fs::create_dir_all(root.client_overrides_dir()).expect("client overrides");
        fs::create_dir_all(root.server_overrides_dir()).expect("server overrides");
        fs::write(root.overrides_dir().join("config/common.txt"), b"common").expect("common file");
        fs::write(root.client_overrides_dir().join("client.txt"), b"client").expect("client file");
        fs::write(root.server_overrides_dir().join("server.txt"), b"server").expect("server file");
        let manifest =
            manifest_from_lock(&lock(true), "Example Author", &no_exclusions()).expect("manifest");
        let mut manifest_bytes = serde_json::to_vec_pretty(&manifest).expect("manifest JSON");
        manifest_bytes.push(b'\n');
        let destination = temp.path().join("pack.zip");
        write_archive(&root, &destination, &manifest_bytes).expect("archive");

        let file = File::open(destination).expect("archive file");
        let mut zip = zip::ZipArchive::new(file).expect("zip");
        let names: Vec<_> = (0..zip.len())
            .map(|index| zip.by_index(index).expect("entry").name().to_string())
            .collect();
        assert_eq!(
            names,
            [
                "manifest.json",
                "overrides/client.txt",
                "overrides/config/common.txt"
            ]
        );
        let mut manifest_json = String::new();
        zip.by_name("manifest.json")
            .expect("manifest.json")
            .read_to_string(&mut manifest_json)
            .expect("manifest JSON");
        let manifest_json: serde_json::Value =
            serde_json::from_str(&manifest_json).expect("parsed manifest JSON");
        assert_eq!(manifest_json["manifestVersion"], 1);
    }
}
