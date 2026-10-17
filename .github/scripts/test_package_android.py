"""Exercise release ABI splits without needing the Android signing tools."""
import importlib.util
import tempfile
import tomllib
import unittest
from pathlib import Path
from zipfile import ZIP_DEFLATED, ZIP_STORED, ZipFile

SPEC = importlib.util.spec_from_file_location(
    "package_android", Path(__file__).with_name("package-android.py")
)
packaging = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(packaging)


class AndroidPackagingTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="nova-apk-test-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.source = self.root / "source.apk"
        with ZipFile(self.source, "w") as apk:
            apk.writestr("classes.dex", b"shared bytecode", compress_type=ZIP_DEFLATED)
            apk.writestr("META-INF/NOTICE", b"keep license notices")
            for name in ("MANIFEST.MF", "CERT.SF", "CERT.RSA"):
                apk.writestr(f"META-INF/{name}", b"old signature")
            for abi in packaging.ABIS:
                for library in ("libnova.so", "libmpv.so"):
                    apk.writestr(
                        f"lib/{abi}/{library}", f"{abi}/{library}".encode(),
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

    def test_vendored_libraries_match_their_android_architectures(self):
        repo = Path(__file__).resolve().parents[2]
        # Check the binary itself: placing an ARM64 library in the ARMv7
        # directory would pass ZIP-path checks but fail on a 32-bit device.
        for abi, elf_class, machine in (
            ("arm64-v8a", 2, 183),
            ("armeabi-v7a", 1, 40),
            ("x86_64", 2, 62),
        ):
            with self.subTest(abi=abi):
                with (repo / "vendor/android-libs" / abi / "libmpv.so").open("rb") as library:
                    header = library.read(20)
                self.assertEqual(header[:6], b"\x7fELF" + bytes((elf_class, 1)))
                self.assertEqual(int.from_bytes(header[18:20], "little"), machine)

    def test_all_variants_preserve_common_and_matching_native_payloads(self):
        for suffix, abis in packaging.VARIANTS.items():
            with self.subTest(suffix=suffix):
                output = self.root / f"Nova-Android{suffix}-test.apk"
                packaging.filter_apk(self.source, output, abis)
                packaging.verify_payload(self.source, output, abis)
                with ZipFile(output) as apk:
                    self.assertEqual(apk.read("classes.dex"), b"shared bytecode")
                    self.assertEqual(apk.read("META-INF/NOTICE"), b"keep license notices")
                    self.assertFalse(any(packaging.signature_entry(name) for name in apk.namelist()))
                    self.assertEqual(
                        {name for name in apk.namelist() if name.startswith("lib/")},
                        {f"lib/{abi}/{lib}" for abi in abis for lib in ("libnova.so", "libmpv.so")},
                    )

    def rewrite(self, source, destination, transform):
        with ZipFile(source) as original, ZipFile(destination, "w") as changed:
            for entry in original.infolist():
                data = original.read(entry)
                result = transform(entry, data)
                if result is not None:
                    changed.writestr(*result)

    def test_missing_armv7_native_library_rejects_the_source(self):
        broken = self.root / "missing.apk"
        self.rewrite(
            self.source, broken,
            lambda entry, data: None if entry.filename == "lib/armeabi-v7a/libmpv.so"
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
