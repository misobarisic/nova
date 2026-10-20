#!/usr/bin/env python3
"""Remove six emoji cmap mappings from the two pinned Roboto text faces.

Requires fonttools==4.61.1. Glyphs, layout tables and all other cmap coverage
remain intact. Regeneration downloads verified immutable sources into memory;
--check is offline, reading the original Git blobs or an explicit --inputs DIR.
"""
import argparse
import hashlib
import io
from pathlib import Path
import subprocess
from urllib.request import urlopen

from fontTools.ttLib import TTFont

ROOT = Path(__file__).resolve().parents[1]
REVISION = "38062f4b4a0be4346d07a928408da21602545e9e"
INPUTS = {
    "Roboto-Regular.ttf": (
        "56a45233d29f11b4dfb86d248e921939d115778f87325e7ae8cc108383d6664d",
        "ddee473e02035bfce6244746d5d2a281616f7ff8",
    ),
    "Roboto-Bold.ttf": (
        "61f89f8db49261c2f6106e8dccc35df7b2f7ed909020db40a3fc905e95f99334",
        "8869666f245c6f474b98024327df7082e4c868b8",
    ),
}
# Keep ASCII #, *, digits, spaces and controls in the text face. Removing the
# keycap mark forces its entire grapheme into Noto, including its ASCII base.
EMOJI = {0x00A9, 0x00AE, 0x203C, 0x2049, 0x20E3, 0x2122}
NOTICE = (
    "Modified for Nova: removed only Unicode cmap mappings U+00A9, U+00AE, "
    "U+203C, U+2049, U+20E3 and U+2122 so bundled Noto renders emoji. "
    "Original Roboto glyph outlines, metrics, shaping and hinting are unchanged."
)


def upstream(name, directory, offline):
    digest, blob = INPUTS[name]
    if directory:
        data = (directory / name).read_bytes()
    elif offline:
        data = subprocess.check_output(
            ["git", "cat-file", "blob", blob], cwd=ROOT,
        )
    else:
        with urlopen(
            f"https://raw.githubusercontent.com/googlefonts/roboto-2/{REVISION}/src/hinted/{name}"
        ) as response:
            data = response.read()
    assert hashlib.sha256(data).hexdigest() == digest, f"unexpected source: {name}"
    return data


def rebuild(data):
    font = TTFont(io.BytesIO(data), recalcTimestamp=False, lazy=True)
    original = font.getBestCmap().copy()
    assert EMOJI <= original.keys()
    glyph_order = font.getGlyphOrder()
    # This is a cmap edit, not glyph subsetting: retain all outlines, metrics,
    # kerning, shaping and hinting bytes exactly as the pinned upstream faces.
    unchanged = {
        tag: font.getTableData(tag)
        for tag in font.keys()
        if tag not in ("GlyphOrder", "head", "cmap", "name")
    }
    names = font["name"]
    original_names = {
        (record.nameID, record.platformID, record.platEncID, record.langID): record.string
        for record in names.names if record.nameID != 10
    }
    for record in list(names.names):
        if record.nameID == 10:
            names.setName(
                NOTICE + "\n" + record.toUnicode(), 10,
                record.platformID, record.platEncID, record.langID,
            )
    if not names.getName(10, 3, 1, 0x409):
        names.setName(NOTICE, 10, 3, 1, 0x409)
    for table in font["cmap"].tables:
        if table.isUnicode():
            for codepoint in EMOJI:
                table.cmap.pop(codepoint, None)
    result = io.BytesIO()
    font.save(result)
    output = result.getvalue()
    prepared = TTFont(io.BytesIO(output), recalcTimestamp=False, lazy=True)
    assert prepared.getBestCmap() == {
        cp: glyph for cp, glyph in original.items() if cp not in EMOJI
    }
    assert prepared.getGlyphOrder() == glyph_order
    for tag, value in unchanged.items():
        assert prepared.getTableData(tag) == value, f"unexpected change to {tag}"
    assert {
        (record.nameID, record.platformID, record.platEncID, record.langID): record.string
        for record in prepared["name"].names if record.nameID != 10
    } == original_names
    assert all(
        record.toUnicode().startswith(NOTICE)
        for record in prepared["name"].names if record.nameID == 10
    )
    assert prepared["name"].getDebugName(1) == "Roboto"
    assert all(
        not EMOJI.intersection(table.cmap)
        for table in prepared["cmap"].tables if table.isUnicode()
    )
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inputs", type=Path, help="directory with the two original pinned faces")
    parser.add_argument("--output", type=Path, default=ROOT, help="prepared font directory")
    parser.add_argument("--check", action="store_true", help="verify prepared bytes offline without writing")
    parser.add_argument("--offline", action="store_true", help="regenerate from original Git blobs without network")
    args = parser.parse_args()
    for name in INPUTS:
        source = upstream(name, args.inputs, args.check or args.offline)
        output = rebuild(source)
        path = args.output / name
        if args.check:
            assert path.read_bytes() == output, f"regenerate {path}"
        else:
            path.write_bytes(output)
        print(f"{name}: {len(output)} bytes; SHA-256 {hashlib.sha256(output).hexdigest()}")
        if name == "Roboto-Regular.ttf":
            # Android libass has no system-font provider. Its separate,
            # unmodified face must retain the symbols delegated to Noto by UI.
            subtitle = args.output / "Roboto-Subtitles.ttf"
            if args.check:
                assert subtitle.read_bytes() == source, f"restore unmodified {subtitle}"
            else:
                subtitle.write_bytes(source)
            print(f"{subtitle.name}: {len(source)} bytes; SHA-256 {hashlib.sha256(source).hexdigest()}")


if __name__ == "__main__":
    main()
