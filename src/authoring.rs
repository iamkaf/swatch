use crate::spec::{ContentKind, ContentSide};
use crate::{PackRoot, Result, curseforge, fetch, load_lock, load_spec, resolve};
use std::fs;
use std::str::FromStr;
use toml_edit::{DocumentMut, Item, Table, value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddOptions {
    pub kind: ContentKind,
    pub side: Option<ContentSide>,
}

impl Default for AddOptions {
    fn default() -> Self {
        Self {
            kind: ContentKind::Mod,
            side: None,
        }
    }
}

pub fn add(
    root: &PackRoot,
    query: &str,
    requested_version: Option<&str>,
    options: AddOptions,
) -> Result<String> {
    let spec = load_spec(root)?;
    if spec
        .content()
        .any(|content| content.id.eq_ignore_ascii_case(query))
    {
        return Err(format!("{} is already in pack.toml", query.trim()).into());
    }

    let resolver = resolve::Resolver::new()?;
    let project = resolver.find_project(query)?;
    if spec.content().any(|content| content.id == project) {
        return Err(format!("{project} is already in pack.toml").into());
    }
    let version = match requested_version {
        Some(version) if !version.trim().is_empty() => version.to_string(),
        _ => resolver.latest_version(&spec.pack, options.kind, &project)?,
    };
    let detected_side = resolver.project_side(&project, options.kind)?;
    let side = if options.kind == ContentKind::Shader {
        ContentSide::Client
    } else {
        options.side.unwrap_or(detected_side)
    };
    append_modrinth(root, options.kind, &project, &version, side)?;
    Ok(project)
}

pub fn remove(root: &PackRoot, query: &str) -> Result<()> {
    let text = fs::read_to_string(root.pack_toml())?;
    let (updated, removed) = remove_entry(&text, query)?;
    if !removed {
        return Err(format!("{query} is not in pack.toml").into());
    }
    crate::spec::PackSpec::parse(&updated)?;
    fs::write(root.pack_toml(), updated)?;
    Ok(())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InstallOptions {
    pub curseforge: bool,
}

pub fn install(root: &PackRoot, options: InstallOptions) -> Result<InstallReport> {
    let spec = load_spec(root)?;
    let lock = match load_lock(root) {
        Ok(lock) if resolve::lock_matches_spec(&spec, &lock) => lock,
        Ok(_) | Err(_) => resolve::resolve_pack(root)?,
    };
    fetch::ensure_all(root, &lock.file)?;
    if options.curseforge {
        curseforge::ensure_mappings(root)?;
    }
    Ok(InstallReport {
        files: lock.file.len(),
    })
}

#[derive(Debug, Clone)]
pub struct InstallReport {
    pub files: usize,
}

fn append_modrinth(
    root: &PackRoot,
    kind: ContentKind,
    project: &str,
    version: &str,
    side: ContentSide,
) -> Result<()> {
    let mut text = fs::read_to_string(root.pack_toml())?;
    let section = match kind {
        ContentKind::Mod if side == ContentSide::Client => "client_mods",
        ContentKind::Mod if side == ContentSide::Server => "server_mods",
        ContentKind::Mod => "mods",
        ContentKind::Shader => "shaders",
    };
    let mut document =
        DocumentMut::from_str(&text).map_err(|error| format!("pack.toml: {error}"))?;
    let section_item = document.entry(section).or_insert(Item::Table(Table::new()));
    let section_table = section_item
        .as_table_mut()
        .ok_or_else(|| format!("pack.toml [{section}] must be a table"))?;
    section_table.insert(project, value(version));
    text = document.to_string();
    crate::spec::PackSpec::parse(&text)?;
    fs::write(root.pack_toml(), text)?;
    Ok(())
}

fn remove_entry(text: &str, query: &str) -> Result<(String, bool)> {
    let mut document =
        DocumentMut::from_str(text).map_err(|error| format!("pack.toml: {error}"))?;
    let query = query.trim();
    for section in ["mods", "client_mods", "server_mods", "shaders"] {
        let Some(item) = document.get_mut(section) else {
            continue;
        };
        let Some(table) = item.as_table_mut() else {
            return Err(format!("pack.toml [{section}] must be a table").into());
        };
        let Some(id) = table
            .iter()
            .map(|(id, _)| id)
            .find(|id| id.eq_ignore_ascii_case(query))
            .map(str::to_owned)
        else {
            continue;
        };
        table.remove(&id);
        return Ok((document.to_string(), true));
    }

    Ok((text.to_string(), false))
}

#[cfg(test)]
mod authoring_tests {
    use super::*;

    #[test]
    fn keeps_unknown_content_when_removing_missing_project() {
        let text = "format = 1\n\n[mods]\nsodium = \"1\"\n";
        let (updated, removed) = remove_entry(text, "iris").expect("remove");
        assert!(!removed);
        assert_eq!(updated, text);
    }

    #[test]
    fn removes_a_project_from_the_keyed_manifest() {
        let text = "format = 1\n\n[mods]\ncreate = \"1\"\n\n[client_mods]\nsodium = \"2\"\n\n[server_mods]\ndedicated = \"3\"\n\n[shaders]\ncomplementary-unbound = \"4\"\n";
        let (updated, removed) = remove_entry(text, "sodium").expect("remove");
        assert!(removed);
        assert!(!updated.contains("sodium"));
        assert!(updated.contains("create = \"1\""));
        assert!(updated.contains("dedicated = \"3\""));
        assert!(updated.contains("complementary-unbound"));
    }

    #[test]
    fn toml_edit_keeps_comments_and_existing_style_when_removing() {
        let text = "format = 1\n\n[mods]\n# keep this comment\ncreate = '1' # keep this inline comment\n# remove this entry\nsodium = '2'\n\n[publish]\ncurseforge = false\n";
        let (updated, removed) = remove_entry(text, "SODIUM").expect("remove");
        assert!(removed);
        assert_eq!(
            updated,
            "format = 1\n\n[mods]\n# keep this comment\ncreate = '1' # keep this inline comment\n\n[publish]\ncurseforge = false\n"
        );
    }

    #[test]
    fn add_options_can_target_server_content() {
        let options = AddOptions {
            kind: ContentKind::Mod,
            side: Some(ContentSide::Server),
        };
        assert_eq!(options.side, Some(ContentSide::Server));
    }

    #[test]
    fn adding_content_preserves_manifest_text_and_uses_server_section() {
        let directory = tempfile::tempdir().expect("temporary pack");
        let root = PackRoot {
            path: directory.path().to_path_buf(),
        };
        let original = r#"format = 1

[pack]
name = "Example"
slug = "example"
version = "1.0.0"
group = "org.example.packs"
minecraft = "26.2"
loader = "forge"
loader_version = "1.0.0"

[mods]
# Keep this comment and ordering.
common = '1.0.0'

[publish]
curseforge = false
"#;
        fs::write(root.pack_toml(), original).expect("write manifest");

        append_modrinth(
            &root,
            ContentKind::Mod,
            "dedicated",
            "2.0.0",
            ContentSide::Server,
        )
        .expect("add server mod");

        let updated = fs::read_to_string(root.pack_toml()).expect("read manifest");
        assert!(updated.starts_with(original));
        assert!(updated.contains("[server_mods]\ndedicated = \"2.0.0\"\n"));
        assert!(updated.contains("# Keep this comment and ordering.\ncommon = '1.0.0'\n"));
        let spec = crate::spec::PackSpec::parse(&updated).expect("updated manifest");
        assert_eq!(
            spec.content()
                .find(|content| content.id == "dedicated")
                .expect("server mod")
                .side,
            ContentSide::Server
        );
    }
}
