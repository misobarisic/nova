# Nova

Nova is a media catalog and player for **Linux, Windows and Android**, with
phone, desktop and TV interfaces. Browse Stremio-compatible addon catalogs,
play direct streams or torrents with the embedded **mpv** player, and optionally
sync your library between devices without a Nova account.

> **Alpha software:** Nova is under active development. Expect bugs, incomplete features, and breaking changes between releases.

## Features

- **Discovery and library:** search addon catalogs, browse recommendations,
  organize titles into categories, and filter your library with Smart
  collections. Continue Watching, watch history and new-episode rows help you
  pick up where you left off.
- **Playback:** audio and subtitle selection, subtitle appearance controls,
  chapters, bookmarks, screenshots, resume preferences, optional next-episode
  autoplay and a sleep timer.
- **Planning:** a synced watch queue for movies and individual episodes, with
  optional receiver-owned continuous playback; viewing history and statistics.
- **Offline viewing:** manual downloads and per-show Smart downloads that keep
  the next few episodes ready within device storage, network and charging
  limits. Automatic downloads require device opt-in.
- **Across devices:** opt-in, end-to-end encrypted library sync over
  [iroh](https://iroh.computer), paired-device remote playback, and watch
  parties. Party stream recommendations highlight matching sources already
  available through your own addons.
- **Profiles:** separate libraries, progress, addons and preferences, with
  optional PINs and restricted title/source allowlists.
- **Calendar and reminders:** browse upcoming releases, choose reminders for
  each new episode or a complete season, and export an ICS calendar. Desktop
  reminders appear in-app while Nova is open; Android supports OS notifications.
  Full-season reminders need confirmed season metadata.
- **Backup and import:** choose profiles and data to export, preview an import,
  merge or replace selected components, and review recovery changes after an
  import.
- **Integrations:** MyAnimeList and AniList tracking, Stremio library sync, and
  Simkl, MDBList and Trakt integrations. Account setup and supported behavior
  vary by service.

For details, see [library behavior](docs/library-behavior.md),
[player behavior](docs/player-behavior.md), [Smart downloads](docs/smart-downloads.md),
[retained casting implementation (disabled)](docs/casting.md),
[watch parties](docs/watch-party.md), [profiles](docs/profiles.md),
[Calendar](docs/calendar.md), [backups](docs/backups.md),
[anime tracking](docs/tracking-behavior.md), [Stremio sync](docs/stremio-sync.md)
and [list services](docs/list-services.md).

## Addons

Configure compatible Stremio-protocol addons in **Settings → Addons**.
JavaScript providers can also be installed from local ZIP packages. Nova does
not bundle JavaScript addons or an addon repository; AniKoto and Supernova
are packaged separately. See [addon packages](docs/addon-packages.md).

## Build and run

Nova uses Rust 2024 and [Slint](https://slint.dev). The Nix development shells
provide the platform toolchains and dependencies.

```sh
nix develop                    # desktop toolchain and native dependencies
cargo run --locked             # run the desktop app
cargo dev                      # hot-reloading UI preview (Linux debug only)

cargo check --workspace --locked
cargo test --workspace --locked
cargo test -p nova-sync --lib --locked
```

For an Android release build:

```sh
nix develop .#android --command make android-build-release
```

Android Cargo builds use `--no-default-features --features android,update-check`.
Android and Windows shells fetch pinned mpv/FFmpeg binaries from
[nova-vendor](https://github.com/misobarisic/nova-vendor); Linux uses system
libraries. See the [project map](docs/project-structure.md) for build targets,
platform dependencies and packaging details.

## Versions and updates

[Releases](https://github.com/misobarisic/nova/releases) provide tagged builds.
About shows the installed version and offers a manual update check. Clean,
matching tagged release builds can check GitHub daily while Nova is in the
foreground; development builds skip automatic checks, including when compiled
in release mode. Development versions include the last reachable tag and a
short commit hash, with `-dirty` for local source changes.

Keep the root Cargo version at the last release between tags. To prepare a
release, bump it on `main` and tag that commit `v<version>`; tagged CI rejects a mismatch.
The tag dispatcher runs the release pipeline in `main`'s cache scope while
checking out that exact tagged commit. Each release target has its own dependency
cache. `NOVA_RELEASE_TAG` supplies the release identity independently of the
workflow ref. A manual dispatch with `publish=false` builds and verifies all
artifacts without publishing. See [release CI and measurements](docs/release-ci.md).
Android's version name and installation code are derived from that version;
packaging reserves an installation epoch to keep current releases newer than
older placeholder builds. See the project map for the version-code scheme.

Packagers can omit automatic and manual update checks with
`cargo build --release --no-default-features --features desktop`.
For Android Make targets, use `UPDATE_FEATURES=`.

## Contributing

Read [AGENTS.md](AGENTS.md) for coding conventions and required checks.
[docs/project-structure.md](docs/project-structure.md) maps the workspace,
UI bridges, data flow, persisted state and protocols. App logic lives in
`src/`; the Slint UI and heavier subsystems live in leaf crates under `crates/`.
Behavior references and headless UI regressions live in `docs/` and `tests/`.

## License

Nova's original code is licensed under GPL-3.0-or-later; see [LICENSE](LICENSE).
Settings → About lists the build's Rust crates and bundled native components,
license texts and source links. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)
for the vendor inventory.
