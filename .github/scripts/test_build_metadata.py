"""Exercise the real build-script catalog command with a std-only Rust probe.

The mock cargo-about records argv; it does not resolve packages or replace real
feature-graph/license inventory validation. No Cargo build or native UI is used.
"""
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
FEATURES = ('desktop', 'android', 'live-preview', 'update-check')
TEMPLATES = ('assets/open_source_crate_sources.hbs', 'assets/open_source_licenses.hbs')


def catalog_function(source):
    start = source.index('fn render_cargo_about(')
    end = source.index('\n}\n', start) + len('\n}')
    return source[start:end]


class BuildMetadata(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temporary = tempfile.TemporaryDirectory(prefix='nova-build-metadata-')
        cls.addClassCleanup(cls.temporary.cleanup)
        cls.directory = Path(cls.temporary.name)
        # Accept a baseline source explicitly to demonstrate the same failing
        # assertions against the old command, without editing the checkout.
        cls.source = Path(os.environ.get('NOVA_BUILD_METADATA_SOURCE', ROOT / 'build.rs')).read_text()
        probe = cls.directory / 'probe.rs'
        probe.write_text('use std::{env, path::PathBuf, process::Command};\n' +
                         catalog_function(cls.source) + '''
fn main() {
    let args: Vec<_> = env::args().collect();
    for template in ["assets/open_source_crate_sources.hbs", "assets/open_source_licenses.hbs"] {
        let result = render_cargo_about(&PathBuf::from(&args[1]), template, &args[2]);
        assert_eq!(result, "complete legal fixture\\n");
    }
}
''')
        cls.binary = cls.directory / 'probe'
        try:
            subprocess.run(['rustc', '--edition=2024', str(probe), '-o', str(cls.binary)],
                           check=True, capture_output=True, text=True)
        except FileNotFoundError as error:
            raise RuntimeError('rustc is required; run this suite in nix develop .#default') from error
        mock = cls.directory / 'cargo-about'
        mock.write_text(f'#!{sys.executable}\n' + '''import json, os, sys
with open(os.environ['NOVA_ABOUT_ARGV'], 'a') as capture:
    capture.write(json.dumps({'cwd': os.getcwd(), 'args': sys.argv[1:]}) + '\\n')
if os.environ.get('NOVA_ABOUT_FAIL'):
    print('injected catalog failure', file=sys.stderr)
    sys.exit(23)
sys.stdout.write('complete legal fixture\\n')
''')
        mock.chmod(0o755)
        cls.manifest = tomllib.loads((ROOT / 'Cargo.toml').read_text())

    def invoke(self, enabled, target, fail=False):
        capture = self.directory / f'{self._testMethodName}-argv.jsonl'
        capture.unlink(missing_ok=True)
        environment = {key: value for key, value in os.environ.items()
                       if not key.startswith('CARGO_FEATURE_')}
        environment['PATH'] = str(self.directory) + os.pathsep + environment['PATH']
        environment['NOVA_ABOUT_ARGV'] = str(capture)
        for feature in enabled:
            environment['CARGO_FEATURE_' + feature.upper().replace('-', '_')] = '1'
        if fail:
            environment['NOVA_ABOUT_FAIL'] = '1'
        result = subprocess.run([str(self.binary), str(ROOT), target], env=environment,
                                capture_output=True, text=True)
        entries = [json.loads(line) for line in capture.read_text().splitlines()]
        if fail:
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('injected catalog failure', result.stderr)
            self.assertIn('cargo-about failed to generate', result.stderr)
            self.assertEqual(len(entries), 1)
            return
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(entries), 2)
        ordered = [feature for feature in FEATURES if feature in enabled]
        for entry, template in zip(entries, TEMPLATES):
            expected = ['generate', '--locked', '--fail', '-c', 'about.toml',
                        '--target', target, '--no-default-features']
            if ordered:
                expected.extend(['--features', ','.join(ordered)])
            expected.append(template)
            self.assertEqual(entry, {'cwd': str(ROOT), 'args': expected})
        print(f'catalog argv: {sorted(enabled)} -> {json.dumps(entries)}', flush=True)

    def test_desktop_defaults(self):
        self.invoke(set(self.manifest['features']['default']), 'x86_64-unknown-linux-gnu')

    def test_desktop_without_update_check(self):
        self.invoke({'desktop'}, 'x86_64-unknown-linux-gnu')

    def test_android_with_update_check(self):
        self.invoke({'android', 'update-check'}, 'aarch64-linux-android')

    def test_android_without_update_check(self):
        self.invoke({'android'}, 'aarch64-linux-android')

    def test_desktop_live_preview(self):
        self.invoke(set(self.manifest['features']['default']) | {'live-preview'}, 'x86_64-unknown-linux-gnu')

    def test_empty_feature_set_stays_without_defaults(self):
        self.invoke(set(), 'x86_64-unknown-linux-gnu')

    def test_catalog_failure_is_fatal(self):
        self.invoke({'android'}, 'aarch64-linux-android', fail=True)

    def test_every_root_feature_is_forwarded_and_watched(self):
        root_features = set(self.manifest['features']) - {'default'}
        forwarded = dict(re.findall(r'\("(CARGO_FEATURE_[A-Z_]+)", "([^"\n]+)"\)', catalog_function(self.source)))
        self.assertEqual(set(forwarded.values()), root_features)
        watch_source = self.source.split('fn generate_license_catalog()', 1)[1].split('let target =', 1)[0]
        for feature in root_features:
            variable = 'CARGO_FEATURE_' + feature.upper().replace('-', '_')
            self.assertEqual(forwarded[variable], feature)
            self.assertIn(f'"{variable}"', watch_source)


if __name__ == '__main__':
    unittest.main()
