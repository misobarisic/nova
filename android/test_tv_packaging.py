"""Lightweight regressions for TV packaging; no Android SDK or Rust build."""
import tomllib
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


class TvPackaging(unittest.TestCase):
    def test_phone_and_tv_share_launcher_and_optional_hardware(self):
        metadata = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["metadata"]["android"]
        app = metadata["application"]
        self.assertEqual(app["banner"], "@drawable/tv_banner")
        native = next(a for a in app["activity"] if a["name"] == ".NovaActivity")
        self.assertTrue(native["supports_picture_in_picture"])
        self.assertTrue(native["resizeable_activity"])
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
