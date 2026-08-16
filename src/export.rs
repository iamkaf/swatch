use crate::spec::{FileSpec, Lockfile, SideRequirement, check_pack_path};
use crate::{PackRoot, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

#[derive(Debug, Serialize)]
pub struct MrpackIndex {
    #[serde(rename = "formatVersion")]
    pub format_version: u32,
    pub game: String,
    #[serde(rename = "versionId")]
    pub version_id: String,
    pub name: String,
    pub files: Vec<MrpackFile>,
    pub dependencies: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct MrpackFile {
    pub path: String,
    pub hashes: BTreeMap<String, String>,
    pub env: MrpackEnv,
    pub downloads: Vec<String>,
    #[serde(rename = "fileSize")]
    pub file_size: u64,
}

#[derive(Debug, Serialize, Clone)]
pub struct MrpackEnv {
    pub client: SideRequirement,
    pub server: SideRequirement,
}

pub fn export(root: &PackRoot) -> Result<std::path::PathBuf> {
    let lock = crate::load_lock(root)?;
    fs::create_dir_all(root.dist_dir())?;
    let index = index_from_lock(&lock)?;
    let index_bytes = serde_json::to_vec_pretty(&index)?;
    let mut index_bytes = index_bytes;
    if !index_bytes.ends_with(b"\n") {
        index_bytes.push(b'\n');
    }
    let name = format!("{}-{}.mrpack", lock.pack.slug, lock.pack.version);
    let dest = root.dist_dir().join(&name);
    write_mrpack(&dest, &index_bytes, root)?;
    Ok(dest)
}

pub fn index_from_lock(lock: &Lockfile) -> Result<MrpackIndex> {
    let mut files = Vec::new();
    for file in &lock.file {
        files.push(mrpack_file(file)?);
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    let mut dependencies = BTreeMap::new();
    dependencies.insert("minecraft".to_string(), lock.pack.minecraft.clone());
    dependencies.insert(
        loader_dependency_key(&lock.pack.loader)?.to_string(),
        lock.pack.loader_version.clone(),
    );
    Ok(MrpackIndex {
        format_version: 2,
        game: "minecraft".to_string(),
        version_id: lock.pack.version.clone(),
        name: lock.pack.name.clone(),
        files,
        dependencies,
    })
}

fn mrpack_file(file: &FileSpec) -> Result<MrpackFile> {
    check_pack_path(&file.path)?;
    let mut hashes = BTreeMap::new();
    hashes.insert("sha1".to_string(), file.sha1.clone());
    hashes.insert("sha512".to_string(), file.sha512.clone());
    Ok(MrpackFile {
        path: file.path.clone(),
        hashes,
        env: MrpackEnv {
            client: file.env.client,
            server: file.env.server,
        },
        downloads: file.downloads.clone(),
        file_size: file.file_size,
    })
}

fn loader_dependency_key(loader: &str) -> Result<&'static str> {
    match loader {
        "fabric" => Ok("fabric-loader"),
        "forge" => Ok("forge"),
        "neoforge" => Ok("neoforge"),
        other => Err(format!("unsupported loader `{other}`").into()),
    }
}

fn write_mrpack(dest: &Path, index_bytes: &[u8], root: &PackRoot) -> Result<()> {
    let mut entries = BTreeMap::new();
    crate::archive::collect_tree(root.overrides_dir(), "overrides", &mut entries)?;
    crate::archive::collect_tree(
        root.client_overrides_dir(),
        "client-overrides",
        &mut entries,
    )?;
    crate::archive::collect_tree(
        root.server_overrides_dir(),
        "server-overrides",
        &mut entries,
    )?;
    let file = File::create(dest)?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("modrinth.index.json", options)?;
    zip.write_all(index_bytes)?;
    for (path, bytes) in entries {
        zip.start_file(path, options)?;
        zip.write_all(&bytes)?;
    }
    zip.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{EnvSpec, FileSpec, PackMeta, SideRequirement};

    #[test]
    fn index_omits_nothing_and_sorts() {
        let lock = Lockfile {
            version: 2,
            pack: PackMeta {
                name: "Example Pack".into(),
                slug: "example-pack".into(),
                version: "1.1.1".into(),
                group: "org.example.packs".into(),
                minecraft: "26.2".into(),
                loader: "fabric".into(),
                loader_version: "0.19.3".into(),
            },
            file: vec![
                FileSpec {
                    id: "b".into(),
                    requested_version: "1.0.0".into(),
                    path: "mods/b.jar".into(),
                    file_size: 1,
                    sha1: "a".repeat(40),
                    sha512: "b".repeat(128),
                    env: EnvSpec {
                        client: SideRequirement::Required,
                        server: SideRequirement::Unsupported,
                    },
                    downloads: vec!["https://cdn.modrinth.com/b.jar".into()],
                },
                FileSpec {
                    id: "a".into(),
                    requested_version: "1.0.0".into(),
                    path: "mods/a.jar".into(),
                    file_size: 1,
                    sha1: "a".repeat(40),
                    sha512: "b".repeat(128),
                    env: EnvSpec {
                        client: SideRequirement::Required,
                        server: SideRequirement::Required,
                    },
                    downloads: vec!["https://cdn.modrinth.com/a.jar".into()],
                },
            ],
            curseforge: Vec::new(),
        };
        let index = index_from_lock(&lock).expect("index");
        assert_eq!(index.files[0].path, "mods/a.jar");
        assert_eq!(index.files[1].env.server, SideRequirement::Unsupported);
        assert_eq!(
            index.dependencies.get("minecraft").map(String::as_str),
            Some("26.2")
        );
        assert_eq!(
            index.dependencies.get("fabric-loader").map(String::as_str),
            Some("0.19.3")
        );
    }
}
