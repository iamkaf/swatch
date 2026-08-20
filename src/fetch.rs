use crate::spec::FileSpec;
use crate::{PackRoot, Result, USER_AGENT, hash};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub fn cached_file(root: &PackRoot, file: &FileSpec) -> PathBuf {
    root.cache_dir().join("objects").join(&file.sha512)
}

pub struct VerifiedFiles {
    paths: BTreeMap<String, PathBuf>,
}

impl VerifiedFiles {
    pub fn path(&self, file: &FileSpec) -> Result<&Path> {
        self.paths
            .get(&file.path)
            .map(PathBuf::as_path)
            .ok_or_else(|| format!("{} was not verified", file.path).into())
    }
}

pub fn ensure_all(root: &PackRoot, files: &[FileSpec]) -> Result<VerifiedFiles> {
    let mut client = None;
    let mut objects: BTreeMap<String, (u64, String, PathBuf)> = BTreeMap::new();
    let mut paths = BTreeMap::new();
    for file in files {
        file.validate()?;
        if let Some((file_size, sha1, path)) = objects.get(&file.sha512) {
            if *file_size != file.file_size || sha1 != &file.sha1 {
                return Err(format!(
                    "{} shares a sha512 pin with conflicting size or sha1 pins",
                    file.path
                )
                .into());
            }
            paths.insert(file.path.clone(), path.clone());
            continue;
        }
        let dest = cached_file(root, file);
        if !migrate_legacy_object(file, &dest)? {
            if dest.is_file() {
                verify_bytes(file, &fs::read(&dest)?)?;
            } else {
                let client = match &client {
                    Some(client) => client,
                    None => client.insert(http_client()?),
                };
                let bytes = download(client, file)?;
                cache_bytes(root, file, &bytes)?;
            }
        }
        objects.insert(
            file.sha512.clone(),
            (file.file_size, file.sha1.clone(), dest.clone()),
        );
        paths.insert(file.path.clone(), dest);
    }
    Ok(VerifiedFiles { paths })
}

fn migrate_legacy_object(file: &FileSpec, dest: &Path) -> Result<bool> {
    if !dest.is_dir() {
        return Ok(false);
    }

    let Some(bytes) = legacy_object_bytes(file, dest)? else {
        fs::remove_dir_all(dest)?;
        return Ok(false);
    };
    let parent = dest
        .parent()
        .ok_or_else(|| crate::Error::from("cache object has no parent directory"))?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(&bytes)?;

    let backup = tempfile::Builder::new()
        .prefix(".swatch-legacy-")
        .tempdir_in(parent)?;
    let legacy = backup.path().join("object");
    fs::rename(dest, &legacy)?;
    if let Err(error) = staged.persist(dest) {
        let persist_error = error.error;
        if let Err(restore_error) = fs::rename(&legacy, dest) {
            return Err(format!(
                "could not migrate {}: {persist_error}; could not restore its legacy cache directory: {restore_error}",
                file.path
            )
            .into());
        }
        return Err(persist_error.into());
    }
    Ok(true)
}

fn legacy_object_bytes(file: &FileSpec, directory: &Path) -> Result<Option<Vec<u8>>> {
    let mut entries: Vec<_> = fs::read_dir(directory)?.collect();
    entries.sort_by_key(|entry| {
        entry
            .as_ref()
            .map(|entry| entry.file_name())
            .unwrap_or_default()
    });
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let bytes = fs::read(entry.path())?;
        if verify_bytes(file, &bytes).is_ok() {
            return Ok(Some(bytes));
        }
    }
    Ok(None)
}

fn cache_bytes(root: &PackRoot, file: &FileSpec, bytes: &[u8]) -> Result<PathBuf> {
    verify_bytes(file, bytes)?;
    let dest = cached_file(root, file);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = dest.with_extension("tmp");
    {
        let mut out = fs::File::create(&tmp)?;
        out.write_all(bytes)?;
    }
    fs::rename(tmp, &dest)?;
    Ok(dest)
}

fn download(client: &reqwest::blocking::Client, file: &FileSpec) -> Result<Vec<u8>> {
    let [url] = file.downloads.as_slice() else {
        return Err(format!("{} must have one download", file.path).into());
    };
    let response = client.get(url).send()?.error_for_status()?;
    Ok(response.bytes()?.to_vec())
}

fn http_client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(180))
        .build()?)
}

fn verify_bytes(file: &FileSpec, bytes: &[u8]) -> Result<()> {
    if bytes.len() as u64 != file.file_size {
        return Err(format!(
            "{} size {} did not match pin {}",
            file.path,
            bytes.len(),
            file.file_size
        )
        .into());
    }
    let sha1 = hash::sha1_hex(bytes);
    if sha1 != file.sha1 {
        return Err(format!("{} sha1 {sha1} did not match pin {}", file.path, file.sha1).into());
    }
    let sha512 = hash::sha512_hex(bytes);
    if sha512 != file.sha512 {
        return Err(format!("{} sha512 did not match pin {}", file.path, file.sha512).into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{EnvSpec, SideRequirement};

    fn file(path: &str) -> FileSpec {
        FileSpec {
            id: "example".into(),
            requested_version: "1.0.0".into(),
            path: path.into(),
            file_size: 0,
            sha1: hash::sha1_hex(&[]),
            sha512: hash::sha512_hex(&[]),
            env: EnvSpec {
                client: SideRequirement::Required,
                server: SideRequirement::Required,
            },
            downloads: vec!["https://example.invalid/example.jar".into()],
        }
    }

    #[test]
    fn object_identity_depends_only_on_sha512() {
        let root = PackRoot {
            path: PathBuf::from("pack"),
        };
        assert_eq!(
            cached_file(&root, &file("mods/one.jar")),
            cached_file(&root, &file("mods/two.jar"))
        );
    }

    #[test]
    fn verifies_one_object_for_multiple_pack_paths() {
        let directory = tempfile::tempdir().expect("temporary pack");
        let root = PackRoot {
            path: directory.path().to_path_buf(),
        };
        let first = file("mods/one.jar");
        let mut second = file("mods/two.jar");
        second.id = "other".into();
        let object = cached_file(&root, &first);
        fs::create_dir_all(object.parent().expect("object directory")).expect("object directory");
        fs::write(&object, []).expect("cached object");

        let verified = ensure_all(&root, &[first.clone(), second.clone()]).expect("verification");
        assert_eq!(verified.path(&first).expect("first object"), object);
        assert_eq!(verified.path(&second).expect("second object"), object);
    }

    #[test]
    fn rejects_a_corrupt_cached_object() {
        let directory = tempfile::tempdir().expect("temporary pack");
        let root = PackRoot {
            path: directory.path().to_path_buf(),
        };
        let file = file("mods/example.jar");
        let object = cached_file(&root, &file);
        fs::create_dir_all(object.parent().expect("object directory")).expect("object directory");
        fs::write(object, b"corrupt").expect("cached object");
        assert!(ensure_all(&root, &[file]).is_err());
    }

    #[test]
    fn migrates_a_verified_legacy_object_without_downloading() {
        let directory = tempfile::tempdir().expect("temporary pack");
        let root = PackRoot {
            path: directory.path().to_path_buf(),
        };
        let file = file("mods/example.jar");
        let object = cached_file(&root, &file);
        let legacy = object.join("example.jar");
        fs::create_dir_all(&object).expect("legacy object directory");
        fs::write(&legacy, []).expect("legacy cached object");

        let verified = ensure_all(&root, std::slice::from_ref(&file)).expect("migration");

        assert_eq!(verified.path(&file).expect("verified object"), object);
        assert!(object.is_file());
        assert!(!legacy.exists());
        assert_eq!(fs::read(object).expect("migrated bytes"), b"");
    }

    #[test]
    fn invalid_legacy_cache_does_not_block_a_fresh_object() {
        let directory = tempfile::tempdir().expect("temporary pack");
        let root = PackRoot {
            path: directory.path().to_path_buf(),
        };
        let file = file("mods/example.jar");
        let object = cached_file(&root, &file);
        fs::create_dir_all(&object).expect("legacy object directory");
        fs::write(object.join("interrupted.jar"), b"corrupt").expect("interrupted object");

        assert!(!migrate_legacy_object(&file, &object).expect("discard invalid legacy cache"));
        assert!(!object.exists());
        cache_bytes(&root, &file, &[]).expect("fresh cached object");
        assert!(object.is_file());
    }
}
