# Compiled Slint module experiment

This isolated Cargo workspace compares compilation boundaries using Nova's real
Settings page and Slint 1.18.0. It is not part of the production workspace or any
default, release, Android, or CI build. All variants use generated Rust; there is
no interpreter and no runtime test harness.

The packages are:

| Package | Boundary |
| --- | --- |
| `nova-settings-monolithic-probe` | A window imports the Settings page from a `.slint` file. |
| `nova-settings-module-probe` | The same page is exported with `as_library("NovaSettingsProbe")`. The package's `links` metadata exposes its source and generated Rust symbols to consumers. |
| `nova-settings-import-probe` | A window imports that page, its row type and globals through `@NovaSettingsProbe`. Rust functions also exercise the shared model and global API. |
| `nova-settings-container-probe` | A small window accepts a `ComponentFactory`; Rust returns a separately compiled page to `ComponentContainer`. Demonstrates generated-code separation only; stock 1.18.0 does not correctly attach that compiled page. |

In Slint 1.18.0, `ComponentContainer` and the markup's `component-factory` type
also require `SLINT_ENABLE_EXPERIMENTAL_FEATURES`. The container's build script
sets it locally before constructing the compiler configuration; Nova's build
environment is unaffected.

`settings-page.slint` wraps the exported page in a finite standalone viewport
with model and state properties. Nova's original page takes its size from
AppWindow; exporting it directly as a standalone library root exposed binding
loops. Both inline consumers give the wrapper their full viewport. The production
Settings source is imported, not copied or modified.

The probe enables the same Slint inspection metadata as Nova's debug builds,
uses `opt-level = 0`, disables Rust debuginfo, and keeps normal incremental
compilation. It does not bundle Nova's translations or run the app's backend.
Generated source sizes and check-time RSS therefore compare these probes, not
full application builds, tests, linking, or runtime/GPU memory.

## Measured results

All three consumers and the shared library passed `cargo check` in the default
Nix environment with Slint 1.18.0 and Rust 1.98.1. These were sequential checks
with normal incremental compilation and warm dependency caches, not controlled
clean-build benchmarks. No tests were run.

| Generated unit | Rust source bytes | Rust source MiB | Sampled peak rustc RSS GiB | Defines the Settings visual tree |
| --- | ---: | ---: | ---: | --- |
| Monolithic consumer | 27,808,160 | 26.520 | 4.200 | Yes |
| Shared library | 27,778,067 | 26.491 | 4.215 | Yes |
| Library-import consumer | 27,738,599 | 26.454 | 3.410 | Yes |
| Factory/container consumer | 56,460 | 0.054 | 0.211 | No |

Ordinary experimental library imports reduce consumer source by only 0.25%:
they share exported Rust types and globals, but still generate the Settings
visual tree in the consumer. The library is an additional compilation unit,
so this form does not solve Nova's large generated-UI problem. The RSS samples
alone do not establish a reproducible memory reduction.

The factory/container boundary keeps the visual tree in the library. Its
consumer generates about 55 KiB and its sampled rustc peak was about 216 MiB.
The library still needs its own roughly 4.2 GiB compiler process; separating
pages would bound the size of each unit rather than eliminate compilation
cost. This demonstrates a compiled boundary, not a measured reduction in
Nova's full build or a working production integration.

## Reproduce

Run from the repository root in the default Nix shell. Run the commands
sequentially; no explicit job count or incremental override is needed. Copying
the main lockfile seeds the same dependency versions; the first check updates
only the experiment's ignored lockfile with its own local packages.

```sh
cp Cargo.lock crates/ui/experiments/modules/Cargo.lock
nix develop .#default --command cargo check --manifest-path crates/ui/experiments/modules/Cargo.toml --target-dir target -p nova-settings-monolithic-probe
nix develop .#default --command cargo check --locked --manifest-path crates/ui/experiments/modules/Cargo.toml --target-dir target -p nova-settings-import-probe
nix develop .#default --command cargo check --locked --manifest-path crates/ui/experiments/modules/Cargo.toml --target-dir target -p nova-settings-container-probe
```

The generated files are `target/debug/build/nova-settings-*-probe-*/out/ui.rs`.
Compare the consumer files, and check whether each still defines the Settings
page's `InnerProbeSettingsPage` tree. The library file is additional generated
code and must also be counted.

## Import constraint

Slint 1.18.0 emits duplicate `library_NovaSettingsProbe_shared_globals` fields if
one file imports a library and then separately re-exports from that same
library. This fails Rust checking with E0124/E0062. The imported probe uses one
combined import and re-exports the already imported names without a second
`from "@NovaSettingsProbe"` statement.

The [upstream module example](https://github.com/slint-ui/slint/tree/v1.18.0/tests/manual/module-builds)
and [compiler configuration](https://docs.slint.dev/latest/docs/rust/slint_build/struct.CompilerConfiguration)
describe the experimental API. The feature is useful for shared types and
globals; its presence alone does not establish a visual-tree compilation
boundary.

## Integration limits

The container package is a compile-only boundary demonstration. Stock 1.18.0's
compiled root has no factory-parent attachment: ordinary `new()` creates a
separate window, and `new_with_existing_window()` replaces the host root. The
prototype deliberately ignores its factory context and must not be used as a
working embedding implementation. None of the runtime behaviors is validated
by `cargo check`.

The production attempt in `crates/ui-settings` uses a narrowly patched compiled
constructor instead. See [`NOVA_PATCH.md`](../../../../vendor/i-slint-compiler/NOVA_PATCH.md)
for attachment/window ownership and preparation constraints, and
[`project-structure.md`](../../../../docs/project-structure.md#4-ui-crates-cratesui-cratesui-settings)
for the Settings bridge, shared globals/catalogs and lifecycle. These probes
remain isolated and use upstream Slint; their measurements predate that
production integration.

## Initial compiled Settings split measurements

The initial production split hosted Settings through `SettingsHost` and its
`ComponentContainer`. `nova-ui-settings` generates the existing Settings visual
tree once, while `nova-ui` imports only its models/globals and creates a fresh
embedded page through the native factory. A typed property/callback bridge
retains AppWindow's backend API and navigation handlers.

Integration samples in default Nix, with normal incremental compilation and
warm dependencies, produced these generated units:

| Unit | Rust source bytes | Rust source MiB | Sampled check-time peak rustc RSS GiB |
| --- | ---: | ---: | ---: |
| Original AppWindow | 69,028,517 | 65.831 | No matching baseline sample |
| Split AppWindow | 42,087,206 | 40.137 | 5.600 |
| Settings module | 28,936,902 | 27.596 | 3.523 |

The largest generated file is about **39% smaller**. Total generated source
across the two units is slightly larger because the boundary adds glue; this
does not eliminate compilation work. A subsequent incremental workspace check
passed and sampled AppWindow at 4.239 GiB. These samples are not controlled
clean-build benchmarks and do not prove a particular reduction in total build
time or peak build-wide memory. Test code generation, linking and runtime/GPU
memory were not measured.

The separate Settings check, full workspace check, default-target workspace
Clippy with `-D warnings`, and formatting check all passed in default Nix.
Clippy sampled the main UI compiler at 5.689 GiB and the concurrent descendant
process total at 5.872 GiB. Each Cargo command ran sequentially without an
explicit job count or incremental override; no tests or platform builds ran.

Settings source edits still invalidate the host's Slint build script and its
dependency checks. The split bounds each generated Rust unit and allows Rust's
incremental reuse across the boundary; it does not guarantee every Settings
edit skips `nova-ui`. The additional page splits below were implemented after this measurement.

The production constructor requires the documented pinned compiler patch.
Runtime focus, popup, touch, rendering and teardown regressions remain
unverified because tests are deferred. The retained `live-preview` feature
currently warns and uses compiled UI too.

### Development build timing comparison

A subsequent comparison built the real UI libraries in default Nix with
`cargo build`, including Rust code generation. Two source-only snapshots used
the same current source, pinned compiler patch, dependency versions, desktop
renderer features and development profiles. One reconstructed the inline
Settings tree; the other retained the compiled factory split. Both reused the
repository's existing `target/`, with normal incremental compilation and one
Cargo command at a time. No target directory was copied or cleaned.

Each variant received one initial build and then the same Settings edit:
both occurrences of `@tr("Make Nova yours")` changed to
`@tr("Make Nova yours 1")`. All four commands succeeded. The command, run
inside the default Nix environment, was:

```sh
cargo build -p nova-ui --locked --features slint/backend-winit,slint/renderer-femtovg --target-dir /home/mb/programming/rust/nova/target --timings --manifest-path /tmp/nova-ui-build-time-pmajl3_o/<variant>/Cargo.toml
```

| Measurement | Inline Settings | Compiled split | Reduction |
| --- | ---: | ---: | ---: |
| Fresh Rust UI compilation, first UI compiler start to last UI compiler finish | 239.88 s | 210.19 s | 12.4% |
| Full UI build command after the identical Settings edit | 113.97 s | 74.03 s | 35.0% |
| Sampled largest UI rustc RSS, initial build | 11.18 GiB | 8.02 GiB | 28.3% |
| Sampled descendant-process RSS peak, Settings edit | 10.75 GiB | 6.54 GiB | 39.1% |

The initial inline command took 327.72 s and also built missing shared
dependency artifacts; the split command took 215.36 s after that warm-up.
Those whole-command initial durations are not a fair comparison. The fresh
Rust row instead uses Cargo's UI-unit timing boundaries, excludes dependency
warm-up and counts both split libraries. Their compiler work partially
overlaps because Cargo pipelines the host after the leaf emits metadata.
The edit row includes Slint generation and both libraries' rebuilds: Settings
edits still invalidate the host, but incremental reuse reduces its work.

These are one-sample development UI-library measurements, not full application
or release benchmarks. RSS is sampled, and summing process RSS can count shared
pages more than once. No tests or runtime/GPU measurements were run. The
compact [measurement evidence](build-time-results.json) preserves commands,
Cargo timing units, sampled peaks and source hashes; complete logs and timing
HTML remain in `/tmp/nova-ui-build-time-pmajl3_o` for this session.

## Shared globals and additional page boundaries

The production split now also includes Home, Discover, Calendar and Detail in
`crates/ui-{home,discover,calendar,detail}`. Their original visual sources remain
in `crates/ui`; AppWindow uses lightweight `*-host.slint` containers. The canonical
models, existing globals and page contracts live in `nova-ui-shared`. Settings
uses the same shared bundle rather than copying globals and linking their fields.

The pinned compiler exposes `shared_globals_novauishared()` and
`new_embedded_with_shared_globals(context, bundle)` so embedded roots reuse the
parent's initialized global objects and window adapter. Callbacks have weak root
references; persistent state and navigation stay on AppWindow, and transient
menus, timers and crossfade images stay page-local. These additional boundaries
have not received a build-time or runtime benchmark; the measurements above
apply only to the earlier Settings comparison.

Generated source sizes from the default-Nix check of this split:

| Unit | Rust source bytes | MiB |
| --- | ---: | ---: |
| `nova-ui` | 15,060,980 | 14.363 |
| `nova-ui-shared` | 659,711 | 0.629 |
| `nova-ui-settings` | 28,503,722 | 27.183 |
| `nova-ui-home` | 4,466,967 | 4.260 |
| `nova-ui-discover` | 5,470,851 | 5.217 |
| `nova-ui-calendar` | 2,347,781 | 2.239 |
| `nova-ui-detail` | 17,394,621 | 16.589 |

AppWindow is 14.4 MiB, compared with 40.1 MiB in the initial Settings split.
At that stage, the largest unit was Settings at 27.2 MiB. Source size describes the compiler
work boundary; it is not a measurement of runtime memory or build-wide RSS.

The shared-globals/page split passes default-Nix `cargo check`, workspace
Clippy with default targets (`--locked -- -D warnings`), and
`cargo fmt --all -- --check`. Commands ran sequentially with normal
incremental compilation in the existing target directory. No tests or
platform builds ran; runtime regressions remain unverified.

### Settings submenu boundaries

Settings now has a compiled navigation shell plus thirteen independently
compiled submenu bodies in `crates/ui-settings-*`. This includes the nested
Downloaded episodes list. The shell keeps the overview, header, shared scroll
viewport, navigation, scroll restoration, transitions and shared overlays.

The submenu factories in `crates/ui-settings/src/submenus.rs` capture weak shell
handles and attach to its window adapter. `SettingsBridge` remains the source of
backend values and shared model handles. `SettingsContent` holds transient
shell state, submenu factories, layout dimensions and helper callbacks; it is
reset before each fresh Settings shell, without resetting saved settings.
Each selected body publishes its intrinsic layout height at the actual width,
so wrapping and scroll extents retain the original layout rules. Original
mount predicates remain intact, including the hidden selected body during the
narrow detail pane's return transition. Unused visual imports were removed to
avoid rebuild dependencies on unrelated submenu widgets.

Generated Rust source sizes after this extraction (default Nix, existing target):

| Unit | Rust source bytes | MiB |
| --- | ---: | ---: |
| `nova-ui-settings` | 5,448,941 | 5.197 |
| `nova-ui-settings-addons` | 1,799,111 | 1.716 |
| `nova-ui-settings-categories` | 1,120,656 | 1.069 |
| `nova-ui-settings-display` | 2,340,123 | 2.232 |
| `nova-ui-settings-downloaded-episodes` | 297,076 | 0.283 |
| `nova-ui-settings-downloads` | 899,364 | 0.858 |
| `nova-ui-settings-home` | 1,612,318 | 1.538 |
| `nova-ui-settings-image-cache` | 4,474,276 | 4.267 |
| `nova-ui-settings-look-feel` | 1,813,391 | 1.729 |
| `nova-ui-settings-p2p` | 2,904,683 | 2.770 |
| `nova-ui-settings-player` | 3,460,490 | 3.300 |
| `nova-ui-settings-sync` | 3,382,411 | 3.226 |
| `nova-ui-settings-theme` | 2,903,183 | 2.769 |
| `nova-ui-settings-tracking` | 1,279,790 | 1.221 |

The shell is about 5.2 MiB and the largest submenu (Image cache) about 4.3 MiB, compared with
27.2 MiB in the former Settings unit. These numbers describe individual compile
units, not total generated bytes, runtime heap, CPU cycles or peak build RSS.
Total generated Settings source is 32.2 MiB (formerly 27.2 MiB): shared visual
controls are still compiled into each unit that uses them.
No runtime or build-time benchmark was performed for this further split.

The extraction passes default-Nix `cargo check`, default-target workspace
Clippy (`--workspace --locked -- -D warnings`) and formatting. Existing inspector
queries for moved controls now name the submenu component; the shell header,
scroll viewport and Home catalog modal retain their original queries. No tests
or platform builds ran. Narrow resize, scroll restoration and keyboard reveal
remain runtime regression checks to perform when tests resume.

## Nested Detail and remaining page split

The next production split separates Detail Tracking, its single active linked-entry
editor, Streams and Episodes. Library, More/About/Downloads, Player OSD and
Onboarding now also have compiled factory boundaries. They reuse the parent
window adapter and the exact shared globals; model and pixel handles stay shared.
Tracking owns a full-screen header and scroll viewport while the underlying
Detail tab and scroll stay retained. Desktop navigation orders Calendar and
Downloads before More; wide More hides those duplicate destinations.

Latest generated source sizes from default-Nix checks with the existing target
directory and normal incremental compilation:

| Unit | MiB |
|---|---:|
| AppWindow | 3.74 |
| Detail shell | 2.32 |
| Detail Tracking | 6.41 |
| Detail active entry | 5.90 |
| Detail Streams | 2.02 |
| Detail Episodes | 1.54 |
| Library | 2.59 |
| More/About/Downloads (one Rust unit) | 5.48 |
| Player OSD | 2.85 |
| Onboarding | 1.39 |

Before this split, AppWindow was about 14.4 MiB and Detail about 16.6 MiB.
These measurements bound generated source per compilation unit; they are not
a build-time or runtime benchmark. Subsequent headless regressions cover native
focus, touch, Back navigation, retained Detail state, Settings overflow and page
transitions. Their first run exposed zero-width Tracking/Episodes bodies,
Settings intrinsic-layout recursion and focus lost during factory attachment;
the fixes explicitly size body slots and restore focus after publication while
preserving active editors and popups. Runtime CPU/GPU and retained-memory
measurements remain unmeasured.
