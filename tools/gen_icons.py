#!/usr/bin/env python3
"""Generate no-afk's icons. No image libraries — raw PNG via zlib.

    python3 tools/gen_icons.py

Tray glyph is an eye: open = awake (session on), closed = normal. Rendered as a
macOS *template* image (black + alpha only), so the OS recolours it for light/dark
menu bars and for the highlighted state. Never bake colour into a template icon.
"""

import math
import pathlib
import struct
import subprocess
import zlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
ICONS = ROOT / "src-tauri" / "icons"

SS = 4  # supersampling factor, for antialiasing


def write_png(path, w, h, px):
    """px: list of (r,g,b,a) rows, row-major."""
    raw = b"".join(b"\x00" + b"".join(struct.pack("4B", *p) for p in row) for row in px)

    def chunk(tag, data):
        c = tag + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c))

    png = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(png)
    return path


def render(size, shader):
    """shader(x, y, size) -> alpha 0..1, sampled SS*SS times per pixel."""
    out = []
    for py in range(size):
        row = []
        for px_ in range(size):
            acc = 0.0
            for sy in range(SS):
                for sx in range(SS):
                    x = px_ + (sx + 0.5) / SS
                    y = py + (sy + 0.5) / SS
                    acc += shader(x, y, size)
            row.append(acc / (SS * SS))
        out.append(row)
    return out


def to_rgba(mask, rgb=(0, 0, 0)):
    return [[(rgb[0], rgb[1], rgb[2], int(round(a * 255))) for a in row] for row in mask]


# --- glyphs ---------------------------------------------------------------


# Coverage helpers. All distances arrive in *normalised* units (the glyph lives in
# u,v ∈ [-0.5, 0.5]) but antialiasing has to happen in *pixels*, so every edge is
# converted with `* s` before the 1px smoothstep. Doing the falloff in normalised
# units instead makes a 0.055-wide stroke bleed across half the icon.


def _fill(sd, s):
    """Coverage for a signed distance (negative = inside), AA'd over one pixel."""
    return max(0.0, min(1.0, 0.5 - sd * s))


def _stroke(d, target, half, s):
    """Coverage for a stroke of half-width `half` centred on the `target` isoline."""
    return _fill(abs(d - target) - half, s)


# Glyph geometry. Menu bar icons get ~18pt of a 22pt canvas, so the ink has to fill
# most of the square — an earlier version used ~30% and rendered as an illegible
# sliver. Both states are sized to the same half-width and centred on v=0 so they
# swap without the icon appearing to jump.
HALF_W = 0.43
STROKE = 0.045


def eye_open(x, y, s, k=1.0):
    """Open eye = awake. `k` shrinks the glyph, for reuse inside the app badge."""
    u, v = (x - s / 2) / s / k, (y - s / 2) / s / k
    s = s * k  # AA still happens in real pixels

    # Lens outline: two circular arcs meeting at the corners (a vesica).
    # Solved so the lens spans exactly ±HALF_W wide and ±b tall:
    #   off = (a² - b²) / 2b,  r = b + off
    a, b = HALF_W, 0.21
    off = (a * a - b * b) / (2 * b)
    r = b + off

    d1 = math.hypot(u, v + off)
    d2 = math.hypot(u, v - off)

    # Clip each arc to the other circle's interior so they stop at the corners.
    edge = max(
        min(_stroke(d1, r, STROKE, s), _fill(d2 - r, s)),
        min(_stroke(d2, r, STROKE, s), _fill(d1 - r, s)),
    )

    # Pupil, clipped to the lens.
    lens = min(_fill(d1 - r, s), _fill(d2 - r, s))
    pupil = min(_fill(math.hypot(u, v) - 0.115, s), lens)

    return max(edge, pupil)


def _seg(px, py, ax, ay, bx, by):
    """Distance from point to line segment a->b."""
    dx, dy = bx - ax, by - ay
    t = ((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)
    t = max(0.0, min(1.0, t))
    return math.hypot(px - (ax + t * dx), py - (ay + t * dy))


def eye_closed(x, y, s, k=1.0):
    """Closed eye = normal sleep behaviour."""
    u, v = (x - s / 2) / s / k, (y - s / 2) / s / k
    s = s * k

    # A shallow valley — the shut lid. Solved so it spans ±HALF_W, bottoms out at
    # v = -0.02 and lifts to v = -0.17 at the corners: a gentle sag, not a semicircle.
    sag, bottom, end = 0.15, -0.02, -0.17
    r = (HALF_W * HALF_W + sag * sag) / (2 * sag)
    cyv = bottom - r
    lid = min(
        _stroke(math.hypot(u, v - cyv), r, STROKE, s),
        _fill(abs(u) - HALF_W, s),
    )
    _ = end  # documented above; derived from sag

    # Three lashes dropping from the lid, splayed outwards. Their extent balances the
    # lid above so the whole glyph sits centred on v = 0.
    lashes = (
        (-0.25, -0.05, -0.31, 0.11),
        (0.0, 0.01, 0.0, 0.17),
        (0.25, -0.05, 0.31, 0.11),
    )
    lash = 0.0
    for ax, ay, bx, by in lashes:
        lash = max(lash, _fill(_seg(u, v, ax, ay, bx, by) - STROKE * 0.85, s))

    return max(lid, lash)


def app_icon(x, y, s):
    """Rounded-square (squircle-ish) badge coverage for the bundle icon."""
    cx, cy = s / 2, s / 2
    u, v = (x - cx) / s, (y - cy) / s
    r = 0.20
    ax, ay = abs(u) - (0.42 - r), abs(v) - (0.42 - r)
    d = math.hypot(max(ax, 0), max(ay, 0)) + min(max(ax, ay), 0) - r
    return max(0.0, min(1.0, -d * s * 0.5 + 0.5))


# Badge gradient, top to bottom. Amber reads as "awake" and, unlike the original
# near-black, stays legible on the DMG's light background.
BADGE_TOP = (255, 179, 64)
BADGE_BOTTOM = (240, 138, 23)
GLYPH = (255, 255, 255)


def compose_app_icon(px):
    """Coloured badge with the eye drawn *on* it in white.

    The glyph is painted rather than knocked out. Knocking it out makes the eye show
    whatever sits behind the icon, so it looked white on a light background and dark
    on a dark one — inconsistent, and muddy on the DMG.
    """
    badge = render(px, app_icon)
    eye = render(px, lambda x, y, s: eye_open(x, y, s, k=0.62))

    rows = []
    for j, (brow, erow) in enumerate(zip(badge, eye)):
        t = j / max(px - 1, 1)
        base = [round(BADGE_TOP[i] + (BADGE_BOTTOM[i] - BADGE_TOP[i]) * t) for i in range(3)]
        row = []
        for b, e in zip(brow, erow):
            # Blend the glyph over the gradient, then let the badge shape set alpha.
            colour = [round(base[i] + (GLYPH[i] - base[i]) * e) for i in range(3)]
            row.append((colour[0], colour[1], colour[2], int(round(b * 255))))
        rows.append(row)
    return rows


def main():
    # Tray: template images, black + alpha. 44px covers @2x menu bars.
    for name, shader in (("tray-active", eye_open), ("tray-idle", eye_closed)):
        for suffix, px in (("", 22), ("@2x", 44)):
            write_png(ICONS / f"{name}{suffix}.png", px, px, to_rgba(render(px, shader)))
        print(f"  {name}.png / {name}@2x.png")

    # App icon: coloured badge with a white eye. The eye is scaled down so it sits
    # inside the badge instead of overflowing it.
    for px in (32, 64, 128, 256, 512, 1024):
        write_png(ICONS / f"{px}x{px}.png", px, px, compose_app_icon(px))
    print("  app icons 32..1024")

    # Tauri expects these exact filenames.
    (ICONS / "icon.png").write_bytes((ICONS / "512x512.png").read_bytes())
    (ICONS / "128x128@2x.png").write_bytes((ICONS / "256x256.png").read_bytes())

    # .icns via iconutil (macOS only).
    iconset = ICONS / "icon.iconset"
    iconset.mkdir(exist_ok=True)
    for px, names in (
        (32, ["icon_16x16@2x.png", "icon_32x32.png"]),
        (64, ["icon_32x32@2x.png"]),
        (128, ["icon_128x128.png"]),
        (256, ["icon_128x128@2x.png", "icon_256x256.png"]),
        (512, ["icon_256x256@2x.png", "icon_512x512.png"]),
        (1024, ["icon_512x512@2x.png"]),
    ):
        for n in names:
            (iconset / n).write_bytes((ICONS / f"{px}x{px}.png").read_bytes())
    try:
        subprocess.run(
            ["iconutil", "-c", "icns", str(iconset), "-o", str(ICONS / "icon.icns")],
            check=True,
        )
        print("  icon.icns")
    except (subprocess.CalledProcessError, FileNotFoundError) as e:
        print(f"  (skipped icns: {e})")

    print(f"\nwrote icons to {ICONS}")


if __name__ == "__main__":
    main()
