#!/usr/bin/env python3
"""Write the 256 px PNG embedded in the app's Windows icon to a file."""

from pathlib import Path
import struct
import sys

ico = Path(sys.argv[1]).read_bytes()
reserved, kind, count = struct.unpack_from("<HHH", ico)
assert (reserved, kind) == (0, 1)
for index in range(count):
    width, height, size, offset = struct.unpack_from("<BB6xII", ico, 6 + 16 * index)
    png = ico[offset : offset + size]
    # ICO encodes a 256 px side as zero.
    if width == height == 0 and png.startswith(b"\x89PNG\r\n\x1a\n"):
        Path(sys.argv[2]).write_bytes(png)
        break
else:
    raise SystemExit("missing a 256 px PNG in the app icon")
