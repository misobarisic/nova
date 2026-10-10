#!/usr/bin/env python3
"""Measure release Nova startup on Linux with isolated, reproducible fixtures.

No build is included in the timed command. All fixture preparation, data copies,
and report IO happen outside app timing. Run --self-test for harness regressions.
"""
import argparse
import csv
import hashlib
import json
import math
import os
from pathlib import Path
import shutil
import statistics
import struct
import subprocess
import sys
import tempfile
import time
import unittest
import zlib

ROOT = Path(__file__).resolve().parents[1]
CASES = ("current", "fresh", "typical", "large-library", "large-cache", "downloads", "services")
BASE_URL = "http://127.0.0.1:9"


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def project_builds():
    """Avoid benchmarking against a compiler saturating this same checkout."""
    active = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdecimal():
            continue
        try:
            name = (entry / "comm").read_text().strip()
            cwd = (entry / "cwd").resolve()
            if name in ("cargo", "cargo-apk2", "rustc") and cwd.is_relative_to(ROOT):
                active.append(entry.name)
        except (OSError, RuntimeError):
            continue
    return active


def require_idle():
    if project_builds():
        raise RuntimeError("a project build is still active; benchmarks must wait")


def require_app_closed():
    for entry in Path("/proc").iterdir():
        if not entry.name.isdecimal():
            continue
        try:
            if (entry / "comm").read_text().strip() == "nova":
                raise RuntimeError("close Nova before taking the current-data snapshot")
        except OSError:
            continue


def copy_tree(source, destination, sparse=False):
    """Never follow symlinks or hard-link writable fixture files to live data."""
    destination.mkdir(parents=True, exist_ok=True)
    if not source.exists():
        return
    for parent, directories, files in os.walk(source, followlinks=False):
        parent = Path(parent)
        directories[:] = [name for name in directories if not (parent / name).is_symlink()]
        target = destination / parent.relative_to(source)
        target.mkdir(parents=True, exist_ok=True)
        for name in files:
            original = parent / name
            if original.is_symlink() or not original.is_file():
                continue
            media = any(part in ("downloads", "torrents") for part in original.relative_to(source).parts[:-1])
            if (sparse or media) and original.suffix not in (".json", ".torrent", ".m3u8"):
                with (target / name).open("wb") as stream:
                    stream.truncate(original.stat().st_size)
                shutil.copystat(original, target / name)
            else:
                shutil.copy2(original, target / name)


def fingerprint(source):
    """Content-check the DB, metadata-check artifact/cache trees outside timing."""
    database = source / "nova.redb"
    files = []
    if source.exists():
        for parent, directories, names in os.walk(source, followlinks=False):
            directories[:] = [n for n in directories if not (Path(parent) / n).is_symlink()]
            for name in names:
                path = Path(parent) / name
                if path.is_file() and not path.is_symlink():
                    info = path.stat()
                    files.append((str(path.relative_to(source)), info.st_size, info.st_mtime_ns))
    return {"database_sha256": digest(database) if database.is_file() else None,
            "files": sorted(files)}


def host_details():
    cpu = next((line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines()
                if line.startswith("model name")), "unknown")
    graphics = []
    for device in sorted(Path("/sys/class/drm").glob("card[0-9]*/device")):
        if not device.parent.name.removeprefix("card").isdecimal():
            continue
        details = {}
        for name in ("vendor", "device"):
            if (device / name).exists():
                details[name] = (device / name).read_text().strip()
        driver = device / "driver"
        if driver.exists():
            details["driver"] = driver.resolve().name
        graphics.append(details)
    return {"cpu": cpu, "logical_cpus": os.cpu_count(), "graphics_devices": graphics}


def fnv1a(value):
    result = 0xCBF29CE484222325
    for byte in value.encode():
        # Match nova_config::fnv1a, including its persisted multiplier.
        result = ((result ^ byte) * 0x1000000001B3) & 0xFFFFFFFFFFFFFFFF
    return f"{result:016x}"


def fixture_png():
    """A valid textured RGBA image; decoding is exercised by the actual app."""
    def chunk(kind, contents):
        return struct.pack(">I", len(contents)) + kind + contents + struct.pack(">I", zlib.crc32(kind + contents))
    width, height = 48, 72
    pixels = b"".join(b"\x00" + bytes((x * 5 % 256, y * 3 % 256, 110, 255)) * width
                      for y, x in enumerate(range(height)))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(pixels)) + chunk(b"IEND", b""))


def fixture_values(case, data):
    if case == "fresh":
        return {}
    count, episode_count = (1000, 100) if case == "large-library" else (100, 24)
    entries, progress, values, previews = [], {}, {}, []
    for index in range(count):
        identity = f"bench:{index:04d}"
        poster = f"{BASE_URL}/art/{index}.png"
        entry = {"id": identity, "type_": "series", "name": f"Benchmark title {index:04d}",
                 "year": "2024", "poster_url": poster, "background_url": poster,
                 "added_at_secs": index + 1}
        entries.append(entry)
        episodes = []
        for number in range(1, episode_count + 1):
            episode_id = f"{identity}:1:{number}"
            episodes.append({"id": episode_id, "name": f"Episode {number}", "season": 1,
                             "episode": number, "released": "2024-01-01T00:00:00Z", "thumbnail": poster})
            if index % 2 == 0 and number <= episode_count // 2:
                progress[f"{identity}\x01{episode_id}"] = {
                    "series_id": identity, "episode_id": episode_id, "position_secs": 1440.0,
                    "duration_secs": 1440.0, "watched": True, "play_count": 1,
                    "updated_at_secs": 1_700_000_000 + number + index}
        values[f"episodes:series\x01{identity}"] = episodes
        values[f"meta_header:series\x01{identity}"] = {
            "poster_url": poster, "background_url": poster, "year": "2024", "description": "Fixture"}
        if index < 60:
            previews.append({"id": identity, "type": "series", "name": entry["name"],
                             "poster": poster, "background": poster, "releaseInfo": "2024"})
    sources, addons = [], []
    for index in range(5):
        url = f"{BASE_URL}/addon-{index}"
        addons.append({"url": url, "enabled": True, "configure_ok": False, "label": f"Fixture {index}"})
        values[f"manifest:{url}"] = {
            "id": f"bench.{index}", "version": "1.0.0", "name": f"Fixture {index}",
            "types": ["series"], "resources": ["catalog", "meta", "stream"],
            "idPrefixes": ["bench:"], "catalogs": [{"type": "series", "id": "bench", "name": "Fixture"}]}
        if index < 3:
            sources.append({"addonUrl": url, "type": "series", "catalogId": "bench", "genre": ""})
    values.update({
        "library": entries, "episode_progress": progress, "addons": addons,
        "providers:bundled:v1": 1,
        "viewing_activity:v1": {entry["id"]: {"phase": "Active"} for entry in entries},
        "settings": {"home_catalog_sources": sources, "home_row_sources": sources},
        "torrent_settings": {"enabled": case == "services"},
        "sync:settings": {"enabled": case == "services", "device_name": "Benchmark",
                          "enable_local_discovery": False},
        "home:showcase:v1": {"catalogs": [{"source": source, "previews": previews[index * 20:index * 20 + 5]}
                                          for index, source in enumerate(sources)]},
        "home:catalog-rows:v1": {"catalogs": [{"source": source, "previews": previews[index * 20:index * 20 + 20]}
                                              for index, source in enumerate(sources)]},
    })
    if case == "downloads":
        jobs = []
        for index in range(150):
            identity = f"fixture-{index}"
            artifact = data / "downloads/http" / identity / "video.mp4"
            artifact.parent.mkdir(parents=True, exist_ok=True)
            with artifact.open("wb") as stream:
                stream.truncate(1024 * 1024)
            if index < 100:
                jobs.append({"id": identity, "media_type": "series", "media_id": entries[0]["id"],
                             "request_id": f"bench:0000:1:{index + 1}", "title": "Fixture download",
                             "phase": "completed", "artifact_path": str(artifact),
                             "bytes_downloaded": 1024 * 1024,
                             "source": {"http": {"url": f"{BASE_URL}/video.mp4"}}})
        values["downloads:v1"] = {"version": 1, "jobs": jobs}
    return values


def env_for(root):
    environment = os.environ.copy()
    for name in ("NOVA_ADDONS", "NOVA_AUTOPLAY_URL", "NOVA_STARTUP_BENCH", "NOVA_STARTUP_BENCH_CONFIG",
                 "NOVA_STARTUP_BENCH_PREPARE", "NOVA_STARTUP_BENCH_TRACE", "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY",
                 "http_proxy", "https_proxy", "all_proxy"):
        environment.pop(name, None)
    environment.update(XDG_DATA_HOME=str(root / "data"), XDG_CACHE_HOME=str(root / "cache"), RUST_LOG="error")
    return environment


def prepare(binary, root, values=None, previous=None, previous_cache=None):
    path = root / "prepare.json"
    configuration = {"data_dir": str(root / "data/nova"), "values": values or {},
                     "rebase_from": str(previous) if previous else None,
                     "rebase_cache_from": str(previous_cache) if previous_cache else None}
    path.write_text(json.dumps(configuration))
    environment = env_for(root)
    environment["NOVA_STARTUP_BENCH_PREPARE"] = str(path)
    subprocess.run([str(binary)], env=environment, check=True, capture_output=True, timeout=120)
    path.unlink()


def make_fixture(binary, case, root, source_data, source_cache):
    data, cache = root / "data/nova", root / "cache/nova"
    data.mkdir(parents=True)
    cache.mkdir(parents=True)
    if case == "current":
        # Copies are taken only with Nova closed. Avoid copying media bytes:
        # reconciliation only uses file existence, size and directory layout.
        for child in source_data.iterdir():
            if child.is_symlink():
                continue
            if child.is_dir():
                copy_tree(child, data / child.name, sparse=child.name == "downloads")
            elif child.is_file():
                shutil.copy2(child, data / child.name)
        copy_tree(source_cache, cache)
        prepare(binary, root, previous=source_data, previous_cache=source_cache)
    elif case != "fresh":
        posters = cache / "posters"
        posters.mkdir()
        png = fixture_png()
        count = 10000 if case == "large-cache" else 1000
        for index in range(count):
            (posters / f"{fnv1a(f'{BASE_URL}/art/{index}.png')}.img").write_bytes(png)
        prepare(binary, root, fixture_values(case, data))


def validate_record(record, navigation):
    if record.get("schema_version") != 1 or record.get("navigation") != navigation:
        raise ValueError("unsupported or mismatched benchmark result")
    measurement = record["measurements"]
    if not measurement["finished"] or not measurement["models_ready"]:
        raise ValueError("startup observation did not finish")
    marks = measurement["milestones_us"]
    required = ("event_loop", "renderer_setup", "first_render_start", "first_render", "home_ready_render")
    if any(name not in marks for name in required):
        raise ValueError("result lacks a completed startup/Home render")
    if not (marks["event_loop"] <= marks["renderer_setup"] <= marks["first_render_start"]
            <= marks["first_render"] <= marks["home_ready_render"]):
        raise ValueError("startup milestones are out of order")
    if marks.get("home_models_ready", math.inf) > marks["home_ready_render"]:
        raise ValueError("a loading shell was counted as ready Home")
    if navigation != "home" and not (marks.get("navigation_dispatch", math.inf)
                                    <= marks.get("navigation_render", -1)):
        raise ValueError("navigation did not complete a rendered frame")
    if measurement["storage_failed"] or measurement["truncated_spans"]:
        raise ValueError("storage failed or span recording was truncated")
    if not measurement["heartbeat_lateness_us"]:
        raise ValueError("post-render responsiveness was not observed")
    spans = {span["id"]: span for span in measurement["spans"]}
    if len(spans) != len(measurement["spans"]):
        raise ValueError("duplicate span ids")
    for span in spans.values():
        if span["parent_id"] is not None:
            parent = spans.get(span["parent_id"])
            if parent is None or not (parent["start_us"] <= span["start_us"]
                                     and span["start_us"] + span["duration_us"]
                                     <= parent["start_us"] + parent["duration_us"]):
                raise ValueError("nested span lies outside its parent")
    return measurement


def percentile(values, fraction):
    ordered = sorted(values)
    position = (len(ordered) - 1) * fraction
    lo, hi = math.floor(position), math.ceil(position)
    return ordered[lo] + (ordered[hi] - ordered[lo]) * (position - lo)


def metrics(measurement):
    marks = measurement["milestones_us"]
    result = {name + "_ms": value / 1000 for name, value in marks.items()}
    late = measurement["heartbeat_lateness_us"]
    result.update(heartbeat_p95_ms=percentile(late, .95) / 1000, heartbeat_max_ms=max(late) / 1000)
    if "navigation_render" in marks:
        result["navigation_latency_ms"] = (marks["navigation_render"] - marks["navigation_dispatch"]) / 1000
    # Repeated scans remain separate spans in raw output. Aggregate their
    # inclusive durations by name, never sum parents and children together.
    for span in measurement["spans"]:
        key = "phase_" + span["name"] + "_ms"
        result[key] = result.get(key, 0) + span["duration_us"] / 1000
    return result


def run_one(binary, case, navigation, fixture, work, validation=False):
    require_idle()
    if work.exists():
        shutil.rmtree(work)
    copy_tree(fixture, work)
    prepare(binary, work, previous=fixture / "data/nova", previous_cache=fixture / "cache/nova")
    output = work / "result.json"
    config = work / "run.json"
    config.write_text(json.dumps({"output": str(output), "navigation": navigation,
                                 "validate_snapshot": validation}))
    environment = env_for(work)
    environment.update(NOVA_STARTUP_BENCH="1", NOVA_STARTUP_BENCH_CONFIG=str(config))
    started = time.monotonic()
    record = None
    try:
        process = subprocess.run([str(binary)], env=environment, capture_output=True, timeout=30)
        (work / "stderr.log").write_bytes(process.stderr)
        if process.returncode:
            return {"success": False, "failure": "app_exit", "exit_code": process.returncode}
        if not output.exists():
            return {"success": False, "failure": "missing_result"}
        record = json.loads(output.read_text())
        measurement = validate_record(record, navigation)
        if validation and not record.get("snapshot_validated"):
            raise ValueError("render snapshot validation did not complete")
        if case not in ("current", "fresh"):
            expected = 1000 if case == "large-library" else 100
            if (measurement["home_counts"]["library"] != expected
                    or measurement["home_counts"]["featured"] != 15
                    or measurement["home_counts"]["catalog_cards"] != 60
                    or "featured_art_render" not in measurement["milestones_us"]):
                raise ValueError("synthetic library or cached Home catalogs were not restored")
        return {"success": True, "result": record, "metrics": metrics(measurement),
                "process_wall_ms": (time.monotonic() - started) * 1000}
    except subprocess.TimeoutExpired as error:
        (work / "stderr.log").write_bytes(error.stderr or b"")
        return {"success": False, "failure": "watchdog_timeout"}
    except (ValueError, KeyError, TypeError) as error:
        failed = {"success": False, "failure": str(error)}
        if record is not None:
            failed["result"] = record
        return failed


def write_reports(output, rows):
    groups = {}
    for row in rows:
        if row["warmup"] or row["validation"]:
            continue
        group = groups.setdefault((row["binary"], row["case"], row["navigation"]), [])
        group.append(row)
    summaries = []
    for (binary, case, navigation), runs in groups.items():
        successful = [run for run in runs if run["success"]]
        names = sorted({name for run in successful for name in run["metrics"]})
        for name in names:
            values = [run["metrics"][name] for run in successful if name in run["metrics"]]
            summaries.append({"binary": binary, "case": case, "navigation": navigation, "metric": name,
                              "runs": len(runs), "successful": len(successful), "samples": len(values),
                              "median_ms": statistics.median(values), "p90_ms": percentile(values, .9),
                              "p95_ms": percentile(values, .95), "min_ms": min(values), "max_ms": max(values)})
        if not successful:
            summaries.append({"binary": binary, "case": case, "navigation": navigation, "metric": "failed",
                              "runs": len(runs), "successful": 0, "samples": 0})
    fields = ("binary", "case", "navigation", "metric", "runs", "successful", "samples", "median_ms", "p90_ms", "p95_ms", "min_ms", "max_ms")
    with (output / "summary.csv").open("w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=fields)
        writer.writeheader()
        writer.writerows(summaries)
    text = ["# Desktop startup benchmark", "", "Fresh processes; filesystem caches warm. Timings start at main entry.",
            "First render precedes presentation. Process wall time includes the three-second observation and shutdown.",
            "", "| Binary | Case | Navigation | Metric | Median ms | p95 ms | Successful/total |",
            "|---|---|---|---|---:|---:|---:|"]
    for row in summaries:
        if row["metric"] in ("first_render_ms", "home_ready_render_ms", "featured_art_render_ms", "heartbeat_max_ms", "navigation_latency_ms", "failed"):
            median, tail = row.get("median_ms"), row.get("p95_ms")
            text.append(f"| {row['binary']} | {row['case']} | {row['navigation']} | {row['metric']} | "
                        f"{f'{median:.2f}' if median is not None else '—'} | {f'{tail:.2f}' if tail is not None else '—'} | "
                        f"{row['successful']}/{row['runs']} |")
    failures = sum(not row["success"] for row in rows)
    text += ["", f"Failures retained in raw.jsonl: {failures}.", "", "Phase timings are inclusive; do not add parent and child spans."]
    (output / "report.md").write_text("\n".join(text) + "\n")


def run_suite(args):
    require_idle()
    if args.android_build_log:
        log = args.android_build_log.read_text()
        if 'Finished `release` profile' not in log or "Signing `" not in log:
            raise RuntimeError("Android build log does not show completed release packaging")
    binaries = [args.binary.resolve()]
    if args.compare:
        binaries.append(args.compare.resolve())
    if any(not binary.is_file() or not os.access(binary, os.X_OK) for binary in binaries):
        raise RuntimeError("build the release executable before benchmarking")
    cases = list(CASES) if args.case == "all" else [args.case]
    navigations = ["home", "library", "settings"] if args.navigation == "all" else [args.navigation]
    source_data = args.source_data.resolve()
    source_cache = args.source_cache.resolve()
    before = None
    if "current" in cases:
        require_app_closed()
        if not (source_data / "nova.redb").is_file():
            raise RuntimeError("current-data case requires an existing nova.redb")
        before = (fingerprint(source_data), fingerprint(source_cache))
    args.output.mkdir(parents=True, exist_ok=False, mode=0o700)
    metadata = {"schema_version": 1, "host": list(os.uname()), "session_type": os.getenv("XDG_SESSION_TYPE"),
                "backend": "winit", "renderer": "femtovg", "filesystem_cache": "warm",
                "window_system": "wayland" if os.getenv("WAYLAND_DISPLAY") or os.getenv("WAYLAND_SOCKET") else "x11",
                "network": "isolated", "iterations": args.iterations, "warmups": args.warmups,
                "cases": cases, "navigations": navigations, "window_physical_pixels": [1280, 800],
                "observation_seconds": 3, "hardware": host_details(), "runner_sha256": digest(Path(__file__)),
                "binaries": [{"path": str(binary), "sha256": digest(binary)} for binary in binaries],
                "source_revision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
                "tracked_diff_sha256": hashlib.sha256(subprocess.check_output(["git", "diff", "HEAD"], cwd=ROOT)).hexdigest()}
    # New benchmark sources may not be tracked yet; the patch hash alone
    # cannot identify the implementation used by an uncommitted build.
    changed = subprocess.check_output(["git", "ls-files", "--modified", "--others", "--exclude-standard", "-z"], cwd=ROOT)
    metadata["changed_source_sha256"] = {
        name: digest(ROOT / name) for name in sorted(set(changed.decode().split("\0")))
        if name and (ROOT / name).is_file()
    }
    if before is not None:
        metadata["source_snapshot_sha256"] = hashlib.sha256(json.dumps(before, sort_keys=True).encode()).hexdigest()
    (args.output / "environment.json").write_text(json.dumps(metadata, indent=2))
    rows = []
    try:
        with tempfile.TemporaryDirectory(prefix="fixtures-", dir=args.output) as directory:
            temporary = Path(directory)
            for case in cases:
                print(f"Preparing {case} fixture", flush=True)
                fixture = temporary / "fixture"
                if fixture.exists():
                    shutil.rmtree(fixture)
                make_fixture(binaries[0], case, fixture, source_data, source_cache)
                for navigation in navigations:
                    # Separate screenshot validation from all timing distributions.
                    total = args.warmups + args.iterations + 1
                    for iteration in range(total):
                        order = range(len(binaries)) if iteration % 2 == 0 else reversed(range(len(binaries)))
                        for index in order:
                            row = {"binary": f"binary-{index}", "case": case, "navigation": navigation,
                                   "iteration": iteration, "validation": iteration == 0,
                                   "warmup": 0 < iteration <= args.warmups}
                            row.update(run_one(binaries[index], case, navigation, fixture, temporary / "work", iteration == 0))
                            rows.append(row)
                            if not row["success"] and (temporary / "work/stderr.log").exists():
                                filename = f"failure-{case}-{navigation}-{index}-{iteration}.log"
                                shutil.copy2(temporary / "work/stderr.log", args.output / filename)
                                row["diagnostic_file"] = filename
                            with (args.output / "raw.jsonl").open("a") as stream:
                                stream.write(json.dumps(row) + "\n")
                            print(f"{case}/{navigation} binary-{index} {iteration}/{total - 1}: "
                                  f"{row.get('metrics', {}).get('first_render_ms', row.get('failure'))}", flush=True)
                            # Failed preflight/validation should not waste 30
                            # launches repeating a broken renderer or fixture.
                            if iteration == 0 and not row["success"]:
                                raise RuntimeError(f"validation failed: {row.get('failure')}")
                write_reports(args.output, rows)
    finally:
        write_reports(args.output, rows)
        if before is not None:
            after = (fingerprint(source_data), fingerprint(source_cache))
            if after != before:
                raise RuntimeError("source data changed during the suite; comparison is invalid")
    print(f"Report: {args.output / 'report.md'}", flush=True)
    return int(any(not row["success"] for row in rows))


class HarnessTests(unittest.TestCase):
    def valid(self):
        return {"schema_version": 1, "navigation": "home", "measurements": {
            "milestones_us": {"event_loop": 10, "renderer_setup": 20, "first_render_start": 30,
                              "first_render": 40, "home_models_ready": 9, "home_ready_render": 40},
            "spans": [{"id": 1, "parent_id": None, "name": "startup", "start_us": 0, "duration_us": 10},
                      {"id": 2, "parent_id": 1, "name": "scan", "start_us": 3, "duration_us": 4}],
            "finished": True, "models_ready": True,
            "storage_failed": False, "truncated_spans": False, "heartbeat_lateness_us": [0, 100]}}

    def test_incomplete_or_out_of_order_result_cannot_pass(self):
        for mutation in (lambda m: m["milestones_us"].pop("first_render"),
                         lambda m: m.update(finished=False),
                         lambda m: m["milestones_us"].update(first_render=1),
                         lambda m: m["milestones_us"].update(home_models_ready=50),
                         lambda m: m["spans"][1].update(duration_us=20)):
            record = self.valid()
            mutation(record["measurements"])
            with self.assertRaises(ValueError):
                validate_record(record, "home")

    def test_failures_and_missing_metrics_remain_in_report(self):
        with tempfile.TemporaryDirectory() as directory:
            rows = [{"binary": "a", "case": "fresh", "navigation": "home", "warmup": False,
                     "validation": False, "success": True, "metrics": {"first_render_ms": 10}},
                    {"binary": "a", "case": "fresh", "navigation": "home", "warmup": False,
                     "validation": False, "success": False, "failure": "watchdog_timeout"}]
            write_reports(Path(directory), rows)
            text = (Path(directory) / "report.md").read_text()
            self.assertIn("1/2", text)
            self.assertIn("Failures retained in raw.jsonl: 1", text)

    def test_copy_does_not_follow_links_or_modify_original(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, target = root / "source", root / "target"
            source.mkdir()
            (source / "file").write_text("original")
            (source / "link").symlink_to(source / "file")
            copy_tree(source, target)
            (target / "file").write_text("changed")
            self.assertEqual((source / "file").read_text(), "original")
            self.assertFalse((target / "link").exists())

    def test_nested_spans_are_preserved_without_double_counting(self):
        measurement = validate_record(self.valid(), "home")
        calculated = metrics(measurement)
        self.assertEqual(calculated["phase_startup_ms"], .010)
        self.assertEqual(calculated["phase_scan_ms"], .004)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/nova")
    parser.add_argument("--compare", type=Path)
    parser.add_argument("--case", choices=("all",) + CASES, default="all")
    parser.add_argument("--navigation", choices=("home", "library", "settings", "all"), default="home")
    parser.add_argument("--window-system", choices=("auto", "wayland", "x11"), default="auto",
                        help="select Wayland or X11; auto uses the current desktop environment")
    parser.add_argument("--iterations", type=int, default=30)
    parser.add_argument("--warmups", type=int, default=5)
    parser.add_argument("--source-data", type=Path, default=Path(os.getenv("XDG_DATA_HOME", str(Path.home() / ".local/share"))) / "nova")
    parser.add_argument("--source-cache", type=Path, default=Path(os.getenv("XDG_CACHE_HOME", str(Path.home() / ".cache"))) / "nova")
    parser.add_argument("--output", type=Path, default=ROOT / "target/startup-bench" / time.strftime("%Y%m%d-%H%M%S"))
    parser.add_argument("--android-build-log", type=Path, help="require successful Android release packaging in this log")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--inside-namespace", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.self_test:
        return not unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(HarnessTests)).wasSuccessful()
    if args.iterations < 1 or args.warmups < 0:
        parser.error("iterations must be positive and warmups nonnegative")
    if sys.platform != "linux":
        parser.error("the initial runner requires Linux network namespaces and a real desktop display")
    if args.window_system == "x11":
        if not os.getenv("DISPLAY"):
            parser.error("X11 requires DISPLAY")
        os.environ.pop("WAYLAND_DISPLAY", None)
        os.environ.pop("WAYLAND_SOCKET", None)
    elif args.window_system == "wayland":
        if not (os.getenv("WAYLAND_DISPLAY") or os.getenv("WAYLAND_SOCKET")):
            parser.error("Wayland requires WAYLAND_DISPLAY or WAYLAND_SOCKET")
        os.environ.pop("DISPLAY", None)
    args.output = args.output.resolve()
    if not args.inside_namespace:
        require_idle()
        environment = os.environ.copy()
        environment["NOVA_BENCH_PARENT_NETNS"] = os.readlink("/proc/self/ns/net")
        # Preserve the desktop UID for D-Bus EXTERNAL authentication while
        # retaining namespace capabilities long enough to enable loopback.
        return subprocess.call(["unshare", "--map-current-user", "--net", "--keep-caps", sys.executable,
                                str(Path(__file__).resolve()), *sys.argv[1:], "--inside-namespace"], env=environment)
    if os.readlink("/proc/self/ns/net") == os.getenv("NOVA_BENCH_PARENT_NETNS", os.readlink("/proc/self/ns/net")):
        raise RuntimeError("network isolation was not established")
    interfaces = [line.split(":", 1)[0].strip() for line in Path("/proc/net/dev").read_text().splitlines()[2:]]
    if interfaces != ["lo"]:
        raise RuntimeError("benchmark namespace unexpectedly has external network interfaces")
    subprocess.run(["ip", "link", "set", "lo", "up"], check=True)
    os.umask(0o077)
    return run_suite(args)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"benchmark stopped: {error}", file=sys.stderr)
        sys.exit(1)
