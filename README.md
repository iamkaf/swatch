# Swatch

Swatch turns a small, exact-pinned Minecraft pack manifest into locked client and server archives. It also records the hashes of authored files, prepares release bytes once, verifies them without credentials, and publishes the verified files to configured destinations.

The manifest, lockfile, and `release.json` formats are experimental. Swatch will keep reading the current v2 lockfile while packs migrate to v3, but other format changes may be breaking until the tool has been exercised by more real packs.

## Build and install

Download the archive for your platform from [GitHub Releases](https://github.com/iamkaf/swatch/releases). Each Swatch release includes `release-manifest.json` with SHA-256 and SHA-512 hashes and a keyless Sigstore bundle. GitHub also records an artifact attestation for every native archive.

To build from source with stable Rust:

```bash
cargo build --release --locked
./target/release/swatch --help
```

## Start a pack

Create a complete pack repository:

```bash
swatch init my-pack \
  --name "My Pack" \
  --minecraft 26.2 \
  --loader neoforge \
  --loader-version 26.2.0
cd my-pack
swatch install
```

`init` writes `pack.toml`, a changelog, empty authored-content roots, a pack-owned `scripts/check` hook, and GitHub check and explicit release workflows. `--slug` defaults to the directory name and `--group` defaults to `org.example.packs`; set both before publishing a real pack.

The generated `scripts/check` is deliberately a no-op. Put gameplay, configuration, and pack-policy checks there. Swatch only requires a zero exit status and does not interpret pack semantics.

## Pin content

Add exact Modrinth versions to the manifest:

```bash
swatch add client-project --version 1.2.3 --client
swatch add datapack-project --version 2.4.1 --datapack
swatch add resource-pack-project --version 1.9.4 --resource-pack
swatch remove client-project
swatch install
```

`add` also accepts `--server` and `--shader`. With no placement flag, Swatch detects mod sides from Modrinth. Resource packs, datapacks, and shaders use their own Modrinth project types and the same exact-version resolution as mods.

`pack.toml` groups dependencies by placement:

```toml
format = 1

[pack]
name = "My Pack"
slug = "my-pack"
version = "0.1.0"
group = "org.example.packs"
minecraft = "26.2"
loader = "neoforge"
loader_version = "26.2.0"

[mods]
shared-library = "1.0.0"

[client_mods]
client-project = "1.2.3"

[server_mods]
dedicated-tools = "2.0.0"

[resource_packs]
resource-pack-project = "1.9.4"

[datapacks]
datapack-project = "2.4.1"
```

The generated `pack.lock.toml` records download URLs, sizes, SHA-1 and SHA-512 pins, and client/server requirements. Verified downloads live in `.cache/objects/<sha512>`.

## Lock authored files

Use the three authored roots for files that are not downloaded dependencies:

- `overrides/` is shared.
- `client-overrides/` is client-only.
- `server-overrides/` is server-only.

`swatch install` records each file's relative path, size, SHA-1, and SHA-512 in lockfile v3. Builds and publication stop if a file changes, appears, or disappears. Run `swatch install` only after reviewing an intended authored-file change. Symbolic links and editor or operating-system junk files are rejected.

## Build client and server archives

```bash
swatch build client
swatch build server
swatch build all
```

The client archive excludes server-only dependencies and authored files. The server archive excludes client-only dependencies and includes shared and server authored files. ZIP entries use a fixed timestamp and sorted paths, so the same inputs produce the same bytes.

## Prepare and publish a release

```bash
swatch prepare
swatch verify
swatch publish
```

`prepare` writes client and server archives plus `dist/release.json`. The JSON contract includes schema version 1, pack version, source revision when Git can provide one, artifact paths, media types, SHA-256 and SHA-512 hashes, and configured destinations. Preparation and verification do not read publication credentials.

When Maven publication is configured, strict preparation reads existing `maven-metadata.xml` without authentication and includes the merged bytes in `release.json`. This keeps later Maven releases exact and prepare-once. A private repository whose metadata cannot be read anonymously cannot use this release path yet.

`verify` checks the manifest, lockfile, authored files, destinations, every prepared byte, and the source revision when Git can provide one. A live `publish` loads that verified snapshot instead of rebuilding it. The generated GitHub release workflow signs `release.json` with keyless Sigstore, creates GitHub artifact attestations, verifies both, and only then creates the release.

An initialized pack declares GitHub as a destination without hard-coding an owner or repository. The generated workflow uses GitHub's `GITHUB_REPOSITORY` value. Set `publish.github.repository = "owner/repository"` when running a live GitHub publish elsewhere.

The existing preview remains available:

```bash
swatch publish --dry-run
```

It prepares a local preview and prints configured upload targets without using credentials. A live publish reads platform credentials only after `swatch prepare` has succeeded. See [MIGRATION.md](./MIGRATION.md) for the v2 lockfile and publication changes.

## CurseForge mappings

CurseForge artifact preparation uses Packwiz as an explicit adapter. Normal installs do not invoke Packwiz. Refresh mappings only when the CurseForge target needs them:

```bash
swatch install --curseforge
```

Swatch looks for `packwiz` on `PATH`. Set `PACKWIZ_BIN` when it lives elsewhere. Mapping additions and exclusions live in `overrides.toml`; the `[publish.curseforge]` table supplies the CurseForge project and author.

## Development

```bash
cargo fmt -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

The checked-in portable fixture runs without network access and covers NeoForge, exact pins, server-only content, the content-addressed cache, installation, side-specific archives, and publication preview.

## License

Use the repository's [security policy](./SECURITY.md) instead of a public issue for suspected vulnerabilities. Swatch is licensed under the [Apache License, Version 2.0](LICENSE).
