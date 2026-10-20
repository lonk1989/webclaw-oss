#!/usr/bin/env python3
"""Generate platform-specific app icons.

macOS (.icns): logo on full-bleed opaque white (system applies squircle mask).
Windows (.ico) + shared PNGs: logo on transparent background.
"""

from __future__ import annotations

import io
import shutil
import struct
import subprocess
import tempfile
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent
ICONS = ROOT / "icons"
SOURCE = ICONS / "source"
MAC_MASTER = SOURCE / "app-icon-macos.png"
WIN_MASTER = SOURCE / "app-icon-windows.png"


def resize(im: Image.Image, size: int) -> Image.Image:
    return im.resize((size, size), Image.Resampling.LANCZOS)


def save_png(im: Image.Image, path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    im.save(path, format="PNG")


def png_bytes(im: Image.Image) -> bytes:
    buf = io.BytesIO()
    im.save(buf, format="PNG")
    return buf.getvalue()


def fit_logo(logo: Image.Image, box: int) -> Image.Image:
    """Trim transparent padding, then scale to fill ``box`` (up or down)."""
    bbox = logo.getbbox()
    if bbox:
        logo = logo.crop(bbox)
    # Scale to cover the shorter side of the box while staying inside.
    lw, lh = logo.size
    if lw <= 0 or lh <= 0:
        raise RuntimeError("logo is empty after trim")
    scale = min(box / lw, box / lh)
    nw = max(1, int(round(lw * scale)))
    nh = max(1, int(round(lh * scale)))
    logo = logo.resize((nw, nh), Image.Resampling.LANCZOS)
    canvas = Image.new("RGBA", (box, box), (0, 0, 0, 0))
    x = (box - nw) // 2
    y = (box - nh) // 2
    canvas.alpha_composite(logo, (x, y))
    return canvas


def build_masters_from_transparent(src: Image.Image, master: int = 1024, content_ratio: float = 0.78) -> tuple[Image.Image, Image.Image]:
    content = int(master * content_ratio)
    fitted = fit_logo(src, content)

    mac = Image.new("RGBA", (master, master), (255, 255, 255, 255))
    x = (master - fitted.size[0]) // 2
    y = (master - fitted.size[1]) // 2
    mac.alpha_composite(fitted, (x, y))
    r, g, b, a = mac.split()
    mac = Image.merge("RGBA", (r, g, b, a.point(lambda _: 255)))

    win = Image.new("RGBA", (master, master), (0, 0, 0, 0))
    win.alpha_composite(fitted, (x, y))
    return mac, win


def write_icns(mac: Image.Image, dest: Path) -> None:
    # Filenames built without embedding "@" in source as a single token that tools may mangle.
    at = chr(64)
    names = [
        (f"icon_16x16.png", 16),
        (f"icon_16x16{at}2x.png", 32),
        (f"icon_32x32.png", 32),
        (f"icon_32x32{at}2x.png", 64),
        (f"icon_128x128.png", 128),
        (f"icon_128x128{at}2x.png", 256),
        (f"icon_256x256.png", 256),
        (f"icon_256x256{at}2x.png", 512),
        (f"icon_512x512.png", 512),
        (f"icon_512x512{at}2x.png", 1024),
    ]
    with tempfile.TemporaryDirectory(prefix="webclaw-iconset-") as tmp:
        iconset = Path(tmp) / "webclaw.iconset"
        iconset.mkdir()
        for name, size in names:
            save_png(resize(mac, size), iconset / name)
        found = sorted(p.name for p in iconset.iterdir())
        expected = [n for n, _ in names]
        missing = [n for n in expected if n not in found]
        if missing:
            raise RuntimeError(f"iconset incomplete, missing: {missing}; found={found}")
        subprocess.check_call(["iconutil", "-c", "icns", str(iconset), "-o", str(dest)])


def write_ico(win: Image.Image, dest: Path) -> None:
    sizes = [16, 32, 48, 64, 128, 256]
    pngs = [(s, png_bytes(resize(win, s))) for s in sizes]
    count = len(pngs)
    header = struct.pack("<HHH", 0, 1, count)
    entries: list[bytes] = []
    blobs: list[bytes] = []
    offset = 6 + 16 * count
    for s, blob in pngs:
        w = 0 if s >= 256 else s
        h = 0 if s >= 256 else s
        entries.append(struct.pack("<BBBBHHII", w, h, 0, 0, 1, 32, len(blob), offset))
        blobs.append(blob)
        offset += len(blob)
    dest.write_bytes(header + b"".join(entries) + b"".join(blobs))


def icns_types(path: Path) -> list[bytes]:
    data = path.read_bytes()
    assert data[:4] == b"icns"
    out: list[bytes] = []
    off = 8
    while off + 8 <= len(data):
        itype = data[off : off + 4]
        isize = struct.unpack(">I", data[off + 4 : off + 8])[0]
        if isize < 8:
            break
        out.append(itype)
        off += isize
    return out


def main() -> None:
    SOURCE.mkdir(parents=True, exist_ok=True)
    logo_src = SOURCE / "logo-transparent.png"

    # Prefer canonical transparent logo; else existing masters; else icons/128x128@2x.png
    if logo_src.is_file():
        src = Image.open(logo_src).convert("RGBA")
        mac, win = build_masters_from_transparent(src)
        save_png(mac, MAC_MASTER)
        save_png(win, WIN_MASTER)
    elif MAC_MASTER.is_file() and WIN_MASTER.is_file():
        mac = Image.open(MAC_MASTER).convert("RGBA")
        win = Image.open(WIN_MASTER).convert("RGBA")
    else:
        fallback = ICONS / "128x128@2x.png"
        if not fallback.is_file():
            raise RuntimeError(f"missing logo source: {logo_src} or {fallback}")
        src = Image.open(fallback).convert("RGBA")
        mac, win = build_masters_from_transparent(src)
        save_png(mac, MAC_MASTER)
        save_png(win, WIN_MASTER)
        save_png(src, logo_src)

    if mac.getpixel((0, 0)) != (255, 255, 255, 255):
        raise RuntimeError("macOS master must be opaque white at corners (full-bleed)")
    if win.getpixel((0, 0))[3] != 0:
        raise RuntimeError("Windows master must be transparent at corners")

    write_icns(mac, ICONS / "icon.icns")
    types = icns_types(ICONS / "icon.icns")
    print("icns bytes", (ICONS / "icon.icns").stat().st_size, "types", types)
    if b"ic10" not in types and b"ic09" not in types:
        raise RuntimeError(f"icns missing high-res slots: {types}")

    # Shared PNGs stay transparent for tray/window chrome.
    for name, size in [("32x32.png", 32), ("128x128.png", 128), ("128x128@2x.png", 256)]:
        save_png(resize(win, size), ICONS / name)

    write_ico(win, ICONS / "icon.ico")
    dup = ROOT / "icon.ico"
    if dup.exists():
        shutil.copy2(ICONS / "icon.ico", dup)

    print("ok: mac=white-fullbleed icns, win=transparent ico+png")


if __name__ == "__main__":
    main()
