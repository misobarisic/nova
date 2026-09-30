# nova — Project Structure

> Orientation map for AI agents and new contributors. Read this first instead of
> crawling the tree: it explains what every crate/module is for, how data flows,
> where state is persisted, and which files to open for a given task.

`nova` is a cross-platform (Linux and Windows desktop + Android) media catalog / player app in
the style of Stremio. It talks to Stremio-protocol addons over HTTP, plays
direct URLs with an in-window **mpv** player, and can stream torrents through an
embedded BitTorrent client. A recent feature adds **cross-device sync** over
[iroh](https://iroh.computer) (the `iroh` branch).

- Language: Rust 2024, UI in [Slint](https://slint.dev) 1.18
- Root package `nova`; heavy subsystems split into leaf crates under `crates/`
- Two entry points: desktop `src/main.rs`, Android `android_main` in `src/lib.rs`; both call `app::run()`

---

## 1. Quick start

```sh
# Dev shell (Nix) — provides Rust, Slint, mpv, and native dependencies
nix develop              # Linux desktop toolchain
nix develop .#android    # + NDK/SDK/JDK/cargo-apk2
nix develop .#windows    # MinGW cross-build tools + pinned libmpv

# Build / check
cargo check              # whole workspace (desktop default features)
cargo build              # desktop binary: target/debug/nova
cargo run                # run desktop app
cargo dev                # alias: run --features live-preview (hot-reload UI, Linux debug only)

# Make targets (enter `nix develop` or `nix develop .#android` first)
make linux-run
make linux-run-preview
make linux-run-release
make android-build
make android-build-release
make android-build-release-universal
make android-run
make android-run-release
# For an emulator: append ANDROID_TARGET=x86_64-linux-android

# Tests
cargo test                              # workspace tests (app unit + integration)
cargo test -p nova-sync --lib            # sync crate unit tests (fast)
cargo test --test settings_sync_overflow # headless Slint UI regression tests

# Android Make rules use cargo-apk2 (not cargo-apk): it compiles android/java/
# to DEX and declares the foreground service + sync job. They enable the
# Android Slint backend with `--features android`; enter `nix develop .#android`
# before invoking them. Make targets never enter Nix shells themselves.

# Windows x86_64 check (cross-build from Linux)
cargo check --workspace --locked --target x86_64-pc-windows-gnu
```

Pull requests run host and Windows-target Cargo checks and exercise the Linux
release packaging job. Pushing a Git tag builds three Android APKs, a Linux
x86_64 `.deb` and AppImage, and a Windows x86_64 ZIP, then attaches all six
files to a GitHub Release. Android release filenames are
`nova-arm64-v8a-<tag>.apk`, `nova-x86_64-<tag>.apk`, and `nova-<tag>.apk`;
the APK without an architecture suffix contains both arm64-v8a and x86_64.
The Windows bundle is named `nova-windows-x86_64-<tag>.zip`. Linux release
files are named `nova-x86_64-<tag>.deb` and
`nova-x86_64-<tag>.AppImage`; both come from one native Ubuntu 22.04 build.
The `.deb` declares shared-library dependencies, while the AppImage bundles
libraries using linuxdeploy. Nova uses Slint's Winit/FemtoVG backend; Qt is
not required.
The Android workflow invokes cargo-apk2 once to build both architectures into
a universal APK. `.github/scripts/package-android.py` derives the two
architecture-specific APKs from that output, aligns and signs all three with
the same release key cargo-apk2 uses (environment override, then Cargo metadata),
and checks signatures, signing certificates, exact ABI sets, native libraries,
and unchanged payload bytes/compression. All three APKs share one Actions
artifact for the release job to collect.

Cargo aliases live in `.cargo/config.toml`. Android linkers come from the
`.#android` dev shell env vars (see `.cargo/config.toml` comments); that shell
also provides `cargo-apk2` (packaged by the flake) and unsets `CPATH` (the
stdenv's host include path, which the NDK clang would otherwise pick up).

**Features** (root `Cargo.toml`):
- `desktop` (default): enables Slint's Winit/FemtoVG backend.
- `android`: enables Slint's Android Activity backend (Skia on Android);
  Android builds use `--no-default-features --features android`.
- `live-preview`: interpreter-backed, hot-reloading Slint UI (Linux debug only; pulls in the Slint interpreter).

---

## 2. Workspace layout

```
nova/
├── .github/                  # GitHub Actions automation
│   ├── scripts/package-android.py # derive, align, sign, verify APK variants
│   └── workflows/
│       └── build-release.yml  # PR checks; tagged Android + Windows release builds
├── Cargo.toml                # workspace + root package `nova`
├── about.toml                # accepted SPDX licenses for the in-app catalog
├── build.rs                  # license catalog generation; native link directives
├── .cargo/config.toml        # `cargo dev` alias; Android linker notes
├── flake.nix                 # devShells: default, android, windows cross-build
├── Makefile                  # Linux and Android build/run targets (use active shell)
├── android/                  # Android source, signing key, and resources
│   ├── java/                 # Java services and activities compiled to DEX by cargo-apk2
│   ├── keystore/
│   │   └── debug.keystore    # Tracked debug key for local APK signing; not for publishing
│   └── res/                  # Android launcher icon mipmaps
├── assets/                   # App logo, fonts, and in-app license/vendor catalog templates
├── vendor/                   # pinned native library inputs and provenance
│   ├── android-libs/         # prebuilt libmpv.so per ABI + SOURCES provenance
│   └── windows-libs/         # Windows libmpv archive pin and SOURCES provenance
├── src/                      # root app crate `nova`
├── crates/                   # leaf crates (see below)
│   ├── ui/translations/      # UI translation catalogs: <code>/LC_MESSAGES/nova-ui.po (build-time bundled)
│   └── download/             # durable stream jobs + progressive HTTP transfer
├── tests/                    # headless Slint UI integration tests
└── docs/
    ├── PROJECT_STRUCTURE.md  # this file
    ├── PLATFORM_STORAGE.md   # per-platform data and cache locations
    ├── sync-followons.md     # sync feature parking lot / status notes
    ├── sync-hardening-plan.md # prioritized correctness/recovery implementation plan
    └── repo-follow-ons.md    # repository maintenance follow-ons
```

### Crate dependency graph

```
                                 ┌─────────────┐
                                 │ nova (root) │  binary + lib (cdylib/rlib for Android)
                                 └──────┬──────┘
  ┌──────────────┬──────────────┬───────┼──────┬──────────────┬──────────────┐
  ▼              ▼              ▼              ▼              ▼              ▼
  nova-ui        nova-player    nova-media     nova-download  nova-torrent   nova-sync    addons
  │              │              │              │              │
  slint          nova-config,   nova-config,   serde,         nova-config,   (standalone)
                 nova-ui,       slint,         tokio,         nova-storage,
                 slint          addons,        reqwest        iroh,tokio
                                image,webp

                                                              │
                                                              nova-storage (redb)
```

| Crate | Path | Role |
|---|---|---|
| `nova` | `src/` | App logic, UI bridge, entry points. Depends on everything. |
| `nova-ui` | `crates/ui` | Compiled Slint components (`.slint` → Rust via `include_modules!`). Leaf, so UI edits don't recompile app logic. |
| `nova-config` | `crates/config` | Shared settings types, runtime cache settings, platform app paths, playback-rate bounds/helpers (`clamp_playback_speed` / `quantize_playback_speed`), `fnv1a`, `now_secs`/`now_ms`. Leaf. |
| `nova-storage` | `crates/storage` | Platform-agnostic persistent KV store (redb) at `<data>/nova.redb`. Leaf. |
| `nova-media` | `crates/media` | HTTP transport (`net`) + decoded/poster image cache (`cache`); artwork decoding is limited to JPEG, PNG, and WebP. |
| `nova-download` | `crates/download` | Durable stream-job model, manifest helpers, cancellation, and progressive/resumable HTTP file transfers. |
| `nova-player` | `crates/player` | In-window mpv player (desktop + Android) + external launch: the *video* app (`open_external` — desktop target app, Android video-MIME `ACTION_VIEW` for stream fallback) and the system *browser* (`open_browser` — `xdg-open`, or on Android `ACTION_VIEW` marked `BROWSABLE` + `FLAG_ACTIVITY_NEW_TASK` so only web-link handlers can claim it, for links like addon config pages); Android JNI glue. |
| `nova-torrent` | `crates/torrent` | Embedded BitTorrent (librqbit); resolves `infoHash` → loopback HTTP URL for mpv. |
| `nova-sync` | `crates/sync` | Cross-device sync over iroh (generic record store + ALPN protocols); emits structured `tracing` events/spans, with subscriber setup owned by the root app. |
| `addons` | `crates/addons` | Stremio addon protocol: URL builders + response parsers; optional blocking HTTP client (`client` feature). |

---

## 3. Root crate `nova` (`src/`)

### Entry points
- `src/main.rs` — desktop `main`; installs jemalloc on Linux, calls `nova::app::run()`.
- `src/lib.rs` — re-exports leaf crates under stable paths (`crate::storage`, `crate::player`, `crate::torrent`, `crate::download`, `crate::net`, `nova_ui::*`), declares `pub mod app`, and holds `android_main` (Slint Android backend init + `app::run()`).
- `src/diagnostics.rs` — initializes the process-wide `tracing-subscriber` once without replacing an embedding subscriber. `RUST_LOG` filters events (default `warn,nova_sync=info`; `RUST_LOG=nova_sync=debug` enables sync diagnostics). Desktop writes to stderr; Android writes directly to logcat under `Nova`, including job-only startup. Sync leaf code emits structured events/spans through `tracing`; it does not install a subscriber.

### `src/app.rs` — the spine
Defines the shared state and the UI bridge:
- **`Shared`** (`Arc<Mutex<Shared>>`): all app state — installed addons, type/catalog defs, current catalog + `MetaPreview`s, library `entries`, `progress`, home `continue_list`/`upcoming_list`, modal/stream state, cache settings, detail snapshots. UI callbacks lock this.
- **`Bridge`** (`Clone`): `slint::Weak<AppWindow>` + `Arc<Mutex<Shared>>` + `catalog_gen` + desktop poster pipeline + `player` + `DownloadCoordinator`. Every UI-mutating method runs on the main thread.
- Data model types: `Installed`, `CatDef`, `TypeDef`, `StreamSource`, `StreamUi`, `ModalItem`, `DetailSnapshot`, `EpisodeProgress`, `PlaybackTarget`, `ContinueEntry`, `UpcomingEntry`, `LibraryEntry`, `AddonStore`, `WatchStatus`, `MetaHeader`, `CacheSettings` (from config).
- Desktop poster pipeline: `PosterStore` (capped, insertion-ordered), `PosterTx` (dual-priority `hi`/`lo` mpsc channels), worker pool.
- Declares the `app` submodules (below) and re-exports their `pub(crate)` items.

### `src/app/*` module map

| File | Responsibility |
|---|---|
| `run.rs` | `app::run()`: window setup, renderer env (`SLINT_BACKEND=winit`, `SLINT_RENDERER=femtovg`), poster worker pool, **all callback wiring** (`app.on_*`), storage init, startup restore, `start_sync`. |
| `bridge.rs` | `Bridge::new` / `Bridge::app()` accessor. |
| `addon_mgr.rs` | Install/remove/refresh addons, manifest cache, addon picker rows, and the Settings → Addons row model (`apply_addon_rows`): the Configure button is offered only once a probe of `<addon base>/configure` has answered 2xx, and opens that page through `player::open_browser` (the system browser on both platforms). Rows print the addon name only; `addon_copy_link` copies the install URL (`row.url`, still in the row model) to the clipboard. |
| `catalog.rs` | Discover: catalog fetch and pagination, metadata prefetch, addon/type/catalog/genre pickers, debounced global search after 2 characters, title-relevance ranking of merged results, stale-query invalidation, independent search-result pagination, and device-local recent-search history (20 unique queries, most recent first). Back cancels pending work and clears the query. |
| `detail.rs` | Detail modal: meta fetch, seasons/episodes (**paginated 50 per page** — `EPISODE_PAGE_SIZE`, page stored on the modal), stream list, per-addon filter pills, pinned download rows. Streams render per addon as each answers (no wait for the slowest), are stably sorted by installed addon order, and the request's downloaded rows are pinned on top from the moment the search starts. |
| `downloads.rs` | Durable one-at-a-time stream download queue, HTTP/torrent workers, pinned-row state, auto-delete-watched, and the Settings → Downloads → Downloaded episodes list (`completed_episode_jobs`). |
| `episodes.rs` | Pure episode/progress helpers: ordering, labels, badges, filters, `progress_map_key`. Series with episodes still to come never complete (unaired episodes can never be watched); a dated-complete series reads "Caught up" (plus the unaired tail when there is one). |
| `home.rs` | Home page: Continue Watching + Upcoming rebuilds. Continue Watching offers the latest in-progress episode (whatever its air date — starting it was explicit), or — once the latest episode is finished — the first dated, released, not-yet-watched episode (via `next_episode_to_watch`, `episodes.rs`), so the next episode appears before it is started. Dateless episodes never surface on their own (no schedule to count down to); the Upcoming caught-up check (`upcoming_tally`) skips them the same way, and the library "N left" badge counts dated episodes only (a started dateless episode still earns ▶ Resume, which outranks even "Caught up"). **Movies are covered too** (one entry while in progress, dropped once watched — they have no "next"): the per-item decision is the pure `continue_resume_id` helper (unit-tested here). Items the user removed from Home are filtered by `continue_hidden` (`id -> removal unix secs`): an id stays hidden while `hidden_at >= updated_at_secs`, so any later resume (local, or a synced peer's progress) brings it back, and the map is pruned/rewritten as progress moves past it. Card menus (Play, Enter series, Remove from Continue Watching) dispatch `Bridge::continue_picked` (resume-and-play; the card tap too), `continue_enter` (open the detail page without starting playback) and `continue_remove`, each resolving the card back to its library entry. Triggers are Library-parity: a desktop right-click pops the native `ContextMenuArea` menu (subpage cards own theirs; the landing rail hosts one that `CarouselDrag` shows at the cursor, since the overlay owns that gesture surface), while touch opens the page-level `MenuSheet` — whose own Cancel row closes it — from both a hold and a mouse right-click. |
| `library.rs` | My Library: entries, categories, persistence (`read/write_persisted_library`). Entering My Library always prefetches episode metadata — the Settings "Prefetch episode metadata" toggle gates Discover only (`catalog.rs`). Full behavior matrix (buckets, badges, checkmarks, menus, categories): `docs/library.md`. |
| `playback.rs` | In-app playback flow, episode progress tracking (movies included: `open_player` arms a `PlaybackTarget` keyed to the movie's own id, and a resume clears any Continue Watching hide stamp), the per-device playback-rate methods (`apply_playback_speed` / `nudge_playback_speed`, applied to the live mpv session and persisted debounced), torrent settings runtime cache. |
| `posters.rs` | Poster / backdrop / episode-thumbnail image pipeline (desktop worker pool + Android fetch path). |
| `settings.rs` | Settings page: cache + torrent settings, episode resume behavior, maintenance, `read/write_settings`. `wire_settings_autosave` captures edits immediately and owns the application-lifetime 600 ms persistence debounce, shared with playback-rate controls. |
| `streams.rs` | Stream-row mapping (`StreamSource`) and display-text helpers. |
| `sync.rs` | Cross-device sync integration: decompose app state into records, apply remote records, Settings → Sync UI, pairing events. |
| `qr.rs` | QR encoding of the sync invite ticket: renders the `NV1` ticket to a Slint image (`image-rendering: pixelated`) for desktop and Android. The Android scanner decodes the same payload, so display and scan agree by construction. |
| `android_bg.rs` | Android-only JNI glue (`#[cfg(target_os = "android")]`): starts/stops/updates the download foreground service and schedules the periodic sync `JobScheduler` job; hosts the headless sync JNI entry. Android API calls live in `android/java/`, so Rust only loads a class through the Context class loader and calls a static method. |
| `android_qr.rs` | Android-only JNI glue (`#[cfg(target_os = "android")]`): launches `QrScanActivity` and decodes each camera frame's luma plane with `rqrr`, then joins the scanned invite on the UI thread. The camera pipeline itself (permission, Camera2 session, preview, `ImageReader`) is Java. |
| `io.rs` | KV JSON read/write, atomic file writes, hashing, formatting. |
| `clipboard.rs` | Best-effort system clipboard writes (`copy_to_clipboard`): Android via the activity's `ClipboardManager` over JNI (`player::set_clipboard`), desktop via `wl-copy`/`xclip`/`xsel`. Used by Settings → Sync "Copy identity"/"Copy invite code" and Settings → Addons "Copy link". |
| `i18n.rs` | UI language (Settings → Display → Language): `apply_language` points Slint's bundled catalogs at the stored language (`slint::select_bundled_translation`) and hands the same setting to `text.rs`, and `language_labels` builds the picker list from `nova_config::Language::ALL`. |
| `text.rs` | Text the backend formats itself (download states, sync status, stream/episode hints, relative air dates, count labels) in the current language: `tr(english)` for fixed strings, named helpers for templated ones, plus the Croatian 1/2-4/5+ noun forms. The `@tr` catalogs cannot reach Rust code, so this is their counterpart. |
| `tests.rs` | Unit tests for the app modules (moved out of `app.rs`). |

### Threading model (important)
- **All Slint property/callback access is main-thread only.** `Bridge` methods are called from UI callbacks on the main thread.
- Background work (HTTP, decode, torrent, sync) runs on worker threads or tokio runtimes and hops back with `slint::invoke_from_event_loop(...)`.
- `nova-media::net` uses a **fire-and-continue** API: callers pass a continuation, never block or spawn threads.
- Remote sync callbacks (`set_on_remote`, `set_pair_callback`) arrive on the tokio runtime thread and are marshalled to the UI thread in `start_sync` (`sync.rs`).
- Stream downloads use the `nova-download` worker/queue; progress is read by the 250 ms UI tick, so no Slint access occurs on transfer threads. The tick refreshes the pinned download rows **in place** (`set_row_data`) when the row set/order is unchanged, instead of replacing the whole `streams` model: replacing the model destroys every row delegate, including the Android hold-to-open action-sheet timer, which then never fires while a transfer is active. The same tick reaps a transfer worker that ended without finalizing (panic/unwind), so a dead worker cannot pin the single active slot and stall every queued download.

---

## 4. UI crate `nova-ui` (`crates/ui`)

- `build.rs` compiles `appwindow.slint` with `slint-build` (AOT), except Linux debug + `live-preview` → interpreter/hot-reload. `SLINT_EMIT_DEBUG_INFO` toggles debug info.
- Runtime Slint dependencies disable default features: desktop enables Winit/FemtoVG, Android enables the Activity/Skia backend, and neither includes the software renderer.
- `src/lib.rs` is just `slint::include_modules!()`. The root `AppWindow` owns every property/callback the backend drives and forwards to page components.

### Translations (i18n)

- **User-facing strings are marked with `@tr("…")`** in the `.slint` files. The whole interface is covered; string *values* that reach the UI as data (addon names, stream labels, disk sizes) stay untranslated. Mark new strings as you add them — Slint falls back to the source text when a catalog has no entry, so marking is always safe.
- **Counts that inflect use Slint's plural form**: `@tr("{n} episode" | "{n} episodes" % count)` (the syntax is `"singular" | "plural" % n`, extra `{}` args follow). The `hr` catalog carries the three Croatian forms and the header's `Plural-Forms` selects them.
- `build.rs` points `slint-build` at `translations/`, so the gettext catalogs are compiled into the binary at build time (domain = `CARGO_PKG_NAME` = `nova-ui`, i.e. `translations/<code>/LC_MESSAGES/nova-ui.po`). The directory must hold at least one catalog — `translations/en/` exists for exactly that reason and is deliberately header-only: English is the source language, and a missing entry falls back to it.
- **Catalogs are context-free**: `build.rs` sets `DefaultTranslationContext::None`, so an entry is keyed by the source string alone. Slint's default context is the *component* a string sits in, which would silently unmatch an entry when a `@tr` literal moves to another component. Regenerate/verify catalogs with `slint-tr-extractor --no-default-translation-context`; a string that means two different things in two places can still name a context (`@tr("ctx" => "…")`).
- **Text the Rust backend formats goes through `src/app/text.rs`** (download states, sync status, stream/episode hints, relative air dates, count labels). Fixed strings are keyed by the English source there too (`text::tr("Queued")`); anything with a value in it has a small named helper (`text::searching_streams(n)`), and Croatian noun forms (1 / 2-4 / 5+) come from the internal `plural` helper. `Bridge::apply_language` calls `text::set_language`, so both mechanisms follow the same setting.
- **Stored identifiers are never translated, only their labels.** The automatic library buckets (`Plan to Watch`, `Watching`, …) and the `WatchStatus` labels are *keys*: the filter comparison, `auto_bucket` and the persisted values use the English identifier, while the UI shows a localized label — `library.rs` pushes `category_names` (values) plus `category_labels` (display) and `Dropdown` renders the latter (`labels` property) while `selected` still reports the value. Watch-status badges are translated at the two display sites (`text::tr(badge_label())`).
- **Languages**: English (source) and Croatian (`hr`, `crates/ui/translations/hr/…` + the table in `src/app/text.rs`). Croatian addresses the user in the formal plural ("vi"); labels stay short, which the Croatian-width phases in `settings_addon_row_fit.rs` / `settings_sync_overflow.rs` enforce (a longer label overflowed a 320px phone row, and one unwrapped card heading widened the whole Sync subpage).
- Switching language is one Slint call — `slint::select_bundled_translation(code)` in `src/app/i18n.rs` — which marks every translation dirty, so all `@tr` bindings re-evaluate in place (no page rebuild, no strings pushed from Rust). `"en"` restores the source strings. The chosen language is Settings → Display → Language (`nova_config::Language`), per-device and never synced.
- Adding a language: catalog directory + a `Language` variant (`ALL`/`index`/`code`/`label` arms in `crates/config/src/lib.rs`; the picker list is built from `ALL`, so the row follows automatically) + the `text.rs` table. Untranslated strings stay English.
- The interpreter-backed dev build (`cargo dev` / `live-preview`) compiles the `.slint` files at runtime; the ahead-of-time generator is the only one that emits the catalog registration, so that build can never select a translation. `apply_language` reports this **once** (then stays quiet — every settings mirror calls it) and the strings stay English there. Use `cargo run` / `cargo build` to see translations.

### Slint files

| File | Component / role |
|---|---|
| `appwindow.slint` | Root `AppWindow`: all backend properties/callbacks, screen switching, player overlay wiring. |
| `types.slint` | Shared structs: `MediaCard`, `StreamRow` (including pinned download state), `EpisodeRow`, `AddonRow`, `CategoryRow`, `SyncPeer`, `SyncInvite`, `TrackRow`, `SeasonCard`, `ContinueRow`, `UpcomingRow`, `SheetItem`. |
| `discover.slint` | Discover browse grid + addon/type/catalog/genre dropdowns in a horizontal Flickable (readable pill widths, early gesture-axis hints keep the whole-page scroll from stealing horizontal drags). Keyboard focus reveals off-screen filters. Global-search results have their own model and scroll offset. Typing starts a debounced search after 2 characters; results remain open while editing shorter queries, and each result delegate owns a one-shot Home-style fade/slide reveal, independent of poster/model updates. Results fade through on submit/back; the browse selection and scroll position are preserved. Column count / card width subtract the wide-layout rail (`rail_w`, 0 on narrow). |
| `home.slint` | Home landing: Continue Watching / Upcoming as horizontal carousels (~2.5 cards visible on narrow), with tappable headers opening vertical "see all" subpages. Both rails are interactive `Flickable`s (native inertia); card `TouchArea`s hint the gesture axis early, blocking the landing page pan while a horizontal drag belongs to the rail and handing vertical gestures back to the page, like Detail's stream-filter pills. Landing cards own taps, hold/right-click menus (native `ContextMenuArea` on desktop, page-level `MenuSheet` on touch), and drag-to-tap suppression; cards and rails disable while the subpage covers them. Scroll offsets (`home_continue_x`, `home_upcoming_x`, etc.) ride on `AppWindow` to survive detail navigation. The subpage slides in and staggers rows; keyboard focus follows only keyboard navigation. Continue cards show Next up / New Episode pills or a resume rail; hover lift is pointer-only to avoid sticky touch hover. The Upcoming subpage also offers a month calendar (`CalToggle` / `CalDayCell`); `home.rs` owns its date arithmetic and selection. |
| `detail.slint` | Detail modal (largest file): tabs, seasons, episodes, streams, right-click/hold stream action sheet, and pinned download status rows. The page scroll is a raw `Flickable` (not a `ScrollView`: the axis lock needs the inner Flickable's `interactive`) with a slim custom scroll indicator (`page_scroll_bar`, the fluent thumb's stand-in) pinned to its right edge. Its top bar is **icon-only** (`TopIconButton`): back, a two-state library bookmark (`IcBookmark`/`IcBookmarkBorder`), and `IcTag` categories with a count badge. The Episodes tab renders **one page of 50 episodes** at a time (pagers above and under the grid; card picks pass `episode_page_start + i`, so they still resolve against the whole season), and the addon filter pills keep a single width while queries are in flight (the loading spinner replaces the pill's trailing padding). The pill bar (`StreamFilterBar`) sits in flow with the streams — one instance per stream list — and pans by dragging on touch; on desktop it also gains step chevrons on both ends (chevron clicks page-step `content-x` by one viewport), whose slots are always laid out so the overflow flag can't loop with the viewport width, fading in only while useful. Its drags are axis-locked against the page scroll (`DetailPage::page_pan_enabled`): the page is a raw `Flickable` there, and the pills report every drag frame (`pan_hint` / `pan_release`, plus the row's own `content-x` changes) so the page's pan is blocked for the gesture and handed back when it turns vertical. Without it the page wins the race — nested Flickables have no axis lock (whoever passes 8px first owns the gesture, and a Flickable never gives a captured gesture back), and a finger on the 30px strip wobbles vertically far enough to trip it. A Home Continue tap deep-links straight to the resume episode's streams (`detail_deep_stream`); system back then closes the modal instead of revealing the skipped episode list, while a manually picked episode keeps the streams → episode-list step. Pinned download rows show the status text (`Downloaded` once complete — the file name is dropped) over a left-anchored progress rail. The picked-episode label lives on a tappable bar above the stream list (`episodes_back`), not in the top bar; that bar's chevron box fills the bar height so the icon centres on the bar (a HorizontalLayout top-aligns a plain `Rectangle` child). |
| `detail.slint` (stream paging) | Stream lists render Rust-provided 25-row slices with previous/next controls above and below the rows; `stream_page_start` keeps card picks and keyboard focus indexes absolute across pages. Using a bottom pager scrolls the outer detail page back to the stream-list start. |
| `library.slint` | My Library grid + the category filter bar ("All" chip + dropdown). Grid geometry subtracts the wide-layout rail (`rail_w`) like Discover. The bar is gated on `category_names` — the dropdown's own model, which always holds the automatic buckets (Plan to Watch / Watching / Completed / On Hold / Dropped) ahead of the user's categories (`category_rows` is the *user* list, also used by Settings → Categories and the detail category picker). The grid restores its absolute scroll offset only once it has measured (`grid_ready` — the restore timer retries rather than adjusting against a half-built layout, whose zero `cols`/`item-width` would jump the listing), and the restore reveals the focused card only when its row is *fully* hidden, so returning from an entry keeps the exact position. |
| `settings.slint` | Settings (largest file): Addons, Categories, Image cache, Display (including the UI **Language** picker, applied by the backend), Player (backend + decoder + episode start behavior), P2P/Torrents, Look and feel, Sync, Downloads, and nested "Downloaded episodes" (section 9 under Downloads) and "Licenses" (section 11 under About) pages. The About landing entry opens app information; its Licenses button opens the source links and generated license-text catalog. Source-link buttons are vertically centered and compact on narrow layouts while retaining 44px touch targets; source rows fit the viewport, and the license catalog character-wraps long tokens to prevent horizontal panning. Addon rows show the addon **name only** — the install URL is not printed as the row description (it wrapped over several lines); a "Copy link" pill copies it instead (`addon_copy_link`). Row actions are icon buttons on both layouts (`AddonIconButton`: configure cog `IcSettingsGear`, refresh `IcSync`, remove `IcDelete`, ↑/↓ text glyphs — all 32px on wide, 44px touch targets on narrow) plus the "Copy link" pill, which keeps its label; icon buttons carry translated accessible names. The wide row is a single aligned 32px line (toggle, cog, Copy link, Refresh, Remove, ↑, ↓); the narrow `AddonRowCard` puts toggle + name + move arrows on the first line (toggle vertically centered on the arrow line) and the actions underneath, so the name keeps full width. The keyboard columns (0–6) walk that same order in both. The Sync section renders the invite ticket as a QR image and (Android only) offers a "Scan QR code" button that opens `QrScanActivity`. `SettingsRow` sizes itself from a hidden measurement of its wrapped title+description (not a fixed line count), so long titles don't clip on narrow screens. |
| `player.slint` | Player overlay (OSD, controls, tracks, subtitles). On Android the volume control is hidden (volume pinned to 100% at play start; swipe adjusts after) and play/pause (larger, on a dark base for bright video) with ±10s sit in a centered floating transport cluster declared before the OSD so the track popups and settings modal paint above it; the bottom bar keeps the seekbar, gear and the timestamp (shown on narrow too). The gear's settings panel has submenus for subtitles, audio, the Android decoder and — on every platform — **playback speed** (Up/Down step it by 0.05; the value is the same per-device setting Settings → Player edits). The video backdrop owns taps (OSD toggle), double-taps on the outer thirds (∓10 s seek: the first tap acts normally, the second seeks and re-wakes), a 500 ms press-and-hold (transient 2× via `playback_speed_preview`, restored on release) and, on Android only, vertical swipes (left = window brightness, right = system volume via `android/java/.../PlayerFx.java` + `src/app/android_player.rs`); popups dismiss and OSD wakes stay immediate. |
| `categories.slint`, `dropdown.slint`, `searchfield.slint`, `menusheet.slint`, `speedcontrol.slint` | Reusable widgets / popups. `searchfield.slint` is the app's single text-input component (custom `TextInput` + placeholder + an in-field clear "×" shown only while it has text); every input uses it, and long text scrolls horizontally to keep the caret visible. `speedcontrol.slint` is the playback-rate control (0.5–2.0 slider + ± 0.05 buttons + two-decimal readout, with 1× / 1.25× / 1.5× / 2× preset chips under the slider) shared by Settings → Player and the player's settings panel; the step buttons carry accessible names ("Slower"/"Faster"). |
| `bottomnav.slint`, `sidenav.slint`, `kbnav.slint`, `icons.slint`, `anim.slint` | Navigation, icons, animations, keyboard-nav helpers. Page navigation is responsive and **per page** — every page hosts its own instance of both and switches on `narrow`: `bottomnav.slint` (`BottomNav`) is the icon-only bottom capsule on narrow layouts, `sidenav.slint` (`SideNav`) is the same design turned upright on wide ones — a full-height, flush, square-cornered 64px panel down the left edge (x/y are pinned inside the component: Slint centres a plain child of a non-layout parent, which would float it mid-window) with the four items listed top to bottom. Pages reserve its footprint by adding `NavMetrics.rail-width` (the exported global) to their own left padding, and subtract the same amount in their grid math; the rail also eats stray taps in that strip. Wide subpages keep the rail (window chrome), the narrow bottom bar hides on them. The purple marker snaps immediately and stays circular; shared `NavFeedback` (in `sidenav.slint`) briefly enlarges only the incoming icon by up to 2px, then settles over 220ms. `NavState.from` suppresses feedback on same-page rebuilds; the existing persisted `anim_nav_slide` switch now controls this icon-only effect, labelled Navigation feedback. |

**Animated feedback:** `menusheet.slint` stays mounted in Home, Library and
Detail: callers bind its `open` property instead of conditionally creating it.
The backdrop fades and the bottom panel slides using `Anim.transition_*`;
closing disables row actions immediately and retains the tap-swallowing exit
layer until the slide ends. Reopening cancels the exit timer, and disabling
motion snaps immediately. `player.slint` keeps its click-through gesture pill
mounted so both reveal and dismissal fade/slide with `Anim.player_*`; seeking
and restoring the held playback rate still happen immediately.

> When adding a UI property/callback: declare it in the page component **and**
> in `AppWindow`, forward it in the `AppWindow` child wiring block, then handle it
> in `src/app/run.rs`.

---

## 5. Persistence

### Primary store: redb (`nova-storage`)
Single database at `<data_dir>/nova.redb`, table `kv` (`&str → &str`). Open once
via `nova_storage::init_at(dir)` before any read/write. The `try_*` APIs distinguish
missing keys from database/transaction failures; legacy convenience wrappers
report failures through `last_error`. Unreadable app snapshots are write-blocked,
and Settings displays a persistence warning. Keys used by the app:

| Key | Content |
|---|---|
| `settings` | `CacheSettings` JSON (image cache + display + player backend/decoder + playback rate). Display's `discover_catalog_addon_names` defaults on and syncs as a settings field; hiding prefixes changes only dropdown labels, not catalog identity. Player backend choice (`player_external`, `desktop_external_app`), decoder (`android_hwdec`), episode start behavior (`episode_start_behavior`), playback rate (`playback_speed`, 0.5–2.0×), and UI `language` are device-local. |
| `library` | `Vec<LibraryEntry>` JSON. |
| `addons` | `Vec<AddonStore>` JSON (desired addons, including entries with unavailable manifests). Configure-page reachability is device-local. |
| `manifest:{url}` | Cached addon `Manifest` JSON (one per addon). |
| `discover:search_history` | Local-only JSON list of the 20 most recent unique completed Discover queries (2–256 characters). Loaded at startup, displayed on focusing the empty Discover input, and erased by Clear history. Never synced. |
| `episode_progress` | `HashMap<String, EpisodeProgress>` JSON (watch history). Tracking = 250 ms tick mirroring mpv props (`playback.rs::note_player_progress_from_ui`): saves throttled to 30 s / 5 s delta (time-based saves skipped while paused), finalize-on-close, external player untracked. Series and movies both tracked: a movie's record is keyed `id\x01id`. |
| `continue_hidden` | `HashMap<String, u64>` JSON — Continue Watching items the user removed, `id -> removal unix secs`. Local mirror of the synced `continue_hidden` domain (so the choice survives with sync off); read at startup in `run.rs`, cleared for an item when playback of it is armed. |
| `torrent_settings` | `TorrentSettings` JSON (runtime mirror). |
| `torrent_cache` | Tracked torrents (`infohash → dir/len/file/last-used`) so restarts adopt downloads instead of orphaning them from trim/clear accounting. Retained offline transfers are protected separately by `nova-torrent`. |
| `downloads:v1` | Local-only `DownloadManifest`: queued/downloading/paused/completed/failed stream jobs, source identity, progress, validators, and artifact paths. Corrupt manifests are quarantined as `downloads:v1.corrupt.<ts>`. On load, interrupted transfers return to `Queued`; completed jobs are validated against disk with **canonicalized** paths (Android reports the same dir as `/data/user/0/...` and `/data/data/...`), and completed HTTP artifacts found under `<data>/downloads/http/` that the manifest no longer references are re-adopted as best-effort entries (torrent artifacts are not, since a sparse partial file is indistinguishable from a complete one). |
| `meta_header:{type}\x01{id}` | Cached detail-header snapshot. |
| `sync:identity` | Ed25519 secret key hex (stable endpoint id). |
| `sync:settings` | `SyncSettings` JSON (enabled, app-running interval, background-sync toggle, device name, confirmation, local-discovery toggle). Local only, never synced. |
| `sync:records` | Legacy whole-store JSON blob; migrated once to the per-record rows below, then removed. |
| `srec:{len}:{domain}{key}` | One sync record row: `Record` JSON (`value` + `version`). Written individually (batched in one transaction on save), so a change touches one row instead of the whole store. |
| `sync:records:hlc` | Persisted hybrid logical clock. |
| `sync:quarantine:<hash>` | Content-addressed JSON evidence (`key`, `raw`), outside active row prefixes; quarantining commits evidence and removal atomically. Old corrupt backups are retained/recovered without recursive suffixes. |
| `sync:baseline:<domain>` | Materialized app values used as deletion/edit authority; unseen remote rows never participate in snapshot diffs. |
| `sync:projection:pending` | Durable set of remotely changed domains awaiting projection; replayed on attachment, the UI poll, and restart. |
| `sync:peer_acks` | Per-peer ack HLC map (local only; gates tombstone GC). |
| `sync:invites` / `sync:pending_join` | Outstanding invites / in-flight join. |

Helpers: `src/app/io.rs` (`read_json`/`write_json`), and per-domain
`read/write_persisted_*` functions. `nova-storage` exposes `write_batch`
(single transaction for many rows) and `scan_prefix` (ordered prefix scan),
which the sync store uses to persist per-record rows without rewriting a blob.
Synced app snapshots, baselines, dirty rows, and HLC commit in one batch through
`sync.rs::persist_sync_snapshot` and `nova_sync::prepare_snapshot`. Dirty work is
retained until commit succeeds. Settings enum recovery is field-isolated and
preserves unsupported raw values until that field is deliberately edited.

### Legacy files
Legacy desktop `settings.toml`, `library.toml`, and `addons.toml` files are no
longer imported. Current settings, library entries, and addons are read only
from the KV store in `<data>/nova.redb`; any old files are left untouched.

### Paths (`nova-config`)
- Platform data and cache paths are listed in [PLATFORM_STORAGE.md](PLATFORM_STORAGE.md).
- Downloaded media lives under `<data>/downloads/` (private and durable; HTTP jobs use per-job `.part` files and torrent jobs use a retained subdirectory).
- `NOVA_DATA_DIR` / `NOVA_CACHE_DIR` are Android test/device-farm fallbacks.
- Linux and macOS use XDG roots; the platform-specific path helper honors custom
  `XDG_DATA_HOME` and `XDG_CACHE_HOME` values.

### In-memory caches
Decoded-image LRU + disk cache live in `nova-media::cache`; `nova-config` holds the
active `CacheSettings` in a global for media/player to read.

---

## 6. Cross-device sync (`crates/sync` + `src/app/sync.rs`)

The most intricate subsystem. Sync is **opt-in** (`sync:settings.enabled`);
nothing binds a socket until `SyncEngine::setup()` runs.

Correctness and recovery work is tracked in
[`sync-hardening-plan.md`](sync-hardening-plan.md) (A–G code implemented; H partially implemented, device/power validation outstanding).

### Design
- Generic, app-agnostic store: `domain -> key -> Record { value, version }`.
- `Version` = HLC (`physical_ms`, `counter`) + `dev` id + `deleted` flag; LWW merge with tombstones. See `merge.rs`, `hlc.rs`, `store.rs`.
- Socket-free `local_store` owns records even with networking disabled. Fresh untouched settings seed only a baseline; explicit settings edits publish intent even when selecting the default. Legacy persisted settings conservatively seed absent keys. Receive/reload reject or quarantine clocks beyond the one-hour drift bound.
- Progress values carry JSON activity/watch/unwatch action registers (`__nova_progress_v1`), merged by deterministic action version. Watch stays sticky unless superseded by explicit unwatch; rewinds use the activity register, position/duration stay coherent, and play count takes max. Legacy values initialize registers from their record version.
- Anti-entropy: each peer exchanges a **per-domain hash** of its version map, then full version maps only for domains that differ, then just the records the other lacks or has stale. One bidirectional QUIC stream per exchange (`protocol.rs`); an unchanged domain transfers no digest at all.
- **Connection reuse:** the dialer keeps one connection per peer and opens a fresh bi stream per pass; the accept handler serves streams for the connection's lifetime. A closed/failed connection is evicted and redialed (`lib.rs`).
- **Recovery/cadence:** periodic wakes honor monotonic per-peer backoff; explicit `sync_now` and completed network refreshes reset backoff for one bounded, coalesced pass. Overlapping network refreshes coalesce, and recovery wakes only after iroh refresh finishes. Interval/foreground changes recompute the pending periodic deadline immediately (`lib.rs`).
- Sync frames are **postcard** inside a codec byte with optional **deflate** (`frame.rs`, `flate2`/miniz_oxide); pairing and removal keep raw postcard. postcard is non-self-describing, so schema changes require an ALPN bump. Current ALPNs:
  - `nova/sync/3` (`ALPN`) — record exchange (per-domain digest hashes + compressed frames)
  - `nova/pair/2` (`PAIR_ALPN`) — invite-ticket pairing
  - `nova/remove/1` (`REMOVE_ALPN`) — one-shot "you were removed" notice

### Crate files
| File | Role |
|---|---|
| `lib.rs` | `SyncEngine` (owns iroh `Endpoint`, tokio runtime, router, worker loops), settings, peer allowlist, `run_pass`/`sync_one`, per-peer connection cache, singleton `install`/`engine`, peers-domain helpers, per-peer acks. |
| `protocol.rs` | Sync `Handler` (serves streams for a reused connection), `Wire` frames, `run`/`exchange`, digest hashing, `apply_frames`, `RemoveHandler`/`notify_removed`. |
| `pair.rs` | `PairHandler`, invite lifecycle, join/accept flow, `PairEvent`. |
| `ticket.rs` | Invite ticket codec (`NV1` + base32) and match code. |
| `store.rs` | Durable `Store` (per-record rows in redb via `nova-storage`), digest/outbound/apply/GC, legacy-blob migration. |
| `local.rs` | Socket-free record owner, atomic app snapshot/baseline preparation, unknown object-field preservation. |
| `progress.rs` | Convergent activity/watch/unwatch registers embedded in progress JSON and action-clock validation. |
| `merge.rs` | `Version`, `Record`, `resolve` (LWW + tombstone tie-break). |
| `hlc.rs` | Hybrid logical clock (tick/observe, forward-drift cap). |
| `frame.rs` | Length-prefixed postcard framing (raw + codec byte with deflate). |

### Domains (app side, `src/app/sync.rs`)
`library`, `progress`, `addons` (one record per addon URL plus a whole-value
LWW `order` record listing installed URLs, so reorder syncs mesh-wide),
`settings`, `category` (per-name union), `continue_hidden` (one record per Home
item the user removed, value = removal unix secs; tombstones carry the
"un-remove" when a resume or prune drops the key, and a stamp written while sync
was off is kept and published like `progress`), plus
`peers` (mesh membership) and `presence` (engine-owned sightings, see below).
`sync_records` compares against the materialized baseline, not the live store;
only baseline keys absent from the edited snapshot become tombstones. New domains ride the existing
record exchange with no wire-schema change (domain names travel as strings);
the app ignores unknown domains, so old peers stay compatible.

### Peers / membership model
- The `peers` domain holds one record per peer endpoint id: value `{name, added_at}`; deletion = tombstone.
- The **live allowlist** (`SyncEngine.peers`) is derived from the store (`reconcile_peers`) and is the trust boundary (`Handler::is_peer`).
- **Additions** happen only via explicit pairing / manual add (`peers_add`, may resurrect a tombstoned peer). A routine sync only refreshes an existing live peer's name (`peers_refresh`) and never inserts/resurrects.
- **Removals** are tombstones (`peers_remove`). Removal is made symmetric:
  - `SyncEngine::remove_peer` sends a best-effort `nova/remove/1` notice so the removed device drops the remover and shows a UI notice.
  - If the notice is missed, a structured QUIC application-close code (`REVOKED_CODE`) triggers mutual removal. An unknown/not-yet-authorized peer does not get that code. Authorization is rechecked for every stream and under the store lock before apply; cached dialed connections close on removal.
- **Stale third-party tombstones** are ignored: if `sync:peer_acks[X]` is newer than a tombstone's HLC, `apply_frames` keeps `X` (a peer you actively sync with survives someone else's removal).
- **Presence (last-seen):** the engine-owned `presence` domain holds one record per `{observer}\x01{peer}` sighting (value = unix secs), written after each completed exchange and throttled to one rewrite per 15 min per peer. Each device writes only its own observer keys (no merge conflicts); readers take the max across observers plus the local ack clock (`SyncEngine::peer_last_seen`, shown as `SyncPeer.last_seen`). `peers_remove` tombstones sightings involving the removed peer.
- **Self-records** are never applied (you never add yourself). A tombstone keyed by your own id sets a "removed us" signal.
- **Tombstone GC** is gated by per-peer acks (`ack_floor`): a tombstone is only reclaimed once every current peer has acked past it (30-day TTL floor).

### App wiring
- Startup establishes/migrates baselines before projecting authoritative records, independently of networking. `Bridge::start_sync` attaches callbacks to `foreground_engine` even when a background lease already owns the engine, applies domains, and requests a pass.
- `sync_apply` reads the socket-free owner; thread-local, nesting-safe `ApplyingGuard` suppresses echo writes. Deferred addon callbacks retain origin and generation, while desired order/flags are read at completion. `settings_edited(field)` preserves explicit default choices; `persistence_failed` exposes storage health through AppWindow and SettingsPage.
- Durable apply marks pending domains before callback delivery. `Done` and valid stream completion are required for success/ack; writer tasks abort on every cancellation path, including joining. Pairing commits invite consumption and trust together, and the joiner installs trust before closing its completion connection.
- Pairing UI: Settings → Sync (`settings.slint`), invite create/join, confirmation prompt with match code. The created ticket is also rendered as a QR image (`qr.rs`); on Android the invite card offers a camera scanner (`android_qr.rs` + `android/java/dev/misob/nova/QrScanActivity.java`) that decodes a ticket and joins automatically.

---

## 7. Platform specifics

| Concern | Desktop (Linux) | Android |
|---|---|---|
| Entry | `src/main.rs` → `app::run()` | `android_main` (`src/lib.rs`) → `app::run()` |
| App icon | `assets/logo.png` is the Slint window icon | Density-specific `android/res/mipmap-*/ic_launcher.png` resources from the same source image |
| Renderer | `winit` + `femtovg` (mpv composites under the Slint scene) | Slint android-activity backend (Skia/GLES); mpv composites into same framebuffer |
| Video | mpv via `libmpv2`, X11; external app per Settings → Player (VLC / mpv / `xdg-open`) | prebuilt `vendor/android-libs/<abi>/libmpv.so` (media-kit `full` flavor, so HDMV PGS / bitmap subtitles decode), JNI for MediaCodec/EGL; external-player `ACTION_VIEW` (Settings → Player backend choice, or fallback). Volume starts pinned to 100% (no volume UI; a vertical swipe adjusts it after) and the transport (⏪10 ▶/⏸ ⏩10) floats centered over the video. |
| HTTP | `reqwest` blocking (+ addon `client` feature) | `reqwest` rustls stack (`default-features=false`) |
| Stream downloads | `nova-download` progressive HTTP + retained `nova-torrent` jobs; one active transfer with a persisted queue. HTTP connects/headers/body-reads are gap-timeout guarded (60 s) and a torrent with no progress is failed after 10 min, so one hung transfer cannot pin the single slot (and every job queued behind it) forever. | Same private app-data queue; process-bound and resumed on next launch. Active transfers keep the process alive through a `dataSync` foreground service (`NovaBackgroundService`, started/stopped from the 250 ms tick via `android_bg.rs`) with a partial wake lock; the notification shows count + progress. Android 15 caps `dataSync` at 6 h/24 h, after which `onTimeout` stops it and jobs resume on the next foreground. |
| Poster loading | worker pool + dual-priority channels + WebP encode | per-grid `net::fetch_image` |
| Allocator | jemalloc | system |
| Storage dir | XDG dirs | app files dir + sibling cache dir |
| Legacy config-file import | no | no |
| Permissions | — | INTERNET, ACCESS_NETWORK_STATE, FOREGROUND_SERVICE, FOREGROUND_SERVICE_DATA_SYNC, WAKE_LOCK, POST_NOTIFICATIONS, RECEIVE_BOOT_COMPLETED, CAMERA (runtime; camera feature declared optional) |
| Invite QR | Ticket displayed as a QR image; paste to join | Same QR display, plus "Scan QR code": `QrScanActivity` (framework Camera2 — cargo-apk2 can bundle no AAR) feeds the luma plane to Rust (`rqrr`), which validates the ticket and joins automatically. |
| Screen stay-awake during playback | D-Bus `ScreenSaver` inhibit (logind `idle` fallback), converged in `Player::tick` + play/toggle/close | `FLAG_KEEP_SCREEN_ON` via `converge_screen_on`, same convergence points; no permission needed |
| Video across lock/unlock | n/a | The MediaCodec output surface does not survive an activity pause/stop, so a lock/unlock (or backgrounding) leaves black video with the OSD + audio still up. `android_main` installs a lifecycle listener (`init_with_event_listener`) that arms a reload on `Pause`→`Resume`; `Player::tick` then reloads the current URL at the live position (`ANDROID_RESUME_PENDING`, unified with the `RenderingSetup` surface-recreation trigger). The resume block runs before the scrub-divergence early return so a blanked `time-pos` cannot strand it. |
| System back | n/a (desktop backends never synthesize `Key.Back`) | Each page scope maps `Key.Back` like `Backspace` (pop one layer); Home root screen calls `exit_to_background` → `move_task_to_back` (process kept, state restored on return). NOTE: with targetSdk ≥ 33 the system routes back through `OnBackInvokedCallback` instead of `KEYCODE_BACK`; until `android:enableOnBackInvokedCallback=false` lands in the cargo-apk manifest (unsupported attribute today), gestures on API 33+ background the app via the OS default instead of popping in-app. |
| Background sync | n/a | While the process lives, the sync tokio `interval_loop` keeps firing passes, independent of the Slint loop: Settings → Sync's "Auto-sync while the app is running" (30 s / 1 / 5 / 15 min) governs it, clamped to a 5-min floor when the app is backgrounded-but-alive. Across process death, a periodic `JobScheduler` job (`NovaSyncJobService`, fixed 15 min, network-required, persisted) wakes the process and runs one bounded pass headlessly (`android_bg.rs` + `nova_sync`), then exits; the next app open projects the merged records into local state. The job is gated by the separate "Background sync" toggle and must not start a foreground service (blocked from background), and a bounded pass fits its window without one. Doze defers jobs to maintenance windows, so background sync is opportunistic; a desktop always-on peer makes it converge quickly. |

`nova-config` exposes `android_fonts_dir()` (libmpv subtitle fonts).

---

## 8. Testing

- **Unit tests**: inline `#[cfg(test)] mod tests` in most modules; app tests in `src/app/tests.rs`; download tests in `crates/download`; sync tests in each `crates/sync/src/*.rs`.
- **Integration tests** (`tests/`, headless via `i-slint-backend-testing`, no display):
  - `animation_feedback.rs` — bottom-sheet entrance/exit geometry, inert closing rows, interrupted exits and animation-off behavior; player gesture-pill entrance/dismissal and independent player/master switches, using mock time.
  - `navbar_motion.rs` — icon-only navigation feedback on both layouts: immediate circular highlight, selected-icon pop, rapid clicks, same-item clicks, detail return and animation switches. `nav_marker_glide.rs` keeps its historical filename but now verifies snapped marker alignment and restored-page placement (no screen-flash API).
  - `settings_overflow.rs`, `settings_sync_overflow.rs`, `detail_overflow.rs` — assert pages don't overflow horizontally with long content (tickets, peer ids, errors).
  - `detail_row_height.rs`, `settings_row_height.rs` — assert rows grow to fit text that wraps on narrow screens (the vertical counterpart to the overflow tests).
  - `settings_addon_row_fit.rs` — every control of a Settings → Addons row (toggle, Copy link pill, Configure/Refresh/Remove/move icon buttons) draws inside that row at phone and desktop widths: the narrow `AddonRowCard` puts the name with the move arrows on the first line and the icon actions underneath, and the test additionally requires the name to stay on the arrows' line (ending where they begin, above the action line).
  - `settings_about.rs` — About opens a nested Licenses page and Back returns to About; source buttons stay centered and inside their rows at phone and desktop widths.
  - `detail_topbar.rs` — the detail top bar is icon-only (no text action buttons) and fits phone width.
  - `detail_episode_pagination.rs` — the Episodes tab renders one page, shows `N episodes · page x/y`, resolves a page-2 card pick against the whole season (`episode_page_start + i`) and dispatches absolute page moves.
  - `stream_card_layout.rs` — a filter pill keeps one width while addons are queried, and the download rail starts at the track's left edge and fills by progress.
  - `detail_stream_pagination.rs` — stream pages render the supplied 25-row slice, and a row on page 2 dispatches its absolute stream index.
  - `stream_pill_scroll.rs` — the pill bar's desktop chevrons step the pills by one viewport with no vertical drift (and restore), a faded chevron is inert, pill taps still filter, fitting bars keep faded chevrons, touch layouts render no chevron slots, and on touch the bar scrolls away with the streams (one in-flow row, no docked copy) without its taps picking filters.
  - `stream_pill_axis_lock.rs` — the pill bar's axis lock against the page pan, with time-spread gestures (`mock_elapsed_time`; a back-to-back drag is not a drag): a horizontal drag pans the row and leaves the page still even once it has drifted well past 8px vertically (the case that used to hand the gesture to the page), a vertical drag starting on the pills still scrolls the page, and page drags are unaffected.
  - `player_gestures.rs` — the player backdrop's double-tap seeks ∓10 s without extra toggles (lone taps act at once), a 500 ms hold previews 2× and the release restores the stored rate, and Android swipes step brightness/volume without seeking (brightness/volume callbacks stubbed, OSD stays up).
  - `library_scroll_restore.rs` — returning from an entry keeps My Library's exact scroll offset (a partially visible focused row is left alone), while a fully hidden focused card is still revealed.
  - `detail_episode_bar.rs` — the picked-episode bar renders on the Episodes tab and its chevron sits on the bar's centre line (a `Rectangle` child of a `HorizontalLayout` is top-aligned, which used to leave the icon riding high over the label).
  - `library_category_filters.rs` — the Library category filter bar renders with no user categories, i.e. the automatic buckets are reachable on a fresh install (the bar used to be gated on the user-category model).
  - `stream_downloads_ui.rs` — pinned download rows, status/progress layout, and stream action callbacks.
  - `home_carousels.rs` — Home's Continue/Upcoming carousels render with rows, a landing horizontal drag scrolls the rail (and coasts on after the release — the rail has no Flickable momentum of its own) without opening a card (and a stationary tap does open it), a section-header tap opens the matching "see all" subpage, and that subpage grid scrolls vertically (guards the `ScrollView` direct-layout-child requirement and the axis-locked landing gesture).
  - `home_continue_menu.rs` — the Continue Watching card menu is Play / Enter series / Remove from Continue Watching (+ the sheet's Cancel) and nothing else: a 600 ms hold, or a right-click on touch, on a landing Continue card (and on a subpage grid card) opens the page-level sheet without also opening the card, and its "Remove from Continue Watching" row dispatches `continue_remove` for that card's index. With `touch_menus` off (desktop) a right-click must *not* open that mobile sheet and must not play/remove the card — the native `ContextMenuArea` popup it shows instead isn't hostable headlessly.
  - `home_carousel_flick.rs` — headless Home carousel gesture cases (touch and pointer input, interrupted gestures, and returning after keyboard focus); review against Slint-native inertia when editing rail interactions.
  - `home_continue_badges.rs` — Continue Watching cards carry a "Next up" badge mid-series and a "New Episode" badge when everything else is already watched (resume cards have neither), on the landing carousel and in the "see all" subpage grid (same card component; the covered landing layer still instantiates its own badge).
  - `home_continue_hover.rs` — the Continue/Upcoming hover lift is pointer-only (hover animations pinned off so the binding, not the 150 ms ease, is read): a pointer hover lifts exactly the hovered card and dropping it drops the lift again, while on a touch platform (`touch_menus`, i.e. Android) neither a press+wiggle+release on a card nor a drag onto the next one leaves any card lifted. Slint latches `TouchArea.has-hover` under a finger when a press is delay-forwarded by the subpage's `ScrollView` (the discarded dispatch never sends the matching `Exit`), which used to strand a card lifted with no touch on it.
  - `home_upcoming_calendar.rs` — the Upcoming subpage's Calendar/List toggle opens the month view (42 `CalDayCell`s, grid cards hidden), tapping a marked day dispatches its epoch (filler/empty days stay silent), tapping a selected-day card dispatches its full-list `index` (not the day-list position), the month step / arrow-key / Enter / Back paths reach the backend (Back closes the calendar before leaving the subpage), and cell taps never open cards through the covered landing layer.
  - `settings_downloads.rs` — Settings → Downloads renders its auto-delete toggle (guards the subpage nesting), and the nested Downloaded episodes list opens, lists completed episodes, dispatches a per-row remove, and returns to Downloads on back.
  - `settings_layout.rs` — a short settings subpage (Categories with no entries) stays content-height and top-aligned rather than stretching to the screen, while a long subpage (Sync with many peers) still scrolls.
  - `settings_playback_speed.rs` — the playback-rate control renders in both hosts (Settings → Player, and the player's settings panel via the OSD gear → Playback speed submenu), its ± buttons report a step to the backend, a slider tap reports a value that tracks the pointer, and the readout shows the shared value two-decimal.
  - `settings_language.rs` — Settings → Display's Language row renders at phone and desktop widths (title + the backend's picker list, inside the window), a pick reaches the settings autosave with the language's picker index, and the bundled catalogs are really in the binary: switching to `hr` re-renders the section header and the row title in Croatian (and `en` restores the source text) while the picker's language names stay untranslated, and an unbundled code is rejected. It also checks the rest of the app in Croatian (My Library heading, the "Sve" filter chip, the `(3 stavke)` plural) and that the stored category values stay English. Run with `--features live-preview` it asserts the documented fallback instead (that build has no catalogs).
  - `android_back_nav.rs` — synthetic `Key.Back` via `Window::dispatch_event`: detail streams→episodes→close, settings subpage→landing→home, player close, Home subpage→landing, Home root → `exit_to_background`. One test fn (backend inits once per process); per-phase ticks because page create/destroy focus races sharing a tick, plus the 280 ms subpage close animation between double-Backs.
- Sync tests use real iroh endpoints with the `Minimal` preset (`presets::Minimal`) and in-memory address lookups; see `crates/sync/src/protocol.rs` tests for patterns.
- `src/app/qr.rs` unit test round-trips a real invite ticket through the QR encoder and `rqrr` decoder (the same crate the Android scanner uses), guarding the display↔scan payload path without a device.
- `src/app/settings.rs` tests exercise real headless settings controls, immediate memory capture, navigation before autosave, current-state projection, and the shared playback-speed debounce. The test runs in an isolated child process because redb and Slint initialize once per process.
- Verification commands: `cargo check`, `cargo test`, `cargo test -p nova-download --lib`, `cargo test -p nova-sync --lib`.

---

## 9. Conventions & gotchas

- **No comments policy is not absolute here** — this codebase is heavily
  commented with rationale. Match the surrounding style; explain *why*, not *what*.
- **Leaf-crate split is deliberate**: editing app logic should not recompile
  heavy deps (Slint codegen, mpv, iroh, librqbit, redb). Keep new heavy
  subsystems in their own crate.
- **postcard wire schemas are not self-describing**: adding/reordering fields in
  `crates/sync` wire structs requires an ALPN version bump (sync is `/3`;
  pairing/removal are unchanged by sync-frame compression). Local storage stays
  JSON, but the sync store is one row per record, not a single blob.
- **Row heights follow wrapped text, not line estimates**: Slint measures
  wrapped `Text` at its intrinsic width during the preferred-height pass, so
  rows that reserve a fixed/estimated height clip on narrow screens. Rows use a
  hidden zero-height measurement copy at the live width (`SettingsRow`, the
  detail stream list) to size themselves; keep that pattern when adding rows
  with variable text.
- **Slint API changes are three-step**: page component + `AppWindow` property/callback + `run.rs` wiring. Missing a step is a common compile/runtime error.
- **New user-facing strings are marked `@tr("…")`** (the `nova-ui` catalogs bundle at build time; see §4). The catalog is English-only today, so marking changes nothing on screen — it is what makes a later language possible. Translation *values* pushed as data (language names, sizes, statuses) stay untranslated.
- **UI thread only** for Slint; use `slint::invoke_from_event_loop` from workers.
- **Android player keyboard/D-pad order** (open note): the centered transport
  keeps the logical keyboard indices (play = 1, rewind = 3, forward = 4) while
  reading 3 → 1 → 4 left-to-right on screen. Left/right traversal (`kb_prev` /
  `kb_next` in `player.slint`) skips the removed volume slot (2) and the narrow
  track buttons (5/6), but does not reorder to mirror the visual layout. Remap
  those helpers if exact on-screen D-pad order is ever wanted.
- **`ScrollView` needs a direct layout child to scroll**: its content size is
  derived from direct layout children only, and a bare `if`-conditional child
  is ignored — the viewport then collapses to its own height and touch drag
  silently does nothing. Wrap view-gated content in an unconditional
  `VerticalLayout` (see `home.slint`'s subpage grid) like Library/Settings do.
- **`ScrollView` content is at least viewport-tall**: the compiler binds
  `content-height = max(flickable.height, layout.min-height)`, so a short page
  still fills the viewport and its unpinned rows/cards absorb the slack
  (`vertical-stretch` defaults to 1) and stretch. Settings subpages keep their
  content top-aligned by pinning the fixed rows (`vertical-stretch: 0`) and
  ending the scroll content with a `Rectangle { vertical-stretch: 1; }` spacer
  that absorbs the excess; long content still scrolls.
- **Page-level overlays are siblings of the page layout, never layout children**: a `MenuSheet` (or any full-page overlay) declared *inside* the page's `VerticalLayout` is laid out as a row — opening it then squeezes/pushes the page content instead of covering it, which looks like "everything got pushed to the top" (most visible on Android). Declare it as a direct child of the page root `Rectangle`, after the layout, as the Library, Detail and Home sheets do.
- **Nested `Flickable`s do not axis-lock**: the page sees the press first and
  holds it for its drag-vs-tap delay. Home uses the Detail stream-pill pattern:
  its interactive inner rails use Slint's drag and native inertia, while each
  card's `TouchArea` reports early touch moves (`touch-finger-id` is nonzero
  before the held-back press). A horizontal hint disables page panning; a
  vertical gesture or release hands it back. The outer page disables mouse
  drag-pan so desktop rail drags reach the inner rail; wheel scrolling still
  moves the page. Covered landing rails and cards disable on subpages because
  opacity alone does not block hit-testing. Detail's pill bar similarly gates
  its page pan via `pan_hint` / `pan_release` and a short release timer.
- **`ApplyingGuard`** must wrap remote-apply code paths so writes don't echo back to sync.
- **Android packaging is `cargo-apk2`, not `cargo-apk`.** The Java background components (`android/java/`) must be compiled to DEX and the `<service>`/`foregroundServiceType` elements emitted, neither of which the old tool can do; `Cargo.toml` therefore uses `use_aapt2`, `java_sources`, `has_code = true`, and an explicit `[[…application.activity]]` (cargo-apk2 generates no implicit activity). The APK launcher icon is configured as `@mipmap/ic_launcher` and packaged from `android/res/`, derived from `assets/logo.png`. The `.#android` shell provides cargo-apk2 from the flake (nixpkgs has no such attr and upstream publishes no binstall artifacts, so it is built with `rustPlatform.buildRustPackage`) and unsets `CPATH` (host include leak breaks the NDK C build).
- **Android builds use `--no-default-features --features android`**; guard desktop-only code with `#[cfg(feature = "desktop")]` or `#[cfg(not(target_os = "android"))]`.
- **Download jobs are local-only**: `downloads:v1` and artifacts are never synced. HTTP resumes require matching `Range`/`ETag`/`Last-Modified`; retained torrent data is protected from playback cache eviction. Transfers are one-at-a-time, so a hung job must not pin the slot: HTTP connects/headers/body-gaps time out (60 s) and a torrent with no progress fails after 10 min; tapping Download again re-arms a `Failed` job in place. A transfer worker carries a done flag; the 250 ms tick reaps one that ended without finalizing (panic/unwind) and the state mutex is poison-tolerant, so a dead worker can never leave every later job stuck in `Queued`. Completed-job validation **canonicalizes** both paths before the root check (Android path aliases), and unreferenced HTTP artifacts under `<data>/downloads/http/` are re-adopted on startup rather than silently dropped (torrent artifacts are not re-adopted: sparse partial files look complete on disk). Deletion (`remove_owned_path`/`remove_owned_dir`) canonicalizes the target the same way — a lexical `starts_with` across the `/data/user/0` vs `/data/data` aliases previously skipped the unlink, clearing the row while leaving the file (and its app-storage footprint) on disk.
- **`Store::try_load` quarantines** unreadable rows/clocks without recursive backups — check `sync:quarantine:*` for evidence. Unreadable identity bytes are not replaced with a new identity.
- **The Android background sync job must open storage before reading settings.** `android_bg::run_headless_sync` sets the files directory and opens storage first, checks both enable flags, and obtains a `BackgroundLease` only when no foreground engine exists. Lifecycle transitions are serialized; foreground attachment takes ownership before job release. `onStopJob` cancels a 120-second-budget `one_shot`, not the foreground worker, and Java finishes at most once. The outcome distinguishes completed/cancelled/timed-out/skipped/failed work; `pass_count` is not a success signal. Device validation remains required.
- The working tree has historically carried uncommitted work on the `iroh`
  branch; check `git status` before assuming a file's committed state.

---

## 10. Where to look for common tasks

| Task | Start here |
|---|---|
| Add a Settings option | `crates/ui/settings.slint`, `src/app/settings.rs`, `src/app/run.rs`, `crates/config/src/lib.rs` |
| Settings edit loss / sync recovery | `src/app/settings.rs` (`capture_settings`, `wire_settings_autosave`, regression test), `crates/sync/src/lib.rs` (`peer_connection`, worker/cadence/recovery), `docs/sync-hardening-plan.md` |
| Add a UI language / translate a string | `crates/ui/translations/<code>/LC_MESSAGES/nova-ui.po` (context-free), `crates/config/src/lib.rs` (`Language`), `src/app/i18n.rs`; mark strings `@tr("…")` in the `.slint` files |
| Add a Discover feature / catalog change | `src/app/catalog.rs`, `crates/ui/discover.slint`, `crates/ui/appwindow.slint`, `crates/addons` |
| Discover reveal / filter-drag regressions | `tests/discover_reveal_and_filters.rs` (animated opacity during poster updates, same-length result replacement, animation-off behavior, horizontal filter drags), `tests/discover_search_ui.rs` (browse/results navigation) |
| Text-input overflow / clear controls | `crates/ui/searchfield.slint` (shared by all inputs; follows the caret on edits and viewport resize), `tests/searchfield_overflow.rs` (compact, touch and prominent fields: long text, End/Home, window shrink, clear buttons) |
| Discover local search history | `src/app/catalog.rs` (local KV + bounded MRU list), `crates/ui/discover.slint` (recent-search panel expands/collapses in the layout on empty-input focus, respecting animation settings), `tests/discover_search_history_ui.rs` (expansion, focus, replay, Back/clear) |
| Discover catalog labels / return animation | `src/app/catalog.rs` (`apply_catalog_labels_to_ui`, separate labels and identity values), Settings → Display's `discover_catalog_addon_names`; `crates/ui/appwindow.slint` (`discover_search_animate_results` survives detail-page recreation, resets for a fresh search), `tests/discover_reveal_and_filters.rs` |
| Change the detail/stream flow | `src/app/detail.rs`, `src/app/streams.rs`, `crates/ui/detail.slint` |
| Add a persisted app field | `src/app.rs` (struct), `src/app/io.rs` + relevant module's `read/write_persisted_*`, then `src/app/sync.rs` if it should sync |
| Touch playback | `crates/player/src/lib.rs`, `src/app/playback.rs`, `crates/ui/player.slint` |
| Bottom-sheet / player feedback motion | `crates/ui/menusheet.slint` (`open` + retained exit), `crates/ui/player.slint` (`flash_pill`), `crates/ui/anim.slint` (category durations), `tests/animation_feedback.rs` |
| Navigation icon feedback (no marker glide) | `crates/ui/sidenav.slint` (`NavFeedback`, `NavState`), `crates/ui/bottomnav.slint`, `src/app/run.rs` (`note_nav_switch`), `tests/navbar_motion.rs`, `tests/nav_marker_glide.rs` |
| Android player gestures (swipe volume/brightness) | `src/app/android_player.rs` (volume via any `Context`, brightness via the stashed `NativeActivity` — the `ndk-context` `Context` is not necessarily an `Activity`), `android/java/dev/misob/nova/PlayerFx.java`, `crates/ui/player.slint` (backdrop state machine) |
| Torrent behavior | `crates/torrent/src/lib.rs`, `src/app/playback.rs` |
| Stream downloads | `crates/download/src/lib.rs`, `src/app/downloads.rs`, `crates/torrent/src/lib.rs`, `crates/ui/detail.slint` |
| HTTP / images | `crates/media/src/net.rs`, `crates/media/src/cache.rs`, `src/app/posters.rs` |
| Sync protocol / peers / pairing | `crates/sync/src/{lib,protocol,pair,store,merge}.rs`, `src/app/sync.rs`, `docs/sync-followons.md` |
| Sync hardening / settings reset investigation | `docs/sync-hardening-plan.md` (findings, phased implementation, regression matrix, migration decisions) |
| Offline mutations / projection replay / progress convergence | `crates/sync/src/{local,store,progress}.rs`, `src/app/{io,sync}.rs`, `crates/sync/tests/{local_mutations,store_persistence}.rs` |
| Sync invite QR / Android camera scan | `src/app/qr.rs`, `src/app/android_qr.rs`, `android/java/dev/misob/nova/QrScanActivity.java`, `crates/ui/settings.slint`, `src/app/run.rs` |
| Android background execution (downloads FGS + periodic sync) | `src/app/android_bg.rs`, `android/java/dev/misob/nova/{NovaBackgroundService,NovaSyncJobService}.java`, `nova_sync::SyncEngine::setup`, `DownloadCoordinator::has_active_work` |
| App paths / shared settings | `crates/config/src/lib.rs` |
| Persistence backend | `crates/storage/src/lib.rs` |
| Sync diagnostic events / log filtering | `src/diagnostics.rs`, `crates/sync/src/{lib,protocol,pair,store}.rs`, `src/app/sync.rs`; `RUST_LOG=nova_sync=debug` (no `NOVA_SYNC_DEBUG`) |
| Android glue / external player | `src/lib.rs` (`android_main`), `crates/player/src/external.rs` (Android), `crates/player/src/lib.rs::open_external` (desktop) |
| Build/packaging | `Cargo.toml` (`[package.metadata.android]`), `build.rs`, `flake.nix`, `Makefile` |
