"""Lock reviewed dated AppImage tools and exercise fail-closed checksum handling."""
import hashlib
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = (ROOT / '.github/scripts/package-linux.sh').read_text()
EXPECTED_TOOLS = [
    (
        'https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage',
        '$tools_dir/linuxdeploy-x86_64.AppImage',
        'c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d',
    ),
    (
        'https://github.com/linuxdeploy/linuxdeploy-plugin-appimage/releases/download/1-alpha-20250213-1/linuxdeploy-plugin-appimage-x86_64.AppImage',
        '$tools_dir/linuxdeploy-plugin-appimage-x86_64.AppImage',
        '992d502a248e14ab185448ddf6f6e7d25558cb84d4623c354c3af350c25fccb3',
    ),
]


class LinuxToolPinTests(unittest.TestCase):
    def test_tools_use_reviewed_dated_urls_and_exact_hashes(self):
        downloads = re.findall(
            r'^download_verified \\\n  "([^"\n]+)" \\\n  "([^"\n]+)" \\\n  "([0-9a-f]{64})"$',
            SCRIPT, re.MULTILINE,
        )
        self.assertEqual(downloads, EXPECTED_TOOLS)
        self.assertNotIn('/download/continuous/', SCRIPT)

    @unittest.skipUnless(shutil.which('bash') and shutil.which('sha256sum'), 'requires packaging shell tools')
    def test_download_verifier_checks_bytes_before_making_tools_executable(self):
        function = re.search(r'^download_verified\(\) \{.*?^\}', SCRIPT, re.MULTILINE | re.DOTALL)
        self.assertIsNotNone(function)
        payload = b'synthetic tool bytes\n'
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            curl = directory / 'curl'
            curl.write_text('#!/bin/sh\nwhile [ "$1" != "--output" ]; do shift; done\nprintf "synthetic tool bytes\\n" > "$2"\n')
            curl.chmod(0o755)
            for expected_hash, succeeds in ((hashlib.sha256(payload).hexdigest(), True), ('0' * 64, False)):
                with self.subTest(succeeds=succeeds):
                    output = directory / 'tool.AppImage'
                    output.unlink(missing_ok=True)
                    env = dict(os.environ, PATH=f'{directory}{os.pathsep}{os.environ["PATH"]}', PIN_URL=EXPECTED_TOOLS[0][0], TOOL_OUTPUT=str(output), EXPECTED_HASH=expected_hash)
                    result = subprocess.run(
                        ['bash', '-c', 'set -euo pipefail\n' + function[0] + '\ndownload_verified "$PIN_URL" "$TOOL_OUTPUT" "$EXPECTED_HASH"'],
                        env=env, text=True, capture_output=True,
                    )
                    self.assertEqual(result.returncode == 0, succeeds, result.stderr)
                    self.assertEqual(output.read_bytes(), payload)
                    self.assertEqual(bool(output.stat().st_mode & stat.S_IXUSR), succeeds)


if __name__ == '__main__':
    unittest.main()
