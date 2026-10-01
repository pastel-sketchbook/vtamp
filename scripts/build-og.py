#!/usr/bin/env python3
"""Compose the social preview card site/og.png from the wordmark, headline, and wide screenshot (Pillow, macOS SF Mono)."""
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[1]
SITE = ROOT / "site"
GROTESK = SITE / "fonts" / "SpaceGrotesk.ttf"
MONO = Path("/System/Library/Fonts/SFNSMono.ttf")
WIDTH, HEIGHT, MARGIN = 1200, 630, 72
# DESIGN.md web palette.
BG, TEXT, GREEN = "#171c18", "#eeeae0", "#b4f676"
CHIP, CHIP_LINE, STAGE, STAGE_LINE = "#232e20", "#596b49", "#1e1e2e", "#585b70"
HEADLINE = [
    [("A music player for the terminal", TEXT)],
    [("that ", TEXT), ("keeps playing", GREEN), (" after you detach.", TEXT)],
]
COMMAND = "brew install rath/tap/vtamp"


def grotesk(size, weight):
    font = ImageFont.truetype(str(GROTESK), size)
    font.set_variation_by_axes([weight])
    return font


def main():
    card = Image.new("RGB", (WIDTH, HEIGHT), BG)
    draw = ImageDraw.Draw(card)

    mark = Image.open(SITE / "mark.png").convert("RGBA").resize((56, 56), Image.LANCZOS)
    card.paste(mark, (MARGIN, 52), mark)
    draw.text((MARGIN + 70, 52), "vtamp", font=grotesk(50, 600), fill=TEXT)

    headline = grotesk(52, 500)
    for row, segments in enumerate(HEADLINE):
        x = MARGIN
        for text, color in segments:
            draw.text((x, 150 + row * 64), text, font=headline, fill=color)
            x += headline.getlength(text)

    mono = ImageFont.truetype(str(MONO), 24)
    width = mono.getlength(COMMAND) + 44
    draw.rectangle((MARGIN, 300, MARGIN + width, 352), fill=CHIP, outline=CHIP_LINE)
    draw.text((MARGIN + 22, 312), COMMAND, font=mono, fill=TEXT)

    shot = Image.open(SITE / "screenshots" / "wide.png").convert("RGB")
    stage_width = WIDTH - 2 * 80
    shot = shot.resize((stage_width, round(shot.height * stage_width / shot.width)), Image.LANCZOS)
    top = 392
    draw.rounded_rectangle((80 - 1, top - 1, 80 + stage_width, top + shot.height), radius=8, fill=STAGE, outline=STAGE_LINE)
    card.paste(shot, (80, top))
    card.save(SITE / "og.png", optimize=True)


if __name__ == "__main__":
    main()
