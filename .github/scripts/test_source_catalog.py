"""Source identity contracts: real workspace metadata plus collision fixtures."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'build-support/source_catalog.py'
spec = importlib.util.spec_from_file_location('source_catalog', SCRIPT)
catalog = importlib.util.module_from_spec(spec)
spec.loader.exec_module(catalog)


def fixture_metadata():
    return {'workspace_root': '/fixture', 'workspace_members': ['local-id'], 'packages': [
        {'id': 'local-id', 'name': 'unrelated-name', 'source': None,
         'manifest_path': '/fixture/crates/new-leaf/Cargo.toml'},
        {'id': 'registry-id', 'name': 'unrelated-name', 'source': 'registry+example',
         'manifest_path': '/registry/unrelated-name/Cargo.toml'}]}


class SourceIdentity(unittest.TestCase):
    def test_exact_manifest_identity_preserves_same_named_external_package(self):
        source = ('unrelated-name 1\tMIT\thttps://crates.io/crates/unrelated-name/1\t/fixture/crates/new-leaf/Cargo.toml\n'
                  'unrelated-name 1\tMIT\thttps://external.example/source\t/registry/unrelated-name/Cargo.toml\n')
        result = catalog.source_catalog(source, '', fixture_metadata())
        self.assertEqual(result, 'unrelated-name 1\tMIT\thttps://github.com/misobarisic/nova/tree/main/crates/new-leaf\n'
                                'unrelated-name 1\tMIT\thttps://external.example/source\n')
        self.assertNotIn('/fixture', result)
        self.assertNotIn('/registry', result)

    def test_manual_alias_requires_exact_workspace_url_and_license(self):
        source = 'unrelated-name 1\tMIT\thttps://registry.example\t/fixture/crates/new-leaf/Cargo.toml\n'
        url = 'https://github.com/misobarisic/nova/tree/main/crates/new-leaf'
        manual = f'Friendly local name\tMIT\t{url}\n'
        self.assertEqual(catalog.source_catalog(source, manual, fixture_metadata()), manual)
        self.assertEqual(len(catalog.source_catalog(source, manual.replace('MIT', 'ISC'), fixture_metadata()).splitlines()), 2)

    def test_external_alias_is_never_removed_by_matching_manual_url(self):
        source = 'external 1\tMIT\thttps://external.example/source\t/registry/unrelated-name/Cargo.toml\n'
        manual = 'Manual external attribution\tMIT\thttps://external.example/source\n'
        self.assertEqual(len(catalog.source_catalog(source, manual, fixture_metadata()).splitlines()), 2)

    def test_only_exact_duplicate_rows_are_deduplicated(self):
        source = 'external 1\tMIT\thttps://example.org\t/registry/pkg/Cargo.toml\n'
        different_license = source.replace('MIT', 'ISC')
        result = catalog.source_catalog(source + source + different_license, '', fixture_metadata())
        self.assertEqual(len(result.splitlines()), 2)

    def test_missing_manifest_identity_is_rejected(self):
        with self.assertRaisesRegex(ValueError, 'fourth column'):
            catalog.source_catalog('external 1\tMIT\thttps://example.org\n', '', fixture_metadata())

    def test_sync_source_uses_actual_pinned_revision(self):
        manifest = tomllib.loads((ROOT / 'Cargo.toml').read_text())
        revision = manifest['workspace']['dependencies']['nova-sync']['rev']
        row = next(line for line in (ROOT / 'assets/open_source_project_sources.tsv').read_text().splitlines() if line.startswith('Nova sync crate\t'))
        self.assertEqual(row.split('\t')[2], f'https://github.com/misobarisic/nova-kit/tree/{revision}/crates/sync')
        self.assertEqual(set(catalog.FEATURES), set(manifest['features']) - {'default'})

    def test_metadata_member_omission_is_rejected(self):
        metadata = fixture_metadata()
        metadata['workspace_members'].append('missing-id')
        with self.assertRaisesRegex(ValueError, 'omits'):
            catalog.workspace_links(metadata)

    def test_feature_and_target_metadata_command_matches_build_inputs(self):
        for features in ({'desktop', 'update-check'}, {'desktop'}, {'android'},
                         {'android', 'update-check'}, {'desktop', 'live-preview', 'update-check'}):
            environment = {'CARGO_FEATURE_' + feature.upper().replace('-', '_'): '1' for feature in features}
            command = catalog.metadata_command('declared-target', environment)
            self.assertEqual(command[:11], ['cargo', 'metadata', '--locked', '--offline', '--no-deps', '--format-version', '1', '--filter-platform', 'declared-target', '--no-default-features', '--features'])
            self.assertEqual(set(command[11].split(',')), features)
        self.assertNotIn('--features', catalog.metadata_command('target', {}))

    def test_failed_metadata_is_fatal_and_actionable(self):
        with tempfile.TemporaryDirectory(prefix='nova-metadata-failure-') as directory:
            cargo = Path(directory) / 'cargo'
            cargo.write_text(f'#!{sys.executable}\nimport sys\nprint("injected metadata failure", file=sys.stderr)\nsys.exit(29)\n')
            cargo.chmod(0o755)
            environment = dict(os.environ, PATH=directory + os.pathsep + os.environ['PATH'])
            result = subprocess.run([sys.executable, '-B', str(SCRIPT), '--manifest-dir', str(ROOT), '--target', 'x86_64-unknown-linux-gnu'], env=environment, input='', capture_output=True, text=True)
            self.assertEqual(result.returncode, 1)
            self.assertIn('injected metadata failure', result.stderr)
            self.assertIn('Python 3 and Cargo', result.stderr)
            self.assertEqual(result.stdout, '')

    def test_obsolete_app_filter_removed_and_paths_not_shipped(self):
        source = (ROOT / 'src/app.rs').read_text().split('pub(crate) fn open_source_license_sources()', 1)[1].split('use nova_config', 1)[0]
        self.assertNotIn('WORKSPACE_CRATES', source)
        self.assertNotIn('package_name', source)
        self.assertIn('a.license == b.license', source)
        self.assertIn('{{{crate.manifest_path}}}', (ROOT / 'assets/open_source_crate_sources.hbs').read_text())


class RealWorkspaceInventory(unittest.TestCase):
    def test_actual_rust_resolver_invokes_generator_and_strips_identity(self):
        build = (ROOT / 'build.rs').read_text()
        start = build.index('fn resolve_workspace_sources(')
        end = build.index('\n/// Map Rust triples', start)
        function = build[start:end]
        with tempfile.TemporaryDirectory(prefix='nova-source-resolver-') as directory:
            directory = Path(directory)
            source = directory / 'probe.rs'
            source.write_text('use std::{io::Write, path::PathBuf, process::Command};\n' + function + '''
fn main() {
    let root = PathBuf::from(std::env::args().nth(1).unwrap());
    let row = format!("nova-tracking 0.1.0\\tGPL-3.0-or-later\\thttps://crates.io/crates/nova-tracking/0.1.0\\t{}/crates/tracking/Cargo.toml\\n", root.display());
    print!("{}", resolve_workspace_sources(&root, "x86_64-unknown-linux-gnu", &row));
}
''')
            binary = directory / 'probe'
            subprocess.run(['rustc', '--edition=2024', str(source), '-o', str(binary)], check=True, capture_output=True, text=True)
            result = subprocess.run([str(binary), str(ROOT)], check=True, capture_output=True, text=True)
            self.assertIn('nova-tracking 0.1.0\tGPL-3.0-or-later\thttps://github.com/misobarisic/nova/tree/main/crates/tracking\n', result.stdout)
            self.assertNotIn(str(ROOT), result.stdout)
            self.assertTrue(all(len(row.split('\t')) == 3 for row in result.stdout.splitlines()))

    def test_every_workspace_identity_has_correct_source_and_manual_coverage(self):
        result = subprocess.run(['cargo', 'metadata', '--locked', '--offline', '--no-deps', '--format-version', '1'], cwd=ROOT, check=True, capture_output=True, text=True)
        metadata = json.loads(result.stdout)
        members = set(metadata['workspace_members'])
        packages = [package for package in metadata['packages'] if package['id'] in members]
        self.assertGreater(len(packages), 0)
        rendered = ''.join(f'{package["name"]} {package["version"]}\t{package["license"]}\thttps://crates.io/crates/{package["name"]}/{package["version"]}\t{package["manifest_path"]}\n' for package in packages)
        static = (ROOT / 'assets/open_source_project_sources.tsv').read_text()
        rows = [line.split('\t') for line in catalog.source_catalog(rendered, static, metadata).splitlines()]
        urls = {row[2] for row in rows}
        for url in catalog.workspace_links(metadata).values():
            self.assertIn(url, urls)
        for package in packages:
            self.assertFalse(any(row[0] == f'{package["name"]} {package["version"]}' and row[2].startswith('https://crates.io/') for row in rows))
        self.assertTrue(any(row[0].startswith('nova-tracking ') for row in rows))
        self.assertTrue(any(row[0].startswith('nova-stremio ') for row in rows))
        self.assertTrue(all(len(row) == 3 for row in rows))
        self.assertEqual(set(static.splitlines()) - {'\t'.join(row) for row in rows}, set())
        print(f'Workspace inventory: {len(packages)} exact manifest identities; all source links represented', flush=True)


if __name__ == '__main__':
    unittest.main()
