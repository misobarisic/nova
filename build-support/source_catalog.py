#!/usr/bin/env python3
"""Resolve workspace source links by Cargo manifest identity, never name prefixes.

Only source navigation rows are processed. License texts and native notices stay
with cargo-about and the existing build script. Requires Python 3 and Cargo.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

FEATURES = ('desktop', 'android', 'live-preview', 'update-check')
REPOSITORY = 'https://github.com/misobarisic/nova/tree/main/'


def metadata_command(target, environment):
    command = ['cargo', 'metadata', '--locked', '--offline', '--no-deps',
               '--format-version', '1', '--filter-platform', target,
               '--no-default-features']
    enabled = [feature for feature in FEATURES
               if 'CARGO_FEATURE_' + feature.upper().replace('-', '_') in environment]
    if enabled:
        command.extend(['--features', ','.join(enabled)])
    return command


def workspace_links(metadata):
    root = Path(metadata['workspace_root']).resolve()
    members = set(metadata['workspace_members'])
    packages = [package for package in metadata['packages'] if package['id'] in members]
    if len(packages) != len(members):
        raise ValueError('Cargo metadata omits a workspace package identity')
    links = {}
    for package in packages:
        manifest = Path(package['manifest_path']).resolve()
        relative = manifest.parent.relative_to(root)
        if package.get('source') is not None:
            raise ValueError('workspace package unexpectedly has an external source')
        path = relative.as_posix() if relative.parts else 'src'
        links[str(manifest)] = REPOSITORY + path
    return links


def source_catalog(rendered, static, metadata):
    links = workspace_links(metadata)
    manual = [line.split('\t') for line in static.splitlines() if line.strip()]
    if any(len(row) != 3 for row in manual):
        raise ValueError('project source rows require exactly three columns')
    # A manual row may replace a generated workspace row only when its exact
    # source URL and license agree. External rows are never aliases by name.
    aliases = {(row[2], row[1]) for row in manual}
    rows = []
    for line in rendered.splitlines():
        if not line.strip():
            continue
        fields = line.split('\t')
        if len(fields) != 4:
            raise ValueError('cargo-about source rows require manifest identity as fourth column')
        label, license_id, url, manifest = fields
        if not manifest or not Path(manifest).is_absolute():
            raise ValueError('cargo-about manifest identity must be an absolute path')
        identity = str(Path(manifest).resolve())
        if identity in links:
            url = links[identity]
            if (url, license_id) in aliases:
                continue
        rows.append((label, license_id, url))
    rows.extend(tuple(row) for row in manual)
    # Exact duplicate rows are redundant; differing license terms stay visible.
    return ''.join('\t'.join(row) + '\n' for row in dict.fromkeys(rows))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest-dir', required=True)
    parser.add_argument('--target', required=True)
    args = parser.parse_args()
    root = Path(args.manifest_dir)
    result = subprocess.run(metadata_command(args.target, os.environ), cwd=root,
                            check=True, capture_output=True, text=True)
    metadata = json.loads(result.stdout)
    static = (root / 'assets/open_source_project_sources.tsv').read_text()
    sys.stdout.write(source_catalog(sys.stdin.read(), static, metadata))


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        detail = getattr(error, 'stderr', '')
        sys.exit(f'workspace source catalog generation failed: {error}\n{detail}\n'
                 'Use the supported Nix shell with Python 3 and Cargo available.')
