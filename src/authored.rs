use crate::hash;
use crate::spec::{AuthoredFile, AuthoredRoot, Lockfile, check_pack_path};
use crate::{BuildSide, PackRoot, Result};
use std::fs;
use std::path::Path;

pub fn scan(root: &PackRoot) -> Result<Vec<AuthoredFile>> {
    let mut files = Vec::new();
    for kind in AuthoredRoot::ALL {
        let path = root.authored_dir(kind);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "authored root cannot be a symbolic link: {}",
                    path.display()
                )
                .into());
            }
            Ok(metadata) if metadata.is_dir() => scan_directory(&path, &path, kind, &mut files)?,
            Ok(_) => {
                return Err(format!("authored root is not a directory: {}", path.display()).into());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    files.sort_by(|left, right| (left.root, &left.path).cmp(&(right.root, &right.path)));
    Ok(files)
}

/// Read the locked authored files for one side, checking each against its pin. Every
/// output reads authored bytes through here, so what ships is exactly what was locked.
pub(crate) fn read_locked<'a>(
    root: &PackRoot,
    lock: &'a Lockfile,
    side: BuildSide,
) -> Result<Vec<(&'a AuthoredFile, Vec<u8>)>> {
    verify(root, &lock.authored)?;
    lock.authored
        .iter()
        .filter(|file| side.accepts_authored(file.root))
        .map(|file| {
            let bytes = fs::read(root.authored_dir(file.root).join(&file.path))?;
            if bytes.len() as u64 != file.file_size
                || hash::sha1_hex(&bytes) != file.sha1
                || hash::sha512_hex(&bytes) != file.sha512
            {
                return Err(format!(
                    "authored file changed during the build: {}/{}; run `swatch install` after reviewing the changes",
                    file.root.dir_name(),
                    file.path
                )
                .into());
            }
            Ok((file, bytes))
        })
        .collect()
}

pub fn verify(root: &PackRoot, expected: &[AuthoredFile]) -> Result<()> {
    let actual = scan(root)?;
    if actual == expected {
        return Ok(());
    }

    let expected_names: Vec<_> = expected
        .iter()
        .map(|file| format!("{}/{}", file.root.dir_name(), file.path))
        .collect();
    let actual_names: Vec<_> = actual
        .iter()
        .map(|file| format!("{}/{}", file.root.dir_name(), file.path))
        .collect();
    Err(format!(
        "authored files differ from pack.lock.toml (locked: {}; found: {}); run `swatch install` after reviewing the changes",
        display_names(&expected_names),
        display_names(&actual_names)
    )
    .into())
}

fn scan_directory(
    base: &Path,
    directory: &Path,
    root: AuthoredRoot,
    files: &mut Vec<AuthoredFile>,
) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(directory)?.collect();
    entries.sort_by_key(|entry| {
        entry
            .as_ref()
            .map(|entry| entry.file_name())
            .unwrap_or_default()
    });
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "authored files cannot contain symbolic links: {}",
                path.display()
            )
            .into());
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if is_junk(&name) {
            return Err(
                format!("remove junk file from authored content: {}", path.display()).into(),
            );
        }
        if metadata.is_dir() {
            scan_directory(base, &path, root, files)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(format!("unsupported authored file type: {}", path.display()).into());
        }
        if name == ".gitkeep" && metadata.len() == 0 {
            continue;
        }
        if name == ".gitkeep" {
            return Err(format!("authored placeholder must be empty: {}", path.display()).into());
        }
        let relative = path
            .strip_prefix(base)
            .map_err(crate::Error::from_display)?
            .to_str()
            .ok_or_else(|| format!("authored path is not UTF-8: {}", path.display()))?
            .replace(std::path::MAIN_SEPARATOR, "/");
        check_pack_path(&relative)?;
        let bytes = fs::read(&path)?;
        files.push(AuthoredFile {
            root,
            path: relative,
            file_size: bytes.len() as u64,
            sha1: hash::sha1_hex(&bytes),
            sha512: hash::sha512_hex(&bytes),
        });
    }
    Ok(())
}

fn is_junk(name: &str) -> bool {
    name == ".DS_Store"
        || name == "Thumbs.db"
        || name == "desktop.ini"
        || name == ".git"
        || name == ".svn"
        || name.starts_with("._")
        || name.ends_with('~')
        || name.ends_with(".bak")
        || name.ends_with(".swp")
}

fn display_names(names: &[String]) -> String {
    if names.is_empty() {
        "none".into()
    } else {
        names.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_all_roots_with_hashes_and_rejects_junk() {
        let directory = tempfile::tempdir().expect("temporary pack");
        let root = PackRoot {
            path: directory.path().into(),
        };
        fs::create_dir_all(root.authored_dir(AuthoredRoot::Shared).join("config"))
            .expect("shared root");
        fs::create_dir_all(root.authored_dir(AuthoredRoot::Client)).expect("client root");
        fs::write(
            root.authored_dir(AuthoredRoot::Shared)
                .join("config/example.json"),
            b"{}\n",
        )
        .expect("shared file");
        fs::write(
            root.authored_dir(AuthoredRoot::Client).join("options.txt"),
            b"client\n",
        )
        .expect("client file");

        let files = scan(&root).expect("authored files");
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].root, AuthoredRoot::Shared);
        assert_eq!(files[0].path, "config/example.json");
        assert_eq!(files[0].file_size, 3);
        assert_eq!(files[1].root, AuthoredRoot::Client);

        fs::write(
            root.authored_dir(AuthoredRoot::Server).join(".DS_Store"),
            b"junk",
        )
        .expect_err("missing server root");
        fs::create_dir_all(root.authored_dir(AuthoredRoot::Server)).expect("server root");
        fs::write(
            root.authored_dir(AuthoredRoot::Server).join(".DS_Store"),
            b"junk",
        )
        .expect("junk");
        assert!(scan(&root).is_err());
    }

    #[test]
    fn rejects_junk_directories() {
        let directory = tempfile::tempdir().expect("temporary pack");
        let root = PackRoot {
            path: directory.path().into(),
        };
        let git = root.authored_dir(AuthoredRoot::Shared).join(".git");
        fs::create_dir_all(&git).expect("junk directory");
        fs::write(git.join("HEAD"), b"ref: refs/heads/main\n").expect("junk file");

        let error = scan(&root).expect_err("junk directory").to_string();
        assert!(error.contains("remove junk file"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temporary pack");
        let root = PackRoot {
            path: directory.path().into(),
        };
        fs::create_dir_all(root.authored_dir(AuthoredRoot::Shared)).expect("shared root");
        fs::write(root.path.join("outside.txt"), b"outside").expect("outside file");
        symlink(
            root.path.join("outside.txt"),
            root.authored_dir(AuthoredRoot::Shared).join("link.txt"),
        )
        .expect("symlink");
        assert!(scan(&root).is_err());
    }
}
