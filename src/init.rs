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
    fs::create_dir_all(options.path.join(".github/actions/setup-swatch"))?;
    fs::write(
        options.path.join(".github/actions/setup-swatch/action.yml"),
        workflow(SETUP_SWATCH_ACTION),
    )?;
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

const SETUP_SWATCH_ACTION: &str = r#"name: Setup Swatch
description: Install the verified Swatch release used by this pack

inputs:
  github-token:
    description: GitHub token used to verify artifact provenance
    required: true

runs:
  using: composite
  steps:
    - name: Install Cosign
      uses: sigstore/cosign-installer@faadad0cce49287aee09b3a48701e75088a2c6ad # v4.0.0
      with:
        cosign-release: v3.0.2

    - name: Download and verify Swatch
      shell: bash
      env:
        GH_TOKEN: ${{ inputs.github-token }}
      run: |
        set -euo pipefail
        archive="swatch-linux-x86_64.tar.gz"
        release="https://github.com/iamkaf/swatch/releases/download/vSWATCH_VERSION"
        download="$RUNNER_TEMP/swatch-SWATCH_VERSION-download"
        install="$RUNNER_TEMP/swatch-SWATCH_VERSION-bin"
        mkdir -p "$download" "$install"
        curl --fail --location --proto '=https' --tlsv1.2 \
          --output "$download/$archive" "$release/$archive"
        curl --fail --location --proto '=https' --tlsv1.2 \
          --output "$download/release-manifest.json" "$release/release-manifest.json"
        curl --fail --location --proto '=https' --tlsv1.2 \
          --output "$download/release-manifest.sigstore.json" \
          "$release/release-manifest.sigstore.json"
        cosign verify-blob \
          --bundle "$download/release-manifest.sigstore.json" \
          --certificate-identity "https://github.com/iamkaf/swatch/.github/workflows/release.yml@refs/heads/main" \
          --certificate-oidc-issuer https://token.actions.githubusercontent.com \
          "$download/release-manifest.json"
        test "$(jq -er '.schemaVersion' "$download/release-manifest.json")" = "1"
        test "$(jq -er '.version' "$download/release-manifest.json")" = "SWATCH_VERSION"
        sha256="$(jq -er --arg path "$archive" \
          '.artifacts | map(select(.path == $path)) | if length == 1 then .[0].sha256 else error("missing or duplicate archive") end' \
          "$download/release-manifest.json")"
        sha512="$(jq -er --arg path "$archive" \
          '.artifacts | map(select(.path == $path)) | if length == 1 then .[0].sha512 else error("missing or duplicate archive") end' \
          "$download/release-manifest.json")"
        printf '%s  %s\n' "$sha256" "$download/$archive" | sha256sum --check --strict
        printf '%s  %s\n' "$sha512" "$download/$archive" | sha512sum --check --strict
        gh attestation verify "$download/$archive" --repo iamkaf/swatch
        tar -xzf "$download/$archive" -C "$install" swatch
        chmod 0755 "$install/swatch"
        echo "$install" >> "$GITHUB_PATH"
        test "$("$install/swatch" --version)" = "swatch SWATCH_VERSION"
"#;

const CHECK_WORKFLOW: &str = r#"name: Check

on:
  push:
  pull_request:
  workflow_dispatch:

permissions:
  contents: read
  attestations: read

jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - name: Checkout
        uses: actions/checkout@fbc6f3992d24b796d5a048ff273f7fcc4a7b6c09 # v5

      - name: Install Swatch
        uses: ./.github/actions/setup-swatch
        with:
          github-token: ${{ github.token }}

      - name: Install and check pack
        run: |
          set -euo pipefail
          swatch install
          sh scripts/check
          test -z "$(git status --porcelain=v1 --untracked-files=all)"
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
      publish:
        description: Publish the verified release to configured targets
        required: true
        default: false
        type: boolean

permissions:
  contents: read

concurrency:
  group: pack-release
  cancel-in-progress: false

jobs:
  prepare:
    name: Prepare and verify
    runs-on: ubuntu-latest
    permissions:
      contents: read
      id-token: write
      attestations: write
    steps:
      - name: Checkout
        uses: actions/checkout@fbc6f3992d24b796d5a048ff273f7fcc4a7b6c09 # v5

      - name: Install Swatch
        uses: ./.github/actions/setup-swatch
        with:
          github-token: ${{ github.token }}

      - name: Prepare pack release
        env:
          TAG: ${{ inputs.tag }}
        run: |
          set -euo pipefail
          test "$GITHUB_REF" = "refs/heads/main"
          version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' pack.toml | head -n 1)"
          test "$TAG" = "v$version"
          swatch install
          sh scripts/check
          test -z "$(git status --porcelain=v1 --untracked-files=all)"
          swatch prepare
          swatch verify
          cosign sign-blob --yes \
            --bundle dist/release.json.sigstore.json \
            dist/release.json

      - name: Attest prepared files
        uses: actions/attest-build-provenance@977bb373ede98d70efdf65b84cb5f73e068dcc2a # v3
        with:
          subject-path: dist/*

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
          for file in dist/*; do
            gh attestation verify "$file" --repo "$GITHUB_REPOSITORY"
          done

      - name: Retain verified release
        uses: actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02 # v4
        with:
          name: verified-pack-release
          path: dist/
          if-no-files-found: error

  publish:
    name: Publish
    needs: prepare
    if: ${{ inputs.publish }}
    runs-on: ubuntu-latest
    permissions:
      contents: write
      attestations: read
    steps:
      - name: Checkout
        uses: actions/checkout@fbc6f3992d24b796d5a048ff273f7fcc4a7b6c09 # v5

      - name: Install Swatch
        uses: ./.github/actions/setup-swatch
        with:
          github-token: ${{ github.token }}

      - name: Download verified release
        uses: actions/download-artifact@634f93cb2916e3fdff6788551b99b062d0335ce0 # v5
        with:
          name: verified-pack-release
          path: dist

      - name: Reverify and publish
        env:
          GITHUB_TOKEN: ${{ github.token }}
          GH_TOKEN: ${{ github.token }}
          TAG: ${{ inputs.tag }}
          MODRINTH_TOKEN: ${{ secrets.MODRINTH_TOKEN }}
          CURSEFORGE_TOKEN: ${{ secrets.CURSEFORGE_TOKEN }}
          MAVEN_PUBLISH_USERNAME: ${{ secrets.MAVEN_PUBLISH_USERNAME }}
          MAVEN_PUBLISH_PASSWORD: ${{ secrets.MAVEN_PUBLISH_PASSWORD }}
        run: |
          set -euo pipefail
          test "$GITHUB_REF" = "refs/heads/main"
          version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' pack.toml | head -n 1)"
          test "$TAG" = "v$version"
          swatch verify
          cosign verify-blob \
            --bundle dist/release.json.sigstore.json \
            --certificate-identity "https://github.com/${GITHUB_REPOSITORY}/.github/workflows/release.yml@refs/heads/main" \
            --certificate-oidc-issuer https://token.actions.githubusercontent.com \
            dist/release.json
          for file in dist/*; do
            gh attestation verify "$file" --repo "$GITHUB_REPOSITORY"
          done
          swatch publish
          gh release upload "$TAG" \
            dist/release.json \
            dist/release.json.sigstore.json \
            --repo "$GITHUB_REPOSITORY"
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
            ".github/actions/setup-swatch/action.yml",
            ".github/workflows/check.yml",
            ".github/workflows/release.yml",
        ] {
            assert!(path.join(expected).exists(), "missing {expected}");
        }
        let check_workflow =
            fs::read_to_string(path.join(".github/workflows/check.yml")).expect("check workflow");
        let release_workflow = fs::read_to_string(path.join(".github/workflows/release.yml"))
            .expect("release workflow");
        let setup_action = fs::read_to_string(path.join(".github/actions/setup-swatch/action.yml"))
            .expect("setup action");

        assert!(setup_action.contains("releases/download/v0.2.0"));
        assert!(setup_action.contains("swatch-linux-x86_64.tar.gz"));
        assert!(setup_action.contains("release-manifest.sigstore.json"));
        assert!(setup_action.contains(".sha256"));
        assert!(setup_action.contains(".sha512"));
        assert!(setup_action.contains(
            "https://github.com/iamkaf/swatch/.github/workflows/release.yml@refs/heads/main"
        ));
        assert!(
            setup_action
                .contains("gh attestation verify \"$download/$archive\" --repo iamkaf/swatch")
        );
        assert!(setup_action.contains("swatch 0.2.0"));
        for generated_workflow in [&check_workflow, &release_workflow] {
            assert!(generated_workflow.contains("uses: ./.github/actions/setup-swatch"));
            assert!(!generated_workflow.contains("cargo install"));
        }

        let clean_check = "git status --porcelain=v1 --untracked-files=all";
        assert!(check_workflow.contains(clean_check));
        assert!(release_workflow.contains(clean_check));
        assert!(release_workflow.contains("test \"$GITHUB_REF\" = \"refs/heads/main\""));
        assert!(release_workflow.contains("test \"$TAG\" = \"v$version\""));

        assert!(release_workflow.contains("default: false\n        type: boolean"));
        assert!(release_workflow.contains("name: Retain verified release"));
        assert!(release_workflow.contains("if: ${{ inputs.publish }}"));
        assert_eq!(release_workflow.matches("contents: write").count(), 1);
        let publish_job = release_workflow
            .split_once("  publish:\n")
            .expect("publish job")
            .1;
        assert!(publish_job.contains("contents: write"));
        assert!(publish_job.contains("attestations: read"));
        assert!(publish_job.contains("name: verified-pack-release"));
        assert!(publish_job.contains("swatch verify"));
        assert!(publish_job.contains("cosign verify-blob"));
        assert!(publish_job.contains("gh attestation verify"));
        assert!(publish_job.contains("swatch publish"));
        for secret in [
            "GITHUB_TOKEN",
            "GH_TOKEN",
            "MODRINTH_TOKEN",
            "CURSEFORGE_TOKEN",
            "MAVEN_PUBLISH_USERNAME",
            "MAVEN_PUBLISH_PASSWORD",
        ] {
            assert!(publish_job.contains(secret), "missing {secret}");
        }
        assert!(publish_job.contains("gh release upload \"$TAG\""));
        assert!(!publish_job.contains("--clobber"));
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
