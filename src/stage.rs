use crate::spec::{AuthoredRoot, Lockfile};
use crate::{BuildSide, PackRoot, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn stage_from_lock(
    root: &PackRoot,
    lock: &Lockfile,
    side: BuildSide,
) -> Result<PathBuf> {
    crate::authored::verify(root, &lock.authored)?;
    let files: Vec<_> = lock.file.iter().filter(|file| side.accepts(file)).collect();
    let verified = crate::fetch::verify_cached(root, &files)?;

    let stage_root = prepare_stage_root(root)?;
    let temporary = tempfile::Builder::new()
        .prefix(&format!(".{}-", side.as_str()))
        .tempdir_in(&stage_root)?;

    for file in files {
        let bytes = fs::read(verified.path(file)?)?;
        crate::fetch::verify_bytes(file, &bytes)?;
        write_file(&temporary.path().join(&file.path), &bytes)?;
    }
    for file in lock
        .authored
        .iter()
        .filter(|file| side.accepts_authored(file.root))
    {
        let source = root
            .path
            .join(crate::authored::root_name(file.root))
            .join(&file.path);
        let bytes = fs::read(source)?;
        if bytes.len() as u64 != file.file_size
            || crate::hash::sha1_hex(&bytes) != file.sha1
            || crate::hash::sha512_hex(&bytes) != file.sha512
        {
            return Err(format!(
                "authored file changed while staging: {}/{}; run `swatch install` after reviewing the changes",
                crate::authored::root_name(file.root),
                file.path
            )
            .into());
        }
        write_file(&temporary.path().join(&file.path), &bytes)?;
    }
    crate::authored::verify(root, &lock.authored)?;

    let destination = stage_root.join(side.as_str());
    remove_existing(&destination)?;
    fs::rename(temporary.path(), &destination)?;
    Ok(destination)
}

fn prepare_stage_root(root: &PackRoot) -> Result<PathBuf> {
    let generated = root.generated_dir();
    ensure_directory(&generated, "generated output root")?;
    let stage = generated.join("stage");
    ensure_directory(&stage, "stage output root")?;
    Ok(stage)
}

fn ensure_directory(path: &Path, name: &str) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => return validate_directory(path, name, &metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(path)?;
    validate_directory(path, name, &metadata)
}

fn validate_directory(path: &Path, name: &str, metadata: &fs::Metadata) -> Result<()> {
    if metadata.file_type().is_symlink() {
        return Err(format!("{name} cannot be a symbolic link: {}", path.display()).into());
    }
    if !metadata.is_dir() {
        return Err(format!("{name} is not a directory: {}", path.display()).into());
    }
    Ok(())
}

fn write_file(destination: &Path, bytes: &[u8]) -> Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| crate::Error::from("staged file has no parent directory"))?;
    fs::create_dir_all(parent)?;
    fs::write(destination, bytes)?;
    Ok(())
}

fn remove_existing(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path)?;
        }
        Ok(_) => fs::remove_file(path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

impl BuildSide {
    fn accepts_authored(self, root: AuthoredRoot) -> bool {
        match self {
            Self::Client => matches!(root, AuthoredRoot::Shared | AuthoredRoot::Client),
            Self::Server => matches!(root, AuthoredRoot::Shared | AuthoredRoot::Server),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash;
    use crate::spec::{AuthoredFile, ContentPlacement, FileSpec, Loader, PackMeta};

    #[test]
    fn stages_locked_files_and_the_matching_authored_roots() {
        let directory = tempfile::tempdir().expect("temporary pack");
        let root = PackRoot {
            path: directory.path().into(),
        };
        let shared = locked_file(
            "shared",
            "mods/shared.jar",
            b"shared",
            ContentPlacement::SharedMod,
        );
        let client = locked_file(
            "client",
            "mods/client.jar",
            b"client",
            ContentPlacement::ClientMod,
        );
        let server = locked_file(
            "server",
            "mods/server.jar",
            b"server",
            ContentPlacement::ServerMod,
        );
        cache(&root, &shared, b"shared");
        cache(&root, &client, b"client");
        cache(&root, &server, b"server");
        authored(
            &root,
            AuthoredRoot::Shared,
            "config/shared.txt",
            b"shared config",
        );
        authored(
            &root,
            AuthoredRoot::Client,
            "config/client.txt",
            b"client config",
        );
        authored(
            &root,
            AuthoredRoot::Server,
            "config/server.txt",
            b"server config",
        );
        let lock = lock(
            vec![shared, client, server],
            crate::authored::scan(&root).expect("authored"),
        );

        let client_stage = stage_from_lock(&root, &lock, BuildSide::Client).expect("client stage");
        assert_eq!(
            fs::read(client_stage.join("mods/shared.jar")).expect("shared"),
            b"shared"
        );
        assert_eq!(
            fs::read(client_stage.join("mods/client.jar")).expect("client"),
            b"client"
        );
        assert!(
            client_stage
                .join("mods/server.jar")
                .try_exists()
                .is_ok_and(|exists| !exists)
        );
        assert!(client_stage.join("config/shared.txt").is_file());
        assert!(client_stage.join("config/client.txt").is_file());
        assert!(!client_stage.join("config/server.txt").exists());

        let server_stage = stage_from_lock(&root, &lock, BuildSide::Server).expect("server stage");
        assert!(server_stage.join("mods/shared.jar").is_file());
        assert!(server_stage.join("mods/server.jar").is_file());
        assert!(!server_stage.join("mods/client.jar").exists());
        assert!(server_stage.join("config/shared.txt").is_file());
        assert!(server_stage.join("config/server.txt").is_file());
        assert!(!server_stage.join("config/client.txt").exists());
    }

    #[test]
    fn rejects_missing_and_corrupt_cache_objects_without_replacing_the_stage() {
        let directory = tempfile::tempdir().expect("temporary pack");
        let root = PackRoot {
            path: directory.path().into(),
        };
        let file = locked_file(
            "client",
            "mods/client.jar",
            b"client",
            ContentPlacement::ClientMod,
        );
        let lock = lock(vec![file.clone()], Vec::new());
        let existing = root.generated_dir().join("stage/client/old.txt");
        fs::create_dir_all(existing.parent().expect("stage directory")).expect("stage directory");
        fs::write(&existing, b"old").expect("old stage");

        let missing = stage_from_lock(&root, &lock, BuildSide::Client)
            .expect_err("missing object")
            .to_string();
        assert!(missing.contains("missing cached object"));
        assert!(existing.is_file());

        cache(&root, &file, b"corrupt");
        let corrupt = stage_from_lock(&root, &lock, BuildSide::Client)
            .expect_err("corrupt object")
            .to_string();
        assert!(corrupt.contains("did not match pin"));
        assert!(existing.is_file());
    }

    #[test]
    fn replaces_the_side_tree_to_prune_stale_files() {
        let directory = tempfile::tempdir().expect("temporary pack");
        let root = PackRoot {
            path: directory.path().into(),
        };
        let file = locked_file(
            "client",
            "mods/client.jar",
            b"client",
            ContentPlacement::ClientMod,
        );
        cache(&root, &file, b"client");
        let stale = root.generated_dir().join("stage/client/mods/stale.jar");
        fs::create_dir_all(stale.parent().expect("stage directory")).expect("stage directory");
        fs::write(&stale, b"stale").expect("stale file");

        let output = stage_from_lock(&root, &lock(vec![file], Vec::new()), BuildSide::Client)
            .expect("stage");

        assert!(output.join("mods/client.jar").is_file());
        assert!(!stale.exists());
    }

    #[test]
    fn rejects_manifest_drift_before_reading_the_cache() {
        let directory = tempfile::tempdir().expect("temporary pack");
        let root = PackRoot {
            path: directory.path().into(),
        };
        let file = locked_file(
            "client",
            "mods/client.jar",
            b"client",
            ContentPlacement::ClientMod,
        );
        fs::write(
            root.pack_toml(),
            r#"format = 1

[pack]
name = "Example Pack"
slug = "example-pack"
version = "1.0.0"
group = "org.example.packs"
minecraft = "26.2"
loader = "fabric"
loader_version = "0.19.3"

[client_mods]
client = "2.0.0"
"#,
        )
        .expect("manifest");
        fs::write(
            root.lock_toml(),
            lock(vec![file], Vec::new()).to_toml().expect("lock TOML"),
        )
        .expect("lockfile");

        let error = crate::stage(&root, BuildSide::Client)
            .expect_err("manifest drift")
            .to_string();

        assert!(error.contains("pack.toml changed since the last install"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_symlinked_generated_root_without_touching_its_target() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temporary pack");
        let outside = tempfile::tempdir().expect("outside directory");
        let root = PackRoot {
            path: directory.path().into(),
        };
        let file = locked_file(
            "client",
            "mods/client.jar",
            b"client",
            ContentPlacement::ClientMod,
        );
        cache(&root, &file, b"client");
        let victim = outside.path().join("stage/client/victim.txt");
        fs::create_dir_all(victim.parent().expect("victim directory")).expect("victim directory");
        fs::write(&victim, b"keep me").expect("victim file");
        symlink(outside.path(), root.generated_dir()).expect("generated symlink");

        let error = stage_from_lock(&root, &lock(vec![file], Vec::new()), BuildSide::Client)
            .expect_err("symlinked generated root")
            .to_string();

        assert!(error.contains("generated output root cannot be a symbolic link"));
        assert_eq!(fs::read(victim).expect("preserved victim"), b"keep me");
    }

    fn locked_file(id: &str, path: &str, bytes: &[u8], placement: ContentPlacement) -> FileSpec {
        FileSpec {
            id: id.into(),
            requested_version: "1.0.0".into(),
            path: path.into(),
            file_size: bytes.len() as u64,
            sha1: hash::sha1_hex(bytes),
            sha512: hash::sha512_hex(bytes),
            env: placement.env(),
            downloads: vec![format!("https://example.invalid/{id}.jar")],
        }
    }

    fn lock(file: Vec<FileSpec>, authored: Vec<AuthoredFile>) -> Lockfile {
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
            file,
        );
        lock.set_authored(authored);
        lock
    }

    fn cache(root: &PackRoot, file: &FileSpec, bytes: &[u8]) {
        let path = crate::fetch::cached_file(root, file);
        fs::create_dir_all(path.parent().expect("object directory")).expect("object directory");
        fs::write(path, bytes).expect("cache object");
    }

    fn authored(root: &PackRoot, kind: AuthoredRoot, path: &str, bytes: &[u8]) {
        let source = root.path.join(crate::authored::root_name(kind)).join(path);
        fs::create_dir_all(source.parent().expect("authored directory"))
            .expect("authored directory");
        fs::write(source, bytes).expect("authored file");
    }
}
