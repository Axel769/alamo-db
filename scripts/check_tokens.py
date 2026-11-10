"""Contrast check of app/src/tokens.css (§9.6.1) and hex export for theme.ts.
Usage: python3 -I check_tokens.py <tokens.css>
"""
import math, re, sys

src = open(sys.argv[1], encoding="utf-8").read()
src = re.sub(r"/\*.*?\*/", "", src, flags=re.S)


def block(selector):
    m = re.search(re.escape(selector) + r"\s*\{(.*?)\}", src, flags=re.S)
    out = {}
    for name, val in re.findall(r"--([a-z0-9-]+)\s*:\s*([^;]+);", m.group(1)):
        out[name] = val.strip()
    return out


def oklch_to_srgb(L, C, h):
    a = C * math.cos(math.radians(h))
    b = C * math.sin(math.radians(h))
    l_ = L + 0.3963377774 * a + 0.2158037573 * b
    m_ = L - 0.1055613458 * a - 0.0638541728 * b
    s_ = L - 0.0894841775 * a - 1.2914855480 * b
    l, m, s = l_**3, m_**3, s_**3
    r = 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s
    g = -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s
    bb = -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s
    return r, g, bb


def gamma(c):
    return 12.92 * c if c <= 0.0031308 else 1.055 * c ** (1 / 2.4) - 0.055


def parse(val):
    m = re.match(r"oklch\(\s*([\d.]+)%\s+([\d.]+)\s+([\d.]+)\s*\)", val)
    if m:
        L, C, h = float(m.group(1)) / 100, float(m.group(2)), float(m.group(3))
        lin = oklch_to_srgb(L, C, h)
        clipped = any(c < -0.002 or c > 1.002 for c in lin)
        lin = tuple(min(1, max(0, c)) for c in lin)
        return lin, clipped
    m = re.match(r"#([0-9a-fA-F]{6})", val)
    if m:
        v = int(m.group(1), 16)
        srgb = ((v >> 16) / 255, ((v >> 8) & 255) / 255, (v & 255) / 255)
        lin = tuple(c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4 for c in srgb)
        return lin, False
    raise ValueError(val)


def hexof(lin):
    return "#" + "".join("%02X" % round(gamma(c) * 255) for c in lin)


def lum(lin):
    r, g, b = lin
    return 0.2126 * r + 0.7152 * g + 0.0722 * b


def contrast(a, b):
    la, lb = lum(a), lum(b)
    hi, lo = max(la, lb), min(la, lb)
    return (hi + 0.05) / (lo + 0.05)


TEXT = ["cell-fg", "cell-muted-fg", "cell-null-fg", "type-numeric", "type-temporal", "type-text", "type-boolean", "type-structured", "cell-prod", "ui-danger"]
BG = ["state-normal", "state-alt", "state-selection", "state-modified", "state-new", "state-error", "cell-header-bg", "cell-panel-bg"]
SYN = ["syn-keyword", "syn-type", "syn-builtin", "syn-number", "syn-string", "syn-quoted", "syn-comment", "syn-operator"]

GLIDE = {
    "typeNumeric": "type-numeric", "typeTemporal": "type-temporal", "typeText": "type-text",
    "typeBoolean": "type-boolean", "typeStructured": "type-structured", "stateNormal": "state-normal",
    "stateAlt": "state-alt", "stateSelection": "state-selection", "stateModified": "state-modified",
    "stateNew": "state-new", "stateDeleted": "state-error", "cellFg": "cell-fg", "cellMutedFg": "cell-muted-fg",
    "cellNullFg": "cell-null-fg", "cellBorder": "cell-border", "cellDivider": "cell-divider",
    "cellPanelBg": "cell-panel-bg", "cellHeaderBg": "cell-header-bg", "cellSelectionBorder": "cell-selection-border",
}

fail = 0
ring_tokens = None
for theme, sel in (("light", ":root"), ("dark", ':root[data-theme="dark"]')):
    toks = block(sel)
    col = {}
    for k, v in toks.items():
        try:
            col[k], clipped = parse(v)
            if clipped:
                print(f"[{theme}] {k} out of sRGB gamut: {v}")
                fail += 1
        except ValueError:
            pass
    worst_text = 99
    for t in TEXT:
        for b in BG:
            if t not in col or b not in col:
                continue
            if t == "cell-prod" and b not in ("state-normal", "state-alt", "cell-header-bg", "cell-panel-bg"):
                continue
            c = contrast(col[t], col[b])
            worst_text = min(worst_text, c)
            if c < 4.5:
                print(f"[{theme}] FAIL text {t} on {b}: {c:.2f}")
                fail += 1
    worst_syn = 99
    for t in SYN:
        for b in ("state-normal", "state-alt"):
            c = contrast(col[t], col[b])
            worst_syn = min(worst_syn, c)
            if c < 4.5:
                print(f"[{theme}] FAIL syntax {t} on {b}: {c:.2f}")
                fail += 1
    ui = [("cell-selection-border", "state-normal"), ("cell-selection-border", "state-alt"), ("cell-divider", "state-normal"),
          ("cell-divider", "cell-header-bg"), ("chrome-fg", "chrome-bg"), ("chrome-fg", "chrome-active"),
          ("ui-accent-fg", "cell-selection-border"), ("chrome-muted", "chrome-bg"), ("cell-fg", "chrome-field")]
    worst_ui = 99
    for a, b in ui:
        if a not in col or b not in col:
            continue
        c = contrast(col[a], col[b])
        need = 4.5 if a in ("chrome-fg", "ui-accent-fg", "chrome-muted") else 3.0
        if a == "cell-divider":
            print(f"[{theme}] note divider {a} on {b}: {c:.2f} (decorative since the 2026-10 mock)")
            continue
        worst_ui = min(worst_ui, c)
        if c < need:
            print(f"[{theme}] FAIL ui {a} on {b}: {c:.2f} (need {need})")
            fail += 1
    if "cell-focus-ring-inner" not in col:
        col["cell-focus-ring-inner"], col["cell-focus-ring-outer"] = ring_tokens
    ring_tokens = (col["cell-focus-ring-inner"], col["cell-focus-ring-outer"])
    ring = max(contrast(col["cell-focus-ring-inner"], col["state-normal"]), contrast(col["cell-focus-ring-outer"], col["state-normal"]))
    selalt = contrast(col["state-selection"], col["state-alt"])
    print(f"[{theme}] worst text {worst_text:.2f} · syntax {worst_syn:.2f} · ui {worst_ui:.2f} · focus ring {ring:.2f} · selection/alt {selalt:.3f}")
    print(f"export const {theme.upper()}: Tokens = {{")
    for js, css in GLIDE.items():
        print(f'  {js}: "{hexof(col[css])}",')
    print("};")

print("FAILURES:", fail)
sys.exit(1 if fail else 0)
