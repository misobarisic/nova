#!/usr/bin/env python3
"""Record isolated B-START raw logs; supply synthetic fixtures and local services.

Example: python3 scripts/benchmark_startup.py target/perf/nova --output /tmp/startup
Default 20 independent launches per condition, alternating off/on/on/off.
Cold data: omit --fixture-data. Warm data: supply a synthetic app-data directory
created by Nova's persistence helpers. Every launch copies it into a fresh root.
No performance conclusion is produced from these raw observations.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import resource
import shutil
import subprocess
import tempfile
import time
import uuid


def command(*args):
    try:
        result = subprocess.run(args, text=True, capture_output=True, check=False)
    except OSError as error:
        return {"command": list(args), "unavailable": str(error)}
    return {"command": list(args), "exit_code": result.returncode,
            "stdout": result.stdout.strip(), "stderr": result.stderr.strip()}


def fixture_digest(root):
    digest = hashlib.sha256()
    if root:
        for path in sorted(root.rglob("*")):
            if path.is_file():
                digest.update(path.relative_to(root).as_posix().encode())
                digest.update(b"\0")
                digest.update(path.read_bytes())
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--fixture-data", type=Path)
    parser.add_argument("--seconds", type=float, default=10)
    parser.add_argument("--runs", type=int, default=20, help="launches per condition")
    parser.add_argument("--context", type=Path, help="JSON hardware/build/renderer context")
    args = parser.parse_args()
    if platform.system() != "Linux":
        parser.error("this harness isolates Linux XDG paths only")
    if args.runs < 1 or not 0 < args.seconds <= 3600:
        parser.error("runs must be positive; seconds must be in (0, 3600]")
    binary = args.binary.resolve(strict=True)
    args.output.mkdir(parents=True, exist_ok=False)
    context = json.loads(args.context.read_text()) if args.context else {}
    context.update({
        "task_id": "BASE-01", "workload": "B-START", "platform": platform.platform(),
        "fixture_sha256": fixture_digest(args.fixture_data),
        "fixture_generator": "external Nova persistence helpers" if args.fixture_data else "empty data directory",
        "harness_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "app_sha": command("git", "rev-parse", "HEAD"),
        "working_tree": command("git", "status", "--short"),
        "lockfile_sha256": hashlib.sha256(Path("Cargo.lock").read_bytes()).hexdigest(),
        "toolchain": [command("rustc", "-Vv"), command("cargo", "--version")],
        "cache_state": "empty app cache; OS page cache uncontrolled",
        "independent_runs_per_condition": args.runs,
        "missing_context": [name for name in (
            "cpu_gpu_ram", "display_refresh_dimensions", "device_power_mode", "kit_sha",
            "profile_features_renderer_allocator", "mpv_ffmpeg_versions") if name not in context],
        "limitations": "Popen timestamp brackets process creation, not entry point; timer observations are not presentation timestamps; external requests must use local fixtures",
    })
    (args.output / "context.json").write_text(json.dumps(context, indent=2) + "\n")
    for index in range(args.runs * 2):
        enabled = index % 4 in (1, 2)
        run_id = uuid.uuid4().hex
        with tempfile.TemporaryDirectory(prefix="nova-b-start-") as temp:
            root = Path(temp)
            data = root / "data" / "nova"
            if args.fixture_data:
                shutil.copytree(args.fixture_data, data)
            else:
                data.mkdir(parents=True)
            env = os.environ.copy()
            env.pop("NOVA_AUTOPLAY_URL", None)
            env.update(XDG_DATA_HOME=str(root / "data"), XDG_CACHE_HOME=str(root / "cache"),
                       NOVA_PERF_RUN_ID=run_id,
                       RUST_LOG="warn,nova_perf=debug" if enabled else "warn,nova_perf=off")
            with (args.output / f"{index:03d}-{run_id}.log").open("w") as log:
                usage_before = resource.getrusage(resource.RUSAGE_CHILDREN)
                elapsed_start = time.monotonic_ns()
                start = time.time_ns()
                process = subprocess.Popen([str(binary)], env=env, stdout=log, stderr=log)
                created = time.time_ns()
                terminated_by_harness = False
                try:
                    returncode = process.wait(timeout=args.seconds)
                except subprocess.TimeoutExpired:
                    terminated_by_harness = True
                    process.terminate()
                    try:
                        returncode = process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        returncode = process.wait()
                usage_after = resource.getrusage(resource.RUSAGE_CHILDREN)
                sample = {"operation": "startup.process_start", "run_id": run_id,
                          "process_elapsed_ns": time.monotonic_ns() - elapsed_start,
                          "child_user_cpu_seconds": usage_after.ru_utime - usage_before.ru_utime,
                          "child_system_cpu_seconds": usage_after.ru_stime - usage_before.ru_stime,
                          "instrumentation": enabled, "popen_before_unix_ns": start,
                          "popen_after_unix_ns": created, "returncode": returncode,
                          "terminated_by_harness": terminated_by_harness}
                with (args.output / "runs.jsonl").open("a") as samples:
                    samples.write(json.dumps(sample) + "\n")


if __name__ == "__main__":
    main()
