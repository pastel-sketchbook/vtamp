#!/usr/bin/env python3
"""Derive web and macOS icons from the approved V-meter artwork (macOS tools)."""
from pathlib import Path
import struct
import subprocess
import tempfile
import zlib

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "assets" / "icon.png"


def preserve_prompt(destination):
    # sips strips PNG text chunks. Keep the approved generation provenance on exports.
    data = destination.read_bytes()
    prompt = (ROOT / "assets" / "icon-prompt.txt").read_text().strip().encode("utf-8")
    payload = b"impeccable:prompt\0" + prompt
    chunk = b"tEXt" + payload
    encoded = struct.pack(">I", len(payload)) + chunk + struct.pack(">I", zlib.crc32(chunk))
    assert data[-12:] == bytes.fromhex("0000000049454e44ae426082")
    destination.write_bytes(data[:-12] + encoded + data[-12:])


def resize(size, destination):
    subprocess.run(["sips", "-z", str(size), str(size), str(SOURCE),
                    "--out", str(destination)], check=True, capture_output=True)


def main():
    for size, name in [(32, "favicon-32.png"), (64, "favicon-64.png"),
                       (128, "mark.png"), (180, "apple-touch-icon.png")]:
        destination = ROOT / "site" / name
        resize(size, destination)
        preserve_prompt(destination)
    with tempfile.TemporaryDirectory(prefix="vtamp-icons-") as directory:
        iconset = Path(directory) / "vtamp.iconset"
        iconset.mkdir()
        for size in (16, 32, 128, 256, 512):
            resize(size, iconset / f"icon_{size}x{size}.png")
            resize(size * 2, iconset / f"icon_{size}x{size}@2x.png")
        subprocess.run(["iconutil", "-c", "icns", str(iconset),
                        "-o", str(ROOT / "assets" / "vtamp.icns")], check=True)
    print("Updated web icons and assets/vtamp.icns")


if __name__ == "__main__":
    main()
