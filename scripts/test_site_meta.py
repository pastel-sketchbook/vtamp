"""Checks the landing pages' canonical, Open Graph, JSON-LD, sitemap, and robots metadata against each other."""
from html.parser import HTMLParser
import json
from pathlib import Path
import re
import struct
import unittest
import xml.etree.ElementTree as ElementTree

ROOT = Path(__file__).resolve().parents[1]
SITE = ROOT / "site"
ORIGIN = "https://vtamp.told.me/"
PAGES = {SITE / "index.html": ORIGIN, SITE / "ko" / "index.html": ORIGIN + "ko/"}
SITEMAP_NS = {"sm": "http://www.sitemaps.org/schemas/sitemap/0.9", "xhtml": "http://www.w3.org/1999/xhtml"}


class HeadCollector(HTMLParser):
    def __init__(self):
        super().__init__()
        self.links = []
        self.meta = {}
        self.json_ld = []
        self.in_json_ld = False

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if tag == "link":
            self.links.append(attrs)
        elif tag == "meta":
            key = attrs.get("property") or attrs.get("name")
            if key:
                self.meta[key] = attrs.get("content")
        elif tag == "script" and attrs.get("type") == "application/ld+json":
            self.in_json_ld = True

    def handle_endtag(self, tag):
        if tag == "script":
            self.in_json_ld = False

    def handle_data(self, data):
        if self.in_json_ld:
            self.json_ld.append(json.loads(data))


def crate_version():
    return re.search(r'^version = "([^"]+)"', (ROOT / "Cargo.toml").read_text(), re.MULTILINE).group(1)


def png_size(path):
    data = path.read_bytes()
    assert data[:8] == b"\x89PNG\r\n\x1a\n", path
    return struct.unpack(">II", data[16:24])


class PageMetadataTests(unittest.TestCase):
    def parse(self, page):
        collector = HeadCollector()
        collector.feed(page.read_text(encoding="utf-8"))
        return collector

    def test_each_page_declares_itself_consistently(self):
        version = crate_version()
        for page, url in PAGES.items():
            with self.subTest(page=page.relative_to(ROOT)):
                head = self.parse(page)
                canonical = [link["href"] for link in head.links if link.get("rel") == "canonical"]
                self.assertEqual(canonical, [url])
                alternates = {link["hreflang"]: link["href"] for link in head.links if link.get("rel") == "alternate"}
                self.assertEqual(alternates, {"en": ORIGIN, "ko": ORIGIN + "ko/", "x-default": ORIGIN})
                self.assertEqual(head.meta["og:url"], url)
                self.assertEqual(head.meta["og:type"], "website")
                self.assertEqual(head.meta["twitter:card"], "summary_large_image")
                self.assertEqual(head.meta["og:description"], head.meta["description"])
                for key in ("og:title", "og:image:alt", "og:locale", "og:locale:alternate"):
                    self.assertTrue(head.meta.get(key), key)
                self.assertEqual(head.meta["og:image"], ORIGIN + "og.png")
                width, height = png_size(SITE / "og.png")
                self.assertEqual((width, height), (1200, 630))
                self.assertEqual((head.meta["og:image:width"], head.meta["og:image:height"]), (str(width), str(height)))
                self.assertEqual(len(head.json_ld), 1)
                data = head.json_ld[0]
                self.assertEqual(data["@type"], "SoftwareApplication")
                self.assertEqual(data["url"], url)
                self.assertEqual(data["description"], head.meta["description"])
                self.assertEqual(data["softwareVersion"], version)
                self.assertIn(f'<span class="version">v{version}</span>', page.read_text(encoding="utf-8"))

    def test_sitemap_and_robots_cover_both_pages(self):
        tree = ElementTree.parse(SITE / "sitemap.xml")
        urls = tree.getroot().findall("sm:url", SITEMAP_NS)
        self.assertEqual({url.find("sm:loc", SITEMAP_NS).text for url in urls}, set(PAGES.values()))
        for url in urls:
            alternates = {link.get("hreflang"): link.get("href") for link in url.findall("xhtml:link", SITEMAP_NS)}
            self.assertEqual(alternates, {"en": ORIGIN, "ko": ORIGIN + "ko/", "x-default": ORIGIN})
        self.assertIn(f"Sitemap: {ORIGIN}sitemap.xml", (SITE / "robots.txt").read_text())


if __name__ == "__main__":
    unittest.main()
