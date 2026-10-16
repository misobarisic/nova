#!/usr/bin/env python3
"""Rebuild the full COLRv1 font, retaining the previous bitmap font's aliases.

Requires fonttools==4.67.0 and uharfbuzz==0.56.3. Downloads immutable upstream
inputs to memory; never writes the bitmap font into the repository.
"""
import argparse
import hashlib
import io
from itertools import product
from pathlib import Path
from urllib.request import urlopen

from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib import TTFont
from fontTools.ttLib.tables.otTables import Ligature
import uharfbuzz as hb

REVISION = "e20cbc2bbec1926686be9f9bee7d1d2cfa1fea0e"
ROOT = Path(__file__).resolve().parents[1]
INPUTS = {
    "NotoColorEmoji.ttf": "15671215ab769fdc7162a045d56fd7d7e477c51b04e6b3c761d914d8fdd6cc44",
    "Noto-COLRv1.ttf": "b8e25ea68db82f9e4d0aee921f4420be2be39887bd5c893a2ad98710531f9d0c",
}


def upstream(name, directory):
    data = ((directory / name).read_bytes() if directory else
            urlopen(f"https://raw.githubusercontent.com/googlefonts/noto-emoji/{REVISION}/2D/fonts/{name}").read())
    assert hashlib.sha256(data).hexdigest() == INPUTS[name], name
    return data


def shape(font, sequence):
    buffer = hb.Buffer()
    buffer.add_codepoints(list(sequence))
    buffer.guess_segment_properties()
    hb.shape(font, buffer)
    return tuple(info.codepoint for info in buffer.glyph_infos)


def ligature_inputs(font):
    # Every ligature input in this pinned baseline maps directly to Unicode;
    # fail rather than silently losing coverage if upstream changes that.
    inverse = {}
    for cp, glyph in font.getBestCmap().items():
        inverse.setdefault(glyph, []).append(cp)
    sequences = set()
    for lookup in font["GSUB"].table.LookupList.Lookup:
        if lookup.LookupType != 4:
            continue
        for table in lookup.SubTable:
            for first, ligatures in table.ligatures.items():
                for ligature in ligatures:
                    glyphs = [first] + ligature.Component
                    assert all(glyph in inverse for glyph in glyphs)
                    sequences.update(product(*(inverse[glyph] for glyph in glyphs)))
    return sorted(sequences)


def rebuild(directory):
    bitmap = upstream("NotoColorEmoji.ttf", directory)
    vector = upstream("Noto-COLRv1.ttf", directory)
    baseline = TTFont(io.BytesIO(bitmap))
    font = TTFont(io.BytesIO(vector), recalcTimestamp=False)
    bitmap_shaper, vector_shaper = hb.Font(hb.Face(bitmap)), hb.Font(hb.Face(vector))
    sequences = ligature_inputs(baseline)
    assert len(sequences) == 4176
    canonical = {}
    for sequence in sequences:
        old, new = shape(bitmap_shaper, sequence), shape(vector_shaper, sequence)
        assert len(old) == 1 and old[0] != 0
        if len(new) == 1 and new[0] != 0:
            canonical.setdefault(old[0], []).append(sequence)
    aliases = []
    for sequence in sequences:
        output = shape(vector_shaper, sequence)
        if len(output) == 1 and output[0] != 0:
            continue
        candidates = canonical[shape(bitmap_shaper, sequence)[0]]
        # Only add aliases with an unambiguous corresponding vector glyph.
        assert len({shape(vector_shaper, candidate) for candidate in candidates}) == 1
        aliases.append((sequence, min(candidates, key=lambda s: (len(s), s))))
    assert len(aliases) == 1612
    cmap = font.getBestCmap()
    table = font["GSUB"].table.LookupList.Lookup[0].SubTable[0]
    for sequence, equivalent in aliases:
        glyphs = [cmap[cp] for cp in sequence]
        ligature = Ligature()
        ligature.Component = glyphs[1:]
        ligature.CompCount = len(glyphs)
        ligature.LigGlyph = font.getGlyphName(shape(vector_shaper, equivalent)[0])
        table.ligatures.setdefault(glyphs[0], []).append(ligature)
    # Longest match must precede any prefix, just as in upstream's lookup.
    for ligatures in table.ligatures.values():
        ligatures.sort(key=lambda lig: -lig.CompCount)
    for cp, name in [(0, "novaNull"), (13, "novaCarriageReturn")]:
        order = font.getGlyphOrder() + [name]
        font["glyf"][name] = TTGlyphPen(None).glyph()
        font.setGlyphOrder(order)
        old_advance = baseline["hmtx"].metrics[baseline.getBestCmap()[cp]][0]
        advance = old_advance * font["head"].unitsPerEm // baseline["head"].unitsPerEm
        font["hmtx"].metrics[name] = (advance, 0)
        font["vmtx"].metrics[name] = font["vmtx"].metrics["space"]
        for subtable in font["cmap"].tables:
            if subtable.isUnicode() and subtable.format in (4, 12):
                subtable.cmap[cp] = name
    result = io.BytesIO()
    font.save(result)
    data = result.getvalue()
    patched = TTFont(io.BytesIO(data))
    assert set(baseline.getBestCmap()) <= set(patched.getBestCmap())
    assert patched["maxp"].numGlyphs == 42332  # No upstream vector glyph removed.
    shaper = hb.Font(hb.Face(data))
    for sequence in sequences:
        output = shape(shaper, sequence)
        assert len(output) == 1 and output[0] != 0, sequence
    for sequence, equivalent in aliases:
        assert shape(shaper, sequence) == shape(shaper, equivalent), sequence
    # One compact manifest covers every baseline cmap entry and GSUB ligature.
    lines = ["# Full bitmap baseline cmap and GSUB inputs; generated by tools/prepare_emoji.py.",
             "# Hex Unicode scalars; optional semicolon names an equivalent canonical sequence."]
    lines += [f"{cp:04X}" for cp in sorted(baseline.getBestCmap())]
    equivalents = dict(aliases)
    for sequence in sequences:
        row = " ".join(f"{cp:04X}" for cp in sequence)
        if sequence in equivalents:
            row += " ; " + " ".join(f"{cp:04X}" for cp in equivalents[sequence])
        lines.append(row)
    print(f"{len(baseline.getBestCmap())} cmap entries, {len(sequences)} ligatures, {len(aliases)} restored aliases")
    print(f"{len(data)} bytes; SHA-256 {hashlib.sha256(data).hexdigest()}")
    return data, ("\n".join(lines) + "\n").encode()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inputs", type=Path, help="directory containing both pinned upstream inputs")
    parser.add_argument("--output", type=Path, default=ROOT / "NotoColorEmoji.ttf")
    parser.add_argument("--manifest", type=Path, default=ROOT / "emoji-coverage.txt")
    parser.add_argument("--check", action="store_true", help="verify generated files without writing")
    args = parser.parse_args()
    font, manifest = rebuild(args.inputs)
    for path, data in [(args.output, font), (args.manifest, manifest)]:
        if args.check:
            assert path.read_bytes() == data, f"regenerate {path}"
        else:
            path.write_bytes(data)


if __name__ == "__main__":
    main()
