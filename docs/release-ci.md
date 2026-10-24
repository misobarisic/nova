# Release CI and dependency caching

Push `v<package.version>` for a commit already on `main`. The lightweight
`dispatch-release.yml` uses its job-scoped `GITHUB_TOKEN` (`actions: write`) to
dispatch `release-on-main.yml` with `ref=main`, the tag and the exact tagged
commit. No warmer or external cache service is needed. Merge this workflow
change before creating the first tag that uses it. Historical tags predating
the release-identity support are rejected by the new dispatcher target.

The dispatched workflow's ref/cache scope is `refs/heads/main`; its source
checkout is the validated tag commit, not the current main HEAD. Trusted
validation code is checked out separately from the workflow's main commit.
The validator checks the full commit SHA, tag resolution, main ancestry and
`v` + Cargo version. Annotated tag objects are pinned too. It refreshes remote
refs before each build, after builds and immediately before publication, so
observed tag movement or deletion fails closed. The publishing job alone has
`contents: write` and explicitly supplies `tag_name` and `target_commitish`.
It waits for the reused Linux/Windows/Android checks and every packaging job.

Concurrency is per tag with cancellation disabled. A second publishing
dispatch is rejected if a release for that tag already exists. Different tags
do not cancel each other. GitHub's ref check and release creation are separate
API operations: the workflow does not claim an atomic lock against a repository
writer mutating a tag during that final API interval.

`NOVA_RELEASE_TAG` takes precedence over GitHub ref-name fallbacks in build
metadata and all packagers. It must match Cargo's version. Debian versions,
Windows ZIPs, Android APK versions/names and the GitHub release therefore agree.
Local builds without this variable keep their existing behavior. Cargo profiles,
dependencies, Nix inputs and Make defaults are unchanged.

## Release cache identities

All use `Swatinem/rust-cache` v2.9.3 pinned to
`d1527c8f67ef9af357290475d4af4b0fab8f12e0`, prefix `nova-release-v1`, default
dependency-only pruning, `cache-targets: true`, and `cache-bin: false`.

| Build | Stable shared identity | Rust environment |
| --- | --- | --- |
| Linux | release-linux-ubuntu22-x86_64-unknown-linux-gnu-cd-release | existing native stable Rust, Ubuntu 22.04 |
| Windows | release-windows-gnu-x86_64-pc-windows-gnu-cd-release | Nix Windows |
| Android ARM64 | release-android-aarch64-aarch64-linux-android-cd-release | Nix Android |
| Android ARMv7 | release-android-armv7-armv7-linux-androideabi-cd-release | Nix Android |
| Android x86_64 | release-android-x86_64-x86_64-linux-android-cd-release | Nix Android |

Nix identities also contain a hash of `flake.lock`, `flake.nix` and `nix/**`.
The action adds runner OS/architecture, actual compiler versions, relevant
Cargo/Rust environment and Cargo manifest/lockfile fingerprints. Android's
command wrapper sends its shell-hook banner to stderr while preserving Rust
and Cargo stdout; direct `nix develop` output otherwise breaks detection.
Check caches retain their existing job-ID keys and remain separate from these
release/profile/target keys; only `main` saves check caches.

Neither tag names nor commit SHAs are in release cache prefixes. Dependency
changes produce new exact keys with environment-compatible fallback restore.
The pinned action deliberately normalizes workspace package versions: a pure
Nova version bump can reuse an exact key without an unnecessary upload. It
does not overwrite exact-hit caches. Successful cold/partial-hit builds save
new dependency keys; failures do not save. A miss runs the normal compilation.
Full executables, APKs, packaging directories and workspace crate artifacts
are not retained in Rust dependency caches. Artifact uploads stay separate.

## Nonpublishing validation and benchmarks

After merge, dispatch against `main` with `publish=false`, for example:

```sh
gh workflow run release-on-main.yml --ref main \
  -f tag=vX.Y.Z -f commit=<full-tagged-commit-sha> -f publish=false
```

This runs source validation, checks, all five release builds and artifact
verification, and may populate release caches, but skips GitHub publication.
It requires a tag on trusted main source containing this release support;
it does not create a public test release. Re-run a completed dry run to measure
an exact hit, then compare the next valid version/dependency change for fallback
reuse. Do not add per-tag cache keys to force new uploads.

Linux and Windows release builds use `cargo build --timings` and upload
`target/cargo-timings/` separately for seven days, including failed runs where
available. Android retains the existing cargo-apk2/Make invocation; use its
Cargo-reported duration and the job log. Record total job duration, Cargo
duration, cache download/extraction, compression/upload time and stored bytes.
Slint code generation, workspace compilation, native tooling and fat LTO can
still dominate; cached dependencies do not remove those costs.

Measured on 2026-10-24 before this release change:

| Measurement | Cold | Warm |
| --- | ---: | ---: |
| Windows GNU check job on main | 12m22s | 5m14s |
| Windows GNU check Cargo time | 10m57s | 4m00s |
| Windows check cache transfer | no download; save about 6s including compression | download about 4s; restore including extraction about 8s; no upload |

The exact-hit check archive was 524,156,182 bytes (499.9 MiB). The three main
check caches added about 1.60 GiB combined compressed data. Evidence:
[cold main run](https://github.com/misobarisic/nova/actions/runs/40103052070/attempts/1),
[warm Windows job](https://github.com/misobarisic/nova/actions/runs/40103052070/job/120904787561).
These are check measurements, not a release speedup claim.

Historical uncached v0.0.43 release baseline:

| Release target | Total job | Cargo cd-release time |
| --- | ---: | ---: |
| Linux Ubuntu 22.04 | 43m49s | 38m45s |
| Windows GNU | 35m05s | 33m29s |
| Android ARM64 | 21m07s | 15m44s |
| Android ARMv7 | 30m35s | 25m14s |
| Android x86_64 | 23m57s | 16m25s |

[Release baseline](https://github.com/misobarisic/nova/actions/runs/40098196815).
Release cache hits, release transfer overhead and release cache sizes have
**not** been measured by this PR: the new dispatch workflow must first exist
on main, and no public test releases are created. Local Git fixtures exercise
successive versions, annotated/moved/deleted tags, wrong commits/versions,
unmerged source, duplicate publication and unsafe inputs. This is validation
of source identity, not an experiment proving compiled-dependency reuse.
Measure all five archives and repository cache eviction after the first dry
run or actual release; do not extrapolate release storage from debug checks.
