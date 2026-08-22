use crate::spec::{
    FileSpec, Loader, Lockfile, SideRequirement, check_pack_path, client_file, server_file,
};
use crate::{PackRoot, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use zip::ZipWriter;

const MODRINTH_FORMAT_VERSION: u32 = 1;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildSide {
    Client,
    Server,
}

impl BuildSide {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Server => "server",
        }
    }

    fn accepts(self, file: &FileSpec) -> bool {
        match self {
            Self::Client => client_file(file),
            Self::Server => server_file(file),
        }
    }
}

pub(crate) fn export_from_lock(
    root: &PackRoot,
    lock: &Lockfile,
    side: BuildSide,
) -> Result<std::path::PathBuf> {
    export_from_lock_to(root, lock, side, &root.dist_dir())
}

pub(crate) fn export_from_lock_to(
    root: &PackRoot,
    lock: &Lockfile,
    side: BuildSide,
    output_dir: &Path,
) -> Result<std::path::PathBuf> {
    crate::authored::verify(root, &lock.authored)?;
    fs::create_dir_all(output_dir)?;
    let index = index_from_lock(lock, side)?;
    let index_bytes = serde_json::to_vec_pretty(&index)?;
    let mut index_bytes = index_bytes;
    if !index_bytes.ends_with(b"\n") {
        index_bytes.push(b'\n');
    }
    let name = format!(
        "{}-{}-{}.mrpack",
        lock.pack.slug,
        lock.pack.version,
        side.as_str()
    );
    let dest = output_dir.join(&name);
    write_mrpack(&dest, &index_bytes, root, side)?;
    crate::authored::verify(root, &lock.authored)?;
    Ok(dest)
}

pub fn index_from_lock(lock: &Lockfile, side: BuildSide) -> Result<MrpackIndex> {
    let mut files = Vec::new();
    for file in lock.file.iter().filter(|file| side.accepts(file)) {
        files.push(mrpack_file(file)?);
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    let mut dependencies = BTreeMap::new();
    dependencies.insert("minecraft".to_string(), lock.pack.minecraft.clone());
    dependencies.insert(
        loader_dependency_key(lock.pack.loader).to_string(),
        lock.pack.loader_version.clone(),
    );
    Ok(MrpackIndex {
        format_version: MODRINTH_FORMAT_VERSION,
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

fn loader_dependency_key(loader: Loader) -> &'static str {
    match loader {
        Loader::Fabric => "fabric-loader",
        Loader::Forge => "forge",
        Loader::NeoForge => "neoforge",
    }
}

fn write_mrpack(dest: &Path, index_bytes: &[u8], root: &PackRoot, side: BuildSide) -> Result<()> {
    let mut entries = BTreeMap::new();
    crate::archive::collect_tree(root.overrides_dir(), "overrides", &mut entries)?;
    match side {
        BuildSide::Client => crate::archive::collect_tree(
            root.client_overrides_dir(),
            "client-overrides",
            &mut entries,
        )?,
        BuildSide::Server => crate::archive::collect_tree(
            root.server_overrides_dir(),
            "server-overrides",
            &mut entries,
        )?,
    }
    let file = File::create(dest)?;
    let mut zip = ZipWriter::new(file);
    let options = crate::archive::zip_options()?;
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
    use crate::spec::{EnvSpec, FileSpec, Loader, PackMeta, SideRequirement};
    use std::io::Read;

    #[test]
    fn index_omits_nothing_and_sorts() {
        let lock = Lockfile {
            version: 1,
            pack: PackMeta {
                name: "Example Pack".into(),
                slug: "example-pack".into(),
                version: "1.1.1".into(),
                group: "org.example.packs".into(),
                minecraft: "26.2".into(),
                loader: Loader::Fabric,
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
            authored: Vec::new(),
            curseforge: Vec::new(),
        };
        let index = index_from_lock(&lock, BuildSide::Client).expect("index");
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

        let temp = tempfile::tempdir().expect("temporary directory");
        let root = PackRoot {
            path: temp.path().into(),
        };
        fs::write(root.lock_toml(), lock.to_toml().expect("lock TOML")).expect("lockfile");
        let archive = export_from_lock(&root, &lock, BuildSide::Client).expect("mrpack");
        let file = File::open(archive).expect("mrpack file");
        let mut zip = zip::ZipArchive::new(file).expect("mrpack zip");
        let mut index_json = String::new();
        zip.by_name("modrinth.index.json")
            .expect("modrinth.index.json")
            .read_to_string(&mut index_json)
            .expect("index JSON");
        let index_json: serde_json::Value =
            serde_json::from_str(&index_json).expect("parsed index JSON");
        assert_eq!(index_json["formatVersion"], 1);
    }

    #[test]
    fn maps_every_loader_to_its_modrinth_dependency() {
        assert_eq!(loader_dependency_key(Loader::Fabric), "fabric-loader");
        assert_eq!(loader_dependency_key(Loader::Forge), "forge");
        assert_eq!(loader_dependency_key(Loader::NeoForge), "neoforge");
    }

    #[test]
    fn archives_are_deterministic_and_side_specific() {
        let directory = tempfile::tempdir().expect("temporary pack");
        let root = PackRoot {
            path: directory.path().into(),
        };
        fs::create_dir_all(root.overrides_dir().join("config")).expect("shared root");
        fs::create_dir_all(root.client_overrides_dir()).expect("client root");
        fs::create_dir_all(root.server_overrides_dir()).expect("server root");
        fs::write(root.overrides_dir().join("config/shared.json"), b"shared\n")
            .expect("shared file");
        fs::write(root.client_overrides_dir().join("client.txt"), b"client\n")
            .expect("client file");
        fs::write(root.server_overrides_dir().join("server.txt"), b"server\n")
            .expect("server file");

        let mut lock = Lockfile::new(
            PackMeta {
                name: "Example Pack".into(),
                slug: "example-pack".into(),
                version: "1.0.0".into(),
                group: "org.example.packs".into(),
                minecraft: "26.2".into(),
                loader: Loader::Fabric,
                loader_version: "0.19.3".into(),
            },
            vec![
                FileSpec {
                    id: "client".into(),
                    requested_version: "1".into(),
                    path: "mods/client.jar".into(),
                    file_size: 0,
                    sha1: "a".repeat(40),
                    sha512: "b".repeat(128),
                    env: crate::spec::ContentPlacement::ClientMod.env(),
                    downloads: vec!["https://example.invalid/client.jar".into()],
                },
                FileSpec {
                    id: "server".into(),
                    requested_version: "1".into(),
                    path: "mods/server.jar".into(),
                    file_size: 0,
                    sha1: "c".repeat(40),
                    sha512: "d".repeat(128),
                    env: crate::spec::ContentPlacement::ServerMod.env(),
                    downloads: vec!["https://example.invalid/server.jar".into()],
                },
            ],
        );
        lock.set_authored(crate::authored::scan(&root).expect("authored pins"));

        let first = export_from_lock(&root, &lock, BuildSide::Client).expect("first client");
        let first_bytes = fs::read(&first).expect("first bytes");
        fs::remove_file(&first).expect("remove first archive");
        fs::write(root.client_overrides_dir().join("client.txt"), b"client\n")
            .expect("rewrite client file");
        let second = export_from_lock(&root, &lock, BuildSide::Client).expect("second client");
        assert_eq!(first_bytes, fs::read(second).expect("second bytes"));

        let mut client =
            zip::ZipArchive::new(std::io::Cursor::new(first_bytes)).expect("client archive");
        let client_names: Vec<_> = (0..client.len())
            .map(|index| client.by_index(index).expect("entry").name().to_string())
            .collect();
        assert_eq!(
            client_names,
            [
                "modrinth.index.json",
                "client-overrides/client.txt",
                "overrides/config/shared.json",
            ]
        );
        for index in 0..client.len() {
            assert_eq!(
                client
                    .by_index(index)
                    .expect("timestamped entry")
                    .last_modified(),
                Some(zip::DateTime::default())
            );
        }

        let first = export_from_lock(&root, &lock, BuildSide::Server).expect("first server");
        let first_bytes = fs::read(&first).expect("first server bytes");
        fs::remove_file(&first).expect("remove first server archive");
        fs::write(root.server_overrides_dir().join("server.txt"), b"server\n")
            .expect("rewrite server file");
        let second = export_from_lock(&root, &lock, BuildSide::Server).expect("second server");
        assert_eq!(first_bytes, fs::read(second).expect("second server bytes"));

        let mut server =
            zip::ZipArchive::new(std::io::Cursor::new(first_bytes)).expect("server zip");
        let server_names: Vec<_> = (0..server.len())
            .map(|index| server.by_index(index).expect("entry").name().to_string())
            .collect();
        assert_eq!(
            server_names,
            [
                "modrinth.index.json",
                "overrides/config/shared.json",
                "server-overrides/server.txt",
            ]
        );
        let mut server_index = String::new();
        server
            .by_name("modrinth.index.json")
            .expect("server index")
            .read_to_string(&mut server_index)
            .expect("server index bytes");
        assert!(server_index.contains("mods/server.jar"));
        assert!(!server_index.contains("mods/client.jar"));
    }
}
