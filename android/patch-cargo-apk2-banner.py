#!/usr/bin/env python3
"""Add Android TV banner metadata support to pinned ndk-build2 1.4.1.

Run from the cargo-apk2 Nix derivation's postPatch, after the Cargo vendor tree
has been copied to writable build storage. Preserve all generated manifest
fields and Cargo's package checksum; update only the patched file checksum.
Remove this patch when upstream Application exposes a banner field.
"""
import hashlib
import json
import sys
from pathlib import Path


def patch(vendor):
    manifests = list(Path(vendor).glob("ndk-build2-1.4.1/src/manifest.rs"))
    if len(manifests) != 1:
        raise RuntimeError("expected exactly one pinned ndk-build2 manifest")
    manifest = manifests[0]
    source = manifest.read_text()
    anchor = '    pub icon: Option<String>,\n'
    if source.count(anchor) != 1:
        raise RuntimeError("ndk-build2 Application schema changed; review banner patch")
    source = source.replace(anchor, anchor + '''    #[serde(
        rename(serialize = "@android:banner"),
        skip_serializing_if = "Option::is_none"
    )]
    pub banner: Option<String>,
''')
    manifest.write_text(source)
    checksums_path = manifest.parents[1] / ".cargo-checksum.json"
    checksums = json.loads(checksums_path.read_text())
    checksums["files"]["src/manifest.rs"] = hashlib.sha256(manifest.read_bytes()).hexdigest()
    checksums_path.write_text(json.dumps(checksums))


if __name__ == "__main__":
    patch(sys.argv[1])
