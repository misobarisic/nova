import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import zipfile

script = Path(__file__).resolve().parents[1] / "package-addon.py"
spec = importlib.util.spec_from_file_location("package_addon", script)
packager = importlib.util.module_from_spec(spec)
spec.loader.exec_module(packager)


class PackagingTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.source = self.root / "source"
        self.addon = self.source / "addons" / "fixture"
        self.addon.mkdir(parents=True)
        self.manifest = {"id": "fixture", "version": "1.2.3", "entry": "index.js", "scripts": ["first.js", "index.js"], "assets": ["logo.jpg"]}
        self.write_manifest()
        for name in ["stremio-manifest.json", "LICENSE", "NOTICE.md", "first.js", "index.js", "logo.jpg"]:
            (self.addon / name).write_text(name)
        (self.addon / ".env").write_text("private")
        (self.addon / "fixtures").mkdir()
        (self.addon / "fixtures" / "response.json").write_text("fixture")

    def tearDown(self):
        self.temporary.cleanup()

    def write_manifest(self):
        (self.addon / "manifest.json").write_text(json.dumps(self.manifest))

    @patch.object(packager.subprocess, "run")
    def test_repeatable_manifest_only_archives_and_hashes(self, validator):
        one = packager.build(self.source, "fixture", self.root / "one", ["validator"])
        two = packager.build(self.source, "fixture", self.root / "two", ["validator"])
        self.assertEqual(one.read_bytes(), two.read_bytes())
        self.assertEqual(validator.call_count, 2)
        self.assertTrue(validator.call_args.kwargs["check"])
        self.assertEqual(one.with_suffix(".zip.sha256").read_text(), f"{hashlib.sha256(one.read_bytes()).hexdigest()}  {one.name}\n")
        with zipfile.ZipFile(one) as archive:
            self.assertEqual(archive.namelist(), sorted(archive.namelist()))
            self.assertEqual(len(archive.namelist()), 7)
            self.assertTrue(all(i.date_time == (1980, 1, 1, 0, 0, 0) and i.compress_type == zipfile.ZIP_STORED for i in archive.infolist()))

    @patch.object(packager.subprocess, "run")
    def test_rejects_declared_private_development_and_traversal_inputs(self, validator):
        for name in [".env", "fixtures/response.json", "../LICENSE", "private.key", "target/output"]:
            self.manifest["assets"] = [name]
            self.write_manifest()
            with self.assertRaises(ValueError):
                packager.build(self.source, "fixture", self.root / "out", ["validator"])
        validator.assert_not_called()

    @patch.object(packager.subprocess, "run")
    def test_absent_scripts_default_to_entry_but_explicit_empty_is_rejected(self, validator):
        self.manifest["scripts"] = []
        self.write_manifest()
        with self.assertRaises(ValueError):
            packager.build(self.source, "fixture", self.root / "out", ["validator"])
        validator.assert_not_called()
        del self.manifest["scripts"]
        self.write_manifest()
        archive = packager.build(self.source, "fixture", self.root / "out", ["validator"])
        with zipfile.ZipFile(archive) as package:
            self.assertIn("index.js", package.namelist())
            self.assertNotIn("first.js", package.namelist())

    @patch.object(packager.subprocess, "run", side_effect=RuntimeError("invalid package"))
    def test_validator_failure_does_not_publish(self, validator):
        with self.assertRaises(RuntimeError):
            packager.build(self.source, "fixture", self.root / "out", ["validator"])
        self.assertEqual(list((self.root / "out").iterdir()), [])


if __name__ == "__main__":
    unittest.main()
