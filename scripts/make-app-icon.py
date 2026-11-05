"""Builds crates/desktop/icons/icon.ico from Hermes's dark app icon (black rounded square, white
face): every size Windows asks for at the usual display scales, each downscaled from the 1024 px
original (never upscaled), largest first (Tauri's own window-icon fallback takes the first entry).

    uv run --no-project --with pillow python -I scripts/make-app-icon.py [source.png]

The source defaults to the pinned Hermes's apps/desktop/assets/icon-dark.png in target/hermes-src
(scripts/build-hermes-webapp.ps1 exports it).
"""

import io
import struct
import sys
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "target" / "hermes-src" / "apps" / "desktop" / "assets" / "icon-dark.png"
OUT = ROOT / "crates" / "desktop" / "icons" / "icon.ico"
# Windows' icon sizes at 100-250% scale: small (16 * scale), large (32 * scale), Explorer's
# views (48, 64, 96, 128, 256).
SIZES = [256, 128, 96, 80, 72, 64, 60, 48, 40, 36, 32, 30, 24, 20, 16]


def main() -> None:
    source = Path(sys.argv[1]) if len(sys.argv) > 1 else SOURCE
    original = Image.open(source).convert("RGBA")
    images = []
    for size in SIZES:
        image = original.resize((size, size), Image.Resampling.LANCZOS)
        data = io.BytesIO()
        image.save(data, format="PNG", optimize=True)
        images.append((size, data.getvalue()))
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    directory, blobs = b"", b""
    for size, png in images:
        # Width and height 0 mean 256.
        directory += struct.pack(
            "<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(png), offset + len(blobs)
        )
        blobs += png
    OUT.write_bytes(header + directory + blobs)
    print(f"wrote {OUT} ({len(images)} sizes, {OUT.stat().st_size} bytes) from {source}")


if __name__ == "__main__":
    main()
