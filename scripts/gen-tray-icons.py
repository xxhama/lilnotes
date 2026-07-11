#!/usr/bin/env python3
"""Generate 44x44 monochrome template PNGs for the macOS menu bar tray icon.

Template images are single-color (black) with an alpha channel; macOS re-renders
them to adapt to light/dark mode. We draw a microphone silhouette:
  - idle:      outline only (hollow)
  - recording: filled

Pure stdlib: zlib + struct + binascii.crc32. No PIL dependency.
"""
import struct
import zlib
from binascii import crc32

SIZE = 44


def chunk(tag: bytes, data: bytes) -> bytes:
    return (
        struct.pack(">I", len(data))
        + tag
        + data
        + struct.pack(">I", crc32(tag + data) & 0xFFFFFFFF)
    )


def write_png(path: str, pixels: list[list[int]]):
    """pixels: SIZE x SIZE array of alpha values (0-255). Color is black."""
    raw = bytearray()
    for y in range(SIZE):
        raw.append(0)  # filter type: none
        for x in range(SIZE):
            raw.append(0)  # R
            raw.append(0)  # G
            raw.append(0)  # B
            raw.append(pixels[y][x])  # A
    sig = b"\x89PNG\r\n\x1a\n"
    ihdr = struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0)  # 8-bit RGBA
    idat = zlib.compress(bytes(raw), 9)
    with open(path, "wb") as f:
        f.write(sig)
        f.write(chunk(b"IHDR", ihdr))
        f.write(chunk(b"IDAT", idat))
        f.write(chunk(b"IEND", b""))


def in_mic_body(x: float, y: float) -> bool:
    """Microphone silhouette membership at float coords (0..43)."""
    cx = 21.5
    # Capsule body: rounded rect, width 14, height 20, top at y=8.
    # Approximate rounded corners with a radius of 7.
    body_left = cx - 7
    body_right = cx + 7
    body_top = 8
    body_bottom = 28
    rx, ry = 7, 7
    # inside rounded rect?
    dx = max(body_left - x, x - body_right, 0)
    dy = max(body_top - y, y - body_bottom, 0)
    in_body = (dx * dx + dy * dy) <= (rx * ry)
    # stem: thin rect under body
    stem_left = cx - 2
    stem_right = cx + 2
    stem_top = 28
    stem_bottom = 33
    in_stem = stem_left <= x <= stem_right and stem_top <= y <= stem_bottom
    # base: wider rect at bottom
    base_left = cx - 7
    base_right = cx + 7
    base_top = 33
    base_bottom = 35
    in_base = base_left <= x <= base_right and base_top <= y <= base_bottom
    return in_body or in_stem or in_base


def outline_alpha(x: int, y: int) -> int:
    """Outline (hollow): a pixel is on if it's inside the body but a
    neighbor is outside — i.e. the boundary of the silhouette."""
    # Sample at pixel centers (x+0.5, y+0.5).
    fx, fy = x + 0.5, y + 0.5
    if not in_mic_body(fx, fy):
        return 0
    # Check 4-neighbors; if any neighbor center is outside, this is an edge.
    neighbors = [(fx - 1, fy), (fx + 1, fy), (fx, fy - 1), (fx, fy + 1)]
    on_edge = any(not in_mic_body(nx, ny) for nx, ny in neighbors)
    return 255 if on_edge else 0


def filled_alpha(x: int, y: int) -> int:
    fx, fy = x + 0.5, y + 0.5
    return 255 if in_mic_body(fx, fy) else 0


def main():
    idle = [[outline_alpha(x, y) for x in range(SIZE)] for y in range(SIZE)]
    rec = [[filled_alpha(x, y) for x in range(SIZE)] for y in range(SIZE)]
    write_png("src-tauri/icons/tray-idle.png", idle)
    write_png("src-tauri/icons/tray-recording.png", rec)
    print("wrote src-tauri/icons/tray-idle.png + tray-recording.png")


if __name__ == "__main__":
    main()