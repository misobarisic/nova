"""Exercise release ABI splits without needing the Android signing tools."""
import importlib.util
import os
import tempfile
import struct
import tomllib
import unittest
import warnings
from unittest.mock import patch
from pathlib import Path
from zipfile import ZIP_DEFLATED, ZIP_STORED, ZipFile, ZipInfo

SPEC = importlib.util.spec_from_file_location(
    "package_android", Path(__file__).with_name("package-android.py")
)
packaging = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(packaging)


def binary_manifest(*, utf8=True, enabled=False, resource_id=0x010104EA, duplicates=1, value_type=0x12):
    """Small compiled XML fixture, including both Android string encodings."""
    strings = ["manifest", "application", packaging.ANDROID_NAMESPACE,
               "extractNativeLibs", "android", "long string " * 24]

    def encode_length(length, byte):
        if byte:
            return bytes((length,)) if length < 0x80 else bytes((0x80 | (length >> 8), length & 0xFF))
        return struct.pack("<H", length) if length < 0x8000 else struct.pack("<HH", 0x8000 | (length >> 16), length & 0xFFFF)

    offsets, encoded = [], bytearray()
    for text in strings:
        offsets.append(len(encoded))
        units = len(text.encode("utf-16-le")) // 2
        if utf8:
            data = text.encode()
            encoded += encode_length(units, True) + encode_length(len(data), True) + data + b"\0"
        else:
            encoded += encode_length(units, False) + text.encode("utf-16-le") + b"\0\0"
    encoded += bytes((-len(encoded)) % 4)
    string_start = 28 + 4 * len(strings)
    pool = struct.pack("<HHI5I", 1, 28, string_start + len(encoded), len(strings), 0,
                       0x100 if utf8 else 0, string_start, 0)
    pool += struct.pack(f"<{len(offsets)}I", *offsets) + encoded
    resources = struct.pack("<HHI4I", 0x180, 8, 24, 0, 0, 0, resource_id)

    def node(kind, data):
        return struct.pack("<HHIII", kind, 16, 16 + len(data), 1, packaging.NO_INDEX) + data

    def start(name, attributes=b""):
        return node(0x102, struct.pack("<II6H", packaging.NO_INDEX, name, 20, 20,
                                      len(attributes) // 20, 0, 0, 0) + attributes)

    attribute = struct.pack("<IIIHBBI", 2, 3, packaging.NO_INDEX, 8, 0, value_type,
                            packaging.NO_INDEX if enabled else 0)
    content = pool + resources + node(0x100, struct.pack("<II", 4, 2))
    content += start(0) + start(1, attribute * duplicates)
    content += node(0x103, struct.pack("<II", packaging.NO_INDEX, 1))
    content += node(0x103, struct.pack("<II", packaging.NO_INDEX, 0))
    content += node(0x101, struct.pack("<II", 4, 2))
    return struct.pack("<HHI", 3, 8, 8 + len(content)) + content


class AndroidPackagingTests(unittest.TestCase):
    def test_dispatched_tag_overrides_main_and_must_match_the_apk_version(self):
        with patch.dict(os.environ, {"GITHUB_REF_NAME": "main", "NOVA_RELEASE_TAG": "v1.2.3"}, clear=True):
            self.assertEqual(packaging.release_version("1.2.3"), "1.2.3")
            with self.assertRaisesRegex(RuntimeError, "match Cargo.toml"):
                packaging.release_version("1.2.4")
        with patch.dict(os.environ, {"GITHUB_REF_NAME": "v1.2.3"}, clear=True):
            self.assertEqual(packaging.release_version("1.2.3"), "1.2.3")
        with patch.dict(os.environ, {}, clear=True):
            self.assertEqual(packaging.release_version("1.2.3"), "local")

    def test_native_manifest_requires_explicit_direct_loading(self):
        manifest = Path(__file__).resolve().parents[2] / "Cargo.toml"
        with manifest.open("rb") as handle:
            application = tomllib.load(handle)["package"]["metadata"]["android"]["application"]
        self.assertIs(application["extract_native_libs"], False)
        for value in ("(type 0x12)0x0", "false"):
            packaging.verify_native_manifest(f"    A: android:extractNativeLibs(0x010104ea)={value}\n")
        for value in ("(type 0x12)0xffffffff", "true", "0x01", "falsehood", ""):
            with self.subTest(value=value), self.assertRaisesRegex(RuntimeError, "extractNativeLibs"):
                packaging.verify_native_manifest(f"    A: android:extractNativeLibs(0x010104ea)={value}\n")
        with self.assertRaisesRegex(RuntimeError, "extractNativeLibs"):
            packaging.verify_native_manifest("E: application\n")

    def test_universal_manifest_requires_explicit_extraction(self):
        for value in ("(type 0x12)0xffffffff", "(type 0x12)0x1", "true"):
            packaging.verify_native_manifest(
                f"    A: android:extractNativeLibs(0x010104ea)={value}\n",
                extract_native_libs=True,
            )
        for tree in ("E: application\n", "A: android:extractNativeLibs=false\n",
                     "A: android:extractNativeLibs=(type 0x12)0x0\n"):
            with self.assertRaisesRegex(RuntimeError, "extractNativeLibs=true"):
                packaging.verify_native_manifest(tree, extract_native_libs=True)

    def test_compiled_manifest_patch_preserves_every_other_byte(self):
        for utf8 in (True, False):
            with self.subTest(utf8=utf8):
                original = binary_manifest(utf8=utf8)
                changed = packaging.set_extract_native_libs(original, True)
                self.assertEqual(changed, binary_manifest(utf8=utf8, enabled=True))
                self.assertEqual(sum(a != b for a, b in zip(original, changed)), 4)
                self.assertEqual(packaging.set_extract_native_libs(changed, False), original)
                self.assertEqual(packaging.set_extract_native_libs(changed, True), changed)

    def test_compiled_manifest_patch_rejects_ambiguous_or_malformed_attributes(self):
        for manifest in (b"", b"<manifest/>", binary_manifest()[:-1],
                         binary_manifest(resource_id=0x010104EB),
                         binary_manifest(duplicates=0), binary_manifest(duplicates=2),
                         binary_manifest(value_type=0x10)):
            with self.subTest(manifest=manifest[:8]), self.assertRaises(RuntimeError):
                packaging.set_extract_native_libs(manifest, True)

    def test_native_storage_and_actual_16k_data_alignment_are_both_required(self):
        aligned = self.root / "aligned.apk"
        with ZipFile(aligned, "w") as apk:
            apk.writestr("classes.dex", b"keep compressed", compress_type=ZIP_DEFLATED)
            entry = ZipInfo("lib/arm64-v8a/libnova.so")
            offset = apk.fp.tell() + 30 + len(entry.filename.encode())
            padding = (-offset) % 16384
            if padding < 4:
                padding += 16384
            entry.extra = struct.pack("<HH", 0xffff, padding - 4) + bytes(padding - 4)
            apk.writestr(entry, b"native payload", compress_type=ZIP_STORED)
        packaging.verify_native_layout(aligned)
        with ZipFile(aligned) as apk:
            self.assertEqual(apk.getinfo("classes.dex").compress_type, ZIP_DEFLATED)
        with self.assertRaisesRegex(RuntimeError, "not 16 KiB aligned"):
            packaging.verify_native_layout(self.source)
        compressed = self.root / "compressed.apk"
        with ZipFile(compressed, "w") as apk:
            apk.writestr("lib/arm64-v8a/libnova.so", b"native payload", compress_type=ZIP_DEFLATED)
        with self.assertRaisesRegex(RuntimeError, "is compressed"):
            packaging.verify_native_layout(compressed)
        packaging.verify_native_layout(compressed, extract_native_libs=True)
        with self.assertRaisesRegex(RuntimeError, "not compressed"):
            packaging.verify_native_layout(aligned, extract_native_libs=True)
        empty = self.root / "empty.apk"
        with ZipFile(empty, "w"):
            pass
        with self.assertRaisesRegex(RuntimeError, "no native libraries"):
            packaging.verify_native_layout(empty)

    def test_installation_epoch_preserves_legacy_local_updates(self):
        def badging(code, version="0.0.31", package="dev.misob.nova"):
            return f"package: name='{package}' versionCode='{code}' versionName='{version}'"

        packaging.verify_version(badging(33554463), "0.0.31")
        for code in (16777247, 16777472, 33554431, 2100000001):
            with self.subTest(code=code):
                with self.assertRaisesRegex(RuntimeError, "installation epoch"):
                    packaging.verify_version(badging(code), "0.0.31")
        for report in (badging(33554463, "0.1.0"), badging(33554463, package="other.app"), ""):
            with self.subTest(report=report):
                with self.assertRaisesRegex(RuntimeError, "package or version name"):
                    packaging.verify_version(report, "0.0.31")

    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="nova-apk-test-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.source = self.root / "source.apk"
        with ZipFile(self.source, "w") as apk:
            apk.writestr("AndroidManifest.xml", binary_manifest(), compress_type=ZIP_DEFLATED)
            apk.writestr("classes.dex", b"shared bytecode", compress_type=ZIP_DEFLATED)
            apk.writestr("META-INF/NOTICE", b"keep license notices")
            for name in ("MANIFEST.MF", "CERT.SF", "CERT.RSA"):
                apk.writestr(f"META-INF/{name}", b"old signature")
            for abi in packaging.ABIS:
                for library in packaging.NATIVE_LIBRARIES:
                    elf_class, machine = packaging.ABI_ELF[abi]
                    header = bytearray(20)
                    header[:6] = b"\x7fELF" + bytes((elf_class, 1))
                    header[18:20] = machine.to_bytes(2, "little")
                    apk.writestr(
                        f"lib/{abi}/{library}", header + f"{abi}/{library}".encode(),
                        compress_type=ZIP_STORED,
                    )

    def test_release_variants_cover_the_cargo_targets_and_include_armv7(self):
        manifest = Path(__file__).resolve().parents[2] / "Cargo.toml"
        with manifest.open("rb") as handle:
            targets = tomllib.load(handle)["package"]["metadata"]["android"]["build_targets"]
        target_abis = {
            "aarch64-linux-android": "arm64-v8a",
            "armv7-linux-androideabi": "armeabi-v7a",
            "x86_64-linux-android": "x86_64",
        }
        expected = {target_abis[target] for target in targets}
        self.assertEqual(packaging.ABIS, expected)
        self.assertEqual(packaging.VARIANTS["-universal"], expected)
        self.assertEqual(packaging.VARIANTS["-armeabi-v7a"], {"armeabi-v7a"})
        self.assertEqual(
            set(packaging.VARIANTS), {"-universal", *(f"-{abi}" for abi in expected)}
        )

    def test_wrong_native_architecture_rejects_the_source(self):
        broken = self.root / "wrong-abi.apk"
        with ZipFile(self.source) as original:
            wrong = original.read("lib/arm64-v8a/libavcodec.so")
        self.rewrite(
            self.source, broken,
            lambda entry, data: (entry, wrong)
            if entry.filename == "lib/armeabi-v7a/libavcodec.so" else (entry, data),
        )
        with self.assertRaisesRegex(RuntimeError, "wrong architecture"):
            packaging.filter_apk(broken, self.root / "output.apk", {"armeabi-v7a"})

    def test_all_variants_preserve_common_and_matching_native_payloads(self):
        for suffix, abis in packaging.VARIANTS.items():
            with self.subTest(suffix=suffix):
                output = self.root / f"Nova-Android{suffix}-test.apk"
                extract = suffix == "-universal"
                packaging.filter_apk(self.source, output, abis, extract_native_libs=extract)
                packaging.verify_payload(self.source, output, abis, extract_native_libs=extract)
                with ZipFile(output) as apk:
                    self.assertEqual(apk.read("classes.dex"), b"shared bytecode")
                    self.assertEqual(apk.read("META-INF/NOTICE"), b"keep license notices")
                    self.assertFalse(any(packaging.signature_entry(name) for name in apk.namelist()))
                    self.assertEqual(
                        {name for name in apk.namelist() if name.startswith("lib/")},
                        {f"lib/{abi}/{lib}" for abi in abis for lib in packaging.NATIVE_LIBRARIES},
                    )
                    self.assertEqual(apk.read("AndroidManifest.xml"), binary_manifest(enabled=extract))
                    self.assertTrue(all(apk.getinfo(name).compress_type == (ZIP_DEFLATED if extract else ZIP_STORED)
                                        for name in apk.namelist() if packaging.native_entry(name)))

    def split_inputs(self):
        inputs = {}
        for abi in sorted(packaging.ABIS):
            inputs[abi] = self.root / f"{abi}.apk"
            packaging.filter_apk(self.source, inputs[abi], {abi})
        return inputs

    def test_parallel_builds_merge_into_all_four_release_variants(self):
        inputs = self.split_inputs()
        # Runners need not produce identical ZIP timestamps or old signatures.
        for abi, source in inputs.items():
            with ZipFile(source, "a") as apk:
                apk.writestr("META-INF/CERT.SF", abi.encode())
        merged = self.root / "merged.apk"
        packaging.merge_apks(dict(reversed(list(inputs.items()))), merged)
        packaging.verify_payload(self.source, merged, packaging.ABIS)
        for suffix, abis in packaging.VARIANTS.items():
            with self.subTest(variant=suffix):
                output = self.root / f"release{suffix}.apk"
                extract = suffix == "-universal"
                packaging.filter_apk(merged, output, abis, extract_native_libs=extract)
                packaging.verify_payload(self.source, output, abis, extract_native_libs=extract)

    def test_parallel_merge_rejects_conflicting_shared_payloads(self):
        for name, compression, data in (
            ("classes.dex", ZIP_DEFLATED, b"different bytecode"),
            ("classes.dex", ZIP_STORED, b"shared bytecode"),
            ("AndroidManifest.xml", ZIP_DEFLATED, binary_manifest(enabled=True)),
            ("META-INF/NOTICE", ZIP_STORED, b"different licenses"),
        ):
            with self.subTest(entry=name, compression=compression):
                inputs = self.split_inputs()
                broken = self.root / "conflicting.apk"

                def corrupt(entry, original):
                    if entry.filename == name:
                        entry.compress_type = compression
                        return entry, data
                    return entry, original

                self.rewrite(inputs["x86_64"], broken, corrupt)
                inputs["x86_64"] = broken
                with self.assertRaisesRegex(RuntimeError, "Shared APK payload differs"):
                    packaging.merge_apks(inputs, self.root / "merged.apk")

    def test_parallel_merge_rejects_missing_shared_entries_and_duplicate_entries(self):
        inputs = self.split_inputs()
        broken = self.root / "missing-shared.apk"
        self.rewrite(inputs["x86_64"], broken, lambda entry, data:
                     None if entry.filename == "classes.dex" else (entry, data))
        with self.assertRaisesRegex(RuntimeError, "Shared APK entry set differs"):
            packaging.merge_apks(dict(inputs, x86_64=broken), self.root / "merged.apk")
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", UserWarning)
            with ZipFile(inputs["x86_64"], "a") as apk:
                apk.writestr("classes.dex", b"duplicated")
        with self.assertRaisesRegex(RuntimeError, "duplicate ZIP entries"):
            packaging.merge_apks(inputs, self.root / "merged.apk")

    def test_parallel_merge_requires_exact_abi_inputs_and_native_architectures(self):
        inputs = self.split_inputs()
        output = self.root / "merged.apk"
        with self.assertRaisesRegex(RuntimeError, "cover exactly"):
            packaging.merge_apks({"arm64-v8a": inputs["arm64-v8a"]}, output)
        with self.assertRaisesRegex(RuntimeError, "APK ABIs"):
            packaging.merge_apks(dict(inputs, x86_64=inputs["arm64-v8a"]), output)
        broken = self.root / "wrong-native.apk"
        self.rewrite(inputs["x86_64"], broken, lambda entry, data:
                     (entry, b"not an ELF library") if entry.filename.endswith("libnova.so")
                     else (entry, data))
        with self.assertRaisesRegex(RuntimeError, "wrong architecture"):
            packaging.merge_apks(dict(inputs, x86_64=broken), output)

    def test_parallel_input_arguments_require_each_abi_once(self):
        self.assertEqual(packaging.abi_apks([]), {})
        arguments = [f"{abi}={abi}.apk" for abi in sorted(packaging.ABIS)]
        self.assertEqual(packaging.abi_apks(arguments),
                         {abi: Path(f"{abi}.apk") for abi in packaging.ABIS})
        for invalid in (arguments[:1], arguments + arguments[:1],
                        ["unsupported=file.apk"], ["arm64-v8a="], ["arm64-v8a"]):
            with self.subTest(arguments=invalid), self.assertRaises(ValueError):
                packaging.abi_apks(invalid)

    def test_universal_verification_allows_only_the_intended_transformations(self):
        universal = self.root / "universal.apk"
        packaging.filter_apk(self.source, universal, packaging.ABIS, extract_native_libs=True)
        packaging.verify_payload(self.source, universal, packaging.ABIS, extract_native_libs=True)
        packaging.verify_native_layout(universal, extract_native_libs=True)
        for name, compression, data in (
            ("AndroidManifest.xml", ZIP_DEFLATED, binary_manifest()),
            ("classes.dex", ZIP_STORED, b"shared bytecode"),
            ("META-INF/NOTICE", ZIP_STORED, b"changed notice"),
        ):
            with self.subTest(entry=name):
                broken = self.root / "changed-universal.apk"

                def corrupt(entry, old_data):
                    if entry.filename == name:
                        entry.compress_type = compression
                        return entry, data
                    return entry, old_data

                self.rewrite(universal, broken, corrupt)
                with self.assertRaisesRegex(RuntimeError, "Packaging changed"):
                    packaging.verify_payload(self.source, broken, packaging.ABIS, extract_native_libs=True)

    def rewrite(self, source, destination, transform):
        with ZipFile(source) as original, ZipFile(destination, "w") as changed:
            for entry in original.infolist():
                data = original.read(entry)
                result = transform(entry, data)
                if result is not None:
                    changed.writestr(*result)

    def test_missing_armv7_native_library_rejects_the_source(self):
        for library in ("libmpv.so", "libavcodec.so", "libavutil.so"):
            with self.subTest(library=library):
                broken = self.root / "missing.apk"
                self.rewrite(
                    self.source, broken,
                    lambda entry, data: None
                    if entry.filename == f"lib/armeabi-v7a/{library}"
                    else (entry, data),
                )
                with self.assertRaisesRegex(RuntimeError, "missing native libraries"):
                    packaging.filter_apk(broken, self.root / "output.apk", {"armeabi-v7a"})

    def test_changed_native_bytes_or_compression_rejects_the_output(self):
        split = self.root / "split.apk"
        abis = {"armeabi-v7a"}
        packaging.filter_apk(self.source, split, abis)
        for change_compression in (False, True):
            with self.subTest(compression=change_compression):
                def corrupt(entry, data):
                    if entry.filename == "lib/armeabi-v7a/libnova.so":
                        if change_compression:
                            entry.compress_type = ZIP_DEFLATED
                        else:
                            data += b"changed"
                    return entry, data

                broken = self.root / "changed.apk"
                self.rewrite(split, broken, corrupt)
                with self.assertRaisesRegex(RuntimeError, "Packaging changed"):
                    packaging.verify_payload(self.source, broken, abis)


if __name__ == "__main__":
    unittest.main()
