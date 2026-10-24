"""Release trust boundaries and package identities, without building or publishing."""

import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
from urllib.error import HTTPError


SCRIPTS = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("validate_release", SCRIPTS / "validate-release.py")
validation = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(validation)


class ReleaseValidationTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="nova-release-validation-")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.remote = self.root / "remote.git"
        self.source = self.root / "source"
        self.source.mkdir()
        self.run_git("init", "--bare", str(self.remote))
        self.git("init", "--initial-branch=main")
        self.git("config", "user.name", "Release validation test")
        self.git("config", "user.email", "test@example.invalid")
        self.git("remote", "add", "origin", str(self.remote))
        workflow = self.source / ".github/workflows/release-on-main.yml"
        workflow.parent.mkdir(parents=True)
        workflow.write_text("fixture release support\n")
        self.commit = self.new_version("1.2.3")
        self.git("tag", "v1.2.3")
        self.git("push", "origin", "main", "--tags")
        self.previous = Path.cwd()
        os.chdir(self.source)
        self.addCleanup(os.chdir, self.previous)

    def run_git(self, *args, cwd=None):
        return subprocess.check_output(["git", *args], cwd=cwd or self.root,
                                       text=True, stderr=subprocess.PIPE).strip()

    def git(self, *args):
        return self.run_git(*args, cwd=self.source)

    def new_version(self, version):
        (self.source / "Cargo.toml").write_text(f'[package]\nname = "nova"\nversion = "{version}"\n')
        self.git("add", ".")
        self.git("-c", "commit.gpgsign=false", "commit", "-m", f"version {version}")
        return self.git("rev-parse", "HEAD")

    def test_successive_release_versions_resolve_the_exact_tagged_sources(self):
        first = validation.validate("v1.2.3", self.commit, check_head=True)
        self.assertEqual(first["version"], "1.2.3")
        second = self.new_version("1.2.4")
        self.git("-c", "tag.gpgsign=false", "tag", "-a", "v1.2.4", "-m", "annotated release")
        self.git("push", "origin", "main", "--tags")
        result = validation.validate("v1.2.4", second, check_head=True)
        self.assertEqual(result["commit"], second)
        self.assertNotEqual(result["tag_object"], second)
        self.git("checkout", "--detach", self.commit)
        self.assertEqual(validation.validate("v1.2.3", self.commit, check_head=True), first)

    def test_wrong_commit_version_and_checkout_are_rejected(self):
        with self.assertRaisesRegex(ValueError, "expected commit"):
            validation.validate("v1.2.3", "a" * 40)
        self.git("tag", "v9.9.9")
        self.git("push", "origin", "--tags")
        with self.assertRaisesRegex(ValueError, "match Cargo.toml"):
            validation.validate("v9.9.9", self.commit)
        self.new_version("1.2.4")
        with self.assertRaisesRegex(ValueError, "checkout"):
            validation.validate("v1.2.3", self.commit, check_head=True)

    def test_unmerged_tag_and_deleted_tag_are_rejected(self):
        self.git("checkout", "-b", "untrusted")
        other = self.new_version("1.2.4")
        self.git("tag", "v1.2.4")
        self.git("push", "origin", "--tags")
        with self.assertRaises(subprocess.CalledProcessError):
            validation.validate("v1.2.4", other)
        self.git("push", "origin", ":refs/tags/v1.2.3")
        with self.assertRaises(subprocess.CalledProcessError):
            validation.validate("v1.2.3", self.commit)

    def test_moved_annotated_tag_is_rejected_even_at_the_same_commit(self):
        self.git("tag", "-d", "v1.2.3")
        self.git("-c", "tag.gpgsign=false", "tag", "-a", "v1.2.3", "-m", "first object")
        self.git("push", "--force", "origin", "--tags")
        result = validation.validate("v1.2.3", self.commit)
        self.git("-c", "tag.gpgsign=false", "tag", "-f", "-a", "v1.2.3", "-m", "replacement object")
        self.git("push", "--force", "origin", "--tags")
        with self.assertRaisesRegex(ValueError, "object moved"):
            validation.validate("v1.2.3", self.commit, tag_object=result["tag_object"], version="1.2.3")

    def test_unchecked_inputs_never_reach_git_or_shell_source(self):
        with patch.object(validation, "git") as git:
            for tag, commit in [("v1.2.3; touch injected", self.commit), ("--help", self.commit),
                                ("v1.2.3", "main"), ("v1.2.3", "a" * 40 + "\ninjected=1")]:
                with self.subTest(tag=tag), self.assertRaises(ValueError):
                    validation.validate(tag, commit)
            git.assert_not_called()

    def test_non_main_or_non_dispatch_context_fails_before_git(self):
        for event, ref in [("pull_request", "refs/heads/main"),
                           ("workflow_dispatch", "refs/heads/feature")]:
            with patch.dict(os.environ, GITHUB_EVENT_NAME=event, GITHUB_REF=ref), \
                    patch.object(validation, "git") as git, patch("sys.argv", ["validate-release.py"]):
                with self.assertRaisesRegex(ValueError, "requires workflow_dispatch"):
                    validation.main()
                git.assert_not_called()

    def test_historical_tag_without_identity_support_fails_closed(self):
        (self.source / ".github/workflows/release-on-main.yml").unlink()
        old = self.new_version("1.2.4")
        self.git("tag", "v1.2.4")
        self.git("push", "origin", "main", "--tags")
        with self.assertRaisesRegex(ValueError, "predates"):
            validation.validate("v1.2.4", old)

    def test_duplicate_publication_and_api_errors_fail_closed(self):
        env = dict(GITHUB_REPOSITORY="owner/nova", GH_TOKEN="fixture-token")
        with patch.dict(os.environ, env), patch.object(validation, "urlopen"):
            with self.assertRaisesRegex(ValueError, "already exists"):
                validation.require_unpublished("v1.2.3")
        for status in (404, 403, 500):
            error = HTTPError("https://api.github.com/fixture", status, "fixture", {}, None)
            with patch.dict(os.environ, env), patch.object(validation, "urlopen", side_effect=error):
                if status == 404:
                    validation.require_unpublished("v1.2.3")
                else:
                    with self.assertRaises(HTTPError):
                        validation.require_unpublished("v1.2.3")


class LinuxIdentityTests(unittest.TestCase):
    def versions(self, **env):
        script = 'source "$1"; nova_linux_versions 1.2.3; printf "%s|%s" "$artifact_tag" "$debian_version"'
        clean = {k: v for k, v in os.environ.items() if not k.startswith(("NOVA_RELEASE", "GITHUB_"))}
        return subprocess.run(["bash", "-eu", "-c", script, "bash", str(SCRIPTS / "package-version.sh")],
                              env=dict(clean, **env), text=True, capture_output=True)

    def test_dispatched_filename_and_debian_version_use_the_tag(self):
        result = self.versions(NOVA_RELEASE_TAG="v1.2.3", GITHUB_REF_NAME="main")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "1.2.3|1.2.3")
        self.assertNotEqual(self.versions(NOVA_RELEASE_TAG="v1.2.4").returncode, 0)

    def test_local_pr_and_tag_push_fallbacks_remain(self):
        self.assertEqual(self.versions().stdout, "local|1.2.3+local")
        self.assertEqual(self.versions(GITHUB_REF_NAME="v1.2.3").stdout, "1.2.3|1.2.3")
        self.assertEqual(self.versions(GITHUB_EVENT_NAME="pull_request", GITHUB_EVENT_NUMBER="42").stdout,
                         "pr-42|1.2.3~pr42")
        self.assertEqual(self.versions(GITHUB_REF_NAME="v1.2.3-rc.1").stdout, "1.2.3-rc.1|1.2.3~rc.1")


if __name__ == "__main__":
    unittest.main()
