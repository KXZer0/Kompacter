#!/usr/bin/env python3
"""Reuse the PNG image embedded in the app's existing 256 px Windows icon."""

from pathlib import Path
import struct
import sys

ico = Path(sys.argv[1]).read_bytes()
reserved, kind, count = struct.unpack_from("<HHH", ico)
assert (reserved, kind) == (0, 1)
for index in range(count):
    width, height, size, offset = struct.unpack_from(
        "<BB6xII", ico, 6 + 16 * index
    )
    if width == height == 0:  # ICO encodes a 256 px side as zero.
        png = ico[offset : offset + size]
        if png.startswith(b"\x89PNG\r\n\x1a\n"):
            image = struct.pack(">4sI", b"ic08", len(png) + 8) + png
            Path(sys.argv[2]).write_bytes(
                struct.pack(">4sI", b"icns", len(image) + 8) + image
            )
            break
else:
    raise SystemExit("missing a 256 px PNG in the app icon")
