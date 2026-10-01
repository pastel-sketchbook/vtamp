"""Checks that the bundled Pretendard subsets cover every character the Korean page renders with them."""
from html.parser import HTMLParser
from pathlib import Path
import unittest

try:
    import brotli  # noqa: F401  (fontTools needs it to open WOFF2)
    from fontTools.ttLib import TTFont
except ImportError:
    TTFont = None

SITE = Path(__file__).resolve().parents[1] / "site"
FONTS = [SITE / "fonts" / f"Pretendard-{weight}.subset.woff2" for weight in ("Regular", "Medium", "SemiBold")]
NOT_RENDERED = {"script", "style"}
# The monospace stack reaches Pretendard only for Hangul; symbols such as ▶ fall through
# to the system font there, exactly as on the English page.
MONOSPACE = {"code", "kbd", "pre"}
HANGUL = ((0x1100, 0x11FF), (0x3130, 0x318F), (0xA960, 0xA97F), (0xAC00, 0xD7FF))


def is_hangul(char):
    return any(low <= ord(char) <= high for low, high in HANGUL)


class TextCollector(HTMLParser):
    """Collects every character Pretendard is expected to draw."""

    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.chars = set()
        self.hidden = 0
        self.mono = 0

    def handle_starttag(self, tag, attrs):
        self.hidden += tag in NOT_RENDERED
        self.mono += tag in MONOSPACE

    def handle_endtag(self, tag):
        self.hidden -= tag in NOT_RENDERED
        self.mono -= tag in MONOSPACE

    def handle_data(self, data):
        if self.hidden:
            return
        for char in data:
            if ord(char) > 0x7F and not char.isspace() and (is_hangul(char) or not self.mono):
                self.chars.add(char)


@unittest.skipUnless(TTFont, "fontTools with brotli is required")
class PretendardCoverageTests(unittest.TestCase):
    def test_korean_page_only_uses_characters_in_the_subsets(self):
        collector = TextCollector()
        collector.feed((SITE / "ko" / "index.html").read_text(encoding="utf-8"))
        wanted = collector.chars
        self.assertTrue(any(is_hangul(char) for char in wanted), "the Korean page should contain Hangul")
        for font in FONTS:
            cmap = TTFont(font).getBestCmap()
            missing = sorted(char for char in wanted if ord(char) not in cmap)
            self.assertEqual(missing, [], f"{font.name} lacks {''.join(missing)!r}; widen the subset or reword")


if __name__ == "__main__":
    unittest.main()
