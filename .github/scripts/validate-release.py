"""Bind a main-context release to one version tag, Git object and source commit."""

import argparse
import os
from pathlib import Path
import re
import subprocess
import sys
from urllib.error import HTTPError
from urllib.parse import quote
from urllib.request import Request, urlopen


def git(*args):
    return subprocess.check_output(["git", *args], text=True, stderr=subprocess.PIPE).strip()


def validate(tag, commit, *, tag_object=None, version=None, check_head=False):
    # Validate before constructing refspecs, commands or GitHub outputs.
    if not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?", tag):
        raise ValueError("expected a version tag such as v0.0.43")
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("expected the full lowercase tagged commit SHA")
    if tag_object is not None and not re.fullmatch(r"[0-9a-f]{40}", tag_object):
        raise ValueError("invalid validated tag object")
    git("check-ref-format", f"refs/tags/{tag}")
    # An explicit fetch fails for deleted tags and refreshes moved refs, including
    # annotated tags. Pin the tag object as well as its peeled commit.
    git("fetch", "--no-tags", "--force", "origin",
        "+refs/heads/main:refs/remotes/origin/main",
        f"+refs/tags/{tag}:refs/tags/{tag}")
    actual_object = git("rev-parse", f"refs/tags/{tag}")
    if tag_object is not None and actual_object != tag_object:
        raise ValueError("release tag object moved after validation")
    if git("rev-parse", f"refs/tags/{tag}^{{commit}}") != commit:
        raise ValueError("release tag does not resolve to the expected commit")
    git("merge-base", "--is-ancestor", commit, "refs/remotes/origin/main")
    if check_head and git("rev-parse", "HEAD") != commit:
        raise ValueError("checkout is not the validated release commit")
    if check_head:
        # Build tools may write outputs, but must not rewrite the tagged source
        # or lockfile. Untracked artifact/control directories remain allowed.
        git("diff", "--quiet", "HEAD", "--")
    # Historical sources predate NOVA_RELEASE_TAG handling; do not silently
    # package them as 'main' through this new workflow.
    try:
        git("cat-file", "-e", f"{commit}:.github/workflows/release-on-main.yml")
    except subprocess.CalledProcessError as error:
        raise ValueError("tag predates main-context release support") from error
    if version is None:
        import tomllib  # Full validation runs on Ubuntu 24.04 / Python 3.12.
        version = tomllib.loads(git("show", f"{commit}:Cargo.toml"))["package"]["version"]
    # Rechecks on Ubuntu 22.04 use the version already bound to this immutable
    # commit by full validation, without adding a Python/toolchain dependency.
    if tag != f"v{version}":
        raise ValueError("release tag must match Cargo.toml")
    return dict(tag=tag, commit=commit, version=version, tag_object=actual_object)


def require_unpublished(tag):
    repo = os.environ["GITHUB_REPOSITORY"]
    if not re.fullmatch(r"[\w.-]+/[\w.-]+", repo):
        raise ValueError("invalid repository")
    request = Request(
        f"{os.environ.get('GITHUB_API_URL', 'https://api.github.com')}/repos/{repo}/releases/tags/{quote(tag, safe='')}",
        headers={"Authorization": f"Bearer {os.environ['GH_TOKEN']}",
                 "Accept": "application/vnd.github+json"},
    )
    try:
        with urlopen(request, timeout=30):
            raise ValueError("release already exists; refusing duplicate publication")
    except HTTPError as error:
        if error.code != 404:
            raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--recheck", action="store_true")
    parser.add_argument("--check-head", action="store_true")
    parser.add_argument("--unpublished", action="store_true")
    args = parser.parse_args()
    if (os.environ.get("GITHUB_EVENT_NAME") != "workflow_dispatch"
            or os.environ.get("GITHUB_REF") != "refs/heads/main"):
        raise ValueError("release execution requires workflow_dispatch on refs/heads/main")
    result = validate(
        os.environ["NOVA_RELEASE_TAG"], os.environ["NOVA_RELEASE_COMMIT"],
        tag_object=os.environ["NOVA_RELEASE_TAG_OBJECT"] if args.recheck else None,
        version=os.environ["NOVA_RELEASE_VERSION"] if args.recheck else None,
        check_head=args.check_head,
    )
    if args.unpublished:
        require_unpublished(result["tag"])
    if not args.recheck and os.environ.get("GITHUB_OUTPUT"):
        with Path(os.environ["GITHUB_OUTPUT"]).open("a") as output:
            for key, value in result.items():
                print(f"{key}={value}", file=output)
    print(f"Validated {result['tag']} at {result['commit']} (tag object {result['tag_object']})")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as error:
        print(f"Release validation failed: {error}", file=sys.stderr)
        sys.exit(1)
