#!/usr/bin/env python3
"""Package universal and per-ABI APKs from one cargo-apk2 build."""
import copy
import os
import re
import subprocess
import tempfile
import tomllib
from pathlib import Path
from zipfile import ZipFile

ABIS = {"arm64-v8a", "x86_64"}
VARIANTS = {"": ABIS, "-arm64-v8a": {"arm64-v8a"}, "-x86_64": {"x86_64"}}


def signature_entry(name):
    upper = name.upper()
    if not upper.startswith("META-INF/"):
        return False
    leaf = upper[len("META-INF/"):]
    return "/" not in leaf and (
        leaf == "MANIFEST.MF"
        or leaf.startswith("SIG-")
        or leaf.endswith((".SF", ".RSA", ".DSA", ".EC"))
    )


def keep_entry(name, abis):
    parts = name.split("/")
    return not signature_entry(name) and not (
        len(parts) >= 2 and parts[0] == "lib" and parts[1] and parts[1] not in abis
    )


def check_abis(entries, expected):
    actual = {
        name.split("/")[1]
        for name in entries
        if name.startswith("lib/") and name.endswith(".so")
    }
    if actual != expected:
        raise RuntimeError(f"APK ABIs {sorted(actual)} != expected {sorted(expected)}")
    required = {
        f"lib/{abi}/{library}"
        for abi in expected
        for library in ("libnova.so", "libmpv.so")
    }
    if missing := required - set(entries):
        raise RuntimeError(f"APK missing native libraries: {sorted(missing)}")


def filter_apk(source, output, abis):
    # Rebuilding the ZIP also removes the APK v2/v3 signing block. Old JAR
    # signatures are removed explicitly; license files under META-INF stay.
    with ZipFile(source) as original, ZipFile(output, "w") as result:
        result.comment = original.comment
        entries = original.infolist()
        if len({entry.filename for entry in entries}) != len(entries):
            raise RuntimeError("Source APK has duplicate ZIP entries")
        check_abis([entry.filename for entry in entries], ABIS)
        for entry in entries:
            if keep_entry(entry.filename, abis):
                result.writestr(copy.copy(entry), original.read(entry))


def verify_payload(source, result, abis):
    with ZipFile(source) as original, ZipFile(result) as packaged:
        expected = {
            entry.filename: entry
            for entry in original.infolist()
            if keep_entry(entry.filename, abis)
        }
        actual = {
            entry.filename: entry
            for entry in packaged.infolist()
            if not signature_entry(entry.filename)
        }
        if len(packaged.namelist()) != len(set(packaged.namelist())):
            raise RuntimeError("Output APK has duplicate ZIP entries")
        if actual.keys() != expected.keys():
            raise RuntimeError("Packaging changed the APK payload entry set")
        check_abis(actual, abis)
        for name, entry in expected.items():
            if original.read(entry) != packaged.read(actual[name]):
                raise RuntimeError(f"Packaging changed payload bytes: {name}")
            if entry.compress_type != actual[name].compress_type:
                raise RuntimeError(f"Packaging changed compression: {name}")


def signing_key(manifest):
    # Match cargo-apk2's release-profile precedence: environment, then TOML.
    if "CARGO_APK_RELEASE_KEYSTORE" in os.environ:
        password = os.environ.get("CARGO_APK_RELEASE_KEYSTORE_PASSWORD")
        if password is None:
            raise RuntimeError("Release keystore environment override needs its password")
        return Path(os.environ["CARGO_APK_RELEASE_KEYSTORE"]), password
    with manifest.open("rb") as handle:
        signing = tomllib.load(handle)["package"]["metadata"]["android"]["signing"]["release"]
    return manifest.parent / signing["path"], signing["keystore_password"]


def run(*args, env=None):
    try:
        return subprocess.run(
            [str(arg) for arg in args], check=True, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env,
        ).stdout
    except subprocess.CalledProcessError as error:
        raise RuntimeError(
            f"{Path(str(args[0])).name} failed: {error.stderr.strip()}"
        ) from error


def signer_certificates(apksigner, apk):
    report = run(apksigner, "verify", "--verbose", "--print-certs", apk)
    fingerprints = re.findall(
        r"Signer #\d+ certificate SHA-256 digest: ([0-9a-fA-F]+)", report
    )
    if not fingerprints:
        raise RuntimeError(f"No verified signer certificate found in {apk}")
    return sorted(value.lower() for value in fingerprints)


def main():
    sources = list(Path("target").rglob("nova.apk"))
    if len(sources) != 1:
        raise RuntimeError(f"Expected one built nova.apk, found {len(sources)}")
    source = sources[0]
    tools = Path(os.environ["ANDROID_HOME"]) / "build-tools" / "35.0.0"
    zipalign, apksigner = tools / "zipalign", tools / "apksigner"
    keystore, password = signing_key(Path("Cargo.toml"))
    if not keystore.is_file():
        raise RuntimeError("Configured Android release keystore does not exist")
    env = dict(os.environ, NOVA_APK_KEYSTORE_PASSWORD=password)
    original_signers = signer_certificates(apksigner, source)
    tag = os.environ["GITHUB_REF_NAME"].replace("/", "-")
    dist = Path("dist")
    dist.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="nova-apk-") as temporary:
        work = Path(temporary)
        for suffix, abis in VARIANTS.items():
            unsigned, aligned = work / "unsigned.apk", work / "aligned.apk"
            output = dist / f"nova{suffix}-{tag}.apk"
            filter_apk(source, unsigned, abis)
            # Align before signing; editing a signed APK invalidates its signature.
            run(zipalign, "-P", "16", "-f", "4", unsigned, aligned)
            run(
                apksigner, "sign", "--ks", keystore,
                "--ks-pass", "env:NOVA_APK_KEYSTORE_PASSWORD",
                "--out", output, aligned, env=env,
            )
            run(zipalign, "-c", "-P", "16", "4", output)
            if signer_certificates(apksigner, output) != original_signers:
                raise RuntimeError(f"Signing certificate changed for {output}")
            verify_payload(source, output, abis)
            print(f"Verified {output}: {', '.join(sorted(abis))}")
    print("Packaged all three APKs from one universal build")


if __name__ == "__main__":
    main()
