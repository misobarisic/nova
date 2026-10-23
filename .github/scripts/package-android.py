#!/usr/bin/env python3
"""Package release APKs from parallel per-ABI or one universal cargo-apk2 build."""
import argparse
import copy
import os
import re
import struct
import subprocess
import tempfile
import tomllib
from contextlib import ExitStack
from pathlib import Path
from zipfile import ZIP_DEFLATED, ZIP_STORED, ZipFile

ABIS = {"arm64-v8a", "armeabi-v7a", "x86_64"}
NATIVE_LIBRARIES = (
    "libnova.so", "libmpv.so", "libavcodec.so", "libavfilter.so",
    "libavformat.so", "libavutil.so", "libswresample.so", "libswscale.so",
)
ABI_ELF = {"arm64-v8a": (2, 183), "armeabi-v7a": (1, 40), "x86_64": (2, 62)}
VARIANTS = {"-universal": ABIS, **{f"-{abi}": {abi} for abi in sorted(ABIS)}}
ANDROID_NAMESPACE = "http://schemas.android.com/apk/res/android"
EXTRACT_NATIVE_LIBS_ID = 0x010104EA
NO_INDEX = 0xFFFFFFFF


def set_extract_native_libs(manifest, enabled):
    """Change only the compiled application boolean, without rebuilding resources.

    The release source deliberately supports direct loading. Its universal
    derivative instead extracts compressed libraries. Matching the binary XML
    element, namespace and resource ID avoids patching an unrelated integer or
    string, and preserves every other manifest attribute byte-for-byte.
    """
    def fail():
        raise RuntimeError("Invalid or unsupported compiled AndroidManifest.xml")

    def chunks():
        if len(manifest) < 8:
            fail()
        kind, header_size, size = struct.unpack_from("<HHI", manifest)
        if kind != 0x0003 or header_size != 8 or size != len(manifest):
            fail()
        offset = header_size
        while offset < size:
            if offset + 8 > size:
                fail()
            kind, header, length = struct.unpack_from("<HHI", manifest, offset)
            if header < 8 or length < header or offset + length > size:
                fail()
            yield kind, offset, header, length
            offset += length

    def strings(offset, header, length):
        if header < 28:
            fail()
        count, styles, flags, start, style_start = struct.unpack_from("<IIIII", manifest, offset + 8)
        end = style_start or length
        if header + (count + styles) * 4 > start or not start <= end <= length:
            fail()
        pool = manifest[offset + start:offset + end]

        def read_length(position, utf8):
            unit, marker = (1, 0x80) if utf8 else (2, 0x8000)
            if position + unit > len(pool):
                fail()
            value = int.from_bytes(pool[position:position + unit], "little")
            position += unit
            if value & marker:
                if position + unit > len(pool):
                    fail()
                value = ((value & (marker - 1)) << (8 if utf8 else 16)) | int.from_bytes(pool[position:position + unit], "little")
                position += unit
            return value, position

        result = []
        for index in range(count):
            position = struct.unpack_from("<I", manifest, offset + header + index * 4)[0]
            utf8 = bool(flags & 0x100)
            size, position = read_length(position, utf8)
            if utf8:
                size, position = read_length(position, True)
            else:
                size *= 2
            terminator = 1 if utf8 else 2
            if position + size + terminator > len(pool) or any(pool[position + size:position + size + terminator]):
                fail()
            try:
                result.append(pool[position:position + size].decode("utf-8" if utf8 else "utf-16-le"))
            except UnicodeError:
                fail()
        return result

    pool, resource_ids, matches = None, None, []
    for kind, offset, header, length in chunks():
        if kind == 0x0001:
            if pool is not None:
                fail()
            pool = strings(offset, header, length)
        elif kind == 0x0180:
            if resource_ids is not None or header != 8 or (length - header) % 4:
                fail()
            resource_ids = struct.unpack_from(f"<{(length - header) // 4}I", manifest, offset + header)
        elif kind == 0x0102:
            if pool is None or resource_ids is None or header < 16 or length < header + 20:
                fail()
            extension = offset + header
            namespace, name, attr_start, attr_size, count = struct.unpack_from("<IIHHH", manifest, extension)
            if name >= len(pool) or attr_start < 20 or attr_size < 20 or header + attr_start + attr_size * count > length:
                fail()
            if namespace != NO_INDEX or pool[name] != "application":
                continue
            for index in range(count):
                attribute = extension + attr_start + index * attr_size
                namespace, name, raw, value_size, reserved, value_type, value = struct.unpack_from("<IIIHBBI", manifest, attribute)
                if name >= len(pool) or namespace != NO_INDEX and namespace >= len(pool):
                    fail()
                if name < len(resource_ids) and resource_ids[name] == EXTRACT_NATIVE_LIBS_ID:
                    if namespace == NO_INDEX or pool[namespace] != ANDROID_NAMESPACE or pool[name] != "extractNativeLibs" or value_size != 8 or reserved != 0 or value_type != 0x12:
                        fail()
                    matches.append(attribute)
    if len(matches) != 1:
        raise RuntimeError("APK must have exactly one compiled application android:extractNativeLibs boolean")
    result = bytearray(manifest)
    # Remove a textual raw value if present; the canonical typed boolean is
    # authoritative, including for aapt's post-signing inspection.
    struct.pack_into("<I", result, matches[0] + 8, NO_INDEX)
    struct.pack_into("<I", result, matches[0] + 16, NO_INDEX if enabled else 0)
    return bytes(result)


def native_entry(name):
    return name.startswith("lib/") and name.endswith(".so")


def verify_version(badging, expected_version):
    package = re.search(
        r"^package: name='dev\.misob\.nova' versionCode='(\d+)' versionName='([^']+)'",
        badging, re.MULTILINE,
    )
    if not package or package[2] != expected_version:
        raise RuntimeError("APK package or version name does not match Cargo.toml")
    # Epoch 1 included local 0.1.0 builds, higher than the subsequent 0.0.x
    # releases. Reject the old mapping before publishing uninstallable updates.
    if not (2 << 24) <= int(package[1]) <= 2100000000:
        raise RuntimeError("APK version code would regress the Android installation epoch")


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
        for library in NATIVE_LIBRARIES
    }
    if missing := required - set(entries):
        raise RuntimeError(f"APK missing native libraries: {sorted(missing)}")


def validate_native_payload(apk, abis):
    entries = apk.namelist()
    if len(set(entries)) != len(entries):
        raise RuntimeError("Source APK has duplicate ZIP entries")
    if "AndroidManifest.xml" not in entries:
        raise RuntimeError("Source APK has no compiled AndroidManifest.xml")
    check_abis(entries, abis)
    # Check actual packaged bytes: swapped ABI directories pass filename checks
    # but fail when Android loads the library.
    for abi in sorted(abis):
        elf_class, machine = ABI_ELF[abi]
        for library in NATIVE_LIBRARIES:
            name = f"lib/{abi}/{library}"
            with apk.open(name) as native:
                header = native.read(20)
            if (
                len(header) != 20
                or header[:6] != b"\x7fELF" + bytes((elf_class, 1))
                or int.from_bytes(header[18:20], "little") != machine
            ):
                raise RuntimeError(f"APK native library has wrong architecture: {name}")


def abi_apks(arguments):
    sources = {}
    for argument in arguments:
        abi, separator, path = argument.partition("=")
        if abi not in ABIS or not separator or not path:
            raise ValueError(f"Expected ABI=PATH for one of {', '.join(sorted(ABIS))}")
        if abi in sources:
            raise ValueError(f"Duplicate APK input for {abi}")
        sources[abi] = Path(path)
    if sources and sources.keys() != ABIS:
        raise ValueError(f"APK inputs must cover exactly {', '.join(sorted(ABIS))}")
    return sources


def merge_apks(sources, output):
    """Combine native payloads only when all builds have identical shared code.

    ZIP timestamps and signatures may differ between runners. DEX, resources,
    manifest, licenses and their compression must match exactly. Never select
    one runner's conflicting application payload silently.
    """
    if sources.keys() != ABIS:
        raise RuntimeError(f"APK inputs must cover exactly {', '.join(sorted(ABIS))}")
    with ExitStack() as stack:
        apks = {abi: stack.enter_context(ZipFile(sources[abi])) for abi in sorted(ABIS)}
        for abi, apk in apks.items():
            validate_native_payload(apk, {abi})

        def shared(apk):
            return {entry.filename: entry for entry in apk.infolist()
                    if not entry.filename.startswith("lib/") and not signature_entry(entry.filename)}

        canonical = next(iter(apks.values()))
        common = shared(canonical)
        for abi, apk in apks.items():
            entries = shared(apk)
            if entries.keys() != common.keys():
                raise RuntimeError(f"Shared APK entry set differs for {abi}")
            for name, entry in entries.items():
                if (entry.compress_type != common[name].compress_type
                        or apk.read(entry) != canonical.read(common[name])):
                    raise RuntimeError(f"Shared APK payload differs for {abi}: {name}")
        with ZipFile(output, "w") as result:
            result.comment = canonical.comment
            for entry in common.values():
                result.writestr(copy.copy(entry), canonical.read(entry))
            for abi, apk in apks.items():
                for entry in apk.infolist():
                    if entry.filename.startswith(f"lib/{abi}/"):
                        result.writestr(copy.copy(entry), apk.read(entry))


def filter_apk(source, output, abis, *, extract_native_libs=False):
    # Rebuilding the ZIP also removes the APK v2/v3 signing block. Old JAR
    # signatures are removed explicitly; license files under META-INF stay.
    with ZipFile(source) as original, ZipFile(output, "w") as result:
        result.comment = original.comment
        validate_native_payload(original, ABIS)
        for entry in original.infolist():
            if keep_entry(entry.filename, abis):
                data = original.read(entry)
                copied = copy.copy(entry)
                if extract_native_libs and entry.filename == "AndroidManifest.xml":
                    data = set_extract_native_libs(data, True)
                if extract_native_libs and native_entry(entry.filename):
                    copied.compress_type = ZIP_DEFLATED
                result.writestr(copied, data, compresslevel=9 if extract_native_libs and native_entry(entry.filename) else None)


def verify_payload(source, result, abis, *, extract_native_libs=False):
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
            expected_data = original.read(entry)
            if extract_native_libs and name == "AndroidManifest.xml":
                expected_data = set_extract_native_libs(expected_data, True)
            if expected_data != packaged.read(actual[name]):
                raise RuntimeError(f"Packaging changed payload bytes: {name}")
            compression = ZIP_DEFLATED if extract_native_libs and native_entry(name) else entry.compress_type
            if compression != actual[name].compress_type:
                raise RuntimeError(f"Packaging changed compression: {name}")



def verify_native_layout(path, *, extract_native_libs=False):
    # zipalign alone ignores compressed entries. Verify storage AND actual local
    # data offsets, including extra fields (central-directory offsets are not
    # the mmap address). 16 KiB boundaries also satisfy 4 KiB devices.
    with ZipFile(path) as apk, Path(path).open("rb") as raw:
        libraries = [entry for entry in apk.infolist() if native_entry(entry.filename)]
        if not libraries:
            raise RuntimeError("APK has no native libraries")
        for entry in libraries:
            if extract_native_libs:
                if entry.compress_type != ZIP_DEFLATED:
                    raise RuntimeError(f"Universal APK native library is not compressed: {entry.filename}")
                continue
            if entry.compress_type != ZIP_STORED:
                raise RuntimeError(f"APK native library is compressed: {entry.filename}")
            raw.seek(entry.header_offset)
            header = raw.read(30)
            if len(header) != 30 or header[:4] != b"PK\x03\x04":
                raise RuntimeError(f"Invalid native ZIP header: {entry.filename}")
            offset = (entry.header_offset + 30
                      + int.from_bytes(header[26:28], "little")
                      + int.from_bytes(header[28:30], "little"))
            if offset % 16384:
                raise RuntimeError(f"APK native library is not 16 KiB aligned: {entry.filename}")


def verify_native_manifest(tree, *, extract_native_libs=False):
    # aapt dump xmltree exposes the binary manifest boolean, not Cargo metadata.
    boolean = r"(?:true|0xffffffff|0x1)" if extract_native_libs else r"(?:false|0x0)"
    if not re.search(
        r"^\s*A: android:extractNativeLibs(?:\(0x[0-9a-fA-F]+\))?="
        rf"(?:\(type 0x12\))?{boolean}\s*$", tree, re.MULTILINE,
    ):
        raise RuntimeError(f"APK must declare android:extractNativeLibs={str(extract_native_libs).lower()}")


def signing_key(manifest, profile="release"):
    # Match cargo-apk2's selected-profile precedence: environment, then TOML.
    keystore_env = f"CARGO_APK_{profile.upper().replace('-', '_')}_KEYSTORE"
    if keystore_env in os.environ:
        password = os.environ.get(f"{keystore_env}_PASSWORD")
        if password is None:
            raise RuntimeError("Release keystore environment override needs its password")
        return Path(os.environ[keystore_env]), password
    with manifest.open("rb") as handle:
        signing = tomllib.load(handle)["package"]["metadata"]["android"]["signing"][profile]
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
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--abi-apk", action="append", default=[], metavar="ABI=PATH",
                        help="Repeat for all three ABIs to assemble parallel builds")
    try:
        sources = abi_apks(parser.parse_args().abi_apk)
    except ValueError as error:
        parser.error(str(error))
    profile = os.environ.get("NOVA_RELEASE_PROFILE", "release")
    source = Path(os.environ.get("CARGO_TARGET_DIR", "target")) / profile / "apk" / "nova.apk"
    inputs = sources.values() if sources else [source]
    tools = Path(os.environ["ANDROID_HOME"]) / "build-tools" / "35.0.0"
    zipalign, apksigner = tools / "zipalign", tools / "apksigner"
    with Path("Cargo.toml").open("rb") as handle:
        version = tomllib.load(handle)["package"]["version"]
    original_signers = None
    for apk in inputs:
        if not apk.is_file():
            raise RuntimeError(f"Missing built APK for profile {profile}: {apk}")
        verify_version(run(tools / "aapt2", "dump", "badging", apk), version)
        verify_native_manifest(run(tools / "aapt", "dump", "xmltree", apk, "AndroidManifest.xml"))
        verify_native_layout(apk)
        signers = signer_certificates(apksigner, apk)
        if original_signers is not None and signers != original_signers:
            raise RuntimeError(f"Signing certificates differ between APK inputs: {apk}")
        original_signers = signers
    keystore, password = signing_key(Path("Cargo.toml"), profile)
    if not keystore.is_file():
        raise RuntimeError("Configured Android release keystore does not exist")
    env = dict(os.environ, NOVA_APK_KEYSTORE_PASSWORD=password)
    tag = os.environ["GITHUB_REF_NAME"].removeprefix("v").replace("/", "-")
    dist = Path("dist")
    dist.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="nova-apk-") as temporary:
        work = Path(temporary)
        if sources:
            source = work / "merged.apk"
            merge_apks(sources, source)
        for suffix, abis in VARIANTS.items():
            extract_native_libs = suffix == "-universal"
            unsigned, aligned = work / "unsigned.apk", work / "aligned.apk"
            output = dist / f"Nova-Android{suffix}-{tag}.apk"
            filter_apk(source, unsigned, abis, extract_native_libs=extract_native_libs)
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
            verify_payload(source, output, abis, extract_native_libs=extract_native_libs)
            verify_native_layout(output, extract_native_libs=extract_native_libs)
            verify_native_manifest(run(
                tools / "aapt", "dump", "xmltree", output, "AndroidManifest.xml",
            ), extract_native_libs=extract_native_libs)
            mode = "compressed/extracted" if extract_native_libs else "stored/direct-loaded"
            print(f"Verified {output}: {', '.join(sorted(abis))} · {mode}")
    origin = "three parallel ABI builds" if sources else "one universal build"
    print(f"Packaged all {len(VARIANTS)} APKs from {origin}")


if __name__ == "__main__":
    main()
