# /// script
# requires-python = ">=3.11"
# dependencies = ["pillow==11.3.0", "fonttools==4.59.0", "resvg-py==0.2.6"]
# ///
"""Compose Google Play Store graphics from the launch scene's Android shots.

Produces everything Google Play Console asks for in the same visual language as
the App Store set (`marketing/store.py`):

- Seven portrait phone screenshots (`1.png` .. `7.png`) at 1260x2240 (exact 9:16
  aspect ratio, 24-bit RGB PNG with no alpha, well within Play's 320-3840 px and
  2:1 ratio limits and above the 1080 px featuring bar). Frames 1-3 share one
  continuous dark stage and glowing flight trail as a 3-panel panorama.
- `panorama-preview.png` and `rest-preview.png` contact sheets for review.
- `feature-graphic.png` at 1024x500 (24-bit RGB PNG, no alpha).
- `app-icon-512.png` at 512x512 (32-bit PNG, vector mark at 80% on black).

    uv run marketing/play_store.py [SHOTS_FOLDER]
"""

from __future__ import annotations

import json
import math
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

sys.path.insert(0, str(Path(__file__).resolve().parent))
from compose import INK, MUTED, ROOT, SHOTS, STAGE, fleet_window, mark, rounded, sans  # noqa: E402

OUT = Path.home() / ".local/state/amux-marketing" / ROOT.name / "play-store"
# Exact 9:16 portrait (2240 / 1260 = 16 / 9 = 1.778 <= 2.0), matching Play's
# recommended ratio while keeping the crispness of the iOS frames.
W, H = 1260, 2240
# Pixel 9 screen is 1080x2424; framed height on each card.
PHONE_H = 1480
PHONE_TOP = 670
BEZEL = 34
OUTER_RADIUS = 136
LIFT = 1.30
TRAIL = [(44, 135, 255), (151, 71, 255), (255, 115, 116), (255, 173, 96)]

# Calibrated lift boxes in Pixel 9 pixels (1080x2424) matching the Compose
# element bounds from `android-store-elements.json`.
DEFAULT_LIFTS: dict[str, tuple[int, int, int, int]] = {
    "fleet": (20, 715, 1060, 1095),
    "ask": (32, 1606, 1048, 2335),
    "chat": (24, 1148, 1056, 1334),
    "review": (0, 868, 1080, 1184),
    "new": (36, 428, 1044, 976),
    "host": (42, 696, 1038, 916),
}


def shot(shots: Path, name: str) -> Image.Image:
    return Image.open(shots / f"android-{name}-dark.png").convert("RGBA")


def resolve_terminal_shots(shots: Path) -> Path:
    """Use `fleet-dark.frame.json` from `shots`, falling back to another tree's
    launch images (e.g. `readme`) if this tree only ran `android-store`."""
    if (shots / "fleet-dark.frame.json").is_file():
        return shots
    base = Path.home() / ".local/state/amux-marketing"
    for candidate in sorted(base.glob("*/launch/images/fleet-dark.frame.json")):
        return candidate.parent
    raise FileNotFoundError(
        f"missing fleet-dark.frame.json in {shots}; run `just scene shots` first"
    )


def android_phone(screen: Image.Image) -> Image.Image:
    """A Pixel-style Android phone outline: rounded screen inside a thin bezel
    with a subtle punch-hole camera at the top centre."""
    body = Image.new("RGBA", (screen.width + 2 * BEZEL, screen.height + 2 * BEZEL), (0, 0, 0, 0))
    draw = ImageDraw.Draw(body)
    draw.rounded_rectangle([0, 0, body.width - 1, body.height - 1], OUTER_RADIUS, fill="#0b0b0c")
    draw.rounded_rectangle(
        [3, 3, body.width - 4, body.height - 4],
        OUTER_RADIUS - 3,
        outline="#3a3a3e",
        width=4,
    )
    body.alpha_composite(rounded(screen, OUTER_RADIUS - BEZEL), (BEZEL, BEZEL))
    # Subtle Pixel punch-hole camera centered in the status bar.
    cam_r = 22
    cx = body.width // 2
    cy = BEZEL + 58
    draw.ellipse([cx - cam_r, cy - cam_r, cx + cam_r, cy + cam_r], fill="#060608")
    return body


def device(screen: Image.Image, height: int = PHONE_H) -> tuple[Image.Image, float]:
    framed = android_phone(screen)
    scale = height / framed.height
    framed = framed.resize((round(framed.width * scale), height), Image.LANCZOS)
    return framed.filter(ImageFilter.UnsharpMask(radius=0.8, percent=50, threshold=0)), scale


def lifted(
    screen: Image.Image,
    box: tuple[int, int, int, int],
    scale: float,
    radius: int = 44,
    lift: float = LIFT,
) -> Image.Image:
    x0, y0, x1, y1 = box
    x0 = max(0, min(screen.width - 2, x0))
    y0 = max(0, min(screen.height - 2, y0))
    x1 = max(x0 + 2, min(screen.width, x1))
    y1 = max(y0 + 2, min(screen.height, y1))
    part = screen.crop((x0, y0, x1, y1))
    size = scale * lift
    part = part.resize((round(part.width * size), round(part.height * size)), Image.LANCZOS)
    r = max(12, round(radius * size))
    part = rounded(part.convert("RGB"), r)
    ImageDraw.Draw(part).rounded_rectangle(
        [0, 0, part.width - 1, part.height - 1],
        r,
        outline=(255, 255, 255, 42),
        width=2,
    )
    return part


def lay(canvas: Image.Image, layer: Image.Image, at: tuple[int, int], depth: int = 34) -> None:
    shadow = Image.new("RGBA", canvas.size, (0, 0, 0, 0))
    shadow.paste((0, 0, 0, 225), (at[0], at[1] + depth), layer.getchannel("A"))
    canvas.alpha_composite(shadow.filter(ImageFilter.GaussianBlur(depth * 1.35)))
    canvas.alpha_composite(layer, at)


def ground(width: int, height: int, glows: list[tuple[int, int, int, tuple]]) -> Image.Image:
    canvas = Image.new("RGBA", (width, height), STAGE + (255,))
    light = Image.new("RGBA", canvas.size, (0, 0, 0, 0))
    draw = ImageDraw.Draw(light)
    for x, y, r, colour in glows:
        draw.ellipse([x - r, y - r, x + r, y + r], fill=colour)
    canvas.alpha_composite(light.filter(ImageFilter.GaussianBlur(240)))
    return canvas


def colour_at(t: float) -> tuple[int, int, int]:
    t = min(max(t, 0.0), 1.0) * (len(TRAIL) - 1)
    i = min(int(t), len(TRAIL) - 2)
    f = t - i
    return tuple(round(a + (b - a) * f) for a, b in zip(TRAIL[i], TRAIL[i + 1]))


def trail(canvas: Image.Image, points: list[tuple[float, float]], width: float) -> None:
    path = []
    for i in range(len(points) - 1):
        p0 = points[max(i - 1, 0)]
        p1, p2 = points[i], points[i + 1]
        p3 = points[min(i + 2, len(points) - 1)]
        for s in range(60):
            t = s / 60
            path.append(tuple(
                0.5 * (
                    2 * b
                    + (-a + c) * t
                    + (2 * a - 5 * b + 4 * c - d) * t * t
                    + (-a + 3 * b - 3 * c + d) * t ** 3
                )
                for a, b, c, d in zip(p0, p1, p2, p3)
            ))
    path.append(points[-1])
    layer = Image.new("RGBA", canvas.size, (0, 0, 0, 0))
    draw = ImageDraw.Draw(layer)
    for i, (x, y) in enumerate(path):
        t = i / (len(path) - 1)
        r = width / 2 * (0.25 + 0.75 * math.sin(math.pi * t))
        draw.ellipse([x - r, y - r, x + r, y + r], fill=colour_at(t) + (255,))
    glow = layer.filter(ImageFilter.GaussianBlur(64))
    glow.putalpha(glow.getchannel("A").point(lambda a: a * 3 // 5))
    canvas.alpha_composite(glow)
    canvas.alpha_composite(layer)


def headline(canvas: Image.Image, centre: int, lines: list[str], sub: str | list[str] | None) -> None:
    draw = ImageDraw.Draw(canvas)
    font = sans(100, 640)
    for line in lines:
        assert draw.textlength(line, font=font) <= W - 120, f"too long for a frame: {line!r}"
    y = 260
    for line in lines:
        draw.text((centre, y), line, font=font, fill=INK, anchor="ms")
        y += 112
    for i, words in enumerate([sub] if isinstance(sub, str) else sub or []):
        draw.text((centre, y + 12 + i * 58), words, font=sans(46, 470), fill=MUTED, anchor="ms")


def android_lock_screen(size: tuple[int, int], status: Image.Image) -> Image.Image:
    """An Android Pixel-style lock screen with amux's push notification."""
    width, height = size
    screen = Image.new("RGBA", size, (10, 12, 20, 255))
    light = Image.new("RGBA", size, (0, 0, 0, 0))
    draw = ImageDraw.Draw(light)
    draw.ellipse([-450, 1150, 980, 2600], fill=(44, 135, 255, 120))
    draw.ellipse([260, 1550, 1520, 2850], fill=(151, 71, 255, 110))
    draw.ellipse([620, 1900, 1520, 2850], fill=(255, 115, 116, 70))
    screen.alpha_composite(light.filter(ImageFilter.GaussianBlur(260)))

    # Keep the Pixel's real right-side status icons (5G, Wi-Fi, battery).
    bar_x = int(width * 0.62)
    bar_h = 132
    bar_luma = status.crop((bar_x, 0, width, bar_h)).convert("L")
    bar_ink = status.crop((bar_x, 0, width, bar_h))
    screen.paste(bar_ink, (bar_x, 0), bar_luma.point(lambda v: min(255, v * 2)))

    draw = ImageDraw.Draw(screen)
    draw.text((width // 2, 310), "Sun, 25 Oct", font=sans(56, 520), fill=(230, 234, 244), anchor="ms")
    draw.text((width // 2, 600), "9:41", font=sans(290, 650), fill=(242, 244, 250), anchor="ms")

    # Material 3 dark notification card.
    left, top, right = 42, 770, width - 42
    bottom = top + 276
    card = Image.new("RGBA", size, (0, 0, 0, 0))
    ImageDraw.Draw(card).rounded_rectangle(
        [left, top, right, bottom],
        58,
        fill=(32, 36, 46, 224),
        outline=(255, 255, 255, 26),
        width=2,
    )
    screen.alpha_composite(card)

    # Round amux app badge.
    badge_size = 104
    badge = Image.new("RGBA", (badge_size, badge_size), (0, 0, 0, 0))
    ImageDraw.Draw(badge).ellipse([0, 0, badge_size - 1, badge_size - 1], fill=(10, 11, 14, 255))
    m = mark(round(badge_size * 0.72))
    badge.alpha_composite(m, ((badge_size - m.width) // 2, (badge_size - m.height) // 2))
    screen.alpha_composite(badge, (left + 38, top + (bottom - top - badge_size) // 2))

    text_x = left + 38 + badge_size + 32
    draw.text((text_x, top + 98), "flaky-sync-test", font=sans(42, 620), fill=(248, 249, 252), anchor="ls")
    draw.text((right - 42, top + 98), "now", font=sans(34, 450), fill=(185, 190, 202), anchor="rs")
    draw.text((text_x, top + 158), "Wants to run", font=sans(40, 440), fill=(224, 227, 235), anchor="ls")
    draw.text((text_x, top + 214), "cargo test -p sync --release", font=sans(38, 440), fill=(210, 215, 226), anchor="ls")

    # Android gesture navigation pill.
    draw.rounded_rectangle([width // 2 - 170, height - 42, width // 2 + 170, height - 30], 6, fill=(235, 238, 245))
    return screen


def held(
    canvas: Image.Image,
    frame: int,
    screen: Image.Image,
    lift: tuple[int, int, int, int] | None = None,
    lift_from: Image.Image | None = None,
    radius: int = 44,
    nudge: tuple[int, int] = (0, 0),
    larger: float = LIFT,
) -> None:
    framed, scale = device(screen)
    x = frame * W + (W - framed.width) // 2
    lay(canvas, framed, (x, PHONE_TOP))
    if lift is None:
        return
    part = lifted(lift_from or screen, lift, scale, radius, larger)
    cx = x + (BEZEL + (lift[0] + lift[2]) / 2) * scale
    cy = PHONE_TOP + (BEZEL + (lift[1] + lift[3]) / 2) * scale
    left = round(cx - part.width / 2) + nudge[0]
    left = min(max(left, frame * W + 44), (frame + 1) * W - 44 - part.width)
    top = round(cy - part.height / 2) + nudge[1]
    top = min(max(top, PHONE_TOP - 80), H - 36 - part.height)
    lay(canvas, part, (left, top))


def load_lifts(shots: Path) -> dict[str, tuple[int, int, int, int]]:
    lifts = dict(DEFAULT_LIFTS)
    overrides_path = shots / "android-lifts.json"
    if overrides_path.is_file():
        raw = json.loads(overrides_path.read_text())
        for k, v in raw.items():
            if isinstance(v, list) and len(v) == 4:
                lifts[k] = tuple(int(n) for n in v)
    return lifts


def panorama(shots: Path, lifts: dict[str, tuple[int, int, int, int]]) -> list[Image.Image]:
    canvas = ground(3 * W, H, [
        (280, 1800, 620, (44, 135, 255, 70)),
        (W + 640, 1180, 720, (151, 71, 255, 60)),
        (2 * W + 860, 760, 720, (255, 115, 116, 55)),
    ])
    trail(
        canvas,
        [(-120, 1960), (480, 1480), (1260, 1280), (2000, 1680), (2580, 1180), (3240, 720), (3920, 400)],
        64,
    )

    fleet = shot(shots, "fleet")
    ask = shot(shots, "ask")
    chat = shot(shots, "chat")

    held(canvas, 0, fleet, lifts["fleet"], lift_from=fleet, radius=40)
    headline(canvas, W // 2, ["Every agent.", "Every machine."], "Claude Code and Codex, in your pocket.")

    held(canvas, 1, android_lock_screen(fleet.size, fleet), lifts["ask"], lift_from=ask, radius=56, nudge=(0, 24), larger=1.24)
    headline(canvas, W + W // 2, ["Never leave an", "agent waiting."], None)

    held(canvas, 2, chat, lifts["chat"], radius=34, larger=1.42)
    headline(canvas, 2 * W + W // 2, ["Claude and Codex,", "working together."], None)

    return [canvas.crop((i * W, 0, (i + 1) * W, H)).convert("RGB") for i in range(3)]


def single(
    screen: Image.Image,
    lines: list[str],
    sub: str | list[str] | None,
    glow: tuple,
    path: list,
    lift: tuple[int, int, int, int] | None = None,
    radius: int = 44,
    nudge: tuple[int, int] = (0, 0),
    larger: float = LIFT,
) -> Image.Image:
    canvas = ground(W, H, [glow])
    trail(canvas, path, 56)
    held(canvas, 0, screen, lift, radius=radius, nudge=nudge, larger=larger)
    headline(canvas, W // 2, lines, sub)
    return canvas.convert("RGB")


def install(shots: Path) -> Image.Image:
    canvas = ground(W, H, [(W // 2, 1340, 800, (44, 135, 255, 60))])
    trail(canvas, [(-180, 1580), (480, 1180), (1440, 860)], 56)
    headline(canvas, W // 2, ["Free. Works with", "amux on your Mac", "or Linux machine."], None)

    draw = ImageDraw.Draw(canvas)
    command = "curl -fsSL https://amux.sh/install | sh"
    mono = ImageFont.truetype(str(ROOT / "crates/shot/assets/JetBrainsMono-Regular.ttf"), 38)
    width = draw.textlength(command, font=mono)
    pill = Image.new("RGBA", canvas.size, (0, 0, 0, 0))
    ImageDraw.Draw(pill).rounded_rectangle(
        [W // 2 - width / 2 - 40, 560, W // 2 + width / 2 + 40, 650],
        24,
        fill=(255, 255, 255, 14),
        outline=(255, 255, 255, 34),
        width=2,
    )
    canvas.alpha_composite(pill)
    draw.text((W // 2, 619), command, font=mono, fill=(223, 227, 232), anchor="ms")

    term_shots = resolve_terminal_shots(shots)
    terminal = fleet_window(term_shots, 64, 23)
    tw = W - 110
    terminal = terminal.resize((tw, round(terminal.height * tw / terminal.width)), Image.LANCZOS)
    lay(canvas, terminal, (55, 760))

    framed, _ = device(shot(shots, "fleet"), height=1160)
    lay(canvas, framed, (W - framed.width - 64, 1020))
    return canvas.convert("RGB")


def feature_graphic(shots: Path) -> Image.Image:
    """Google Play mandatory 1024x500 feature graphic (24-bit RGB, no alpha)."""
    fw, fh = 1024, 500
    card = ground(fw, fh, [
        (180, 260, 360, (44, 135, 255, 55)),
        (680, 140, 320, (151, 71, 255, 45)),
        (940, 380, 280, (255, 115, 116, 40)),
    ])
    trail(card, [(-60, 440), (260, 380), (540, 290), (820, 170), (1100, 90)], 22)

    term_shots = resolve_terminal_shots(shots)
    terminal = fleet_window(term_shots, 56, 17)
    terminal = terminal.resize((round(terminal.width * 0.42), round(terminal.height * 0.42)), Image.LANCZOS)
    lay(card, terminal, (466, 72), depth=16)

    handset, _ = device(shot(shots, "ask"), height=432)
    lay(card, handset, (fw - 44 - handset.width, 34), depth=18)

    card.alpha_composite(mark(108), (52, 86))
    draw = ImageDraw.Draw(card)
    draw.text((178, 174), "amux", font=sans(94, 640), fill=INK, anchor="ls")
    for i, line in enumerate(["Every agent.", "Every machine.", "In your pocket."]):
        draw.text((58, 264 + i * 48), line, font=sans(36, 480), fill=MUTED, anchor="ls")
    return card.convert("RGB")


def app_icon_512() -> Image.Image:
    """Google Play 512x512 high-res icon: vector mark at 80% on opaque black."""
    canvas = Image.new("RGBA", (512, 512), "#000000")
    m = mark(round(512 * 0.8))
    offset = (512 - m.width) // 2
    canvas.alpha_composite(m, (offset, offset))
    return canvas


def main(arguments: list[str]) -> int:
    shots = Path(arguments[0]) if arguments else SHOTS
    OUT.mkdir(parents=True, exist_ok=True)
    lifts = load_lifts(shots)

    frames = panorama(shots, lifts)
    frames.append(single(
        shot(shots, "review"),
        ["Review the diff.", "Comment on the line."],
        None,
        (W // 2, 1280, 720, (44, 135, 255, 60)),
        [(-180, 1020), (580, 1340), (1440, 1100)],
        lifts["review"],
        radius=34,
    ))
    frames.append(single(
        shot(shots, "new"),
        ["Start an agent", "anywhere."],
        None,
        (W // 2, 1200, 720, (151, 71, 255, 55)),
        [(-180, 1720), (660, 1400), (1440, 1660)],
        lifts["new"],
        radius=34,
    ))
    frames.append(single(
        shot(shots, "host"),
        ["Private by design."],
        ["Your code stays on your machines.", "End-to-end encrypted."],
        (W // 2, 1200, 720, (255, 115, 116, 45)),
        [(-180, 820), (740, 1200), (1440, 1040)],
        lifts["host"],
        radius=34,
    ))
    frames.append(install(shots))

    for i, frame in enumerate(frames, 1):
        out = OUT / f"{i}.png"
        frame.save(out, optimize=True)
        print(f"play-store: {out} {frame.width}x{frame.height}")

    # Review contact sheets (matching iOS store.py).
    gap = 48
    pano = Image.new("RGB", (3 * W + 2 * gap, H), (255, 255, 255))
    for i in range(3):
        pano.paste(frames[i], (i * (W + gap), 0))
    pano.resize((pano.width // 4, pano.height // 4), Image.LANCZOS).save(OUT / "panorama-preview.png")

    rest = Image.new("RGB", (4 * W + 3 * gap, H), (255, 255, 255))
    for i in range(4):
        rest.paste(frames[3 + i], (i * (W + gap), 0))
    rest.resize((rest.width // 4, rest.height // 4), Image.LANCZOS).save(OUT / "rest-preview.png")

    fg = feature_graphic(shots)
    fg.save(OUT / "feature-graphic.png", optimize=True)
    print(f"play-store: {OUT / 'feature-graphic.png'} {fg.width}x{fg.height}")

    icon = app_icon_512()
    icon.save(OUT / "app-icon-512.png", optimize=True)
    print(f"play-store: {OUT / 'app-icon-512.png'} {icon.width}x{icon.height}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
