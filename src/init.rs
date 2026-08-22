use crate::Result;
use crate::spec::PackSpec;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitOptions {
    pub path: PathBuf,
    pub name: String,
    pub slug: String,
    pub group: String,
    pub minecraft: String,
    pub loader: String,
    pub loader_version: String,
}

pub fn init(options: &InitOptions) -> Result<PathBuf> {
    if options.path.exists() && fs::read_dir(&options.path)?.next().is_some() {
        return Err(format!(
            "cannot initialize non-empty directory {}",
            options.path.display()
        )
        .into());
    }
    let manifest = pack_manifest(options);
    PackSpec::parse(&manifest)?;
    fs::create_dir_all(&options.path)?;

    fs::write(options.path.join("pack.toml"), manifest)?;
    fs::write(options.path.join("overrides.toml"), OVERRIDES_TOML)?;
    fs::write(options.path.join("CHANGELOG.md"), CHANGELOG)?;
    fs::write(options.path.join("README.md"), pack_readme(options))?;
    fs::write(options.path.join(".gitignore"), GITIGNORE)?;
    for root in ["overrides", "client-overrides", "server-overrides"] {
        fs::create_dir_all(options.path.join(root))?;
        fs::write(options.path.join(root).join(".gitkeep"), "")?;
    }
    fs::create_dir_all(options.path.join("scripts"))?;
    let check = options.path.join("scripts/check");
    fs::write(&check, CHECK_SCRIPT)?;
    make_executable(&check)?;
    fs::create_dir_all(options.path.join(".github/workflows"))?;
    fs::write(
        options.path.join(".github/workflows/check.yml"),
        workflow(CHECK_WORKFLOW),
    )?;
    fs::write(
        options.path.join(".github/workflows/release.yml"),
        workflow(RELEASE_WORKFLOW),
    )?;
    Ok(options.path.clone())
}

fn pack_manifest(options: &InitOptions) -> String {
    format!(
        "format = 1\n\n[pack]\nname = {}\nslug = {}\nversion = \"0.1.0\"\ngroup = {}\nminecraft = {}\nloader = {}\nloader_version = {}\n\n[mods]\n\n[client_mods]\n\n[server_mods]\n\n[shaders]\n\n[resource_packs]\n\n[datapacks]\n\n[publish]\nchangelog = \"CHANGELOG.md\"\n\n[publish.github]\n",
        toml_string(&options.name),
        toml_string(&options.slug),
        toml_string(&options.group),
        toml_string(&options.minecraft),
        toml_string(&options.loader),
        toml_string(&options.loader_version),
    )
}

fn pack_readme(options: &InitOptions) -> String {
    format!(
        "# {}\n\nThis repository builds the Minecraft {} client and server packs with Swatch.\n\n```bash\nswatch install\nsh scripts/check\nswatch build all\nswatch prepare\nswatch verify\n```\n\nPut files used by both sides in `overrides/`, client-only files in `client-overrides/`, and server-only files in `server-overrides/`. Run `swatch install` after changing those files so their hashes are recorded in `pack.lock.toml`.\n\n`scripts/check` owns this pack's gameplay and policy checks. It must exit with status 0 when the pack is ready. Swatch treats the pack contents as opaque files.\n",
        options.name, options.minecraft
    )
}

fn workflow(template: &str) -> String {
    template.replace("SWATCH_VERSION", env!("CARGO_PKG_VERSION"))
}

fn toml_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

const OVERRIDES_TOML: &str = "[curseforge]\nadd = []\nexclude = []\n";
const CHANGELOG: &str = "# Changelog\n\n## 0.1.0\n\n- Initial pack.\n";
const GITIGNORE: &str = ".cache/\ndist/\ngenerated/\n";
const CHECK_SCRIPT: &str = "#!/usr/bin/env sh\nset -eu\n\n# Add pack-specific checks here.\n";

const CHECK_WORKFLOW: &str = r#"name: Check

on:
  push:
  pull_request:
  workflow_dispatch:

permissions:
  contents: read

jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - name: Checkout
        uses: actions/checkout@fbc6f3992d24b796d5a048ff273f7fcc4a7b6c09 # v5

      - name: Install Rust
        uses: dtolnay/rust-toolchain@4360b52568e2003a75bf9bc1d59f33a8e3fc893c # stable

      - name: Install Swatch
        run: |
          cargo install --locked --git https://github.com/iamkaf/swatch --tag vSWATCH_VERSION swatch
          test "$(swatch --version)" = "swatch SWATCH_VERSION"

      - name: Install and check pack
        run: |
          swatch install
          test -z "$(git status --porcelain -- pack.lock.toml)"
          sh scripts/check
          swatch build all
"#;

const RELEASE_WORKFLOW: &str = r#"name: Release

on:
  workflow_dispatch:
    inputs:
      tag:
        description: Release tag matching pack.toml, such as v1.2.0
        required: true
        type: string

permissions:
  contents: write
  id-token: write
  attestations: write

concurrency:
  group: pack-release
  cancel-in-progress: false

jobs:
  release:
    runs-on: ubuntu-latest
    steps:
      - name: Checkout
        uses: actions/checkout@fbc6f3992d24b796d5a048ff273f7fcc4a7b6c09 # v5

      - name: Install Rust
        uses: dtolnay/rust-toolchain@4360b52568e2003a75bf9bc1d59f33a8e3fc893c # stable

      - name: Install Swatch
        run: |
          cargo install --locked --git https://github.com/iamkaf/swatch --tag vSWATCH_VERSION swatch
          test "$(swatch --version)" = "swatch SWATCH_VERSION"

      - name: Install Cosign
        uses: sigstore/cosign-installer@faadad0cce49287aee09b3a48701e75088a2c6ad # v4.0.0
        with:
          cosign-release: v3.0.2

      - name: Prepare pack release
        env:
          TAG: ${{ inputs.tag }}
        run: |
          set -euo pipefail
          test "$GITHUB_REF" = "refs/heads/main"
          version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' pack.toml | head -n 1)"
          test "$TAG" = "v$version"
          swatch install
          test -z "$(git status --porcelain -- pack.lock.toml)"
          sh scripts/check
          swatch prepare
          swatch verify
          cosign sign-blob --yes \
            --bundle dist/release.json.sigstore.json \
            dist/release.json

      - name: Attest client archive
        uses: actions/attest-build-provenance@977bb373ede98d70efdf65b84cb5f73e068dcc2a # v3
        with:
          subject-path: dist/*-client.mrpack

      - name: Attest server archive
        uses: actions/attest-build-provenance@977bb373ede98d70efdf65b84cb5f73e068dcc2a # v3
        with:
          subject-path: dist/*-server.mrpack

      - name: Verify prepared bytes
        env:
          GH_TOKEN: ${{ github.token }}
        run: |
          set -euo pipefail
          swatch verify
          cosign verify-blob \
            --bundle dist/release.json.sigstore.json \
            --certificate-identity "https://github.com/${GITHUB_REPOSITORY}/.github/workflows/release.yml@refs/heads/main" \
            --certificate-oidc-issuer https://token.actions.githubusercontent.com \
            dist/release.json
          gh attestation verify dist/*-client.mrpack --repo "$GITHUB_REPOSITORY"
          gh attestation verify dist/*-server.mrpack --repo "$GITHUB_REPOSITORY"

      - name: Create GitHub release
        env:
          GH_TOKEN: ${{ github.token }}
          TAG: ${{ inputs.tag }}
        run: |
          gh release create "$TAG" \
            dist/*-client.mrpack \
            dist/*-server.mrpack \
            dist/release.json \
            dist/release.json.sigstore.json \
            --target "$GITHUB_SHA" \
            --title "$TAG" \
            --notes-file dist/release-notes.md
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_a_complete_pack_repository() {
        let directory = tempfile::tempdir().expect("temporary parent");
        let path = directory.path().join("example-pack");
        init(&InitOptions {
            path: path.clone(),
            name: "Example Pack".into(),
            slug: "example-pack".into(),
            group: "org.example.packs".into(),
            minecraft: "26.2".into(),
            loader: "neoforge".into(),
            loader_version: "26.2.0".into(),
        })
        .expect("initialize pack");

        assert!(
            PackSpec::parse(&fs::read_to_string(path.join("pack.toml")).expect("manifest")).is_ok()
        );
        for expected in [
            "CHANGELOG.md",
            "README.md",
            "overrides/.gitkeep",
            "client-overrides/.gitkeep",
            "server-overrides/.gitkeep",
            "scripts/check",
            ".github/workflows/check.yml",
            ".github/workflows/release.yml",
        ] {
            assert!(path.join(expected).exists(), "missing {expected}");
        }
        let check_workflow =
            fs::read_to_string(path.join(".github/workflows/check.yml")).expect("check workflow");
        let release_workflow = fs::read_to_string(path.join(".github/workflows/release.yml"))
            .expect("release workflow");
        for workflow in [&check_workflow, &release_workflow] {
            assert!(workflow.contains(
                "cargo install --locked --git https://github.com/iamkaf/swatch --tag v0.2.0 swatch"
            ));
            assert!(workflow.contains("test \"$(swatch --version)\" = \"swatch 0.2.0\""));
        }
        assert!(release_workflow.contains("workflow_dispatch"));
    }

    #[test]
    fn refuses_non_empty_directories() {
        let directory = tempfile::tempdir().expect("temporary directory");
        fs::write(directory.path().join("existing"), "keep").expect("existing file");
        let options = InitOptions {
            path: directory.path().into(),
            name: "Example".into(),
            slug: "example".into(),
            group: "org.example".into(),
            minecraft: "26.2".into(),
            loader: "fabric".into(),
            loader_version: "1.0.0".into(),
        };
        assert!(init(&options).is_err());
        assert!(directory.path().join("existing").is_file());
    }
}
