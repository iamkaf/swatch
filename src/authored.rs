use crate::hash;
use crate::spec::{AuthoredFile, AuthoredRoot, check_pack_path};
use crate::{PackRoot, Result};
use std::fs;
use std::path::Path;

const ROOTS: [(AuthoredRoot, &str); 3] = [
    (AuthoredRoot::Shared, "overrides"),
    (AuthoredRoot::Client, "client-overrides"),
    (AuthoredRoot::Server, "server-overrides"),
];

pub fn scan(root: &PackRoot) -> Result<Vec<AuthoredFile>> {
    let mut files = Vec::new();
    for (kind, directory) in ROOTS {
        let path = root.path.join(directory);
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

pub fn verify(root: &PackRoot, expected: &[AuthoredFile]) -> Result<()> {
    let actual = scan(root)?;
    if actual == expected {
        return Ok(());
    }

    let expected_names: Vec<_> = expected
        .iter()
        .map(|file| format!("{}/{}", root_name(file.root), file.path))
        .collect();
    let actual_names: Vec<_> = actual
        .iter()
        .map(|file| format!("{}/{}", root_name(file.root), file.path))
        .collect();
    Err(format!(
        "authored files differ from pack.lock.toml (locked: {}; found: {}); run `swatch install` after reviewing the changes",
        display_names(&expected_names),
        display_names(&actual_names)
    )
    .into())
}

pub fn root_name(root: AuthoredRoot) -> &'static str {
    match root {
        AuthoredRoot::Shared => "overrides",
        AuthoredRoot::Client => "client-overrides",
        AuthoredRoot::Server => "server-overrides",
    }
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
        if metadata.is_dir() {
            scan_directory(base, &path, root, files)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(format!("unsupported authored file type: {}", path.display()).into());
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == ".gitkeep" && metadata.len() == 0 {
            continue;
        }
        if name == ".gitkeep" {
            return Err(format!("authored placeholder must be empty: {}", path.display()).into());
        }
        if is_junk(&name) {
            return Err(
                format!("remove junk file from authored content: {}", path.display()).into(),
            );
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

pub(crate) fn is_junk(name: &str) -> bool {
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
        fs::create_dir_all(root.overrides_dir().join("config")).expect("shared root");
        fs::create_dir_all(root.client_overrides_dir()).expect("client root");
        fs::write(root.overrides_dir().join("config/example.json"), b"{}\n").expect("shared file");
        fs::write(root.client_overrides_dir().join("options.txt"), b"client\n")
            .expect("client file");

        let files = scan(&root).expect("authored files");
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].root, AuthoredRoot::Shared);
        assert_eq!(files[0].path, "config/example.json");
        assert_eq!(files[0].file_size, 3);
        assert_eq!(files[1].root, AuthoredRoot::Client);

        fs::write(root.server_overrides_dir().join(".DS_Store"), b"junk")
            .expect_err("missing server root");
        fs::create_dir_all(root.server_overrides_dir()).expect("server root");
        fs::write(root.server_overrides_dir().join(".DS_Store"), b"junk").expect("junk");
        assert!(scan(&root).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temporary pack");
        let root = PackRoot {
            path: directory.path().into(),
        };
        fs::create_dir_all(root.overrides_dir()).expect("shared root");
        fs::write(root.path.join("outside.txt"), b"outside").expect("outside file");
        symlink(
            root.path.join("outside.txt"),
            root.overrides_dir().join("link.txt"),
        )
        .expect("symlink");
        assert!(scan(&root).is_err());
    }
}
