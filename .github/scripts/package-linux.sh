#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

binary="target/release/nova"
if [[ ! -x "$binary" ]]; then
  echo "Missing release binary: $binary (run cargo build --release --bin nova first)" >&2
  exit 1
fi

cargo_version="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json, sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "nova"))')"
event_name="${GITHUB_EVENT_NAME:-local}"
if [[ "$event_name" == "pull_request" ]]; then
  artifact_tag="pr-${GITHUB_EVENT_NUMBER:-0}"
  debian_version="${cargo_version}~pr${GITHUB_EVENT_NUMBER:-0}"
else
  raw_tag="${GITHUB_REF_NAME:-local}"
  artifact_tag="$(printf '%s' "$raw_tag" | sed -E 's/[^A-Za-z0-9._+-]+/-/g')"
  upstream_version="${raw_tag#v}"
  upstream_version="$(printf '%s' "$upstream_version" | sed -E 's/-/~/g; s/[^A-Za-z0-9.+~]/./g')"
  if [[ "$upstream_version" =~ ^[0-9] ]]; then
    debian_version="$upstream_version"
  else
    debian_version="${cargo_version}+${upstream_version}"
  fi
fi
if [[ -z "$artifact_tag" ]]; then
  artifact_tag="local"
fi
dpkg --validate-version "$debian_version"

work="target/linux-package"
tools_dir="$work/tools"
appdir="$work/nova.AppDir"
deb_work="$work/deb-work"
deb_root="$work/deb-root"
dist="$repo_root/dist"
rm -rf "$work"
mkdir -p "$tools_dir" "$appdir" "$deb_work/debian" "$deb_root/DEBIAN" "$dist"

download_verified() {
  local url="$1"
  local output="$2"
  local expected_sha256="$3"

  curl --fail --location --retry 3 --silent --show-error "$url" --output "$output"
  printf '%s  %s\n' "$expected_sha256" "$output" | sha256sum --check
  chmod 755 "$output"
}

# AppImage's continuous release assets are mutable, so pin their published
# SHA-256 digests and fail closed if the upstream binaries change.
download_verified \
  "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage" \
  "$tools_dir/linuxdeploy-x86_64.AppImage" \
  "36a2d7e274d12e1050d0e9ecfe11d339ed54720b2bec464c286d53f8b07f5c62"
download_verified \
  "https://github.com/linuxdeploy/linuxdeploy-plugin-appimage/releases/download/continuous/linuxdeploy-plugin-appimage-x86_64.AppImage" \
  "$tools_dir/linuxdeploy-plugin-appimage-x86_64.AppImage" \
  "0441769ab38009504d2678c38cd7e526955388dd30a215b4a20afaa5471652f2"
download_verified \
  "https://github.com/linuxdeploy/linuxdeploy-plugin-qt/releases/download/continuous/linuxdeploy-plugin-qt-x86_64.AppImage" \
  "$tools_dir/linuxdeploy-plugin-qt-x86_64.AppImage" \
  "cfc1055b2b9dbc08412b579f20990b7b41a17b61beaa5847dc9477c96c9e9617"

appimage="$dist/nova-x86_64-${artifact_tag}.AppImage"
deb_file="$dist/nova-x86_64-${artifact_tag}.deb"
app_icon="$appdir/usr/share/icons/hicolor/512x512/apps/nova.png"

install -D -m 755 "$binary" "$appdir/usr/bin/nova"
install -D -m 644 assets/logo.png "$app_icon"
install -D -m 644 LICENSE "$appdir/usr/share/doc/nova/LICENSE"
install -D -m 644 THIRD_PARTY_NOTICES.md "$appdir/usr/share/doc/nova/THIRD_PARTY_NOTICES.md"
install -D -m 644 .github/scripts/nova.desktop "$deb_root/usr/share/applications/nova.desktop"
install -D -m 644 assets/logo.png "$deb_root/usr/share/icons/hicolor/512x512/apps/nova.png"
install -D -m 644 LICENSE "$deb_root/usr/share/doc/nova/LICENSE"
install -D -m 644 THIRD_PARTY_NOTICES.md "$deb_root/usr/share/doc/nova/THIRD_PARTY_NOTICES.md"
mkdir -p "$appdir/usr/share/applications"
sed 's/^Exec=nova$/Exec=AppRun/' .github/scripts/nova.desktop \
  > "$appdir/usr/share/applications/nova.desktop"
desktop-file-validate .github/scripts/nova.desktop
desktop-file-validate "$appdir/usr/share/applications/nova.desktop"

uses_qt=false
if ldd "$binary" | grep -q 'libQt6'; then
  uses_qt=true
fi

extra_dependencies=""
if [[ "$uses_qt" == true ]]; then
  extra_dependencies=", qt6-qpa-plugins, qt6-wayland"
fi

cat > "$deb_work/debian/control" <<EOF
Source: nova
Section: video
Priority: optional
Maintainer: Nova project <50531162+misobarisic@users.noreply.github.com>
Standards-Version: 4.6.0

Package: nova
Architecture: amd64
Depends: \${shlibs:Depends}, \${misc:Depends}$extra_dependencies
Description: Cross-platform media catalog and player
 Nova is a media catalog and player for desktop and Android.
EOF

shlib_vars="$(cd "$deb_work" && dpkg-shlibdeps -O -e"$repo_root/$binary")"
shlib_dependencies="$(sed -n 's/^shlibs:Depends=//p' <<<"$shlib_vars")"
if [[ -z "$shlib_dependencies" ]]; then
  echo "dpkg-shlibdeps did not report runtime dependencies for $binary" >&2
  exit 1
fi
dependencies="$shlib_dependencies"
if [[ "$uses_qt" == true ]]; then
  dependencies="${dependencies}, qt6-qpa-plugins, qt6-wayland"
fi

cat > "$deb_root/DEBIAN/control" <<EOF
Package: nova
Version: $debian_version
Section: video
Priority: optional
Architecture: amd64
Maintainer: Nova project <50531162+misobarisic@users.noreply.github.com>
Depends: $dependencies
Description: Cross-platform media catalog and player
 Nova is a media catalog and player for desktop and Android.
EOF
chmod 644 "$deb_root/DEBIAN/control"
dpkg-deb --root-owner-group --build "$deb_root" "$deb_file"

# AppImage tools are AppImages themselves. This lets the build work on hosted
# runners without relying on FUSE being available.
export APPIMAGE_EXTRACT_AND_RUN=1
export ARCH=x86_64

linuxdeploy_args=(
  --appdir "$appdir"
  --executable "$appdir/usr/bin/nova"
  --desktop-file "$appdir/usr/share/applications/nova.desktop"
  --icon-file "$app_icon"
)

if [[ "$uses_qt" == true ]]; then
  qmake="$(command -v qmake6 || command -v qmake || true)"
  if [[ -z "$qmake" ]]; then
    echo "Qt is linked but qmake was not found; install qt6-base-dev" >&2
    exit 1
  fi
  QMAKE="$qmake" \
  EXTRA_PLATFORM_PLUGINS="libqwayland-egl.so;libqwayland-generic.so" \
  LINUXDEPLOY_OUTPUT_VERSION="$debian_version" \
  LDAI_OUTPUT="$appimage" \
    "$tools_dir/linuxdeploy-x86_64.AppImage" \
      "${linuxdeploy_args[@]}" --plugin qt --output appimage
else
  LINUXDEPLOY_OUTPUT_VERSION="$debian_version" \
  LDAI_OUTPUT="$appimage" \
    "$tools_dir/linuxdeploy-x86_64.AppImage" \
      "${linuxdeploy_args[@]}" --output appimage
fi

test -s "$appimage"
chmod 755 "$appimage"
echo "Created $deb_file"
echo "Created $appimage"
