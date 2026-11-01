#!/usr/bin/env bash
# Exercise the packaging shell's real SDK tools without building an application.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"
tools="$ANDROID_HOME/build-tools/35.0.0"
test ! -e "$ANDROID_HOME/ndk"
test ! -e "$ANDROID_HOME/emulator"
"$tools/aapt" version
"$tools/aapt2" version
"$tools/apksigner" version

smoke_dir="$(mktemp -d)"
trap 'rm -rf "$smoke_dir"' EXIT
python3 - "$smoke_dir/input.apk" <<'PY'
import importlib.util
from pathlib import Path
import sys
from zipfile import ZipFile, ZIP_STORED

spec = importlib.util.spec_from_file_location(
    "apk_fixtures", Path(".github/scripts/test_package_android.py")
)
fixtures = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixtures)
with ZipFile(sys.argv[1], "w", compression=ZIP_STORED) as apk:
    apk.writestr("AndroidManifest.xml", fixtures.binary_manifest())
    apk.writestr("assets/packaging-smoke.txt", "Nova packaging toolchain check\n")
PY
"$tools/zipalign" -P 16 -f 4 "$smoke_dir/input.apk" "$smoke_dir/aligned.apk"
# This is the tracked public debug key, never a private release credential.
"$tools/apksigner" sign --ks android/keystore/debug.keystore \
    --ks-pass pass:android --key-pass pass:android --min-sdk-version 26 \
    --v4-signing-enabled false --out "$smoke_dir/signed.apk" "$smoke_dir/aligned.apk"
"$tools/zipalign" -c -P 16 4 "$smoke_dir/signed.apk"
"$tools/apksigner" verify --min-sdk-version 26 --verbose "$smoke_dir/signed.apk"
