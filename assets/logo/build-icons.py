# /// script
# requires-python = ">=3.10"
# dependencies = ["resvg-py==0.2.6"]
# ///
"""Generate every zWorkTracker icon from the master, assets/logo/logo.svg.

Run from the repo root:  uv run assets/logo/build-icons.py
The logo is the clock alone on a transparent canvas (no tile). Renderer: resvg (resvg-py wheel). The logo and favicon SVGs are derived mechanically from the
element ids (wl-b-*) in logo.svg; nothing is drawn by hand. Output is deterministic.
The .icns needs macOS iconutil; on other systems the existing icon.icns is left untouched.
"""
import struct, subprocess, sys, tempfile, xml.etree.ElementTree as ET
from pathlib import Path
import resvg_py

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
MASTER = HERE / "logo.svg"
ICONS = REPO / "src-tauri/icons"
SITE = REPO / "site/assets"
NS = "http://www.w3.org/2000/svg"
ET.register_namespace("", NS)
q = lambda t: f"{{{NS}}}{t}"
SMALL = 32   # sizes up to this use the simplified favicon art (png, ico, icns)
FAV_HUES = ["#ff7e5c", "#a2dc6c", "#4caaf8", "#a968f8"]

def tree(): return ET.parse(MASTER).getroot()
def by_id(root, i): return next(e for e in root.iter() if e.get("id") == i)
def drop(root, i):
    el = by_id(root, i); next(p for p in root.iter() if el in list(p)).remove(el)

def prune_defs(root):
    text = ET.tostring(root, encoding="unicode")
    defs = root.find(q("defs"))
    for g in list(defs):
        rest = text.replace(ET.tostring(g, encoding="unicode"), "")
        if f"url(#{g.get('id')})" not in rest: defs.remove(g)

def write(root, path):
    root.attrib.pop("width", None); root.attrib.pop("height", None)
    ET.indent(root, space="")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(ET.tostring(root, encoding="unicode").replace("\n", "") + "\n")

def thicken(r):
    for i in ("wl-b-dots", "wl-b-case-hl", "wl-b-pin-dot"): drop(r, i)
    by_id(r, "wl-b-ring").set("stroke-width", "44")
    by_id(r, "wl-b-dial").set("r", "66")
    by_id(r, "wl-b-hands").set("stroke-width", "18")
    by_id(r, "wl-b-pin").set("r", "17")

def derive_logo(out):
    r = tree(); thicken(r); prune_defs(r); write(r, out)

def derive_favicon(out):
    r = tree(); thicken(r)
    by_id(r, "wl-b-outline").set("stroke-width", "14")   # keeps the edge visible at 16 px
    for i, hue in enumerate(FAV_HUES): by_id(r, f"wl-b-arc{i}").set("stroke", hue)
    prune_defs(r); write(r, out)

def render(svg, size, out=None, background=None):
    png = bytes(resvg_py.svg_to_bytes(svg_path=str(svg), width=size, height=size, background=background))
    if out: out.parent.mkdir(parents=True, exist_ok=True); out.write_bytes(png)
    return png

def ico(entries, out):
    head = struct.pack("<HHH", 0, 1, len(entries)); off = 6 + 16 * len(entries); dirs = b""; blobs = b""
    for s, png in entries:
        dirs += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(png), off + len(blobs)); blobs += png
    out.write_bytes(head + dirs + blobs)

def main():
    fav = SITE / "favicon.svg"
    derive_logo(SITE / "logo.svg"); derive_favicon(fav)
    render(fav, 64, SITE / "icon-64.png")
    render(MASTER, 180, SITE / "apple-touch-icon.png", background="#ffffff")  # iOS fills transparency with black, so this one is opaque
    render(fav, 32, ICONS / "32x32.png"); render(MASTER, 128, ICONS / "128x128.png")
    render(MASTER, 256, ICONS / "128x128@2x.png"); render(MASTER, 512, ICONS / "icon.png")
    src = lambda s: fav if s <= SMALL else MASTER
    for n in ("idle", "running"):   # tray: drawn by hand on a 32 grid (round case + 2 hands / play), not derived
        render(HERE / f"tray-{n}.svg", 32, ICONS / f"tray-{n}.png")
    ico([(s, render(src(s), s)) for s in (16, 24, 32, 48, 64, 128, 256)], ICONS / "icon.ico")
    if sys.platform == "darwin":
        names = {"icon_16x16": 16, "icon_16x16@2x": 32, "icon_32x32": 32, "icon_32x32@2x": 64, "icon_128x128": 128,
                 "icon_128x128@2x": 256, "icon_256x256": 256, "icon_256x256@2x": 512, "icon_512x512": 512, "icon_512x512@2x": 1024}
        with tempfile.TemporaryDirectory() as t:
            iconset = Path(t) / "icon.iconset"
            for n, s in names.items(): render(src(s), s, iconset / f"{n}.png")
            subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o", str(ICONS / "icon.icns")], check=True)
    else:
        print("skipped icon.icns (needs macOS iconutil)")
    print("done")

if __name__ == "__main__":
    main()
