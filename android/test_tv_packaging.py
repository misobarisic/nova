"""Lightweight regressions for TV packaging; no Android SDK or Rust build."""
import hashlib
import importlib.util
import json
import tempfile
import tomllib
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("banner_patch", ROOT / "android/patch-cargo-apk2-banner.py")
BANNER_PATCH = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BANNER_PATCH)


class TvPackaging(unittest.TestCase):
    def test_banner_patch_preserves_schema_and_updates_checksum(self):
        source = 'pub struct Application {\n    pub icon: Option<String>,\n    pub label: String,\n}\n'
        with tempfile.TemporaryDirectory() as temp:
            package = Path(temp) / "ndk-build2-1.4.1"
            (package / "src").mkdir(parents=True)
            manifest = package / "src/manifest.rs"
            manifest.write_text(source)
            checksums = package / ".cargo-checksum.json"
            checksums.write_text(json.dumps({"package": "upstream", "files": {"src/manifest.rs": "old", "other": "keep"}}))
            BANNER_PATCH.patch(temp)
            patched = manifest.read_text()
            self.assertIn('rename(serialize = "@android:banner")', patched)
            self.assertEqual(patched.count("pub banner: Option<String>"), 1)
            self.assertIn("pub label: String", patched)
            result = json.loads(checksums.read_text())
            self.assertEqual(result["package"], "upstream")
            self.assertEqual(result["files"]["other"], "keep")
            self.assertEqual(result["files"]["src/manifest.rs"], hashlib.sha256(manifest.read_bytes()).hexdigest())

    def test_phone_and_tv_share_launcher_and_optional_hardware(self):
        metadata = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["metadata"]["android"]
        app = metadata["application"]
        self.assertEqual(app["banner"], "@drawable/tv_banner")
        native = next(a for a in app["activity"] if a["name"] == "android.app.NativeActivity")
        categories = native["intent_filter"][0]["categories"]
        self.assertIn("android.intent.category.LAUNCHER", categories)
        self.assertIn("android.intent.category.LEANBACK_LAUNCHER", categories)
        features = {f["name"]: f["required"] for f in metadata["uses_feature"]}
        self.assertFalse(features["android.software.leanback"])
        self.assertFalse(features["android.hardware.touchscreen"])
        self.assertFalse(features["android.hardware.camera"])
        self.assertFalse(features["android.hardware.camera.autofocus"])
        for asset in ("tv_banner", "tv_wordmark"):
            ET.parse(ROOT / f"android/res/drawable/{asset}.xml")


if __name__ == "__main__":
    unittest.main()
