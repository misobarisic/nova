#!/usr/bin/env python3
"""Build deterministic Nova addon packages from an explicitly selected checkout."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import zipfile


def build(source_root, addon_id, output_dir, validator):
    if not addon_id or any(c not in "abcdefghijklmnopqrstuvwxyz0123456789_-" for c in addon_id):
        raise ValueError("invalid addon ID")
    directory = source_root / "addons" / addon_id
    if directory.is_symlink():
        raise ValueError("symlink addon directories are not package inputs")
    manifest = json.loads((directory / "manifest.json").read_text())
    if manifest["id"] != addon_id:
        raise ValueError("directory and manifest IDs differ")
    scripts = manifest.get("scripts", [manifest["entry"]])
    if not scripts:
        raise ValueError("explicit scripts list must not be empty")
    if scripts[-1] != manifest["entry"] or len(set(scripts)) != len(scripts):
        raise ValueError("scripts must be unique with entry last")
    names = ["manifest.json", "stremio-manifest.json", "LICENSE", "NOTICE.md"] + scripts + manifest.get("assets", [])
    if len(set(names)) != len(names):
        raise ValueError("duplicate declarations")
    output_dir.mkdir(parents=True, exist_ok=True)
    version = manifest["version"]
    if any(c not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.-+" for c in version):
        raise ValueError("invalid version")
    target = output_dir / f"{addon_id}-{version}.zip"
    # Validate before publishing either the archive or checksum.
    with tempfile.TemporaryDirectory() as temporary:
        archive = Path(temporary) / "package.zip"
        with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_STORED) as package:
            for name in sorted(names):
                parts = name.split("/")
                if not name or any(part in ("", ".", "..") for part in parts) or any(c in name for c in "\\:"):
                    raise ValueError("unsafe declared path")
                if any(part.startswith(".") or part in {"fixtures", "node_modules", "target", "dist", "build"} for part in parts) or Path(name).suffix.lower() in {".pem", ".key", ".keystore", ".p12", ".pfx"}:
                    raise ValueError("private or development files are not package inputs")
                path = directory / name
                if any((directory.joinpath(*parts[:i])).is_symlink() for i in range(1, len(parts)+1)):
                    raise ValueError("symlinks are not package inputs")
                info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
                info.create_system = 3
                info.external_attr = 0o100644 << 16
                package.writestr(info, path.read_bytes())
        subprocess.run([*validator, str(archive)], check=True)
        data = archive.read_bytes()
        target.write_bytes(data)
        target.with_suffix(".zip.sha256").write_text(f"{hashlib.sha256(data).hexdigest()}  {target.name}\n")
    print(target)
    return target


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("id", nargs="?")
    parser.add_argument("--all", action="store_true")
    parser.add_argument("--source-root", required=True, type=Path)
    parser.add_argument("--output-dir", type=Path, default=Path("dist"))
    parser.add_argument("--runtime-root", type=Path, help="Nova checkout containing the Rust validator")
    parser.add_argument("--validator", type=Path, help="prebuilt validate-package executable")
    args = parser.parse_args()
    if bool(args.id) == args.all:
        parser.error("choose one addon ID or --all")
    root = args.runtime_root.resolve() if args.runtime_root else Path(__file__).resolve().parents[1]
    if not args.validator and not (root / "crates/providers/Cargo.toml").is_file():
        parser.error("provide --validator or --runtime-root to verify packages with Nova's Rust parser")
    validator = [str(args.validator.resolve())] if args.validator else ["cargo", "run", "--quiet", "--locked", "--manifest-path", str(root / "crates/providers/Cargo.toml"), "--example", "validate-package", "--"]
    ids = sorted(p.name for p in (args.source_root / "addons").iterdir() if p.is_dir()) if args.all else [args.id]
    for addon_id in ids:
        build(args.source_root.resolve(), addon_id, args.output_dir, validator)


if __name__ == "__main__":
    main()
