"""Run the Windows packager twice against read-only, symlinked Nix-like inputs."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest
import zipfile


class WindowsPackagingTests(unittest.TestCase):
    def test_repeated_packaging_preserves_inputs_and_complete_flat_archive(self):
        script = Path(__file__).resolve().with_name("package-windows.sh")
        with tempfile.TemporaryDirectory(prefix="nova-windows-package-") as scratch:
            root = Path(scratch)
            executable = root / "target/x86_64-pc-windows-gnu/release/nova.exe"
            executable.parent.mkdir(parents=True)
            executable.write_bytes(b"MZ application fixture")
            (root / "LICENSE").write_text("application license")

            media = root / "media"
            inputs = root / "inputs"
            media.mkdir()
            originals = {}
            for directory, names in {
                "64": ["libmpv-2.dll", "avcodec-63.dll", "libmpv.dll.a"],
                "licenses": ["mpv/Copyright"],
                "metadata": ["ffmpeg-config.h"],
            }.items():
                for name in names:
                    source = inputs / directory / name
                    source.parent.mkdir(parents=True, exist_ok=True)
                    source.write_text(f"native fixture: {name}")
                    source.chmod(0o444)
                    originals[source] = source.read_bytes()
                (media / directory).symlink_to(inputs / directory, target_is_directory=True)
            (media / "SOURCES").write_text("native source metadata")
            for directory in sorted(inputs.rglob("*"), reverse=True):
                if directory.is_dir():
                    directory.chmod(0o555)

            runtime = root / "compiler-runtime"
            runtime.mkdir()
            (runtime / "compiler.dll").write_bytes(b"compiler runtime fixture")
            tools = root / "tools"
            tools.mkdir()
            query = tools / "nix-store"
            query.write_text('#!/bin/sh\nprintf "%s\\n" "$MINGW_CC"\n')
            query.chmod(0o755)
            env = dict(
                os.environ,
                MPV_SOURCE=str(media),
                MINGW_CC=str(runtime),
                GITHUB_REF_NAME="v0.0.31",
                NOVA_RELEASE_PROFILE="release",
                PATH=f"{tools}:{os.environ['PATH']}",
            )
            for _ in range(2):
                subprocess.run(["bash", str(script)], cwd=root, env=env, check=True)
                with zipfile.ZipFile(root / "dist/Nova-Windows-x86_64-0.0.31.zip") as archive:
                    self.assertEqual(set(archive.namelist()), {
                        "nova.exe", "LICENSE", "THIRD-PARTY-SOURCES.txt",
                        "libmpv-2.dll", "avcodec-63.dll", "compiler.dll",
                        "licenses/mpv/Copyright", "native-media/ffmpeg-config.h",
                    })
            for source, contents in originals.items():
                self.assertEqual(source.read_bytes(), contents)
                self.assertEqual(source.stat().st_mode & 0o222, 0)


if __name__ == "__main__":
    unittest.main()
