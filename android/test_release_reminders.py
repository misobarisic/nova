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

    def test_headless_context_does_not_own_foreground_permission_ui(self):
        adapter = (ROOT / "src/app/android_bg.rs").read_text()
        entry = (ROOT / "src/lib.rs").read_text()
        self.assertNotIn("ndk_context::android_context()", adapter)
        self.assertIn("let (vm, context) = APP_CONTEXT.get()?;", adapter)
        self.assertIn('"getApplicationContext"', adapter)
        self.assertIn("retain_app_context(env, context)", adapter)
        self.assertIn("initialize_foreground_context(app.vm_as_ptr(), app.activity_as_ptr())", entry)
        # Service and notification delivery keep using Application, while both
        # permission entry points resolve the resumed Activity independently.
        reminder = (JAVA / "NovaReleaseJobService.java").read_text()
        download = (JAVA / "NovaBackgroundService.java").read_text()
        self.assertIn("NovaActivity.requestNotificationPermission(0x54)", reminder)
        self.assertIn("NovaActivity.requestNotificationPermission(0x53)", download)
        self.assertNotIn("context instanceof Activity", reminder)
        self.assertNotIn("context instanceof android.app.Activity", download)
        activity = (JAVA / "NovaActivity.java").read_text()
        prompt = activity.split("public static void requestNotificationPermission", 1)[1].split("private static final String TRANSPORT", 1)[0]
        self.assertLess(prompt.index("new Handler(Looper.getMainLooper()).post"), prompt.index("foreground.get()"))
        self.assertIn("activity.isFinishing() || activity.isDestroyed()", prompt)
        resume = activity.split("protected void onResume()", 1)[1].split("protected void onPause()", 1)[0]
        pause = activity.split("protected void onPause()", 1)[1].split("protected void onStop()", 1)[0]
        self.assertIn("foreground = new WeakReference<>(this)", resume)
        self.assertIn("foreground.get() == this", pause)
        self.assertIn("foreground = new WeakReference<>(null)", pause)

    def test_document_io_owns_request_and_finishes_current_rotation_instance(self):
        source = (JAVA / "DocumentActivity.java").read_text()
        self.assertIn("AtomicReference<Request> pending", source)
        self.assertIn("final byte[] exportBytes", source)
        self.assertNotIn("static volatile byte[] exportBytes", source)
        self.assertIn("pending.compareAndSet(null, operation)", source)
        # Both launch failure and native completion release only their own
        # request. Old workers cannot clear or finish a subsequent operation.
        self.assertIn("catch (RuntimeException error) { pending.compareAndSet(operation, null); throw error; }", source)
        self.assertIn("operation == null || operation.id != request", source)
        self.assertIn('!operation.token.equals(getIntent().getStringExtra("token"))', source)
        self.assertIn('intent.putExtra("token", operation.token)', source)
        self.assertIn("operation.current = new WeakReference<>(this)", source)
        self.assertIn("request.reading.compareAndSet(false, true)", source)
        worker = source.split("new Thread(() ->", 1)[1].split('}, "NovaDocument").start()', 1)[0]
        self.assertIn("context.getContentResolver().openOutputStream", worker)
        self.assertIn("context.getContentResolver().openInputStream", worker)
        self.assertIn("pending.get() != request", worker)
        self.assertNotIn("operation", worker)
        completion = source.split("private static void complete", 1)[1].split("@Override public void onDestroy", 1)[0]
        self.assertLess(completion.index("pending.compareAndSet(request, null)"), completion.index("nativeResult(request.id"))
        self.assertEqual(source.count("nativeResult(request.id"), 1)
        self.assertLess(completion.index("new Handler(Looper.getMainLooper()).post"), completion.index("request.current.get()"))
        self.assertIn("activity.operation == request", completion)
        self.assertIn("!isChangingConfigurations() && isFinishing()", source)

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
