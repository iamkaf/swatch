use crate::spec::{ContentPlacement, Lockfile};
use crate::{PackRoot, Result, curseforge, fetch, load_lock, load_spec, resolve};
use std::fs;
use std::io::Write;
use std::str::FromStr;
use toml_edit::{DocumentMut, Item, Table, value};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AddOptions {
    pub placement: Option<ContentPlacement>,
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
        _ => resolver.latest_version(
            &spec.pack,
            options.placement.unwrap_or(ContentPlacement::SharedMod),
            &project,
        )?,
    };
    let detected = resolver.project_placement(
        &project,
        options.placement.unwrap_or(ContentPlacement::SharedMod),
    )?;
    let placement = options.placement.unwrap_or(detected);
    append_modrinth(root, placement, &project, &version)?;
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
    let previous = load_lock(root).ok();
    let mut lock = match previous.as_ref() {
        Some(lock) if resolve::lock_matches_spec(&spec, lock) => lock.clone(),
        previous => resolve::resolve_candidate(&spec, previous)?,
    };
    lock.set_authored(crate::authored::scan(root)?);
    let verified = fetch::ensure_all(root, &lock.file)?;
    if options.curseforge {
        lock = curseforge::ensure_mappings(root, lock, &verified)?;
    }
    write_lock(root, &lock)?;
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
    placement: ContentPlacement,
    project: &str,
    version: &str,
) -> Result<()> {
    let mut text = fs::read_to_string(root.pack_toml())?;
    let section = placement.manifest_table();
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

fn write_lock(root: &PackRoot, lock: &Lockfile) -> Result<()> {
    let text = lock.to_toml()?;
    let parent = root
        .lock_toml()
        .parent()
        .ok_or_else(|| crate::Error::from("pack.lock.toml has no parent directory"))?
        .to_path_buf();
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(text.as_bytes())?;
    temporary
        .persist(root.lock_toml())
        .map_err(|error| crate::Error::from_display(error.error))?;
    Ok(())
}

fn remove_entry(text: &str, query: &str) -> Result<(String, bool)> {
    let mut document =
        DocumentMut::from_str(text).map_err(|error| format!("pack.toml: {error}"))?;
    let query = query.trim();
    for section in [
        "mods",
        "client_mods",
        "server_mods",
        "shaders",
        "resource_packs",
        "datapacks",
    ] {
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
    use crate::spec::{EnvSpec, FileSpec, Loader, PackMeta, SideRequirement};

    fn lock() -> Lockfile {
        Lockfile::new(
            PackMeta {
                name: "Example Pack".into(),
                slug: "example-pack".into(),
                version: "1.0.0".into(),
                group: "org.example.packs".into(),
                minecraft: "26.2".into(),
                loader: Loader::Fabric,
                loader_version: "0.19.3".into(),
            },
            vec![FileSpec {
                id: "example".into(),
                requested_version: "1.0.0".into(),
                path: "mods/example.jar".into(),
                file_size: 0,
                sha1: "a".repeat(40),
                sha512: "b".repeat(128),
                env: EnvSpec {
                    client: SideRequirement::Required,
                    server: SideRequirement::Required,
                },
                downloads: vec!["https://example.invalid/example.jar".into()],
            }],
        )
    }

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
            placement: Some(ContentPlacement::ServerMod),
        };
        assert_eq!(options.placement, Some(ContentPlacement::ServerMod));
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

        append_modrinth(&root, ContentPlacement::ServerMod, "dedicated", "2.0.0")
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
                .placement,
            ContentPlacement::ServerMod
        );
    }

    #[test]
    fn lock_commit_replaces_only_with_a_valid_candidate() {
        let directory = tempfile::tempdir().expect("temporary pack");
        let root = PackRoot {
            path: directory.path().to_path_buf(),
        };
        fs::write(root.lock_toml(), "old lock\n").expect("old lock");

        let valid = lock();
        write_lock(&root, &valid).expect("commit valid lock");
        let committed = fs::read_to_string(root.lock_toml()).expect("committed lock");
        assert_eq!(Lockfile::parse(&committed).expect("valid lock"), valid);

        let mut invalid = valid;
        invalid.file[0].path = "../outside.jar".into();
        assert!(write_lock(&root, &invalid).is_err());
        assert_eq!(
            fs::read_to_string(root.lock_toml()).expect("retained lock"),
            committed
        );
    }

    #[test]
    fn install_locks_authored_files_and_build_rejects_drift() {
        let directory = tempfile::tempdir().expect("temporary pack");
        let root = PackRoot {
            path: directory.path().to_path_buf(),
        };
        fs::write(
            root.pack_toml(),
            r#"format = 1

[pack]
name = "Example"
slug = "example"
version = "1.0.0"
group = "org.example.packs"
minecraft = "26.2"
loader = "fabric"
loader_version = "0.19.3"
"#,
        )
        .expect("manifest");
        fs::create_dir_all(root.overrides_dir().join("config")).expect("authored root");
        let authored = root.overrides_dir().join("config/example.json");
        fs::write(&authored, b"{}\n").expect("authored file");

        install(&root, InstallOptions::default()).expect("install");
        let lock = crate::load_lock(&root).expect("lockfile");
        assert_eq!(lock.version, 1);
        assert_eq!(lock.authored.len(), 1);
        crate::build(&root, crate::BuildSide::Client).expect("locked build");

        fs::write(&authored, b"{\"changed\":true}\n").expect("change authored file");
        let error = crate::build(&root, crate::BuildSide::Client)
            .expect_err("authored drift")
            .to_string();
        assert!(error.contains("authored files differ"));
    }
}
