"""Source-only CI contracts; no GitHub API, Cargo, or native libraries required."""
import contextlib
import fnmatch
import importlib.util
import io
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / '.github/workflows/build-release.yml'
RUNNER = ROOT / '.github/scripts/run-ci-tests.py'
spec = importlib.util.spec_from_file_location('ci_test_runner', RUNNER)
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


def block(source, key, indent):
    match = re.search(rf'^{" " * indent}{re.escape(key)}:\n(.*?)(?=^\S|^ {"" if indent == 0 else " " * (indent - 1)}\S|\Z)', source, re.M | re.S)
    if not match:
        raise AssertionError(f'missing {key}')
    return match.group(1)


def jobs(source):
    body = source.split('\njobs:\n', 1)[1]
    return dict(re.findall(r'^  ([\w-]+):\n(.*?)(?=^  [\w-]+:|\Z)', body, re.M | re.S))


class TriggerContract(unittest.TestCase):
    def setUp(self):
        self.source = WORKFLOW.read_text()

    def test_both_filters_trigger_for_tests_scripts_and_makefile(self):
        for event in ('pull_request', 'push'):
            event_source = block(self.source, event, 2)
            paths = re.findall(r'^      - "([^"\n]+)"$', event_source.split('    paths:\n')[1].split('    tags:')[0], re.M)
            for path in ('tests/library/fixture.rs', 'scripts/tests/test_new.py',
                         'scripts/package-addon.py', 'Makefile', '.github/scripts/test_ci_checks.py'):
                self.assertTrue(any(fnmatch.fnmatchcase(path, pattern) for pattern in paths), (event, path))
        self.assertIn('    tags:\n      - "**"', self.source)

    def test_mandatory_native_gates_and_release_dependencies(self):
        definitions = jobs(self.source)
        condition = "if: github.event_name == 'pull_request' || (github.event_name == 'push' && github.ref == 'refs/heads/main')"
        for name in ('test-backend', 'test-ui'):
            job = definitions[name]
            self.assertIn(condition, job)
            self.assertIn('timeout-minutes: 60', job)
            self.assertIn('contents: read', job)
            self.assertIn('persist-credentials: false', job)
            self.assertIn('nix develop .#default --command', job)
            self.assertNotIn('continue-on-error', job)
            for action in re.findall(r'uses: (\S+)', job):
                self.assertRegex(action, r'@[0-9a-f]{40}$')
        for target in ('nova-config', 'nova-providers', 'nova-media', 'nova-tracking', 'nova-stremio', 'nova-list-services'):
            self.assertIn(f'-p {target}', definitions['test-backend'])
        leaf_step = definitions['test-backend'].split('      - name: Test backend leaf crates and bounded media transport\n', 1)[1].split('      - name:', 1)[0]
        self.assertIn('-- --test-threads=1', leaf_step)
        for module in ('profile_actions_tests', 'sync::tests', 'artwork_tests'):
            self.assertIn(f'app::{module}::', definitions['test-backend'])
        for suite in ('ui_discover', 'ui_library', 'ui_downloads', 'ui_navigation', 'ui_settings_sync'):
            self.assertIn(f'--test {suite}', definitions['test-ui'])
        for name in ('android-build', 'android-package', 'linux-build', 'windows-build', 'release'):
            self.assertIn("if: github.event_name == 'push' && startsWith(github.ref, 'refs/tags/')", definitions[name])
            dependencies = re.findall(r'needs: \[([^\]]+)\]', definitions[name])
            self.assertFalse(any('check-' in item or 'test-' in item for item in dependencies))
        self.assertIn('needs: [android-package, linux-build, windows-build]', definitions['release'])

    def test_source_discovery_and_native_steps_remain_separate(self):
        definitions = jobs(self.source)
        for directory in ('.github/scripts', 'android', 'scripts/tests'):
            self.assertIn(f'run-ci-tests.py --discover {directory}', definitions['check-linux'])
        self.assertIn('nix develop .#default --command python3 -B .github/scripts/run-ci-tests.py --discover .github/scripts', definitions['check-linux'])
        self.assertIn('LIBGL_ALWAYS_SOFTWARE: "1"', definitions['check-linux'])
        self.assertIn('export __EGL_VENDOR_LIBRARY_FILENAMES=', definitions['check-linux'])
        self.assertIn('nix develop .#android --command python3 -B android/test_runtime_packaging.py', definitions['check-android'])
        self.assertIn('nix develop .#android-package --command bash .github/scripts/check-android-packaging-tools.sh', definitions['check-android'])


class FailurePropagation(unittest.TestCase):
    def run_fake_rust(self, output, status=0):
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            return runner.rust([sys.executable, '-c', f'import sys; print({output!r}); sys.exit({status})'])

    def test_nonempty_success(self):
        self.assertEqual(self.run_fake_rust('test result: ok. 2 passed; 0 failed; 1 ignored;'), 0)

    def test_failing_process_propagates(self):
        self.assertEqual(self.run_fake_rust('test result: FAILED. 0 passed; 1 failed;', 7), 7)

    def test_zero_tests_rejected(self):
        self.assertEqual(self.run_fake_rust('test result: ok. 0 passed; 0 failed; 0 ignored;'), 1)

    def test_no_harness_result_rejected(self):
        self.assertEqual(self.run_fake_rust('build succeeded'), 1)

    def test_empty_target_cannot_hide_behind_other_targets(self):
        self.assertEqual(self.run_fake_rust('test result: ok. 2 passed; 0 failed; 0 ignored;\ntest result: ok. 0 passed; 0 failed; 0 ignored;'), 1)

    def test_python_empty_discovery_rejected(self):
        with tempfile.TemporaryDirectory() as directory, contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(runner.python_suite(directory, 'test_*.py'), 1)

    def test_intentionally_failing_python_regression_fails_gate(self):
        with tempfile.TemporaryDirectory() as directory:
            Path(directory, 'test_ci_injected_failure.py').write_text('import unittest\nclass Regression(unittest.TestCase):\n def test_failure(self): self.fail("injected CI failure")\n')
            result = subprocess.run([sys.executable, '-B', str(RUNNER), '--discover', directory], capture_output=True, text=True)
            self.assertEqual(result.returncode, 1)
            self.assertIn('FAILED (failures=1)', result.stderr)
            Path(directory, 'test_ci_injected_failure.py').write_text('import unittest\nclass Regression(unittest.TestCase):\n def test_failure(self): self.assertEqual(2 + 2, 4)\n')
            result = subprocess.run([sys.executable, '-B', str(RUNNER), '--discover', directory], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('Ran 1 test', result.stderr)


if __name__ == '__main__':
    unittest.main()
