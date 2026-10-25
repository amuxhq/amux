# /// script
# requires-python = ">=3.11"
# dependencies = ["pillow==11.3.0", "fonttools==4.59.0", "resvg-py==0.2.6"]
# ///
"""Compose the README's pictures from the scene's shots.

`just scene shots` leaves each terminal screen as a frame (`*.frame.json`,
every cell's text and colours) and each phone screen as a PNG. This script
redraws the frames at twice the terminal renderer's size, so they stay sharp
on high-density screens, puts each in a window, puts the phone screens in a
device outline, and writes the README's images, light and dark, to
`images/readme/`. Two are drawn once for both: the agents' own interfaces
side by side, and amux's chat in three terminals' colours, which shows
light and dark itself. Last, the cards for a shared link to the repository
and for an X profile's header.

    uv run marketing/compose.py [SHOTS_FOLDER]
"""

from __future__ import annotations

from io import BytesIO
import json
import sys
from pathlib import Path

from fontTools.ttLib import TTFont
from PIL import Image, ImageDraw, ImageFilter, ImageFont
import resvg_py

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
ASSETS = ROOT / "crates/shot/assets"
OUT = ROOT / "images/readme"
SHOTS = Path.home() / ".local/state/amux-marketing" / ROOT.name / "launch/images"

# The terminal renderer's cell is 10x22 at 16px; this is the same at 2x.
SCALE = 2
CELL_W, CELL_H = 10 * SCALE, 22 * SCALE
FONT_SIZE = 16 * SCALE
BASELINE = 17 * SCALE

FONTS = {
    (False, False): ImageFont.truetype(str(ASSETS / "JetBrainsMono-Regular.ttf"), FONT_SIZE),
    (True, False): ImageFont.truetype(str(ASSETS / "JetBrainsMono-Bold.ttf"), FONT_SIZE),
    (False, True): ImageFont.truetype(str(ASSETS / "JetBrainsMono-Italic.ttf"), FONT_SIZE),
    (True, True): ImageFont.truetype(str(ASSETS / "JetBrainsMono-BoldItalic.ttf"), FONT_SIZE),
}
SYMBOLS = ImageFont.truetype(str(ASSETS / "DejaVuSans.ttf"), FONT_SIZE)
MONO_GLYPHS = set(TTFont(str(ASSETS / "JetBrainsMono-Regular.ttf")).getBestCmap())

# The window and device around the screens, per theme.
CHROME = {
    "dark": {"bar": "#1b1e25", "edge": "#2c303a", "title": "#868b96", "shadow": 110},
    "light": {"bar": "#ececea", "edge": "#d6d8dc", "title": "#6a7080", "shadow": 55},
}
LIGHTS = ["#ff5f57", "#febc2e", "#28c840"]
# The pictures drawn once sit on both of GitHub's pages: a shadow between.
SHADOW_BOTH = 80
# amux's chat in each terminal, back to front, with the window's title.
THEMES = [
    ("gruvbox-dark", "amux — Gruvbox Dark"),
    ("solarized-light", "amux — Solarized Light"),
    ("catppuccin-mocha", "amux — Catppuccin Mocha"),
]


def rgb(value: str) -> tuple[int, int, int]:
    value = value.lstrip("#")
    return tuple(int(value[i:i + 2], 16) for i in (0, 2, 4))


def mix(a: tuple, b: tuple, t: float) -> tuple:
    return tuple(round(x + (y - x) * t) for x, y in zip(a, b))


def render(frame_path: Path) -> Image.Image:
    """A terminal frame drawn cell by cell, as crates/shot draws it."""
    frame = json.loads(frame_path.read_text())
    default_fg, default_bg = rgb(frame["default"]["fg"]), rgb(frame["default"]["bg"])
    image = Image.new("RGB", (frame["width"] * CELL_W, frame["height"] * CELL_H), default_bg)
    # The terminal's own ground, which a program may paint over at any cell.
    image.info["ground"] = default_bg
    draw = ImageDraw.Draw(image)
    for y, runs in enumerate(frame["rows"][: frame["height"]]):
        top = y * CELL_H
        for run in runs:
            fg = default_fg if run["fg"] == "default" else rgb(run["fg"])
            bg = default_bg if run["bg"] == "default" else rgb(run["bg"])
            styles = set(run.get("styles", []))
            if "reversed" in styles:
                fg, bg = bg, fg
            if "dim" in styles:
                fg = mix(fg, bg, 0.5)
            font = FONTS[("bold" in styles, "italic" in styles)]
            for i, char in enumerate(run["text"]):
                left = (run["x"] + i) * CELL_W
                draw.rectangle([left, top, left + CELL_W - 1, top + CELL_H - 1], fill=bg)
                if char == " ":
                    continue
                if char in "█▀▄▌":
                    box = {
                        "█": (0, 0, CELL_W, CELL_H), "▀": (0, 0, CELL_W, CELL_H // 2),
                        "▄": (0, CELL_H // 2, CELL_W, CELL_H), "▌": (0, 0, CELL_W // 2, CELL_H),
                    }[char]
                    draw.rectangle([left + box[0], top + box[1], left + box[2] - 1, top + box[3] - 1], fill=fg)
                    continue
                if char in "─│╭╮╯╰":
                    line(draw, char, left, top, fg)
                    continue
                if char in "⏸⏵":
                    # Media marks Claude Code draws, which neither font has.
                    mid, h = top + CELL_H // 2, CELL_H // 4
                    if char == "⏸":
                        for x in (left + CELL_W * 3 // 10, left + CELL_W * 6 // 10):
                            draw.rectangle([x, mid - h, x + CELL_W // 6, mid + h], fill=fg)
                    else:
                        draw.polygon([(left + CELL_W // 4, mid - h), (left + CELL_W // 4, mid + h),
                                      (left + CELL_W * 3 // 4, mid)], fill=fg)
                    continue
                face = font if ord(char) in MONO_GLYPHS else SYMBOLS
                draw.text((left, top + BASELINE), char, font=face, fill=fg, anchor="ls")
            if "underlined" in styles:
                width = len(run["text"]) * CELL_W
                left = run["x"] * CELL_W
                draw.rectangle([left, top + BASELINE + 3, left + width - 1, top + BASELINE + 4], fill=fg)
    return image


def line(draw: ImageDraw.ImageDraw, char: str, left: int, top: int, fg: tuple) -> None:
    """Box-drawing lines through the middle of the cell, so rules and the
    composer's edge join across cells with no seams."""
    cx, cy, w = left + CELL_W // 2, top + CELL_H // 2, SCALE
    right, bottom = left + CELL_W - 1, top + CELL_H - 1
    if char == "─":
        draw.rectangle([left, cy, right, cy + w - 1], fill=fg)
    elif char == "│":
        draw.rectangle([cx, top, cx + w - 1, bottom], fill=fg)
    else:
        r = CELL_W // 2 - 1
        box = {
            "╭": (cx, cy, cx + 2 * r, cy + 2 * r, 180, 270),
            "╮": (cx - 2 * r, cy, cx, cy + 2 * r, 270, 360),
            "╯": (cx - 2 * r, cy - 2 * r, cx, cy, 0, 90),
            "╰": (cx, cy - 2 * r, cx + 2 * r, cy, 90, 180),
        }[char]
        draw.arc(box[:4], box[4], box[5], fill=fg, width=w)
        if char in "╭╮":
            draw.rectangle([cx, cy + r, cx + w - 1, bottom], fill=fg)
        else:
            draw.rectangle([cx, top, cx + w - 1, cy - r], fill=fg)
        if char in "╭╰":
            draw.rectangle([cx + r, cy, right, cy + w - 1], fill=fg)
        else:
            draw.rectangle([left, cy, cx - r, cy + w - 1], fill=fg)


def rounded(image: Image.Image, radius: int) -> Image.Image:
    mask = Image.new("L", image.size, 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, image.width - 1, image.height - 1], radius, fill=255)
    out = image.convert("RGBA")
    out.putalpha(mask)
    return out


def chrome_of(screen: Image.Image) -> dict:
    """A window's bar, edge and title in a terminal's own colours: its
    ground moved a little toward the light or the dark."""
    ground = screen.info["ground"]
    dark = sum(ground) / 3 < 128
    toward = (255, 255, 255) if dark else (0, 0, 0)
    hexed = lambda c: "#%02x%02x%02x" % c
    return {"bar": hexed(mix(ground, toward, 0.06)), "edge": hexed(mix(ground, toward, 0.16)),
            "title": hexed(mix(ground, toward, 0.5))}


def window(screen: Image.Image, theme: str, title: str) -> Image.Image:
    """A terminal window: a title bar with its three lights, then the screen.
    `theme` is "dark" or "light", or "own" for the screen's own colours."""
    chrome = chrome_of(screen) if theme == "own" else CHROME[theme]
    bar = 34 * SCALE
    pad = 6 * SCALE
    body = Image.new("RGB", (screen.width + 2 * pad, screen.height + bar + pad), screen.info["ground"])
    draw = ImageDraw.Draw(body)
    draw.rectangle([0, 0, body.width, bar - 1], fill=chrome["bar"])
    for i, colour in enumerate(LIGHTS):
        cx, cy, r = (20 + i * 20) * SCALE, bar // 2, 6 * SCALE
        draw.ellipse([cx - r, cy - r, cx + r, cy + r], fill=colour)
    font = ImageFont.truetype(str(ASSETS / "JetBrainsMono-Regular.ttf"), 13 * SCALE)
    draw.text((body.width // 2, bar // 2), title, font=font, fill=chrome["title"], anchor="mm")
    body.paste(screen, (pad, bar))
    framed = rounded(body, 12 * SCALE)
    edge = ImageDraw.Draw(framed)
    edge.rounded_rectangle([0, 0, framed.width - 1, framed.height - 1], 12 * SCALE, outline=chrome["edge"], width=SCALE)
    return framed


def phone(screen: Image.Image, theme: str) -> Image.Image:
    """A phone outline: the screen's own rounded corners inside a thin bezel."""
    bezel = 40
    body = Image.new("RGBA", (screen.width + 2 * bezel, screen.height + 2 * bezel), (0, 0, 0, 0))
    draw = ImageDraw.Draw(body)
    outer = 200
    draw.rounded_rectangle([0, 0, body.width - 1, body.height - 1], outer, fill="#0b0b0c")
    draw.rounded_rectangle([3, 3, body.width - 4, body.height - 4], outer - 3,
                           outline="#3a3a3e" if theme == "dark" else "#55565b", width=4)
    body.alpha_composite(rounded(screen, outer - bezel), (bezel, bezel))
    return body


def shadowed(layers: list[tuple[Image.Image, tuple[int, int]]], theme: str, margin: int = 140) -> Image.Image:
    """The layers on a transparent canvas, each over a soft shadow, so the
    picture sits on GitHub's light and dark pages alike. `theme` "both" is
    for a picture shown on either."""
    strength = SHADOW_BOTH if theme == "both" else CHROME[theme]["shadow"]
    width = max(x + layer.width for layer, (x, _) in layers) + 2 * margin
    height = max(y + layer.height for layer, (_, y) in layers) + 2 * margin
    canvas = Image.new("RGBA", (width, height), (0, 0, 0, 0))
    for layer, (x, y) in layers:
        shadow = Image.new("RGBA", canvas.size, (0, 0, 0, 0))
        alpha = layer.getchannel("A").point(lambda a: a * strength // 255)
        shadow.paste((0, 0, 0, 255), (margin + x, margin + y + 30), alpha)
        canvas.alpha_composite(shadow.filter(ImageFilter.GaussianBlur(50)))
        canvas.alpha_composite(layer, (margin + x, margin + y))
    return canvas


def save(image: Image.Image, name: str, width: int) -> None:
    if image.width > width:
        image = image.resize((width, round(image.height * width / image.width)), Image.LANCZOS)
    OUT.mkdir(parents=True, exist_ok=True)
    image.save(OUT / f"{name}.png", optimize=True)
    print(f"compose: images/readme/{name}.png {image.width}x{image.height}")


# The cards for links and profiles: drawn on the film's stage, in its type.
STAGE = (7, 8, 11)
INK, MUTED = (242, 244, 247), (139, 145, 156)


def sans(size: int, weight: int) -> ImageFont.FreeTypeFont:
    font = ImageFont.truetype(str(HERE / "fonts/InstrumentSans.ttf"), size)
    font.set_variation_by_axes([100, weight])
    return font


def mark(size: int) -> Image.Image:
    svg = (ROOT / "images/brand/amux.svg").read_text()
    return Image.open(BytesIO(resvg_py.svg_to_bytes(svg_string=svg, width=size, height=size))).convert("RGBA")


def stage(size: tuple[int, int], glow: tuple[int, int, int, int]) -> Image.Image:
    """The dark ground with a faint blue light behind the words."""
    card = Image.new("RGBA", size, STAGE + (255,))
    light = Image.new("RGBA", size, (0, 0, 0, 0))
    ImageDraw.Draw(light).ellipse(glow, fill=(44, 135, 255, 30))
    card.alpha_composite(light.filter(ImageFilter.GaussianBlur(120)))
    return card


def fleet_window(shots: Path, columns: int, rows: int) -> Image.Image:
    """The fleet's top-left corner in its window, at 2x. What is cut away
    sits behind the phone or below the rows that matter."""
    frame = render(shots / "fleet-dark.frame.json").crop((0, 0, columns * CELL_W, rows * CELL_H))
    frame.info["ground"] = frame.getpixel((0, 0))
    return window(frame, "dark", "amux — studio")


def fleet_at_half(shots: Path, columns: int, rows: int) -> Image.Image:
    """The fleet's corner halved exactly, so its text is as sharp as the
    terminal's own at 1x."""
    framed = fleet_window(shots, columns, rows)
    return framed.resize((framed.width // 2, framed.height // 2), Image.LANCZOS)


def phone_at(shots: Path, height: int) -> Image.Image:
    """The request on the phone, shrunk once from the simulator's pixels."""
    handset = phone(Image.open(shots / "phone-ask-dark.png").convert("RGBA"), "dark")
    handset = handset.resize((round(handset.width * height / handset.height), height), Image.LANCZOS)
    return handset.filter(ImageFilter.UnsharpMask(radius=0.6, percent=60, threshold=0))


def lay(card: Image.Image, layer: Image.Image, at: tuple[int, int]) -> None:
    shadow = Image.new("RGBA", card.size, (0, 0, 0, 0))
    shadow.paste((0, 0, 0, 200), (at[0], at[1] + 16), layer.getchannel("A"))
    card.alpha_composite(shadow.filter(ImageFilter.GaussianBlur(28)))
    card.alpha_composite(layer, at)


def social(shots: Path) -> Image.Image:
    """The card a shared link to the repository shows, at GitHub's 1280x640.
    It is usually seen far smaller, so it carries the name and the promise
    in large type and nothing small enough to need reading."""
    card = stage((1280, 640), (-200, 40, 760, 600))
    lay(card, fleet_at_half(shots, 70, 23), (452, 58))
    handset = phone_at(shots, 556)
    lay(card, handset, (1280 - 52 - handset.width, 42))
    card.alpha_composite(mark(148), (62, 108))
    draw = ImageDraw.Draw(card)
    draw.text((72, 360), "amux", font=sans(112, 620), fill=INK, anchor="ls")
    for i, text in enumerate(["Your agents,", "everywhere,", "all at once."]):
        draw.text((74, 424 + i * 48), text, font=sans(42, 470), fill=MUTED, anchor="ls")
    return card.convert("RGB")


def banner(shots: Path) -> Image.Image:
    """An X profile's header, 1500x500. The profile picture covers the
    bottom-left corner, so the words keep to the top-left and the
    pictures to the right."""
    card = stage((1500, 500), (-160, -60, 820, 420))
    lay(card, fleet_at_half(shots, 52, 14), (836, 86))
    handset = phone_at(shots, 428)
    lay(card, handset, (1500 - 64 - handset.width, 36))
    card.alpha_composite(mark(104), (92, 92))
    draw = ImageDraw.Draw(card)
    draw.text((218, 172), "amux", font=sans(100, 620), fill=INK, anchor="ls")
    draw.text((100, 246), "Your agents, everywhere, all at once.", font=sans(36, 470), fill=MUTED, anchor="ls")
    return card.convert("RGB")


def main(arguments: list[str]) -> int:
    shots = Path(arguments[0]) if arguments else SHOTS
    for theme in ("dark", "light"):
        def term(name: str, title: str) -> Image.Image:
            return window(render(shots / f"{name}-{theme}.frame.json"), theme, title)

        def handset(name: str) -> Image.Image:
            return phone(Image.open(shots / f"phone-{name}-{theme}.png").convert("RGBA"), theme)

        fleet = term("fleet", "amux — studio")
        ask = handset("ask")
        ask = ask.resize((round(ask.width * fleet.height * 1.02 / ask.height), round(fleet.height * 1.02)), Image.LANCZOS)
        save(shadowed([(fleet, (0, 0)), (ask, (fleet.width - ask.width * 2 // 5, fleet.height // 6))], theme),
             f"hero-{theme}", 2400)
        save(shadowed([(term("review", "amux — studio"), (0, 0))], theme), f"review-{theme}", 2000)
        save(shadowed([(term("chat", "amux — studio"), (0, 0))], theme), f"chat-{theme}", 2000)
        pair_fleet, pair_ask = handset("fleet"), handset("ask")
        gap = 120
        save(shadowed([(pair_fleet, (0, 0)), (pair_ask, (pair_fleet.width + gap, 0))], theme),
             f"phone-{theme}", 1800)
    # From `scene native`, which runs the real programs: kept until it is
    # run again.
    claude, codex = shots / "native-claude_pty.frame.json", shots / "native-codex.frame.json"
    if claude.exists() and codex.exists():
        left = window(render(claude), "own", "amux attach error-copy")
        right = window(render(codex), "own", "amux attach ledger-race")
        save(shadowed([(left, (0, 0)), (right, (left.width + 80, 0))], "both"), "native", 2400)
    else:
        print("compose: no native shots; `just scene native` takes them", file=sys.stderr)
    windows = [window(render(shots / f"theme-{name}.frame.json"), "own", title) for name, title in THEMES]
    step_x, step_y = windows[0].width * 26 // 100, windows[0].height * 18 // 100
    save(shadowed([(w, (i * step_x, i * step_y)) for i, w in enumerate(windows)], "both"), "themes", 2400)
    save(social(shots), "social", 1280)
    save(banner(shots), "banner", 1500)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
