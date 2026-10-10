# Desktop startup benchmarks

`scripts/benchmark-startup.py` measures the real desktop executable with Winit,
FemtoVG and mpv. It launches fresh processes on the existing desktop display;
filesystem caches are warm. The runner does not build or measure Android.

Finish the Android build before running benchmarks. The runner refuses to run
while Cargo or rustc is building this checkout. A completed Android release log
can also be supplied as a gate:

```sh
nix develop --command cargo build --release --locked -j 1
python3 scripts/benchmark-startup.py --self-test
nix develop --command python3 scripts/benchmark-startup.py \
  --android-build-log /tmp/nova-release-apk.log
```

The default suite runs all seven fixtures, with one separate screenshot
validation, five discarded warmups and 30 measured launches per fixture. It
requires Linux user/network namespaces, `unshare`, `ip`, Python 3.11 or later,
and a working local desktop display. Render validation failures stop the suite;
failed measured launches remain in the report.

| Fixture | Contents |
|---|---|
| `current` | Independent copy of the current database and cache; Nova must be closed while the snapshot is taken. |
| `fresh` | Empty data and cache directories, using production defaults. |
| `typical` | 100 series, 24 episodes each, playback history, five cached addon manifests, cached Home catalogs and artwork. |
| `large-library` | 1,000 series with 100 episodes each. |
| `large-cache` | Typical data with 10,000 cached image files. |
| `downloads` | Typical data with 100 completed downloads and 50 orphan directories. |
| `services` | Typical data with sync and torrent services enabled, no paired peers and local discovery disabled. |

Each launch restores the pristine fixture into isolated `XDG_DATA_HOME` and
`XDG_CACHE_HOME` directories. Symlinks are skipped and writable files are never
hard-linked to live data. Media files are represented by sparse files with the
same size; these fixtures test startup reconciliation, not playback. Download
paths, torrent storage and persisted tracked-torrent directories are relocated
inside the fixture. All app processes
run in an offline network namespace with loopback only. The current database
hash and source tree metadata are checked again after the suite.

Reports and temporary copies have private permissions because current-data
fixtures can contain credentials. Temporary fixtures are removed on completion.
Keep reports containing failure diagnostics private: stderr can include paths
or application data.

For a narrower run, navigation coverage or an alternating A/B comparison:

```sh
nix develop --command python3 scripts/benchmark-startup.py --case typical
nix develop --command python3 scripts/benchmark-startup.py \
  --case current --navigation all
nix develop --command python3 scripts/benchmark-startup.py \
  --binary /path/to/baseline/nova --compare target/release/nova --case typical
```

`--source-data`, `--source-cache`, `--output`, `--iterations`, `--warmups` and
`--window-system auto|wayland|x11`
override their defaults. Compared binaries must support the same benchmark
configuration and result schema. The runner alternates their order and restores
the same fixture before every launch. Keep the window visible during measurement:
Wayland compositors can suspend frame callbacks for fully covered windows.
`--window-system x11` uses the existing X11/Xwayland display when necessary;
`environment.json` records the selected window system. Compare distributions
collected with the same window system.

Timing begins at `main` entry. The benchmark records nested startup phases,
renderer setup, the first completed render, a completed render after local Home
models are restored, and the first render with nonempty featured artwork
assigned to the UI. That artwork marker does not wait for reveal animations
to finish. A valid empty Home qualifies as ready; a loading shell does not.
Slint's `AfterRendering` callback precedes presentation, so these timestamps do
not measure when pixels reach the physical screen.

`app::run()` creates the window/backend, opens storage, initializes the
player and workers, and wires callbacks before restoring addons and preferences.
It loads Library/progress snapshots, seeds only missing sync baselines, replays
unapplied sync domains, and then derives Library and Home models once. A store
without projection receipts requires one migration replay. Preferences precede
Home construction, and bulk addon restoration preserves cached Home catalogs.
On desktop, the first `AfterRendering` queues download startup and sync/torrent
engine construction; engine setup runs off the UI thread. Settings cache-size
scans run on workers when Settings is mounted, coalescing repeated requests and
rejecting stale results. Android keeps its existing service startup timing.

For three seconds after the first render, a 16 ms timer records event-loop
lateness. Library or Settings navigation is dispatched after that interval and
measured through the next completed render of the selected page. Screenshot
validation runs separately and is excluded from timing distributions. Process
wall time includes the observation interval and shutdown and is not a startup
metric. Phase durations are inclusive: do not add parent and child spans.

Results default to `target/startup-bench/<timestamp>/`:

- `raw.jsonl`: every validation, warmup and measured launch, including failures,
  timestamps, nested spans and heartbeat samples.
- `summary.csv`: per-metric median, p90, p95, minimum, maximum and sample counts.
- `report.md`: the main startup, responsiveness and navigation measurements.
- `environment.json`: host/session details, binary hashes, source revision,
  tracked diff hash and suite configuration.
- `failure-*.log`: stderr from failed launches, when available.

There are no CI performance thresholds yet. Compare release builds on the same
machine and session, with other build activity stopped. The ordinary app path
does not collect measurements. The desktop-only recorder is enabled by
`NOVA_STARTUP_BENCH=1` and `NOVA_STARTUP_BENCH_CONFIG`; the runner also uses
`NOVA_STARTUP_BENCH_PREPARE` to seed and relocate fixtures without opening a UI.
Keep the `models_ready` marker with the completion of local model restoration
if startup work is later deferred, so early shells cannot improve the ready-Home
measurement artificially.
