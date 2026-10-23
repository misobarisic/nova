"""Source configuration regression for no-Activity JobScheduler ownership.

Runs without the explicitly canceled Android build. It cannot establish Android
runtime/Doze behavior; it verifies app-wide identifiers and ownership guards.
"""
import pathlib
import re
import unittest

ROOT = pathlib.Path(__file__).resolve().parent
JAVA = ROOT / "java" / "dev" / "misob" / "nova"


class JobOwnership(unittest.TestCase):
    def test_services_have_distinct_application_wide_job_ids(self):
        owners = {}
        for path in JAVA.glob("*JobService.java"):
            source = path.read_text()
            match = re.search(r"(?:JOB_ID|JOB)\s*=\s*(0x[0-9a-fA-F]+|\d+)", source)
            self.assertIsNotNone(match, path.name)
            identifier = int(match.group(1), 0)
            self.assertNotIn(identifier, owners, f"{path.name} collides with {owners.get(identifier)}")
            owners[identifier] = path.name
        self.assertGreaterEqual(len(owners), 3)

    def test_offline_disabled_does_not_cancel_another_component(self):
        source = (JAVA / "NovaOfflineJobService.java").read_text()
        self.assertRegex(source, r"if\s*\(!enabled\).*?component\.equals\(old\.getService\(\)\).*?scheduler\.cancel\(JOB_ID\)")
        self.assertIn('!component.equals(old.getService())', source)
        self.assertIn('refusing replacement', source)
        self.assertNotIn('startForegroundService', source)
        self.assertIn('nativeCancel()', source)
        self.assertIn('NETWORK_TYPE_UNMETERED', source)


if __name__ == "__main__":
    unittest.main()
