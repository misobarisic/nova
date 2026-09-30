# nova

A cross-platform (Linux desktop + Android) media catalog / player app in the
style of Stremio. It talks to Stremio-protocol addons over HTTP, plays direct
URLs with an in-window **mpv** player, streams torrents through an embedded
BitTorrent client, and syncs library state across devices over
[iroh](https://iroh.computer) (opt-in, end-to-end encrypted, no account).

> **Alpha software:** Nova is under active development. Expect bugs, incomplete features, and breaking changes between releases.

- Language: Rust 2024, UI in [Slint](https://slint.dev) 1.18
- Root package `nova`; heavy subsystems (player, sync, torrent, storage, UI)
  live in leaf crates under `crates/` so app-logic edits stay cheap to build

## Quick start

```sh
nix develop              # desktop toolchain (rust, slint deps, mpv, …)
cargo run                # run the desktop app (binary: target/debug/nova)
cargo dev                # hot-reloading UI preview (Linux debug only)

cargo check              # whole workspace
cargo test               # workspace tests (unit + headless Slint UI tests)
cargo test -p nova-sync --lib   # fast sync-crate unit tests
```

Android Cargo builds use `--no-default-features --features android`. Enter
`nix develop .#android` before using Android Make targets; Make assumes the
required toolchain is already active. See `docs/PROJECT_STRUCTURE.md` for all
targets.

## License

Nova's original code is licensed under GPL-3.0-or-later; see [`LICENSE`](LICENSE).
The Settings → About page lists the Rust crates and bundled native components
used by the build, includes their license texts, and opens source links. The
catalog is generated from the locked dependency graph at build time. See
[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md) for the vendor inventory.

## Where things live

- `src/` — app logic + UI bridge (state, pages, playback, sync wiring)
- `crates/ui/` — Slint components (`.slint` sources of truth for the UI)
- `crates/sync/` — cross-device record sync over iroh
- `crates/player/` — in-window mpv playback (+ Android JNI glue)
- `crates/torrent/`, `crates/download/`, `crates/media/`, `crates/storage/`, `crates/config/`, `crates/addons/`
- `tests/` — headless Slint integration tests (run with `i-slint-backend-testing`, no display)
- `docs/` — `PROJECT_STRUCTURE.md` is the canonical map of the project
  (workspace layout, data flow, persistence keys, sync protocol); read it
  before exploring, and keep it current with structural changes

## Notes for contributors

- All Slint property/callback access is main-thread only; background work
  hops back with `slint::invoke_from_event_loop`.
- A Slint change is three steps: page component + `AppWindow`
  property/callback + `src/app/run.rs` wiring.
- User-facing Slint strings use `@tr("…")` (English source, Croatian
  catalog bundled); text the Rust side formats goes through
  `src/app/text.rs`.
- `cargo check` + the test suite are the gate; do not commit unless asked.

### Commit messages

Use Conventional Commit subjects:

`<type>(<scope>): <imperative summary>`

Keep the summary concise and lowercase after the colon (preserve proper names). Omit the scope when it does not help identify the change. Common types are `feat`, `fix`, `refactor`, `perf`, `ui`, `docs`, `test`, `build`, `ci`, and `chore`.

Examples:

```text
feat(player): add subtitle selection
fix(settings): prevent horizontal panning on Licenses
ci(release): publish Linux .deb and AppImage builds
```

For breaking changes, add `!` after the type or scope and explain the impact in the commit body, for example `feat(sync)!: change pairing protocol`.
