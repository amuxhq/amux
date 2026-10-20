# /// script
# requires-python = ">=3.11"
# dependencies = ["resvg-py==0.2.6", "pillow==11.3.0"]
# ///
"""Export the SVG master and make a visual review sheet."""

import argparse
from io import BytesIO
import json
from pathlib import Path
import xml.etree.ElementTree as ET

from PIL import Image, ImageDraw, ImageFont
import resvg_py

ROOT = Path(__file__).resolve().parents[1]
SVG = ROOT / "images/brand/amux.svg"
NS = "{http://www.w3.org/2000/svg}"


def render(source, size):
    return Image.open(BytesIO(resvg_py.svg_to_bytes(
        svg_string=source, width=size, height=size))).convert("RGBA")


def monochrome(source):
    root = ET.fromstring(source)
    outline = root.find(f"{NS}defs/{NS}path").attrib["d"]
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="1024" '
            f'height="1024" viewBox="{root.attrib["viewBox"]}">'
            f'<title>amux</title><path fill="currentColor" d="{outline}"/></svg>')


def app_icons(out, source, white):
    """The iPhone app's icon in its three appearances, at the one size the
    asset catalogue compiles every other size from.

    The mark is drawn smaller than its own canvas puts it: the home screen
    rounds the corners off, and the canvas's clear space alone leaves the
    A's feet close to them. The default icon is opaque on black, because the
    App Store rejects an icon with an alpha channel. The dark icon keeps its
    transparency so the system's dark backdrop shows through, and the tinted
    one is the white silhouette the system recolours."""
    def placed(artwork, ground):
        canvas = Image.new("RGBA", (1024, 1024), ground)
        mark = render(artwork, round(1024 * 0.8))
        offset = (1024 - mark.width) // 2
        canvas.alpha_composite(mark, (offset, offset))
        return canvas
    placed(source, "#000000").convert("RGB").save(out / "app-icon.png")
    placed(source, (0, 0, 0, 0)).save(out / "app-icon-dark.png")
    placed(white, (0, 0, 0, 0)).save(out / "app-icon-tinted.png")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=ROOT / "target/brand")
    parser.add_argument("--reference", type=Path,
                        help="Original 1254×1254 raster, compared in the master's viewBox")
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    source = SVG.read_text()
    mono = monochrome(source)
    (args.out / "amux.svg").write_text(source)
    (args.out / "amux-mono.svg").write_text(mono)
    variants = {"amux": source, "amux-black": mono.replace("currentColor", "#000000"),
                "amux-white": mono.replace("currentColor", "#ffffff")}
    for name, artwork in variants.items():
        (args.out / f"{name}.svg").write_text(artwork)
        for size in (16, 24, 32, 48, 64, 128, 256, 512, 1024, 2048):
            render(artwork, size).save(args.out / f"{name}-{size}.png")

    app_icons(args.out, source, variants["amux-white"])

    sheet = Image.new("RGB", (1440, 1160), "#eceef2")
    draw = ImageDraw.Draw(sheet)
    font = ImageFont.load_default(size=20)
    small = ImageFont.load_default(size=15)

    def label(x, y, text, color="#222735", face=font):
        draw.text((x, y), text, fill=color, font=face)

    def panel(x, y, size, background, artwork):
        draw.rectangle((x, y, x + size - 1, y + size - 1), fill=background)
        sheet.paste(artwork, (x, y), artwork if artwork.mode == "RGBA" else None)

    label(32, 24, "AMUX / VECTOR MASTER")
    label(32, 62, "Paper plane, flight trail, ribbon folds and size review", face=small)
    label(32, 108, "Reference" if args.reference else "Monochrome silhouette")
    if args.reference:
        reference = Image.open(args.reference).convert("RGB")
        if reference.size != (1254, 1254):
            raise ValueError("Reference must be the original 1254×1254 image")
        reference = reference.crop((128, 128, 1126, 1126)).resize((440, 440), Image.Resampling.LANCZOS)
        panel(32, 144, 440, "white", reference)
    else:
        panel(32, 144, 440, "white", render(mono, 440))
    label(500, 108, "Vector / rounded tips, open gap")
    panel(500, 144, 440, "white", render(source, 440))
    label(968, 108, "Transparent on dark")
    panel(968, 144, 440, "#151c2b", render(source, 440))

    label(32, 620, "At actual pixel sizes")
    for y, bg, fg in ((665, "#ffffff", "#222735"), (895, "#151c2b", "#ffffff")):
        draw.rounded_rectangle((32, y, 1408, y + 204), radius=12, fill=bg)
        for x, size in zip((70, 190, 320, 460, 630, 840), (16, 24, 32, 48, 64, 128)):
            label(x, y + 18, f"{size}px", fg, small)
            artwork = render(source, size)
            sheet.paste(artwork, (x, y + 61), artwork)
        solid = render(mono.replace("currentColor", fg), 128)
        sheet.paste(solid, (1180, y + 61), solid)
        label(1150, y + 18, "Single colour", fg, small)
    sheet.save(args.out / "review.png")
    if args.reference:
        sheet.crop((16, 96, 952, 600)).save(args.out / "comparison.png")

    sheet = Image.new("RGB", (1024, 1032), "#eceef2")
    draw = ImageDraw.Draw(sheet)
    label(32, 24, "AMUX / MONOCHROME")
    for x, bg, name, title in ((32, "#ffffff", "amux-black", "Black on light"),
                                (528, "#151c2b", "amux-white", "White on dark")):
        label(x, 68, title)
        panel(x, 104, 464, bg, render(variants[name], 464))
    label(32, 600, "At actual pixel sizes")
    for y, bg, fg, name in ((644, "#ffffff", "#222735", "amux-black"),
                             (830, "#151c2b", "#ffffff", "amux-white")):
        draw.rounded_rectangle((32, y, 992, y + 174), radius=12, fill=bg)
        for x, size in zip((70, 190, 320, 460, 630, 820), (16, 24, 32, 48, 64, 128)):
            label(x, y + 14, f"{size}px", fg, small)
            artwork = render(variants[name], size)
            sheet.paste(artwork, (x, y + 40), artwork)
    sheet.save(args.out / "monochrome-review.png")

    full = render(source, 1024)
    alpha = full.getchannel("A")
    # These probes distinguish real negative space from a painted white plane.
    for point in ((0, 0), (513, 483), (523, 571), (553, 719)):
        if alpha.getpixel(point) != 0:
            raise ValueError(f"Expected transparent canvas/plane at {point}")
    if alpha.getpixel((514, 187)) != 255:
        raise ValueError("Crown must be opaque")
    # The swept lower-left leg is a separate ribbon. A bridge here changes
    # the negative-space symbol and closes the reference's central gap.
    mask = render(source, 256).getchannel("A")
    pixels = {(i % 256, i // 256) for i, value in enumerate(mask.getdata()) if value > 127}
    components = 0
    while pixels:
        pending = [pixels.pop()]
        while pending:
            x, y = pending.pop()
            # Diagonal neighbours keep rasterised pointed tips connected.
            for dx, dy in ((-1, -1), (0, -1), (1, -1), (-1, 0),
                           (1, 0), (-1, 1), (0, 1), (1, 1)):
                neighbour = (x + dx, y + dy)
                if neighbour in pixels:
                    pixels.remove(neighbour)
                    pending.append(neighbour)
        components += 1
    if components != 2:
        raise ValueError(f"Expected two disconnected ribbon shapes, found {components}")
    metrics = {"canvas": list(full.size), "artwork_bounds": list(alpha.getbbox()),
               "transparent_plane": True, "ribbon_components": components,
               "embedded_rasters": "<image" in source}
    (args.out / "checks.json").write_text(json.dumps(metrics, indent=2) + "\n")
    print(json.dumps(metrics, indent=2))
    print(f"Review: {args.out / 'review.png'}")


if __name__ == "__main__":
    main()
