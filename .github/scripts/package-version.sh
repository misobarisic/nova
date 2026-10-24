# Shared release identity; source this without running build or packaging tools.
nova_package_tag() {
  local version="$1"
  if [[ -n "${NOVA_RELEASE_TAG:-}" && "$NOVA_RELEASE_TAG" != "v$version" ]]; then
    echo "NOVA_RELEASE_TAG must match Cargo.toml (v$version)" >&2
    return 1
  fi
  printf '%s\n' "${NOVA_RELEASE_TAG:-${GITHUB_REF_NAME:-local}}"
}

nova_linux_versions() {
  local cargo_version="$1" raw_tag upstream_version
  if [[ -z "${NOVA_RELEASE_TAG:-}" && "${GITHUB_EVENT_NAME:-local}" == "pull_request" ]]; then
    artifact_tag="pr-${GITHUB_EVENT_NUMBER:-0}"
    debian_version="${cargo_version}~pr${GITHUB_EVENT_NUMBER:-0}"
  else
    raw_tag="$(nova_package_tag "$cargo_version")" || return
    artifact_tag="$(printf '%s' "${raw_tag#v}" | sed -E 's/[^A-Za-z0-9._+-]+/-/g')"
    upstream_version="$(printf '%s' "${raw_tag#v}" | sed -E 's/-/~/g; s/[^A-Za-z0-9.+~]/./g')"
    if [[ "$upstream_version" =~ ^[0-9] ]]; then
      debian_version="$upstream_version"
    else
      debian_version="${cargo_version}+${upstream_version}"
    fi
  fi
  artifact_tag="${artifact_tag:-local}"
}
