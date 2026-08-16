# Swatch

Swatch is a Rust command-line tool for authoring lockfile-first Minecraft modpacks. It reads a small `pack.toml`, resolves exact files into `pack.lock.toml`, verifies a content-addressed cache, and prepares the artifacts described by the pack.

The manifest and lockfile formats are still experimental. They may change before Swatch has been used by a second real pack.

## Build and install

Download the archive for your platform from
[GitHub Releases](https://github.com/iamkaf/swatch/releases). Each release also
includes `SHA256SUMS` for its archives.

To build Swatch from source with the stable Rust toolchain:

```bash
cargo build --release
./target/release/swatch --help
```

For local use, copy `target/release/swatch` into a directory on your `PATH`, or invoke it with its full path from a pack directory.

## Quick start

From a directory containing `pack.toml`:

```bash
swatch install
swatch publish --dry-run
```

`install` resolves the manifest when its lockfile is missing or stale, downloads and verifies every locked file, and keeps the verified bytes in `.cache/objects/`. Add or remove Modrinth content with:

```bash
swatch add example-mod --version 1.2.3
swatch add client-mod --client
swatch add server-mod --server
swatch add example-shader --shader
swatch remove example-mod
```

`publish --dry-run` prepares the same artifacts as a live publication and prints the configured destinations without uploading anything. Credentials are only read by a live `publish`.

## The pack manifest

Swatch currently accepts format `1` manifests. The `[pack]` table identifies the pack and its Minecraft loader:

```toml
format = 1

[pack]
name = "Example Pack"
slug = "example-pack"
version = "0.1.0"
group = "org.example.packs"
minecraft = "26.2"
loader = "neoforge"
loader_version = "26.2.0"
```

Content is grouped by where it can run:

- `[mods]` contains files required on both client and server.
- `[client_mods]` contains files that must stay off the server.
- `[server_mods]` contains server-only files and stays off the client.
- `[shaders]` contains client shader packs.

Each entry maps a Modrinth project slug to an exact project version:

```toml
[mods]
shared-library = "1.0.0"

[server_mods]
dedicated-tools = "2.0.0"
```

The generated `pack.lock.toml` records download URLs, sizes, SHA-1 and SHA-512 hashes, side requirements, and any CurseForge file mappings. The cache is keyed by SHA-512. `overrides/`, `client-overrides/`, and `server-overrides/` are copied into the exported Modrinth pack when present.

Swatch currently supports Fabric, Forge, and NeoForge manifests and exports Modrinth `.mrpack` files. A pack may also configure CurseForge, GitHub, or Maven publication in its `[publish]` table.

## CurseForge mappings

CurseForge artifact preparation uses Packwiz as an explicit adapter. Normal installs do not invoke Packwiz. Request mappings only when needed:

```bash
swatch install --curseforge
```

Swatch looks for `packwiz` on `PATH`. Set `PACKWIZ_BIN` when it lives elsewhere:

```bash
PACKWIZ_BIN=/path/to/packwiz swatch install --curseforge
```

CurseForge mapping additions and exclusions live in the pack's `overrides.toml`. The `[publish.curseforge]` table controls the project and author used for a CurseForge export.

## Development

```bash
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

The checked-in portable fixture under `tests/fixtures/` runs without network access. It uses a different pack identity, NeoForge, server-only content, a populated cache, install, and publication dry-run. This catches pack-specific assumptions before they ship.

## License

Swatch is licensed under the [Apache License, Version 2.0](LICENSE).
