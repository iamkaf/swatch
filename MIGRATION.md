# Migration

## Lockfile v2 to v3

Lockfile v3 adds hashes for files under `overrides/`, `client-overrides/`, and `server-overrides/`. Swatch still reads a v2 lockfile so existing packs can migrate in place.

Before:

```bash
swatch install
# pack.lock.toml remains version = 2
```

After updating Swatch, review the authored roots and run:

```bash
swatch install
git diff -- pack.lock.toml
```

Swatch rewrites the lock as version 3 and adds one `[[authored]]` entry per file. No manifest edit is required. Subsequent builds fail when authored bytes drift from those pins.

## Separate client and server archives

The old build path produced `<slug>-<version>.mrpack`. Swatch now names the two outputs explicitly:

```text
dist/<slug>-<version>-client.mrpack
dist/<slug>-<version>-server.mrpack
```

Use `swatch build client`, `swatch build server`, or `swatch build all`. Publication targets that consume a Modrinth pack use the client archive. GitHub publication includes both.

## Prepare before live publication

Previously, `swatch publish` built artifacts and uploaded them in one process. Live publication now requires a verified prepared snapshot:

```bash
swatch prepare
swatch verify
swatch publish
```

This lets CI sign and attest the prepared bytes before an upload step reads credentials. If `pack.toml`, `pack.lock.toml`, authored files, configured destinations, the source revision, or any prepared artifact changes, verification fails and the release must be prepared again.

`swatch publish --dry-run` remains a one-command preview. It prepares local artifacts, writes `dist/release.json`, and prints targets without credentials. It continues to work without a changelog, but a strict `swatch prepare` requires release notes when Modrinth, CurseForge, or GitHub publication is configured.
