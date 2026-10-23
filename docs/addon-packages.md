# Addon packages

Nova ships the JavaScript sandbox and a local ZIP installer. It does not ship
AniKoto, Supernova, a default addon repository, or an automatic download source.
The production addon sources live in the separate sibling `nova-addons` checkout,
under `addons/<id>`. Copies under `crates/providers/test-fixtures/addons` are test
inputs and do not enter production builds.

## Install and update

Open **Settings → Addons → Install addon ZIP…**, then select a package from your
device. Nova validates the archive before publishing the installed addon. Remote
Stremio addon URLs continue to use the existing URL installation flow.

Packages retain the provider URL `nova-provider://<id>`, namespaced media and
episode IDs, and existing plugin setting keys. Importing the corresponding ZIP
makes a retained but unavailable AniKoto or Supernova installation usable again;
it does not change library identities. Reimporting identical bytes is idempotent.
An update must use a higher semantic version: downgrades and different bytes
claiming the same version are rejected. Synced selections obey the same rule;
a rejected selection cannot become active after restarting the app.

With sync enabled, paired devices share the desired addon list, settings and
package descriptors. A device missing an archive automatically requests it
from a currently paired peer. Package bytes travel over the separate
`nova/addon-package/1` protocol; ordinary records retain `nova/sync/6`. Transfer
is limited to 8 MiB, cancellable and timed out; removal from the paired mesh
revokes transfer authorization. The receiving app validates the ZIP and its
SHA-256, ID and version before making it available. If no peer has the package,
the desired installation remains unavailable until the archive can be obtained.
Settings → Sync includes addon installation in its status: it shows pending
packages, active addon downloads, outgoing ZIP transfers and retry waits instead
of reporting completion while required code is missing. A previous valid package
can remain active during an update; it does not count as completion of the new
descriptor. **Sync now** retries failed addon downloads immediately without
duplicating transfers already in progress. Validation and activation finish the
receiving stage, and removing a desired addon clears it from the pending count.

## Build a package

From the Nova runtime checkout, explicitly select the addon source checkout:

```sh
python3 scripts/package-addon.py --source-root ../nova-addons --all --output-dir dist/addons
python3 scripts/package-addon.py --source-root ../nova-addons anikoto --output-dir dist/addons
```

Choose either `--all` or one addon ID. The script runs the runtime's
`validate-package` example before publishing each `<id>-<version>.zip` and its
adjacent `.zip.sha256`. For an already built validator, pass
`--validator /path/to/validate-package`.

Only declared scripts, declared assets, both manifests, `LICENSE` and `NOTICE.md`
enter the archive. Sorted entries, fixed timestamps and stored compression make
identical inputs byte-identical. Development directories, symlinks and private
key file extensions are rejected. Packaging is separate from normal Nova builds.

## ZIP format v1

The archive contains these files at its root, or inside one enclosing directory:

| File | Contract |
| --- | --- |
| `manifest.json` | Plugin manifest with `formatVersion: 1`, lowercase ID, name, semantic version, entry script and permissions. |
| `stremio-manifest.json` | Stremio manifest with ID `nova.<id>` and the same version. |
| `index.js` or another declared entry | JavaScript implementing the runtime provider contract. |
| `LICENSE`, `NOTICE.md` | Package licensing and attribution. |
| Additional scripts/assets | Explicitly listed in `scripts` / `assets`; no undeclared files. |

A minimal plugin manifest is:

```json
{
  "formatVersion": 1,
  "id": "example",
  "name": "Example",
  "version": "1.0.0",
  "entry": "index.js",
  "permissions": { "domains": ["example.org"] }
}
```

When `scripts` is absent, Nova loads `entry` alone. Otherwise it assembles
scripts in the declared order, with the entry last. Assets are served only when
declared. Permissions, runtime limits, configuration fields and contextual stream
capabilities use the contracts in [the provider guide](../crates/providers/README.md)
and `crates/providers/src/runtime.rs`.

The parser caps archives at 8 MiB, expanded files at 16 MiB, entries at 256 and
assembled JavaScript at 2 MiB. It rejects traversal, unsafe paths, duplicates,
symlinks, encrypted entries, invalid identities and undeclared files.

## Local persistence

`addon_packages:v1` stores a JSON map from provider ID to
`{ "id", "version", "sha256" }`; records sync in the `addon_packages` domain.
`addon_package_ids:v1` retains known provider IDs locally so removed/unavailable
native IDs remain distinguishable from ordinary addon IDs. Immutable archives
live at `<app_data_dir>/addons/packages/<lowercase-sha256>.zip`. Only currently
registered descriptor content is served to peers; hashes are never arbitrary
file paths.

JSON repository catalogs, repository configuration, signing and automatic
repository updates are deferred. There is no default repository to configure in
this version.
