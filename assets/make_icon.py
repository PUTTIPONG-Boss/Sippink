"""Draws Sippink's 16x16 pixel-art icon and writes icon.png (256 px) and
icon.ico (16-256 px, PNG entries). Standard library only.

    python make_icon.py
"""

import struct
import zlib
from pathlib import Path

# A glass of green drink with a straw, in a square frame. Colours match
# src/theme.rs.
ART = [
    "################",
    "#..............#",
    "#............s.#",
    "#...........s..#",
    "#..w.......s.w.#",
    "#..w......s..w.#",
    "#..w.....s...w.#",
    "#..wgggggsgggw.#",
    "#..wgGgggsgggw.#",
    "#..wgGgggsgggw.#",
    "#..wggggggGggw.#",
    "#..wgggggggggw.#",
    "#...wgggggggw..#",
    "#....wwwwwww...#",
    "#..............#",
    "################",
]
COLOURS = {
    ".": (10, 14, 10),     # BG
    "#": (125, 255, 143),  # FG frame
    "w": (190, 230, 196),  # glass
    "g": (60, 190, 85),    # drink
    "G": (160, 255, 170),  # bubbles
    "s": (255, 204, 51),   # straw (AMBER)
}


def pixels(size):
    scale = size // len(ART)
    rows = []
    for line in ART:
        row = b"".join(bytes(COLOURS[c]) + b"\xff" for c in line for _ in range(scale))
        rows.extend([row] * scale)
    return rows


def png(size):
    raw = b"".join(b"\x00" + r for r in pixels(size))

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")


def ico(sizes):
    images = [png(s) for s in sizes]
    out = struct.pack("<HHH", 0, 1, len(sizes))
    offset = 6 + 16 * len(sizes)
    for s, data in zip(sizes, images):
        dim = 0 if s >= 256 else s
        out += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    return out + b"".join(images)


if __name__ == "__main__":
    here = Path(__file__).parent
    assert all(len(r) == 16 for r in ART)
    (here / "icon.png").write_bytes(png(256))
    (here / "icon.ico").write_bytes(ico([16, 32, 48, 64, 128, 256]))
    print("wrote icon.png, icon.ico")
