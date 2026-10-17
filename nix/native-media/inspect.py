#!/usr/bin/env python3
"""Validate the actual libraries before copying a native-media bundle."""

import argparse
import pathlib
import re
import subprocess


def output(*command):
    return subprocess.check_output(command, text=True)


def require(condition, message):
    if not condition:
        raise SystemExit(message)


parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("bundle", type=pathlib.Path)
parser.add_argument("--target", required=True)
parser.add_argument("--tool", required=True, help="target readelf (Android) or objdump (MinGW)")
args = parser.parse_args()
root = args.bundle

config = (root / "share/nova-native/ffmpeg-config.mak").read_text()
encoders = set(re.findall(r"^CONFIG_(\w+)_ENCODER=yes$", config, re.M))
require(encoders == {"FRAME_THREAD", "PNG", "MJPEG"}, f"unexpected encoders: {encoders}")
require(re.search(r"^CONFIG_PGSSUB_DECODER=yes$", config, re.M), "PGS decoding is missing")
require(re.search(r"^CONFIG_LIBDAV1D_DECODER=yes$", config, re.M), "dav1d decoding is missing")
pcs = list((root / "lib/pkgconfig").glob("*.pc"))
require(len(pcs) == 7, f"expected seven pkg-config modules, found {len(pcs)}")
require(all("/nix/store/" not in pc.read_text() for pc in pcs), "pkg-config is not relocatable")

if args.target.startswith("android-"):
    libraries = sorted((root / "lib").glob("*.so"))
    expected = {"libmpv.so", "libavcodec.so", "libavformat.so", "libavfilter.so", "libavutil.so", "libswscale.so", "libswresample.so"}
    require({path.name for path in libraries} == expected, "unexpected Android library set")
    system = {"libc.so", "libm.so", "libdl.so", "libandroid.so", "libOpenSLES.so", "libEGL.so", "libGLESv2.so", "libmediandk.so", "liblog.so"}
    machine = {"android-arm64": "AArch64", "android-armv7": "ARM", "android-x86_64": "Advanced Micro Devices X86-64"}[args.target]
    for library in libraries:
        header = output(args.tool, "-h", str(library))
        require(machine in header, f"wrong architecture: {library.name}")
        dynamic = output(args.tool, "-d", str(library))
        require("RPATH" not in dynamic and "RUNPATH" not in dynamic, f"runtime search path: {library.name}")
        needed = set(re.findall(r"Shared library: \[(.*?)\]", dynamic))
        require(needed <= expected | system, f"unbundled dependencies in {library.name}: {needed - expected - system}")
        require(f"[{library.name}]" in dynamic, f"missing SONAME: {library.name}")
        if args.target != "android-armv7":
            for line in output(args.tool, "-Wl", str(library)).splitlines():
                fields = line.split()
                if fields and fields[0] == "LOAD":
                    require(int(fields[-1], 16) >= 16384, f"LOAD alignment: {library.name}")
                if fields and fields[0] == "GNU_RELRO":
                    require((int(fields[2], 16) + int(fields[5], 16)) % 16384 == 0, f"RELRO alignment: {library.name}")
    exports = output(args.tool, "--dyn-syms", "--wide", str(root / "lib/libavformat.so"))
    for symbol in ("avformat_open_input", "avformat_alloc_output_context2", "av_interleaved_write_frame"):
        require(re.search(r"\b" + symbol + r"(?:@|\s|$)", exports), f"missing public FFmpeg API: {symbol}")
    mpv_exports = output(args.tool, "--dyn-syms", "--wide", str(root / "lib/libmpv.so"))
    require("mpv_render_context_create" in mpv_exports, "missing mpv render API")
else:
    libraries = sorted((root / "lib").glob("*.dll"))
    bundled = {path.name.lower() for path in libraries}
    # Windows system imports observed for this profile; reject accidental C++
    # runtime or other DLL dependencies that won't exist on a clean machine.
    system = {"advapi32.dll", "avrt.dll", "bcrypt.dll", "crypt32.dll", "d3d11.dll", "d3dcompiler_47.dll", "dbghelp.dll", "dwmapi.dll", "dxgi.dll", "gdi32.dll", "imm32.dll", "kernel32.dll", "ksuser.dll", "mfplat.dll", "msvcrt.dll", "ncrypt.dll", "ntdll.dll", "ole32.dll", "oleaut32.dll", "opengl32.dll", "psapi.dll", "secur32.dll", "shcore.dll", "shell32.dll", "shlwapi.dll", "ucrtbase.dll", "user32.dll", "uuid.dll", "uxtheme.dll", "version.dll", "winmm.dll", "ws2_32.dll"}
    require("libmpv-2.dll" in bundled and len(libraries) >= 8, "incomplete Windows bundle")
    for library in libraries:
        headers = output(args.tool, "-p", str(library))
        require("pei-x86-64" in headers, f"wrong architecture: {library.name}")
        imports = set(re.findall(r"DLL Name: (\S+)", headers))
        missing = {name for name in imports if name.lower() not in bundled | system and not name.lower().startswith("api-ms-win-")}
        require(not missing, f"unbundled dependencies in {library.name}: {missing}")
    exports = output(args.tool, "-p", str(root / "lib/avformat-63.dll"))
    require("av_interleaved_write_frame" in exports, "missing public FFmpeg remux API")

print(f"{args.target}: {len(libraries)} libraries; dependencies, APIs and build profile verified")
