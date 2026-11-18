#!/usr/bin/env python3
"""Convert devin_theme_gen.py HTML frames into a single pen.dev `.pen` document.

Usage (from repo root):

    python3 docs/design/devin_theme_gen.py /tmp/soyo_frames
    python3 docs/design/html_to_pen.py /tmp/soyo_frames docs/design/desktop-ui.pen

Requires `playwright` plus a system Chrome (`channel="chrome"` headless). The
browser only computes layout and computed styles; the mapping below is
deterministic:

  element with children  -> frame (layout "none", clip when overflow clips)
  direct text runs       -> text nodes (Range API rects, computed font styles)
  <svg>                  -> path nodes (path/circle/rect/line/poly* -> geometry)
  <img>                  -> rectangle with image fill (data URLs deduped to
                          ./assets/<hash>.png next to the .pen file)
  leaf box with visuals  -> rectangle (fill/stroke/cornerRadius/effect)

Positions are converted from viewport-absolute to parent-relative, matching the
.pen schema where children are positioned relative to the parent's top-left
corner. Unrenderable CSS (dashed borders, blend modes, pseudo elements, WebGL)
is approximated or skipped; see the report printed on stderr.
"""
import base64
import hashlib
import json
import math
import re
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

FRAME_W, FRAME_H = 1520, 980
GRID_COLS, GRID_GAP = 4, 80
PEN_VERSION = "2.23"

EXTRACT_JS = r"""
() => {
  const SKIP = new Set(["SCRIPT","STYLE","HEAD","META","LINK","TITLE","BR","NOSCRIPT"]);
  function textRect(node) {
    const r = document.createRange();
    r.selectNodeContents(node);
    const b = r.getBoundingClientRect();
    return { x: b.x, y: b.y, w: b.width, h: b.height };
  }
  function collect(el) {
    const cs = getComputedStyle(el);
    const r = el.getBoundingClientRect();
    const out = {
      tag: el.tagName.toLowerCase(),
      cls: (typeof el.className === "string" ? el.className : "") || el.getAttribute("class") || "",
      rect: { x: r.x, y: r.y, w: r.width, h: r.height },
      display: cs.display, visibility: cs.visibility,
      opacity: parseFloat(cs.opacity),
      bg: cs.backgroundColor, bgImage: cs.backgroundImage,
      bTop: [cs.borderTopWidth, cs.borderTopStyle, cs.borderTopColor],
      bRight: [cs.borderRightWidth, cs.borderRightStyle, cs.borderRightColor],
      bBottom: [cs.borderBottomWidth, cs.borderBottomStyle, cs.borderBottomColor],
      bLeft: [cs.borderLeftWidth, cs.borderLeftStyle, cs.borderLeftColor],
      radius: [cs.borderTopLeftRadius, cs.borderTopRightRadius,
               cs.borderBottomRightRadius, cs.borderBottomLeftRadius],
      shadow: cs.boxShadow,
      color: cs.color,
      fontFamily: cs.fontFamily, fontSize: cs.fontSize,
      fontWeight: cs.fontWeight, fontStyle: cs.fontStyle,
      letterSpacing: cs.letterSpacing, lineHeight: cs.lineHeight,
      textAlign: cs.textAlign, textDecoration: cs.textDecorationLine,
      overflowX: cs.overflowX, overflowY: cs.overflowY,
      children: [],
    };
    if (out.tag === "svg") {
      out.svg = el.outerHTML;
      return out;
    }
    if (out.tag === "img") {
      out.src = el.getAttribute("src") || "";
      return out;
    }
    for (const n of el.childNodes) {
      if (n.nodeType === 3) {
        const t = n.textContent || "";
        if (t.trim()) {
          out.children.push({ tag: "#text", text: t.trim(), rect: textRect(n),
                              color: out.color, fontFamily: out.fontFamily,
                              fontSize: out.fontSize, fontWeight: out.fontWeight,
                              fontStyle: out.fontStyle, letterSpacing: out.letterSpacing,
                              lineHeight: out.lineHeight, textAlign: out.textAlign,
                              textDecoration: out.textDecoration, opacity: 1 });
        }
      } else if (n.nodeType === 1 && !SKIP.has(n.tagName)) {
        out.children.push(collect(n));
      }
    }
    return out;
  }
  return collect(document.body);
}
"""

_css_color = re.compile(r"rgba?\(([^)]*)\)")


def css_color(value, warnings):
    """`rgb[a](...)` / hex -> #RRGGBB[AA]; None for transparent/unparsed."""
    if not value or value == "none" or value == "transparent":
        return None
    value = value.strip()
    if value.startswith("#"):
        h = value.lstrip("#")
        if len(h) == 3:
            h = "".join(c * 2 for c in h)
        if len(h) == 4:
            h = "".join(c * 2 for c in h)
        if len(h) in (6, 8):
            return "#" + h.lower()
        warnings.append(f"unparsed color {value}")
        return None
    m = _css_color.match(value)
    if m:
        parts = [p.strip() for p in m.group(1).split(",")]
        try:
            if len(parts) >= 3 and "/" not in parts[0]:
                r, g, b = (round(float(p.rstrip("%")) * 2.55) if p.endswith("%") else round(float(p)) for p in parts[:3])
                a = float(parts[3]) if len(parts) > 3 else 1.0
            else:  # space-separated modern syntax r g b / a
                nums = re.split(r"[\s/]+", m.group(1).strip())
                r, g, b = (round(float(p.rstrip("%")) * 2.55) if p.endswith("%") else round(float(p)) for p in nums[:3])
                a = float(nums[3]) if len(nums) > 3 else 1.0
        except (ValueError, IndexError):
            warnings.append(f"unparsed color {value}")
            return None
        hexpart = "#{:02x}{:02x}{:02x}".format(max(0, min(255, r)), max(0, min(255, g)), max(0, min(255, b)))
        if a < 0.999:
            hexpart += "{:02x}".format(max(0, min(255, round(a * 255))))
        return hexpart
    warnings.append(f"unparsed color {value}")
    return None


def px(value, fallback=0.0):
    try:
        return float(str(value).rstrip("px")) if value and value != "normal" else fallback
    except ValueError:
        return fallback


def parse_gradients(bg_image, warnings, save_asset):
    """background-image layers -> pen fill dicts (gradient/image)."""
    if not bg_image or bg_image == "none":
        return []
    # Split top-level commas properly
    raw_layers, depth, buf = [], 0, ""
    for ch in bg_image:
        if ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
        if ch == "," and depth == 0:
            raw_layers.append(buf)
            buf = ""
        else:
            buf += ch
    raw_layers.append(buf)
    fills = []
    for layer in raw_layers:
        layer = layer.strip()
        m = re.match(r"(linear|radial|conic)-gradient\((.*)\)$", layer, re.S)
        if m:
            gtype, inner = m.group(1), m.group(2)
            parts, depth, buf = [], 0, ""
            for ch in inner:
                if ch == "(":
                    depth += 1
                elif ch == ")":
                    depth -= 1
                if ch == "," and depth == 0:
                    parts.append(buf.strip())
                    buf = ""
                else:
                    buf += ch
            parts.append(buf.strip())
            angle = None
            # radial/conic shape & position prelude: `circle at 50% 50%` etc.
            if parts and re.match(r"^(circle|ellipse|closest-|farthest-|at\s)", parts[0]):
                parts.pop(0)
            if parts and re.match(r"^-?[\d.]+deg$", parts[0]):
                angle = float(parts.pop(0)[:-3])
            elif parts and parts[0].startswith("to "):
                dirs = {"to top": 0.0, "to right": 90.0, "to bottom": 180.0, "to left": 270.0}
                angle = dirs.get(parts.pop(0), 180.0)
            stops = []
            for i, part in enumerate(parts):
                pm = re.match(r"(.+?)\s+(-?[\d.]+)%$", part)
                if pm:
                    col, pos = pm.group(1), float(pm.group(2)) / 100.0
                else:
                    col, pos = part, (i / max(len(parts) - 1, 1))
                c = css_color(col, warnings)
                if c:
                    stops.append({"color": c, "position": round(pos, 4)})
            if len(stops) < 2:
                if stops:
                    stops.append(dict(stops[0]))
                else:
                    continue
            pen_type = {"linear": "linear", "radial": "radial", "conic": "angular"}[gtype]
            fill = {"type": "gradient", "gradientType": pen_type, "colors": stops}
            if angle is not None and gtype == "linear":
                fill["rotation"] = (360.0 - angle) % 360.0
            fills.append(fill)
        elif layer.startswith("url("):
            url = layer[4:-1].strip().strip("\"'")
            fills.append({"type": "image", "url": save_asset(url), "mode": "cover"})
        else:
            warnings.append(f"unparsed background layer {layer[:60]}")
    return fills


def parse_shadows(shadow, warnings):
    if not shadow or shadow == "none":
        return []
    effects = []
    raw, depth, buf = [], 0, ""
    for ch in shadow:
        if ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
        if ch == "," and depth == 0:
            raw.append(buf)
            buf = ""
        else:
            buf += ch
    raw.append(buf)
    for entry in raw:
        entry = entry.strip()
        inset = "inset" in entry
        cm = _css_color.search(entry)
        hm = None if cm else re.search(r"#[0-9a-fA-F]{3,8}\b", entry)
        raw_color = cm.group(0) if cm else (hm.group(0) if hm else None)
        color = css_color(raw_color, warnings) if raw_color else None
        if cm:
            rest = entry[: cm.start()] + entry[cm.end() :]
        elif hm:
            rest = entry[: hm.start()] + entry[hm.end() :]
        else:
            rest = entry
        nums = re.findall(r"-?[\d.]+px", rest)
        if len(nums) >= 2:
            off = {"x": px(nums[0]), "y": px(nums[1])}
            blur = px(nums[2]) if len(nums) > 2 else 0.0
            effects.append({
                "type": "shadow",
                "shadowType": "inner" if inset else "outer",
                "offset": off, "blur": blur,
                "color": color or "#00000066",
            })
    return effects


def svg_to_paths(svg_markup, node_rect, warnings):
    """Serialize <svg> child shapes to pen path entries [(geometry, vb, fill, stroke, sw)]."""
    try:
        root = ET.fromstring(svg_markup)
    except ET.ParseError:
        warnings.append("unparseable svg")
        return []
    ns = "{http://www.w3.org/2000/svg}"
    vb_attr = root.get("viewBox")
    vb = [float(v) for v in vb_attr.split()] if vb_attr else [0, 0, 24, 24]
    geoms = []
    for sh in root.iter():
        tag = sh.tag.replace(ns, "")
        d = sh.get("d")
        try:
            if tag == "path" and d:
                geoms.append(d.strip())
            elif tag == "circle":
                cx, cy, r = float(sh.get("cx", 0)), float(sh.get("cy", 0)), float(sh.get("r", 0))
                geoms.append(f"M{cx - r} {cy}a{r} {r} 0 1 0 {2 * r} 0a{r} {r} 0 1 0 {-2 * r} 0")
            elif tag == "ellipse":
                cx, cy = float(sh.get("cx", 0)), float(sh.get("cy", 0))
                rx, ry = float(sh.get("rx", 0)), float(sh.get("ry", 0))
                geoms.append(f"M{cx - rx} {cy}a{rx} {ry} 0 1 0 {2 * rx} 0a{rx} {ry} 0 1 0 {-2 * rx} 0")
            elif tag == "rect":
                x, y = float(sh.get("x", 0)), float(sh.get("y", 0))
                w, h = float(sh.get("width", 0)), float(sh.get("height", 0))
                geoms.append(f"M{x} {y}h{w}v{h}h{-w}Z")
            elif tag == "line":
                geoms.append(f"M{sh.get('x1', 0)} {sh.get('y1', 0)}L{sh.get('x2', 0)} {sh.get('y2', 0)}")
            elif tag in ("polyline", "polygon"):
                pts = re.split(r"[\s,]+", (sh.get("points") or "").strip())
                pts = [p for p in pts if p]
                if len(pts) >= 4:
                    d = f"M{pts[0]} {pts[1]}" + "".join(f"L{pts[i]} {pts[i + 1]}" for i in range(2, len(pts) - 1, 2))
                    geoms.append(d + ("Z" if tag == "polygon" else ""))
        except (ValueError, TypeError):
            warnings.append(f"unparsed svg shape {tag}")
    if not geoms:
        return []
    svg_fill = root.get("fill")
    svg_stroke = root.get("stroke")
    svg_sw = float(root.get("stroke-width", 1))
    scale = (node_rect["w"] / vb[2]) if vb[2] else 1.0
    return [{
        "geometry": " ".join(geoms),
        "viewBox": vb,
        "fill": None if svg_fill == "none" else svg_fill,
        "stroke": None if svg_stroke in (None, "none") else svg_stroke,
        "strokeWidth": round(svg_sw * scale, 3),
        "linecap": root.get("stroke-linecap"),
        "linejoin": root.get("stroke-linejoin"),
    }]


class PenWriter:
    def __init__(self, assets_dir, warnings):
        self.assets_dir = assets_dir
        self.assets_dir.mkdir(parents=True, exist_ok=True)
        self.warnings = warnings
        self.counter = 0
        self.asset_cache = {}

    def nid(self):
        self.counter += 1
        return f"n{self.counter}"

    def save_asset(self, url):
        if not url.startswith("data:"):
            return url
        if url in self.asset_cache:
            return self.asset_cache[url]
        head, _, payload = url.partition(",")
        ext = "png" if "png" in head else "svg" if "svg" in head else "bin"
        try:
            raw = base64.b64decode(payload)
        except Exception:
            self.warnings.append("bad data url")
            return ""
        name = hashlib.sha256(raw).hexdigest()[:12] + "." + ext
        (self.assets_dir / name).write_bytes(raw)
        rel = f"./assets/{name}"
        self.asset_cache[url] = rel
        return rel

    def text_node(self, src, ox, oy):
        rect = src["rect"]
        node = {
            "id": self.nid(), "type": "text",
            "x": round(rect["x"] - ox, 2), "y": round(rect["y"] - oy, 2),
            "width": round(rect["w"], 2), "height": round(rect["h"], 2),
            "textGrowth": "auto",
            "content": src.get("text", ""),
            "fontSize": px(src.get("fontSize"), 13),
            "fontWeight": str(src.get("fontWeight", "400")),
            "fontFamily": src.get("fontFamily", ""),
        }
        fill = css_color(src.get("color"), self.warnings)
        if fill:
            node["fill"] = fill
        if src.get("fontStyle") == "italic":
            node["fontStyle"] = "italic"
        ls = px(src.get("letterSpacing"))
        if ls:
            node["letterSpacing"] = ls
        if "underline" in (src.get("textDecoration") or ""):
            node["underline"] = True
        if "line-through" in (src.get("textDecoration") or ""):
            node["strikethrough"] = True
        ta = src.get("textAlign")
        if ta in ("center", "right", "justify"):
            node["textAlign"] = ta
        lh, fs = px(src.get("lineHeight")), px(src.get("fontSize"), 13)
        if lh and fs and abs(lh / fs - 1.2) > 0.05:
            node["lineHeight"] = round(lh / fs, 3)
        return node

    def box_style(self, node, src):
        """Apply fill/stroke/radius/effects/opacity shared by frame & rectangle."""
        fills = parse_gradients(src.get("bgImage"), self.warnings, self.save_asset)
        bg = css_color(src.get("bg"), self.warnings)
        if bg:
            fills.append(bg)
        if fills:
            node["fill"] = fills if len(fills) > 1 else fills[0]
        # borders
        sides = {k: (px(v[0]), v[1], css_color(v[2], self.warnings))
                 for k, v in (("top", src.get("bTop")), ("right", src.get("bRight")),
                              ("bottom", src.get("bBottom")), ("left", src.get("bLeft")))}
        painted = {k: v for k, v in sides.items() if v[0] > 0 and v[1] not in (None, "none", "hidden")}
        if painted:
            colors = {v[2] for v in painted.values()}
            widths = {v[0] for v in painted.values()}
            stroke = next(v[2] for v in painted.values() if v[2]) or next(iter(colors)) or "#000000"
            node["stroke"] = stroke
            if len(widths) == 1:
                node["strokeWidth"] = widths.pop()
            else:
                node["strokeWidth"] = {k: v[0] for k, v in painted.items()}
                self.warnings.append("per-side border widths flattened")
            if len(colors - {None}) > 1:
                self.warnings.append("per-side border colors flattened")
            if any(v[1] != "solid" for v in painted.values()):
                self.warnings.append("non-solid border style flattened")
        radius = [px(r) for r in (src.get("radius") or ["0"] * 4)]
        if any(radius):
            node["cornerRadius"] = radius[0] if len(set(radius)) == 1 else [round(r, 2) for r in radius]
        effects = parse_shadows(src.get("shadow"), self.warnings)
        if effects:
            node["effect"] = effects if len(effects) > 1 else effects[0]
        if src.get("opacity", 1) < 0.999:
            node["opacity"] = src["opacity"]

    def walk(self, src, ox, oy):
        """DOM element dict -> pen node (or None when not rendered)."""
        if src.get("display") == "none" or src.get("visibility") in ("hidden", "collapse"):
            return None
        rect = src.get("rect", {})
        x, y, w, h = rect.get("x", 0) - ox, rect.get("y", 0) - oy, rect.get("w", 0), rect.get("h", 0)
        if src.get("tag") != "#text" and (w <= 0.5 or h <= 0.5):
            return None

        if src["tag"] == "#text":
            if not src.get("text"):
                return None
            return self.text_node(src, ox, oy)

        if src["tag"] == "svg":
            paths = svg_to_paths(src.get("svg", ""), src["rect"], self.warnings)
            nodes = []
            for p in paths:
                node = {
                    "id": self.nid(), "type": "path",
                    "x": round(x, 2), "y": round(y, 2),
                    "width": round(w, 2), "height": round(h, 2),
                    "geometry": p["geometry"], "viewBox": p["viewBox"],
                }
                def _resolve(v):
                    if not v or v == "none":
                        return None
                    if v == "currentColor":
                        return css_color(src.get("color"), self.warnings)
                    return css_color(v, self.warnings)

                stroke = _resolve(p["stroke"]) or _resolve(src.get("color"))
                fill = _resolve(p["fill"])
                if fill:
                    node["fill"] = fill
                elif stroke:
                    node["stroke"] = stroke
                    node["strokeWidth"] = p["strokeWidth"]
                    if p.get("linecap") in ("round", "square"):
                        node["strokeLinecap"] = p["linecap"]
                    if p.get("linejoin") in ("round", "bevel"):
                        node["strokeLinejoin"] = p["linejoin"]
                if src.get("opacity", 1) < 0.999:
                    node["opacity"] = src["opacity"]
                nodes.append(node)
            return nodes[0] if len(nodes) == 1 else {
                "id": self.nid(), "type": "group",
                "x": round(x, 2), "y": round(y, 2), "children": nodes,
            } if nodes else None

        if src["tag"] == "img":
            node = {
                "id": self.nid(), "type": "rectangle",
                "x": round(x, 2), "y": round(y, 2),
                "width": round(w, 2), "height": round(h, 2),
                "fill": {"type": "image", "url": self.save_asset(src.get("src", "")), "mode": "cover"},
            }
            radius = [px(r) for r in (src.get("radius") or ["0"] * 4)]
            if any(radius):
                node["cornerRadius"] = radius[0] if len(set(radius)) == 1 else [round(r, 2) for r in radius]
            return node

        kids_src = src.get("children", [])
        kids = [k for k in (self.walk(c, rect.get("x", 0), rect.get("y", 0)) for c in kids_src) if k]

        # Element whose only rendered child is one direct text run collapses
        # into a single text node (all runs share the parent's computed style).
        if len(kids_src) == 1 and kids_src[0].get("tag") == "#text":
            merged = dict(kids_src[0])
            merged["rect"] = src["rect"]
            merged.setdefault("opacity", 1)
            return self.text_node(merged, ox, oy)

        if kids:
            node = {
                "id": self.nid(), "type": "frame", "layout": "none",
                "x": round(x, 2), "y": round(y, 2),
                "width": round(w, 2), "height": round(h, 2),
                "children": kids,
            }
            cls = src.get("cls", "")
            if cls:
                node["name"] = cls.split()[0][:40]
            self.box_style(node, src)
            if "hidden" in (src.get("overflowX") or "") or "hidden" in (src.get("overflowY") or ""):
                node["clip"] = True
            return node

        # Leaf: text-only element, or a visually painted box.
        if src["tag"] in ("span", "a", "b", "i", "strong", "em", "code", "small", "u"):
            return self.text_node({**src, "text": src.get("text", "")}, ox, oy)
        has_visual = (
            css_color(src.get("bg"), self.warnings)
            or (src.get("bgImage") not in (None, "none"))
            or any(px(v[0]) > 0 and v[1] not in (None, "none", "hidden")
                   for v in (src.get("bTop"), src.get("bRight"), src.get("bBottom"), src.get("bLeft")))
            or (src.get("shadow") not in (None, "none"))
        )
        if has_visual:
            node = {
                "id": self.nid(), "type": "rectangle",
                "x": round(x, 2), "y": round(y, 2),
                "width": round(w, 2), "height": round(h, 2),
            }
            cls = src.get("cls", "")
            if cls:
                node["name"] = cls.split()[0][:40]
            self.box_style(node, src)
            return node
        if src.get("text"):
            return self.text_node({**src}, ox, oy)
        return None


def frame_name(stem):
    n, _, rest = stem.partition("-")
    return f"{n} {rest.replace('-', ' ').title()}"


def collect_ids(node, acc):
    acc.add(node["id"])
    for c in node.get("children", []):
        collect_ids(c, acc)


def main():
    src_dir = Path(sys.argv[1] if len(sys.argv) > 1 else "/tmp/soyo_devin_theme")
    out_path = Path(sys.argv[2] if len(sys.argv) > 2 else "docs/design/desktop-ui.pen")
    warnings = []
    from playwright.sync_api import sync_playwright

    files = sorted(src_dir.glob("*.html"))
    if not files:
        sys.exit(f"no .html frames under {src_dir}")

    writer = PenWriter(out_path.parent / "assets", warnings)
    frames = []
    with sync_playwright() as p:
        browser = p.chromium.launch(channel="chrome", headless=True)
        page = browser.new_page(viewport={"width": FRAME_W, "height": FRAME_H})
        for i, html in enumerate(files):
            page.goto(html.resolve().as_uri())
            page.wait_for_timeout(120)
            tree = page.evaluate(EXTRACT_JS)
            node = writer.walk(tree, 0, 0)
            if node is None:
                warnings.append(f"{html.name}: empty frame")
                continue
            node.update({
                "name": frame_name(html.stem),
                "x": (i % GRID_COLS) * (FRAME_W + GRID_GAP),
                "y": (i // GRID_COLS) * (FRAME_H + GRID_GAP),
                "width": FRAME_W, "height": FRAME_H,
                "clip": True,
                "layout": "none",
            })
            frames.append(node)
        browser.close()

    ids = set()
    for f in frames:
        collect_ids(f, ids)
    assert len(ids) == sum(
        1 for f in frames for _ in iter_nodes(f)
    ), "duplicate pen ids"
    doc = {"version": PEN_VERSION, "children": frames}
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(doc, ensure_ascii=False, indent=1) + "\n")
    total = len(ids)
    print(f"wrote {out_path} ({len(frames)} frames, {total} nodes)", file=sys.stderr)
    for w in sorted(set(warnings)):
        print(f"  warn: {w}", file=sys.stderr)


def iter_nodes(node):
    yield node
    for c in node.get("children", []):
        yield from iter_nodes(c)


if __name__ == "__main__":
    main()
