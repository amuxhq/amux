# /// script
# requires-python = ">=3.11"
# dependencies = ["pillow==11.3.0", "fonttools==4.59.0", "resvg-py==0.2.6"]
# ///
"""Compose the App Store's screenshots from the scene's phone shots.

Seven portrait frames at 1320x2868, the 6.9-inch iPhone's size, which the
App Store shrinks for every smaller phone. The first three are one panorama:
search results show them side by side, so they share one stage and one
flight trail, and read as a single wide picture. Each frame puts the phone's
real screen in a device outline and lifts the one part that matters out of
it, larger, so it still reads at the size of a search result.

    uv run marketing/store.py [SHOTS_FOLDER]

The words are the film's Instrument Sans; the drawn lock screen uses the
system's SF Pro, so this runs on a Mac.
"""

from __future__ import annotations

import math
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

sys.path.insert(0, str(Path(__file__).resolve().parent))
from compose import INK, MUTED, ROOT, SHOTS, STAGE, fleet_window, phone, rounded, sans  # noqa: E402

OUT = Path.home() / ".local/state/amux-marketing" / ROOT.name / "store"
W, H = 1320, 2868
# The phone's height on a frame; the device outline is the screenshot's
# 1206x2622 plus its bezel, so this shrinks it once to about 0.73.
PHONE_H = 1900
PHONE_TOP = 840
# A lifted part is drawn this much larger than it is inside the phone.
LIFT = 1.32
# The logo's colours, for the trail and the glow.
TRAIL = [(44, 135, 255), (151, 71, 255), (255, 115, 116), (255, 173, 96)]
SF = "/System/Library/Fonts/SFNS.ttf"
# The fleet's rows lifted on the first frame: checkout-redesign and the
# subagents it started, on studio and build.
FLEET_LIFT = (24, 1060, 1182, 1650)
# The request card, opened from the notification.
# The two subagents checkout-redesign started, on build in Codex and here
# in Claude.
CHAT_LIFT = (36, 1330, 1176, 1530)
# Frames 4 to 6: the comment on the review page, where the new agent runs,
# and what a paired machine's page says about the connection.
REVIEW_LIFT = (0, 960, 1206, 1300)
NEW_LIFT = (24, 500, 1182, 1010)
HOST_LIFT = (24, 860, 1182, 1100)
ASK_LIFT = (37, 1660, 1169, 2485)


def sf(size: int, weight: int) -> ImageFont.FreeTypeFont:
    font = ImageFont.truetype(SF, size)
    font.set_variation_by_axes([100, min(max(size / 3, 17), 96), 400, weight])
    return font


def shot(shots: Path, name: str) -> Image.Image:
    return Image.open(shots / f"phone-{name}-dark.png").convert("RGBA")


def device(screen: Image.Image) -> tuple[Image.Image, float]:
    """The screen in its outline at the frame's phone height, and the scale
    from the screen's pixels to the frame's."""
    framed = phone(screen, "dark")
    scale = PHONE_H / framed.height
    framed = framed.resize((round(framed.width * scale), PHONE_H), Image.LANCZOS)
    return framed.filter(ImageFilter.UnsharpMask(radius=0.8, percent=50, threshold=0)), scale


def lifted(screen: Image.Image, box: tuple[int, int, int, int], scale: float, radius: int = 54,
           lift: float = LIFT) -> Image.Image:
    """A part of the screen, cut at `box` in the screen's pixels, drawn
    `lift` times larger than the phone shows it, with a hairline edge."""
    part = screen.crop(box)
    size = scale * lift
    part = part.resize((round(part.width * size), round(part.height * size)), Image.LANCZOS)
    part = rounded(part.convert("RGB"), round(radius * size))
    ImageDraw.Draw(part).rounded_rectangle(
        [0, 0, part.width - 1, part.height - 1], round(radius * size), outline=(255, 255, 255, 40), width=2)
    return part


def lay(canvas: Image.Image, layer: Image.Image, at: tuple[int, int], depth: int = 40) -> None:
    shadow = Image.new("RGBA", canvas.size, (0, 0, 0, 0))
    shadow.paste((0, 0, 0, 230), (at[0], at[1] + depth), layer.getchannel("A"))
    canvas.alpha_composite(shadow.filter(ImageFilter.GaussianBlur(depth * 1.4)))
    canvas.alpha_composite(layer, at)


def ground(width: int, glows: list[tuple[int, int, int, tuple]]) -> Image.Image:
    canvas = Image.new("RGBA", (width, H), STAGE + (255,))
    light = Image.new("RGBA", canvas.size, (0, 0, 0, 0))
    draw = ImageDraw.Draw(light)
    for x, y, r, colour in glows:
        draw.ellipse([x - r, y - r, x + r, y + r], fill=colour)
    canvas.alpha_composite(light.filter(ImageFilter.GaussianBlur(260)))
    return canvas


def colour_at(t: float) -> tuple[int, int, int]:
    t = min(max(t, 0.0), 1.0) * (len(TRAIL) - 1)
    i = min(int(t), len(TRAIL) - 2)
    f = t - i
    return tuple(round(a + (b - a) * f) for a, b in zip(TRAIL[i], TRAIL[i + 1]))


def trail(canvas: Image.Image, points: list[tuple[float, float]], width: float) -> None:
    """The logo's flight trail: a ribbon through `points` (a Catmull-Rom
    curve), swelling in the middle and tapering at both ends, coloured
    along its length from the logo's blue to its amber, over a soft glow."""
    path = []
    for i in range(len(points) - 1):
        p0 = points[max(i - 1, 0)]
        p1, p2 = points[i], points[i + 1]
        p3 = points[min(i + 2, len(points) - 1)]
        for s in range(60):
            t = s / 60
            path.append(tuple(
                0.5 * (2 * b + (-a + c) * t + (2 * a - 5 * b + 4 * c - d) * t * t + (-a + 3 * b - 3 * c + d) * t ** 3)
                for a, b, c, d in zip(p0, p1, p2, p3)))
    path.append(points[-1])
    layer = Image.new("RGBA", canvas.size, (0, 0, 0, 0))
    draw = ImageDraw.Draw(layer)
    for i, (x, y) in enumerate(path):
        t = i / (len(path) - 1)
        r = width / 2 * (0.25 + 0.75 * math.sin(math.pi * t))
        draw.ellipse([x - r, y - r, x + r, y + r], fill=colour_at(t) + (255,))
    glow = layer.filter(ImageFilter.GaussianBlur(70))
    glow.putalpha(glow.getchannel("A").point(lambda a: a * 3 // 5))
    canvas.alpha_composite(glow)
    canvas.alpha_composite(layer)


def headline(canvas: Image.Image, centre: int, lines: list[str], sub: str | list[str] | None) -> None:
    draw = ImageDraw.Draw(canvas)
    # One size for every frame, so the set reads as one voice; a headline
    # is written short enough to fit it.
    font = sans(110, 640)
    for line in lines:
        assert draw.textlength(line, font=font) <= W - 140, f"too long for a frame: {line!r}"
    y = 330
    for line in lines:
        draw.text((centre, y), line, font=font, fill=INK, anchor="ms")
        y += 122
    for i, words in enumerate([sub] if isinstance(sub, str) else sub or []):
        draw.text((centre, y + 14 + i * 64), words, font=sans(52, 470), fill=MUTED, anchor="ms")


def lock_screen(size: tuple[int, int], status: Image.Image) -> Image.Image:
    """A lock screen with amux's notification: drawn, since a simulator
    cannot be given permission to show one, in the system's own type."""
    width, height = size
    screen = Image.new("RGBA", size, (10, 12, 20, 255))
    light = Image.new("RGBA", size, (0, 0, 0, 0))
    draw = ImageDraw.Draw(light)
    draw.ellipse([-500, 1300, 1100, 2900], fill=(44, 135, 255, 120))
    draw.ellipse([300, 1700, 1700, 3100], fill=(151, 71, 255, 110))
    draw.ellipse([700, 2100, 1700, 3100], fill=(255, 115, 116, 70))
    screen.alpha_composite(light.filter(ImageFilter.GaussianBlur(300)))
    # The status bar's right half is the phone's own, from a real shot, kept
    # where it is lighter than the ground: its signal, Wi-Fi and battery.
    bar = status.crop((700, 0, width, 160)).convert("L")
    ink = status.crop((700, 0, width, 160))
    screen.paste(ink, (700, 0), bar.point(lambda v: min(255, v * 2)))
    draw = ImageDraw.Draw(screen)
    draw.text((width // 2, 330), "Sunday 25 October", font=sf(66, 560), fill=(235, 238, 245), anchor="ms")
    draw.text((width // 2, 640), "9:41", font=sf(330, 700), fill=(240, 243, 250), anchor="ms")
    # The notification.
    left, top, right = 40, 900, width - 40
    bottom = top + 300
    card = Image.new("RGBA", size, (0, 0, 0, 0))
    ImageDraw.Draw(card).rounded_rectangle([left, top, right, bottom], 72, fill=(60, 62, 74, 200))
    screen.alpha_composite(card)
    icon = Image.open(ROOT / "apps/apple/Amux/Assets.xcassets/AppIcon.appiconset/AppIcon.png").convert("RGBA")
    icon = rounded(icon.resize((126, 126), Image.LANCZOS).convert("RGB"), 30)
    screen.alpha_composite(icon, (left + 40, top + 87))
    text_x = left + 40 + 126 + 36
    draw.text((text_x, top + 118), "flaky-sync-test", font=sf(46, 650), fill=(250, 250, 252), anchor="ls")
    draw.text((right - 44, top + 118), "now", font=sf(40, 450), fill=(200, 202, 212), anchor="rs")
    draw.text((text_x, top + 178), "Wants to run", font=sf(46, 430), fill=(240, 240, 245), anchor="ls")
    draw.text((text_x, top + 236), "cargo test -p sync --release", font=sf(46, 430), fill=(240, 240, 245), anchor="ls")
    # The flashlight and camera.
    for cx in (190, width - 190):
        draw.ellipse([cx - 76, 2420 - 76, cx + 76, 2420 + 76], fill=(40, 42, 50, 255))
    draw.rounded_rectangle([width // 2 - 200, 2582, width // 2 + 200, 2596], 7, fill=(240, 240, 245))
    return screen


def held(canvas: Image.Image, frame: int, screen: Image.Image, lift: tuple[int, int, int, int] | None = None,
         lift_from: Image.Image | None = None, radius: int = 54, nudge: tuple[int, int] = (0, 0),
         larger: float = LIFT) -> None:
    """The phone showing `screen`, centred on frame number `frame`, and the
    part of `lift_from` (the same screen unless another is named) at `lift`
    grown out of where it sits on the phone, kept inside the frame."""
    framed, scale = device(screen)
    x = frame * W + (W - framed.width) // 2
    lay(canvas, framed, (x, PHONE_TOP))
    if lift is None:
        return
    part = lifted(lift_from or screen, lift, scale, radius, larger)
    # The screen sits inside the outline's bezel, which `phone` draws 40
    # pixels wide before the outline is shrunk.
    cx = x + (40 + (lift[0] + lift[2]) / 2) * scale
    cy = PHONE_TOP + (40 + (lift[1] + lift[3]) / 2) * scale
    left = round(cx - part.width / 2) + nudge[0]
    left = min(max(left, frame * W + 48), (frame + 1) * W - 48 - part.width)
    lay(canvas, part, (left, round(cy - part.height / 2) + nudge[1]))


def panorama(shots: Path) -> list[Image.Image]:
    canvas = ground(3 * W, [
        (300, 2300, 700, (44, 135, 255, 70)),
        (W + 700, 1500, 800, (151, 71, 255, 60)),
        (2 * W + 900, 900, 800, (255, 115, 116, 55)),
    ])
    trail(canvas, [(-120, 2500), (500, 1900), (1320, 1650), (2100, 2150), (2700, 1500), (3400, 900), (4100, 500)], 74)

    fleet = shot(shots, "fleet")
    ask = shot(shots, "ask")
    chat = shot(shots, "chat")

    # 1: every agent on every machine.
    held(canvas, 0, fleet, FLEET_LIFT, lift_from=fleet, radius=44)
    headline(canvas, W // 2, ["Every agent.", "Every machine."], "Claude Code and Codex, in your pocket.")
    # 2: never leave an agent waiting, from the lock screen to the request.
    held(canvas, 1, lock_screen(fleet.size, fleet), ASK_LIFT, lift_from=ask, nudge=(0, 150))
    headline(canvas, W + W // 2, ["Never leave an", "agent waiting."], None)
    # 3: Claude and Codex, working together.
    # Two short rows: lifted further than the others, so they read at the
    # size of a search result.
    held(canvas, 2, chat, CHAT_LIFT, radius=36, larger=1.5)
    headline(canvas, 2 * W + W // 2, ["Claude and Codex,", "working together."], None)

    return [canvas.crop((i * W, 0, (i + 1) * W, H)).convert("RGB") for i in range(3)]


def single(screen: Image.Image, lines: list[str], sub: str | None, glow: tuple, path: list,
           lift: tuple[int, int, int, int] | None = None, radius: int = 54,
           nudge: tuple[int, int] = (0, 0)) -> Image.Image:
    """One frame of its own: the panorama's stage, a stretch of the trail,
    the headline and the phone."""
    canvas = ground(W, [glow])
    trail(canvas, path, 64)
    held(canvas, 0, screen, lift, radius=radius, nudge=nudge)
    headline(canvas, W // 2, lines, sub)
    return canvas.convert("RGB")


def install(shots: Path) -> Image.Image:
    """The last frame: amux runs on your computer, and the phone is its
    client. The terminal app's fleet behind the phone's, and the one line
    that installs it."""
    canvas = ground(W, [(W // 2, 1700, 900, (44, 135, 255, 60))])
    trail(canvas, [(-200, 2000), (500, 1500), (1500, 1100)], 64)
    headline(canvas, W // 2, ["Free. Works with", "amux on your Mac", "or Linux machine."], None)
    draw = ImageDraw.Draw(canvas)
    command = "curl -fsSL https://amux.sh/install | sh"
    mono = ImageFont.truetype(str(ROOT / "crates/shot/assets/JetBrainsMono-Regular.ttf"), 40)
    width = draw.textlength(command, font=mono)
    pill = Image.new("RGBA", canvas.size, (0, 0, 0, 0))
    ImageDraw.Draw(pill).rounded_rectangle([W // 2 - width / 2 - 44, 720, W // 2 + width / 2 + 44, 820], 26,
                                           fill=(255, 255, 255, 14), outline=(255, 255, 255, 34), width=2)
    canvas.alpha_composite(pill)
    draw.text((W // 2, 784), command, font=mono, fill=(223, 227, 232), anchor="ms")
    terminal = fleet_window(shots, 64, 23)
    width = W - 120
    terminal = terminal.resize((width, round(terminal.height * width / terminal.width)), Image.LANCZOS)
    lay(canvas, terminal, (60, 960))
    framed, _ = device(shot(shots, "fleet"))
    framed = framed.resize((framed.width * 3 // 4, framed.height * 3 // 4), Image.LANCZOS)
    lay(canvas, framed, (W - framed.width - 70, 1300))
    return canvas.convert("RGB")


def main(arguments: list[str]) -> int:
    shots = Path(arguments[0]) if arguments else SHOTS
    OUT.mkdir(parents=True, exist_ok=True)
    frames = panorama(shots)
    frames.append(single(shot(shots, "review"), ["Review the diff.", "Comment on the line."], None,
                         (W // 2, 1600, 800, (44, 135, 255, 60)), [(-200, 1300), (600, 1700), (1500, 1400)],
                         REVIEW_LIFT, radius=36))
    frames.append(single(shot(shots, "new"), ["Start an agent", "anywhere."], None,
                         (W // 2, 1500, 800, (151, 71, 255, 55)), [(-200, 2200), (700, 1800), (1500, 2100)],
                         NEW_LIFT, radius=36))
    frames.append(single(shot(shots, "host"), ["Private by design."],
                         ["Your code stays on your machines.", "End-to-end encrypted."],
                         (W // 2, 1500, 800, (255, 115, 116, 45)), [(-200, 1000), (800, 1500), (1500, 1300)],
                         HOST_LIFT, radius=36))
    frames.append(install(shots))
    for i, frame in enumerate(frames, 1):
        frame.save(OUT / f"{i}.png", optimize=True)
        print(f"store: {OUT / f'{i}.png'}")
    strip = Image.new("RGB", (3 * W + 2 * 60, H), (255, 255, 255))
    for i, frame in enumerate(frames):
        strip.paste(frame, (i * (W + 60), 0))
    strip.resize((strip.width // 4, strip.height // 4), Image.LANCZOS).save(OUT / "panorama-preview.png")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
