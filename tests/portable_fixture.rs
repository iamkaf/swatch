use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::process::Command;

use swatch::spec::{ContentPlacement, Loader};
use swatch::{PackRoot, load_lock, load_spec};
use zip::ZipArchive;

#[test]
fn portable_neoforge_fixture_installs_and_prepares_without_network() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/portable-pack");
    let temporary = tempfile::tempdir().expect("temporary pack root");
    copy_tree(&fixture, temporary.path()).expect("copy portable fixture");
    let root = PackRoot::discover(temporary.path()).expect("discover fixture pack");
    let spec = load_spec(&root).expect("load fixture manifest");
    let lock = load_lock(&root).expect("load fixture lockfile");

    assert_eq!(spec.pack.name, "Copper Valley");
    assert_eq!(spec.pack.slug, "copper-valley");
    assert_eq!(spec.pack.group, "org.example.packs");
    assert_eq!(spec.pack.loader, Loader::NeoForge);
    assert!(spec.content().any(|content| {
        content.id == "dedicated-fixture" && content.placement == ContentPlacement::ServerMod
    }));
    assert!(lock.file.iter().any(|file| {
        file.id == "dedicated-fixture"
            && file.env.client == swatch::spec::SideRequirement::Unsupported
            && file.env.server == swatch::spec::SideRequirement::Required
    }));

    let binary = env!("CARGO_BIN_EXE_swatch");
    let install = offline_command(binary, &root.path)
        .arg("install")
        .output()
        .expect("run install");
    assert!(
        install.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&install.stderr)
    );

    let publish = offline_command(binary, &root.path)
        .args(["publish", "--dry-run"])
        .output()
        .expect("run publication dry-run");
    assert!(
        publish.status.success(),
        "publication dry-run failed: {}",
        String::from_utf8_lossy(&publish.stderr)
    );
    let output = String::from_utf8(publish.stdout).expect("UTF-8 dry-run output");
    for target in ["Modrinth", "CurseForge", "GitHub", "Maven"] {
        assert!(
            output.contains(&format!("DRY {target}")),
            "missing {target}: {output}"
        );
    }

    let archive_path = root.dist_dir().join("copper-valley-0.1.0-client.mrpack");
    let archive = File::open(&archive_path).expect("open prepared mrpack");
    let mut archive = ZipArchive::new(archive).expect("read prepared mrpack");
    let mut index = String::new();
    archive
        .by_name("modrinth.index.json")
        .expect("index in mrpack")
        .read_to_string(&mut index)
        .expect("read index");
    let index: serde_json::Value = serde_json::from_str(&index).expect("parse index");
    assert_eq!(index["dependencies"]["neoforge"], "26.2.0");
    assert!(
        !index["files"]
            .as_array()
            .expect("files")
            .iter()
            .any(|file| file["path"] == "mods/dedicated-fixture.jar")
    );

    let server_path = root.dist_dir().join("copper-valley-0.1.0-server.mrpack");
    let mut server = ZipArchive::new(File::open(server_path).expect("server archive"))
        .expect("read server archive");
    let mut server_index = String::new();
    server
        .by_name("modrinth.index.json")
        .expect("server index")
        .read_to_string(&mut server_index)
        .expect("read server index");
    let server_index: serde_json::Value =
        serde_json::from_str(&server_index).expect("parse server index");
    assert!(
        server_index["files"]
            .as_array()
            .expect("files")
            .iter()
            .any(|file| {
                file["path"] == "mods/dedicated-fixture.jar"
                    && file["env"]["client"] == "unsupported"
                    && file["env"]["server"] == "required"
            })
    );

    let release: serde_json::Value = serde_json::from_slice(
        &fs::read(root.dist_dir().join("release.json")).expect("release manifest"),
    )
    .expect("parse release manifest");
    assert_eq!(release["schemaVersion"], 1);
    assert_eq!(release["packVersion"], "0.1.0");
    assert_eq!(release["preparationMode"], "preview");
    assert!(
        release["artifacts"]
            .as_array()
            .expect("release artifacts")
            .iter()
            .all(|artifact| artifact["sha256"]
                .as_str()
                .is_some_and(|hash| hash.len() == 64)
                && artifact["sha512"]
                    .as_str()
                    .is_some_and(|hash| hash.len() == 128))
    );

    assert!(
        root.dist_dir()
            .join("copper-valley-0.1.0-curseforge.zip")
            .is_file()
    );
    assert!(root.dist_dir().join("copper-valley-0.1.0.pom").is_file());
}

fn offline_command(binary: &str, root: &Path) -> Command {
    let mut command = Command::new(binary);
    command
        .current_dir(root)
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("ALL_PROXY", "http://127.0.0.1:1")
        .env("NO_PROXY", "");
    command
}

fn copy_tree(source: &Path, destination: &Path) -> std::io::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            fs::create_dir_all(&target)?;
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
