use crate::Result;
use crate::spec::check_pack_path;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn collect_tree(
    dir: PathBuf,
    prefix: &str,
    output: &mut BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    if dir.is_dir() {
        collect_dir(&dir, prefix, output)?;
    }
    Ok(())
}

fn collect_dir(dir: &Path, prefix: &str, output: &mut BTreeMap<String, Vec<u8>>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == ".DS_Store" || name.starts_with("._") || name.ends_with(".bak") {
            continue;
        }
        let archive_path = format!("{prefix}/{name}");
        check_pack_path(&archive_path)?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "refusing symbolic link in pack overrides: {}",
                path.display()
            )
            .into());
        }
        if metadata.is_dir() {
            collect_dir(&path, &archive_path, output)?;
        } else if metadata.is_file()
            && output
                .insert(archive_path.clone(), fs::read(&path)?)
                .is_some()
        {
            return Err(format!("duplicate pack override {archive_path}").into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merged_trees_cannot_overwrite_each_other() {
        let first = tempfile::tempdir().expect("first override tree");
        let second = tempfile::tempdir().expect("second override tree");
        fs::write(first.path().join("options.txt"), "first").expect("first override");
        fs::write(second.path().join("options.txt"), "second").expect("second override");

        let mut files = BTreeMap::new();
        collect_tree(first.path().into(), "overrides", &mut files).expect("first tree");
        assert!(collect_tree(second.path().into(), "overrides", &mut files).is_err());
    }
}
