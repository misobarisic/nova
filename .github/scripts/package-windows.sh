#!/usr/bin/env bash
set -euo pipefail

: "${MPV_SOURCE:?run through nix develop .#windows}"
: "${MINGW_CC:?run through nix develop .#windows}"

tag="${GITHUB_REF_NAME//\//-}"
bundle="target/windows-package"
dist="dist"
rm -rf "$bundle"
mkdir -p "$bundle" "$dist"

cp target/x86_64-pc-windows-gnu/release/nova.exe "$bundle/nova.exe"
cp LICENSE "$bundle/LICENSE"
cp vendor/windows-libs/SOURCES "$bundle/THIRD-PARTY-SOURCES.txt"

find "$MPV_SOURCE/64" -maxdepth 1 -type f -iname "*.dll" -exec cp {} "$bundle/" \;
test -f "$bundle/libmpv-2.dll"

# Include only DLL files from the compiler's Nix dependency closure. This
# supplies MinGW's runtime DLLs without committing toolchain binaries.
while IFS= read -r store_path; do
  while IFS= read -r -d '' dll; do
    name="${dll##*/}"
    if [[ ! -e "$bundle/$name" ]]; then
      cp "$dll" "$bundle/$name"
    fi
  done < <(find -L "$store_path" -type f -iname "*.dll" -print0 2>/dev/null || true)
done < <(nix-store -qR "$MINGW_CC")

python3 - "$tag" "$bundle" "$dist" <<'PY'
from pathlib import Path
from sys import argv
from zipfile import ZIP_DEFLATED, ZipFile

tag, bundle_name, dist_name = argv[1:]
bundle = Path(bundle_name)
dist = Path(dist_name)
output = dist / f"nova-{tag}-windows-x86_64.zip"
files = sorted(path for path in bundle.iterdir() if path.is_file())
if not any(path.name.lower() == "libmpv-2.dll" for path in files):
    raise SystemExit("Windows bundle is missing libmpv-2.dll")
with ZipFile(output, "w", compression=ZIP_DEFLATED, compresslevel=9) as archive:
    for path in files:
        archive.write(path, arcname=path.name)
print(f"Created {output} with {len(files)} files")
PY
