# AGENTS.md

Guidance for AI agents working in this repository.

## Start here

Use relevant sections of [`docs/PROJECT_STRUCTURE.md`](docs/PROJECT_STRUCTURE.md)
for orientation and code navigation. Treat it as a guide, not a substitute for
the source: inspect code, tests, and configuration as needed, and use search
when it helps answer the task. If the current change alters documented project
structure or behavior, update the matching section as described below. Flag
unrelated documentation drift without expanding the task.

## Keep the structure doc current

`docs/PROJECT_STRUCTURE.md` is a living document. Update it **in the same
change** whenever a task alters the shape of the project, including:

- Adding, removing, renaming, or moving a crate, module, or top-level source file.
- Changing a crate's responsibility, dependencies, features, or entry point.
- Adding/removing a Slint component or a page-level property/callback pattern.
- Adding/removing persisted data (KV keys, files, sync domains) or changing the
  storage backend.
- Changing the sync protocol (ALPNs, wire schema, peer/membership model).
- Changing platform behavior (desktop vs Android), build/packaging, or features.
- Adding a new "where to look for X" entry that a future agent would need.

Keep edits surgical: update the relevant table/section and the quick-reference
index. Do not rewrite the whole file, and do not add speculative sections for
things that do not exist. If you are unsure whether a change belongs in the doc,
add it only if it helps someone navigate the code without reading it.

## Build & verify

Run the narrowest check that covers your change, then the broader ones:

```sh
cargo check                         # whole workspace (default = desktop)
cargo test -p nova-sync --lib       # fast unit tests for the sync crate
cargo test                          # workspace tests (app unit + integration)
cargo test --test settings_sync_overflow   # headless Slint UI regression tests
```

- Android is built with `--no-default-features --features android`; verify
  desktop-only code is gated (`#[cfg(feature = "desktop")]`,
  `#[cfg(not(target_os = "android"))]`).
- Pull request CI runs default workspace and Windows GNU-target `cargo check`
  (see `.github/workflows/build-release.yml`); it does not run tests or Clippy.
  Run relevant tests locally for behavior changes.
- There is no dedicated lint target.
- If you add a test, put it next to the code (`#[cfg(test)] mod tests`) or in
  `tests/` for headless Slint tests.

## Conventions & gotchas

- **Comments are welcome here.** Match the surrounding style: explain *why*, not
  *what*. Do not strip existing rationale comments.
- **Leaf-crate split is intentional.** Heavy subsystems (Slint codegen, mpv,
  iroh, librqbit, redb) live in `crates/*` so app-logic edits don't trigger
  expensive recompiles. Keep new heavy subsystems in their own crate.
- **Slint changes are three-step:** page component + `AppWindow` property/callback
  + `src/app/run.rs` wiring. Missing a step is a common failure.
- **UI thread only** for Slint access. Hop back from workers with
  `slint::invoke_from_event_loop`.
- **Mark user-facing Slint strings with `@tr("…")`** (plural form:
  `@tr("{n} item" | "{n} items" % n)`). The `nova-ui` catalogs bundle at build
  time (`crates/ui/translations/<code>/LC_MESSAGES/nova-ui.po`; English is the
  source language, Croatian is the second one); the language is picked in
  Settings → Display and applied by `src/app/i18n.rs`. Adding a language =
  catalog + a `nova_config::Language` variant + the `src/app/text.rs` table.
  Catalogs are context-free (`build.rs`), so an entry is keyed by the source
  string alone.
- **Text the Rust side formats** (statuses, hints, dates, counts) goes through
  `src/app/text.rs` — keyed by the English source as well, so both mechanisms
  stay greppable. Stored *identifiers* (the automatic library buckets,
  `WatchStatus` labels) keep their English value and are translated for display
  only (`category_labels`, `text::tr(badge_label())`).
- **`ApplyingGuard`** must wrap remote-apply paths so writes don't echo back into
  sync.
- **postcard wire schemas are not self-describing:** changing/reordering fields
  in `crates/sync` wire structs requires an ALPN version bump. Local storage
  stays JSON.
- **Peer membership:** only explicit pairing/manual add may re-add a removed
  peer; routine syncs must never resurrect a tombstoned peer.
- **`Store::load` quarantines** an unreadable sync blob (`sync:records.corrupt.*`)
  instead of wiping it — check there if sync state looks empty.
- **The project was renamed `sl` → `nova`.** Data on disk and on the wire moved
  with it: `~/.local/share/nova` + `~/.cache/nova`, `nova.redb`, the Android
  package `dev.misob.nova`, the gettext domain `nova-ui`, `NOVA_*` env vars, the
  `NV1` invite-ticket prefix and the `nova/{sync,pair,remove}` ALPNs. Old wire
  names are deliberately *not* accepted: a Nova install cannot pair or sync with
  a `sl` one.

## Git

- Do **not** commit, amend, push, or open PRs unless the user explicitly asks.
- Check `git status` before editing or staging, and preserve unrelated existing
  work.
- Stage only intended files. Do not add credentials or private production
  signing keys.
- `android/keystore/debug.keystore` is the intentionally tracked public debug
  key used by Cargo's `dev`, `release`, and `dev-release` Android signing
  profiles. Preserve it for local/debug builds; never use it for published
  releases.
