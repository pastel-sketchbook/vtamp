#!/usr/bin/env python3
"""Compose the social preview cards site/og.png and site/og-ko.png (Pillow, fontTools, macOS SF Mono)."""
import io
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[1]
SITE = ROOT / "site"
FONTS = SITE / "fonts"
MONO = Path("/System/Library/Fonts/SFNSMono.ttf")
WIDTH, HEIGHT, MARGIN = 1200, 630, 72
# DESIGN.md web palette.
BG, TEXT, GREEN, CHIP, CHIP_LINE = "#171c18", "#eeeae0", "#b4f676", "#232e20", "#596b49"
COMMAND = "brew install rath/tap/vtamp"
# The compact Queue capture tilts 30° counter-clockwise behind the copy, zoomed past the canvas.
BACKDROP = SITE / "screenshots" / "compact-queue.png"
TILT, ZOOM, CENTER = 30, 1.4, (840, 330)
# Scrim alpha over the backdrop: nearly opaque behind the copy at the top, lighter toward the bottom.
SCRIM_TOP, SCRIM_BOTTOM = 240, 112
CARDS = {
    "og.png": ([("A music player for the terminal", TEXT)], [("that ", TEXT), ("keeps playing", GREEN), (" after you detach.", TEXT)]),
    "og-ko.png": ([("화면을 닫아도 ", TEXT), ("재생이 멈추지 않는", GREEN)], [("터미널 음악 플레이어.", TEXT)]),
}


def grotesk(size, weight):
    font = ImageFont.truetype(str(FONTS / "SpaceGrotesk.ttf"), size)
    font.set_variation_by_axes([weight])
    return font


def pretendard(size):
    # Pillow reads TrueType, not WOFF2, so unpack the bundled Medium subset in memory.
    from fontTools.ttLib import TTFont

    font = TTFont(FONTS / "Pretendard-Medium.subset.woff2")
    font.flavor = None
    buffer = io.BytesIO()
    font.save(buffer)
    buffer.seek(0)
    return ImageFont.truetype(buffer, size)


def backdrop():
    card = Image.new("RGBA", (WIDTH, HEIGHT), BG)
    shot = Image.open(BACKDROP).convert("RGBA")
    scale = WIDTH / shot.width * ZOOM
    shot = shot.resize((round(shot.width * scale), round(shot.height * scale)), Image.LANCZOS)
    shot = shot.rotate(TILT, resample=Image.BICUBIC, expand=True)
    card.alpha_composite(shot, (CENTER[0] - shot.width // 2, CENTER[1] - shot.height // 2))
    ramp = Image.linear_gradient("L").resize((WIDTH, HEIGHT))
    alpha = ramp.point(lambda value: round(SCRIM_TOP + (SCRIM_BOTTOM - SCRIM_TOP) * value / 255))
    scrim = Image.new("RGBA", (WIDTH, HEIGHT), BG)
    scrim.putalpha(alpha)
    card.alpha_composite(scrim)
    return card.convert("RGB")


def compose(name, headline, font):
    card = backdrop()
    draw = ImageDraw.Draw(card)

    mark = Image.open(SITE / "mark.png").convert("RGBA").resize((56, 56), Image.LANCZOS)
    card.paste(mark, (MARGIN, 52), mark)
    draw.text((MARGIN + 70, 52), "vtamp", font=grotesk(50, 600), fill=TEXT)

    for row, segments in enumerate(headline):
        x = MARGIN
        for text, color in segments:
            draw.text((x, 150 + row * 64), text, font=font, fill=color)
            x += font.getlength(text)

    mono = ImageFont.truetype(str(MONO), 24)
    width = mono.getlength(COMMAND) + 44
    draw.rectangle((MARGIN, 300, MARGIN + width, 352), fill=CHIP, outline=CHIP_LINE)
    draw.text((MARGIN + 22, 312), COMMAND, font=mono, fill=TEXT)
    card.save(SITE / name, optimize=True)


def main():
    compose("og.png", CARDS["og.png"], grotesk(52, 500))
    compose("og-ko.png", CARDS["og-ko.png"], pretendard(52))


if __name__ == "__main__":
    main()
