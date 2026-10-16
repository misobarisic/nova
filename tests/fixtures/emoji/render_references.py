#!/usr/bin/env python3
"""Regenerate independent CPU Skia references (skia-python==144.0.post2).

These are test data only. No Skia Python dependency is used by Nova.
"""
import json
from pathlib import Path

import skia

ROOT = Path(__file__).resolve().parent
CASES = [
    ("sweep_0_90", 0xF0202, {}, 0xFF000000),
    ("sweep_end_135", 0xF0202, {"SWPE": 45}, 0xFF000000),
    ("clip_center", 0xF0C04, {}, 0xFF000000),
    ("clip_left_inset", 0xF0C04, {"CLXI": 250}, 0xFF000000),
    ("radial_contained", 0xF0503, {}, 0xFF000000),
    ("radial_radius_varied", 0xF0503, {"GRR0": 250}, 0xFF000000),
    ("composite_src_in", 0xF0A05, {}, 0xFF000000),
    ("radial_both_negative", 0xF0503, {"COL1": -1.5, "COL2": -1.5, "COL3": -1.5}, 0xFF000000),
    ("alpha_gradient", 0xF1000, {"APH2": -1}, 0xFF000000),
    ("foreground_solid", 0xF0B07, {}, 0xFF00FF00),
]


def main():
    assert skia.__version__ == "144.0.post2", skia.__version__
    face = skia.FontMgr.RefDefault().makeFromFile(str(ROOT / "test_glyphs-glyf_colr_1_variable.ttf"))
    manifest = {"renderer": f"skia-python {skia.__version__}", "size": 100,
                "canvas": [128, 128], "origin": [14, 114], "cases": []}
    for name, cp, variation, foreground in CASES:
        coords = skia.FontArguments.VariationPosition.Coordinates([
            skia.FontArguments.VariationPosition.Coordinate(int.from_bytes(tag.encode(), "big"), value)
            for tag, value in variation.items()
        ])
        position = skia.FontArguments.VariationPosition(coords)
        args = skia.FontArguments().setVariationDesignPosition(position)
        varied = face.makeClone(args)
        font = skia.Font(varied, 100)
        font.setHinting(skia.FontHinting.kNone)
        font.setSubpixel(True)
        surface = skia.Surface(128, 128)
        canvas = surface.getCanvas()
        canvas.clear(skia.ColorTRANSPARENT)
        canvas.drawSimpleText(chr(cp), 14, 114, font, skia.Paint(Color=foreground, AntiAlias=True))
        image = surface.makeImageSnapshot()
        path = ROOT / f"skia-{name}.png"
        image.save(str(path), skia.kPNG)
        manifest["cases"].append({"file": path.name, "codepoint": f"{cp:X}",
                                  "glyph": font.unicharToGlyph(cp), "variations": variation,
                                  "foreground_argb": f"{foreground:08X}"})
    (ROOT / "skia-references.json").write_text(json.dumps(manifest, indent=2) + "\n")


if __name__ == "__main__":
    main()
