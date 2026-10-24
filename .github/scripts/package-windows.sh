#!/usr/bin/env bash
set -euo pipefail

: "${MPV_SOURCE:?run through nix develop .#windows}"
: "${MINGW_CC:?run through nix develop .#windows}"

source "$(dirname "${BASH_SOURCE[0]}")/package-version.sh"
# Only explicit release identities need manifest validation; the legacy local
# filename fallback remains usable without invoking Cargo or entering Nix.
cargo_version=""
if [[ -n "${NOVA_RELEASE_TAG:-}" ]]; then
  cargo_version="$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["package"]["version"])')"
fi
tag="$(nova_package_tag "$cargo_version")"
tag="${tag#v}"
tag="${tag//\//-}"
bundle="target/windows-package"
dist="dist"
# Older runs may have copied read-only directory modes from Nix inputs.
if [[ -d "$bundle" ]]; then
  find "$bundle" -type d -exec chmod u+w {} +
fi
rm -rf "$bundle"
mkdir -p "$bundle" "$dist"

profile="${NOVA_RELEASE_PROFILE:-release}"
cp "target/x86_64-pc-windows-gnu/$profile/nova.exe" "$bundle/nova.exe"
cp LICENSE "$bundle/LICENSE"
cp "$MPV_SOURCE/SOURCES" "$bundle/THIRD-PARTY-SOURCES.txt"
cp -RL "$MPV_SOURCE/licenses" "$bundle/licenses"
cp -RL "$MPV_SOURCE/metadata" "$bundle/native-media"
# Keep generated staging directories removable on the next packaging run.
find "$bundle" -type d -exec chmod u+w {} +

find -L "$MPV_SOURCE/64" -maxdepth 1 -type f -iname "*.dll" -exec cp {} "$bundle/" \;
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
output = dist / f"Nova-Windows-x86_64-{tag}.zip"
files = sorted(path for path in bundle.rglob("*") if path.is_file())
if not any(path.name.lower() == "libmpv-2.dll" for path in files):
    raise SystemExit("Windows bundle is missing libmpv-2.dll")
with ZipFile(output, "w", compression=ZIP_DEFLATED, compresslevel=9) as archive:
    for path in files:
        archive.write(path, arcname=path.relative_to(bundle))
print(f"Created {output} with {len(files)} files")
PY
