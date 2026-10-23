"""Android reminder source/manifest contracts; no APK or device execution."""
from pathlib import Path
import subprocess
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[1]
JAVA = ROOT / "android/java/dev/misob/nova"


class ReleaseReminders(unittest.TestCase):
    def test_receiver_is_private_and_silence_is_available_at_minimum_sdk(self):
        metadata = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["metadata"]["android"]
        self.assertGreaterEqual(metadata["sdk"]["min_sdk_version"], 26)
        receiver = next(row for row in metadata["application"]["receiver"] if row["name"] == ".NovaReleaseDismissReceiver")
        self.assertFalse(receiver["exported"])
        source = (JAVA / "NovaReleaseJobService.java").read_text()
        # Group alert behavior predates our API26 minimum; Builder.setSilent
        # does not. Suppression must cover a NEW quiet repost, not only updates.
        self.assertNotIn(".setSilent(", source)
        self.assertIn("setGroupAlertBehavior(Notification.GROUP_ALERT_SUMMARY)", source)
        self.assertIn('setGroup("nova-release-silent")', source)
        self.assertIn("setDeleteIntent(delete)", source)
        self.assertIn("PendingIntent.getBroadcast", source)
        self.assertIn("nativeAcknowledge(context,profile,keys)", source)
        receiver_source = (JAVA / "NovaReleaseDismissReceiver.java").read_text()
        self.assertIn("goAsync()", receiver_source)
        self.assertIn("result.finish()", receiver_source)
        self.assertNotIn("startActivity", receiver_source)
        self.assertNotIn("startService", receiver_source)

    def test_packager_patch_adds_serialized_receiver_metadata(self):
        patch = ROOT / "android/cargo-apk2-receivers.patch"
        fixture = '''pub struct Application {
    #[serde(rename = "service")]
    #[serde(default)]
    pub services: Vec<Service>,
}
'''
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "ndk-build2/src/manifest.rs"
            path.parent.mkdir(parents=True)
            path.write_text(fixture)
            subprocess.run(["git", "apply", "--check", str(patch)], cwd=temporary, check=True, capture_output=True)
            subprocess.run(["git", "apply", str(patch)], cwd=temporary, check=True, capture_output=True)
            self.assertIn('#[serde(rename = "receiver", default)]', path.read_text())
            self.assertIn("pub receivers: Vec<Service>", path.read_text())
        self.assertIn("./android/cargo-apk2-receivers.patch", (ROOT / "flake.nix").read_text())

    def test_both_cold_native_entries_initialize_profiles_before_owned_reads(self):
        source = (ROOT / "src/app/release_reminders_android.rs").read_text()
        self.assertEqual(source.count("if !profiles::is_initialized()"), 2)
        self.assertEqual(source.count("profiles::initialize()?"), 2)
        self.assertIn("sync::project_headless_profile(&owner)", source)
        self.assertNotIn("Bridge::new", source)
        self.assertNotIn("AppWindow::new", source)


if __name__ == "__main__":
    unittest.main()
