#!/usr/bin/env python3
"""Builds the README graphics in assets/readme/: hand-laid SVGs in trill's style, in hark's amber.

    HARK_FONTS=/path/to/JetBrainsMono-2.304/fonts/ttf python3 assets/readme/make.py

Fonts: JetBrains Mono (SIL OFL 1.1, https://github.com/JetBrains/JetBrainsMono) for text, DejaVu
Sans Mono (free license) for the few symbols it lacks. Each SVG embeds a renamed subset of just the
characters it uses, so it looks the same on every machine and stays small. Needs fonttools + brotli.

The screenshots are real: `screens/*.ansi` are `tmux capture-pane -e` captures of hark itself,
rendered cell by cell. Borders, half-block cover art and the progress bar are drawn as shapes, so
they join without gaps at any zoom.
"""

import base64, html, io, os, re
from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont

HERE = Path(__file__).resolve().parent
FONTS = Path(os.environ.get("HARK_FONTS", ""))
JB = {400: FONTS / "JetBrainsMono-Regular.ttf", 500: FONTS / "JetBrainsMono-Medium.ttf", 600: FONTS / "JetBrainsMono-SemiBold.ttf"}
FALLBACK = Path(os.environ.get("HARK_FALLBACK", "/usr/share/fonts/TTF/DejaVuSansMono.ttf"))

# palette: trill's greys, hark's amber
BG, EDGE, PANE, TILE = "#0d0d0d", "#1f1f1f", "#2a2a2a", "#1b1b1b"
TXT, MID, DIM = "#ececec", "#8f8f8f", "#5b5b5b"
ACC, ACC2 = "#ffb03b", "#ff5f6d"
W = 880

JB_CMAP = set(TTFont(JB[400]).getBestCmap()) if JB[400].exists() else set()

LOGO = [
    "    __               __  ",
    "   / /_  ____ ______/ /__",
    "  / __ \\/ __ `/ ___/ //_/",
    " / / / / /_/ / /  / ,<   ",
    "/_/ /_/\\__,_/_/  /_/|_|  ",
]


def woff2(path, text, family):
    opts = subset.Options()
    opts.flavor = "woff2"
    opts.layout_features = []
    opts.hinting = False
    opts.notdef_outline = True
    opts.name_IDs = [1, 2, 4, 6]
    f = subset.load_font(str(path), opts)
    s = subset.Subsetter(opts)
    s.populate(text=text)
    s.subset(f)
    for rec in f["name"].names:  # renamed, as the OFL asks of modified versions
        if rec.nameID in (1, 4):
            rec.string = family
        elif rec.nameID == 6:
            rec.string = family.replace(" ", "")
    buf = io.BytesIO()
    subset.save_font(f, buf, opts)
    return base64.b64encode(buf.getvalue()).decode()


class Svg:
    def __init__(self, w, h, label):
        self.w, self.h, self.label = w, h, label
        self.els = []
        self.used = {400: set(), 500: set(), 600: set()}
        self.fallback = set()
        self.defs = []

    def add(self, s):
        self.els.append(s)

    def rect(self, x, y, w, h, fill, rx=0, stroke=None, extra=""):
        st = f' stroke="{stroke}"' if stroke else ""
        rr = f' rx="{rx}"' if rx else ""
        self.add(f'<rect x="{x:.2f}" y="{y:.2f}" width="{w:.2f}" height="{h:.2f}"{rr} fill="{fill}"{st}{extra}/>')

    def text(self, x, y, s, size=13, weight=400, fill=TXT, anchor="start", ls=None, fit=None):
        for c in s:
            (self.used[weight] if ord(c) in JB_CMAP or ord(c) < 128 else self.fallback).add(c)
        attrs = f' letter-spacing="{ls}"' if ls is not None else ""
        if fit:
            attrs += f' textLength="{fit:.2f}" lengthAdjust="spacingAndGlyphs"'
        self.add(f'<text x="{x:.2f}" y="{y:.2f}" font-size="{size}" font-weight="{weight}" fill="{fill}" text-anchor="{anchor}" xml:space="preserve"{attrs}>{html.escape(s, quote=False)}</text>')

    def gradient(self, gid, a=ACC, b=ACC2, x2="1", y2="0"):
        self.defs.append(f'<linearGradient id="{gid}" x1="0" y1="0" x2="{x2}" y2="{y2}"><stop offset="0" stop-color="{a}"/><stop offset="1" stop-color="{b}"/></linearGradient>')

    def card(self):
        self.rect(0, 0, self.w, self.h, BG, rx=6)
        self.rect(0.5, 0.5, self.w - 1, self.h - 1, "none", rx=6, stroke=EDGE)

    def pane(self, x, y, w, h, title, note=""):
        """A box with its title set into the top edge, like a pane in the app."""
        self.rect(x + 0.5, y + 0.5, w, h, "none", rx=3, stroke=PANE)
        tw = len(title) * 13 * 0.6 + 14
        self.rect(x + 12, y - 2, tw, 5, BG)
        self.text(x + 18, y + 4.5, title, 13, 500, MID)
        if note:
            nw = len(note) * 12 * 0.6 + 14
            self.rect(x + w - 10 - nw, y - 2, nw, 5, BG)
            self.text(x + w - 4, y + 4.5, note, 12, 400, DIM, "end")

    def save(self, name):
        faces = []
        for wgt, chars in self.used.items():
            if chars:
                faces.append(f"@font-face{{font-family:hm;font-weight:{wgt};src:url(data:font/woff2;base64,{woff2(JB[wgt], ''.join(sorted(chars)), 'harkmono')})}}")
        if self.fallback:
            faces.append(f"@font-face{{font-family:hf;src:url(data:font/woff2;base64,{woff2(FALLBACK, ''.join(sorted(self.fallback)), 'harkmono fallback')})}}")
        style = "\n".join(faces) + "\ntext{font-family:hm,hf,ui-monospace,SFMono-Regular,Menlo,Consolas,'DejaVu Sans Mono',monospace}"
        defs = f"<defs>{''.join(self.defs)}</defs>" if self.defs else ""
        out = (
            f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {self.w} {self.h}" width="{self.w}" height="{self.h}" role="img" aria-label="{html.escape(self.label)}">\n'
            f"<style>\n{style}\n</style>\n{defs}\n" + "\n".join(self.els) + "\n</svg>\n"
        )
        (HERE / name).write_text(out, encoding="utf-8")
        print(f"{name}: {len(out) / 1024:.1f} KB")


def pill(s, x, y, label, lit=True, size=12):
    w = len(label) * size * 0.6 + 12
    if lit:
        s.rect(x, y - size + 1, w, size + 6, ACC, rx=2)
        s.text(x + w / 2, y + 1, label, size, 600, BG, "middle")
    else:
        s.text(x + w / 2, y + 1, label, size, 400, DIM, "middle")
    return w


def progress_bar(s, x, y, w, frac, h=5, gid="pg"):
    s.rect(x, y, w, h, TILE)
    s.rect(x, y, w * frac, h, f"url(#{gid})")


def pie(s, cx, cy, r, frac, color=ACC):
    """◔ ◑ ◕ as shapes (fonts disagree on them)."""
    import math

    s.add(f'<circle cx="{cx}" cy="{cy}" r="{r}" fill="none" stroke="{color}" stroke-width="1.4"/>')
    if frac >= 1:
        s.add(f'<circle cx="{cx}" cy="{cy}" r="{r}" fill="{color}"/>')
        return
    a = frac * 2 * math.pi
    x2, y2 = cx + r * math.sin(a), cy - r * math.cos(a)
    big = 1 if frac > 0.5 else 0
    s.add(f'<path d="M{cx} {cy}L{cx} {cy - r}A{r} {r} 0 {big} 1 {x2:.2f} {y2:.2f}z" fill="{color}"/>')


# ---------------------------------------------------------------------------- hero


def hero():
    h = 352
    s = Svg(W, h, "hark: podcasts in your terminal. Every open podcast, your place never lost. About 11 MB of RAM with 8 shows and 3,500 episodes, 0% CPU while paused, a 3,000-episode feed read in 19 ms, one 4.7 MB binary with nothing else to install.")
    s.gradient("lg", x2="1", y2="0")
    s.gradient("pg")
    s.card()
    for i, row in enumerate(LOGO):
        s.text(44, 70 + i * 25, row, 21, 600, "url(#lg)")
    x = 400
    s.text(x, 82, "Podcasts in your terminal.", 27, 500, TXT, ls="-.6")
    s.text(x, 112, "Every open podcast. Your place, never lost.", 14, 400, MID)
    # a now-playing line, as in the app
    s.add(f'<path d="M{x} 151L{x + 8.5} 155.5L{x} 160z" fill="{ACC}"/>')
    s.text(x + 18, 160, "Terminal Delirium", 13, 600, ACC)
    s.text(x + 18 + 18 * 7.8, 160, "Radiolab", 13, 400, MID)
    s.text(W - 102, 160, "15:33 left", 12, 400, DIM, "end")
    pill(s, W - 92, 160, "1.5×")
    s.text(x, 186, "21:31", 11, 400, MID)
    progress_bar(s, x + 42, 180, W - 44 - x - 42 - 44, 0.48)
    s.text(W - 44, 186, "44:51", 11, 400, MID, "end")
    s.add(f'<line x1="44" y1="238" x2="{W - 44}" y2="238" stroke="{PANE}"/>')
    stats = [("~11 MB", "RAM with 8 shows"), ("0%", "CPU while paused"), ("19 ms", "for 3,044 episodes"), ("4.7 MB", "one binary, no mpv")]
    cw = (W - 88) / 4
    for i, (big, small) in enumerate(stats):
        cx = 44 + i * cw
        if i:
            s.add(f'<line x1="{cx - 16:.1f}" y1="260" x2="{cx - 16:.1f}" y2="320" stroke="{PANE}"/>')
        s.text(cx, 290, big, 25, 500, TXT, ls="-.5")
        s.text(cx, 314, small, 12, 400, MID)
    s.save("hero.svg")


# ---------------------------------------------------------------------------- screenshots

PAL16 = ["#1e1e1e", "#f2777a", "#99cc99", "#ffcc66", "#6699cc", "#cc99cc", "#66cccc", "#d0d0d0", "#6b6b6b", "#ff8c8c", "#b5e8b5", "#ffe08a", "#8fb8e8", "#e0b0e0", "#8ce0e0", "#ffffff"]
TERM_FG, TERM_BG = "#d4d4d4", "#101012"


def xterm256(n):
    if n < 16:
        return PAL16[n]
    if n < 232:
        n -= 16
        lv = [0, 95, 135, 175, 215, 255]
        return "#%02x%02x%02x" % (lv[n // 36], lv[n // 6 % 6], lv[n % 6])
    v = 8 + (n - 232) * 10
    return "#%02x%02x%02x" % (v, v, v)


def mix(a, b, t):
    pa = [int(a[i : i + 2], 16) for i in (1, 3, 5)]
    pb = [int(b[i : i + 2], 16) for i in (1, 3, 5)]
    return "#%02x%02x%02x" % tuple(round(x + (y - x) * t) for x, y in zip(pa, pb))


def parse_ansi(path, cols):
    """tmux capture-pane -e → rows of cells (char, fg, bg, bold)."""
    rows = []
    for line in path.read_text(encoding="utf-8").split("\n"):
        fg, bg, bold, dim, rev = None, None, False, False, False
        cells = []
        for tok in re.split(r"(\x1b\[[0-9;]*m)", line):
            if tok.startswith("\x1b["):
                codes = [int(c) if c else 0 for c in tok[2:-1].split(";")] or [0]
                i = 0
                while i < len(codes):
                    c = codes[i]
                    if c == 0:
                        fg, bg, bold, dim, rev = None, None, False, False, False
                    elif c == 1:
                        bold = True
                    elif c == 2:
                        dim = True
                    elif c == 22:
                        bold = dim = False
                    elif c == 7:
                        rev = True
                    elif c == 27:
                        rev = False
                    elif 30 <= c <= 37:
                        fg = PAL16[c - 30]
                    elif 90 <= c <= 97:
                        fg = PAL16[c - 90 + 8]
                    elif 40 <= c <= 47:
                        bg = PAL16[c - 40]
                    elif 100 <= c <= 107:
                        bg = PAL16[c - 100 + 8]
                    elif c == 39:
                        fg = None
                    elif c == 49:
                        bg = None
                    elif c in (38, 48) and i + 1 < len(codes):
                        if codes[i + 1] == 5:
                            col = xterm256(codes[i + 2])
                            i += 2
                        else:
                            col = "#%02x%02x%02x" % tuple(codes[i + 2 : i + 5])
                            i += 4
                        if c == 38:
                            fg = col
                        else:
                            bg = col
                    i += 1
                continue
            for ch in tok:
                f, b = fg or TERM_FG, bg or TERM_BG
                if rev:
                    f, b = b, f
                if dim:
                    f = mix(f, b, 0.45)
                cells.append((ch, f, b, bold))
        cells += [(" ", TERM_FG, TERM_BG, False)] * (cols - len(cells))
        rows.append(cells[:cols])
    return rows


BOX = set("─│╭╮╰╯")
BLOCKS = {"▀": "top", "▄": "bottom", "█": "full", "▏": 1, "▎": 2, "▍": 3, "▌": 4, "▋": 5, "▊": 6, "▉": 7}


def screenshot(name, ansi, label, cols=140, rows=42, crop=None):
    cells = parse_ansi(HERE / "screens" / ansi, cols)[:rows]
    if crop:
        cells = cells[crop[0] : crop[1]]
    fs, cw, lh = 12.5, 7.5, 16.5
    pad, bar = 16, 30
    tw, th = cols * cw, len(cells) * lh
    sw, sh = tw + pad * 2, th + pad * 2 + bar
    s = Svg(round(sw), round(sh), label)
    s.rect(0, 0, sw, sh, TERM_BG, rx=10)
    s.rect(0.5, 0.5, sw - 1, sh - 1, "none", rx=10, stroke="#2c2c30")
    s.rect(1, 1, sw - 2, bar - 1, "#18181b", rx=9)
    s.rect(1, bar - 8, sw - 2, 8, "#18181b")
    s.add(f'<line x1="1" y1="{bar}" x2="{sw - 1}" y2="{bar}" stroke="#2c2c30"/>')
    for i, c in enumerate(["#ff5f57", "#febc2e", "#28c840"]):
        s.add(f'<circle cx="{20 + i * 18}" cy="{bar / 2}" r="5.5" fill="{c}"/>')
    s.text(sw / 2, bar / 2 + 4.5, "hark", 12, 500, "#9a9aa0", "middle")
    ox, oy = pad, bar + pad
    for r, row in enumerate(cells):
        y = oy + r * lh
        # backgrounds, merged into runs
        c = 0
        while c < cols:
            b = row[c][2]
            e = c
            while e < cols and row[e][2] == b:
                e += 1
            if b != TERM_BG:
                s.rect(ox + c * cw, y, (e - c) * cw + 0.3, lh + 0.3, b)
            c = e
        # shapes for borders and blocks; text in runs of one style
        c = 0
        while c < cols:
            ch, f, b, bold = row[c]
            x = ox + c * cw
            if ch in BOX:
                mx, my = x + cw / 2, y + lh / 2
                d = {"─": f"M{x} {my}H{x + cw}", "│": f"M{mx} {y}V{y + lh}",
                     "╭": f"M{x + cw} {my}H{mx + 3}Q{mx} {my} {mx} {my + 3}V{y + lh}", "╮": f"M{x} {my}H{mx - 3}Q{mx} {my} {mx} {my + 3}V{y + lh}",
                     "╰": f"M{mx} {y}V{my - 3}Q{mx} {my} {mx + 3} {my}H{x + cw}", "╯": f"M{mx} {y}V{my - 3}Q{mx} {my} {mx - 3} {my}H{x}"}[ch]
                s.add(f'<path d="{d}" fill="none" stroke="{f}" stroke-width="1.1"/>')
                c += 1
                continue
            if ch in BLOCKS:
                k = BLOCKS[ch]
                if k == "top":
                    s.rect(x, y, cw + 0.3, lh / 2 + 0.2, f)
                elif k == "bottom":
                    s.rect(x, y + lh / 2, cw + 0.3, lh / 2 + 0.3, f)
                elif k == "full":
                    s.rect(x, y, cw + 0.3, lh + 0.3, f)
                else:
                    s.rect(x, y, cw * k / 8, lh + 0.3, f)
                c += 1
                continue
            if ch == "⏸":
                s.rect(x + 1.5, y + 4, 1.8, lh - 8, f)
                s.rect(x + 4.6, y + 4, 1.8, lh - 8, f)
                c += 1
                continue
            e = c
            while e < cols and row[e][0] not in BOX and row[e][0] not in BLOCKS and row[e][0] != "⏸" and row[e][1] == f and row[e][3] == bold:
                e += 1
            run = "".join(x[0] for x in row[c:e])
            if run.strip():
                s.text(x, y + lh - 4.5, run, fs, 600 if bold else 400, f, fit=(e - c) * cw)
            c = e
    s.save(name)


# ---------------------------------------------------------------------------- features


def features():
    items = [
        ("/", "Search", "every show in Apple's directory"),
        ("o", "Discover", "top charts by genre, and For you"),
        ("f", "Preview, then follow", "play any show before following"),
        ("◔", "Your place, never lost", "saved every 10 s, smart resume"),
        ("[ ]", "Speed, not chipmunks", "0.5–3×, pitch kept (Sonic)"),
        ("a", "A queue that plays on", "A plays next, n skips to it"),
        ("L", "Show notes", "clean text, links listed"),
        ("i", "Cover art", "real images in kitty, Ghostty, iTerm2"),
        ("▶", "Media keys", "KDE widget, lock screen, playerctl"),
        ("t", "trill's look", "themes, mouse, ? for every key"),
    ]
    h = 372
    s = Svg(W, h, "What hark does, by key: " + "; ".join(f"{k}: {a}, {b}" for k, a, b in items) + ". Plus an Inbox of new episodes, In progress, OPML import from any podcast app, and the mouse.")
    s.card()
    s.pane(24, 26, W - 49, h - 53, "what it does", "press ? in hark for every key")
    for i, (k, a, b) in enumerate(items):
        col, row = i % 2, i // 2
        x, y = 52 + col * 400, 62 + row * 56
        s.rect(x + 0.5, y + 0.5, 29, 29, TILE, rx=3, stroke=PANE)
        if k == "◔":
            pie(s, x + 15, y + 15, 7, 0.3)
        elif k == "▶":
            s.add(f'<path d="M{x + 11} {y + 9}L{x + 21} {y + 15}L{x + 11} {y + 21}z" fill="{ACC}"/>')
        else:
            s.text(x + 15, y + 20, k, 14 if len(k) == 1 else 12, 600, ACC, "middle")
        s.text(x + 46, y + 12, a, 14, 500, TXT)
        s.text(x + 46, y + 30, b, 12, 400, MID)
    s.text(W / 2, h - 13, "plus an Inbox of new episodes · In progress · OPML import from any podcast app · the mouse", 12, 400, DIM, "middle")
    s.save("features.svg")


# ---------------------------------------------------------------------------- progress


def progress():
    h = 352
    s = Svg(W, h, "Your place, never lost. hark saves where you are every 10 seconds while playing, and on pause, seek, switching episodes, quitting, closing the terminal and logging out. Tested: closing the terminal saved the exact second; a kill -9 lost 6 seconds. Coming back rewinds a little for context: 3 seconds after a minute away, 10 after an hour, 20 after a day. Stopping in the last 30 seconds, the outro, marks the episode played.")
    s.gradient("pg")
    s.card()
    s.pane(24, 26, W - 49, h - 53, "your place, never lost", "tested, not promised")
    # an episode as a timeline
    x0, x1, y = 60, W - 60, 92
    progress_bar(s, x0, y, x1 - x0, 0.58, h=8)
    outro = x1 - (x1 - x0) * 0.045
    s.rect(outro, y, x1 - outro, 8, "#3a2a14")
    for i in range(1, 24):
        tx = x0 + (x1 - x0) * 0.58 * i / 24
        s.add(f'<line x1="{tx:.1f}" y1="{y - 5}" x2="{tx:.1f}" y2="{y}" stroke="{DIM}"/>')
    px = x0 + (x1 - x0) * 0.58
    s.add(f'<circle cx="{px:.1f}" cy="{y + 4}" r="6" fill="{TXT}"/>')
    s.text(x0, y - 12, "saved every 10 s", 12, 400, MID)
    s.text(px, y - 12, "you are here", 12, 500, TXT, "middle")
    s.text(x1, y - 12, "outro", 12, 400, ACC, "end")
    s.text(x0, y + 26, "0:00", 11, 400, DIM)
    s.text(x1, y + 26, "44:51", 11, 400, DIM, "end")
    cards = [
        ("Nothing to lose", ["saved every 10 s, and on", "pause, seek, switch, quit,", "a closed terminal, logout"], "tested: kill -9 lost 6 s"),
        ("Smart resume", ["back a bit, for context:", "3 s after a minute away", "10 s after an hour", "20 s after a day"], "you hear the sentence again"),
        ("Done means done", ["stop in the last 30 s and", "it's marked played;", "seek back and it isn't"], "no 'In progress' forever"),
    ]
    cw = (W - 88 - 2 * 16) / 3
    for i, (title, lines, foot) in enumerate(cards):
        x = 44 + i * (cw + 16)
        top = 150
        s.rect(x + 0.5, top + 0.5, cw, 150, TILE, rx=4, stroke=PANE)
        s.text(x + 16, top + 28, title, 14, 600, ACC)
        for j, l in enumerate(lines):
            s.text(x + 16, top + 52 + j * 18, l, 12, 400, TXT if j == 0 else MID)
        s.text(x + 16, top + 136, foot, 11, 400, DIM)
    s.save("progress.svg")


# ---------------------------------------------------------------------------- numbers


def numbers():
    h = 300
    s = Svg(W, h, "Footprint, measured on an i9-14900HX. Memory (PSS) while streaming an episode: hark 8 MB in all; mpv, tuned down, playing the same episode: 61 MB. hark idle with 8 shows and 3,514 episodes: 10.6 MB; streaming with the cover drawn: 15 to 18 MB. CPU: 0 paused, about 1.3 to 2% playing at 1.5x. A 20.7 MB feed is read in 19 ms; from a link to playing in about 0.6 s.")
    s.gradient("pg")
    s.card()
    s.pane(24, 26, W - 49, h - 53, "footprint", "measured · PSS · i9-14900HX")
    bars = [("hark", "player only, streaming", 8.1, "8.1 MB", True), ("mpv", "same episode, tuned down", 61.0, "61 MB", False), ("hark", "whole app, idle, 8 shows", 10.6, "10.6 MB", True), ("hark", "whole app, playing + art", 16.0, "15–18 MB", True)]
    full = 61.0
    x0, bw = 300, W - 300 - 140
    for i, (who, what, mb, label, ours) in enumerate(bars):
        y = 66 + i * 34
        s.text(52, y + 13, who, 13, 600 if ours else 400, ACC if ours else MID)
        s.text(96, y + 13, what, 12, 400, MID)
        w = bw * mb / full
        s.rect(x0, y + 2, w, 14, "url(#pg)" if ours else "#3a3a3a", rx=2)
        s.text(x0 + w + 10, y + 14, label, 13, 500, TXT if ours else MID)
    s.add(f'<line x1="52" y1="206" x2="{W - 52}" y2="206" stroke="{PANE}"/>')
    stats = [("0%", "CPU while paused"), ("1.3–2%", "CPU playing at 1.5×"), ("19 ms", "read a 20.7 MB feed"), ("0.6 s", "from a link to playing")]
    cw = (W - 104) / 4
    for i, (big, small) in enumerate(stats):
        x = 52 + i * cw
        s.text(x, 240, big, 20, 500, TXT, ls="-.4")
        s.text(x, 260, small, 12, 400, MID)
    s.save("numbers.svg")


# ---------------------------------------------------------------------------- how it works


def how():
    h = 330
    s = Svg(W, h, "How it works. hark reads public RSS feeds and streams episodes from wherever the show hosts them. One async thread draws the screen and does the networking; audio runs on its own thread: symphonia decodes, Sonic changes the speed without changing the pitch, cpal plays through PipeWire or PulseAudio. Feeds, show notes and the stream are cached on disk; your subscriptions, place and queue are plain files. The desktop gets MPRIS. Apple's podcast directory is asked only when you search or browse.")
    s.card()
    s.pane(24, 26, W - 49, h - 53, "how it works", "no account · no mpv · no analytics")

    def box(x, y, w, hgt, title, lines, hot=False):
        s.rect(x + 0.5, y + 0.5, w, hgt, TILE if hot else BG, rx=4, stroke=ACC if hot else PANE)
        s.text(x + 14, y + 22, title, 13, 600, ACC if hot else TXT)
        for j, l in enumerate(lines):
            s.text(x + 14, y + 42 + j * 17, l, 11.5, 400, MID)

    def arrow(x1, y1, x2, y2, label=""):
        s.add(f'<path d="M{x1} {y1}L{x2} {y2}" stroke="{DIM}" stroke-width="1.2" fill="none" marker-end="url(#ah)"/>')
        if label:
            s.text((x1 + x2) / 2, (y1 + y2) / 2 - 6, label, 10.5, 400, DIM, "middle")

    s.defs.append(f'<marker id="ah" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto"><path d="M0 0L8 4L0 8z" fill="{DIM}"/></marker>')
    box(52, 58, 196, 92, "Podcast feeds", ["public RSS, any host", "asked 'only if changed'", "8 at a time"])
    box(52, 176, 196, 92, "Apple directory", ["search and charts,", "only when you look"])
    box(318, 58, 244, 120, "hark", ["one async thread:", "screen + network", "redraws only on change", "input sleeps on its own thread"], hot=True)
    box(318, 200, 244, 68, "Audio thread", ["symphonia → Sonic → cpal", "sleeps until it's needed"])
    box(632, 58, 196, 92, "On disk", ["subscriptions.opml", "state.tsv · queue.tsv", "feeds + notes cached"])
    box(632, 176, 196, 92, "Desktop", ["MPRIS: media keys,", "KDE widget, lock screen", "PipeWire / PulseAudio"])
    arrow(248, 104, 316, 104, "feeds")
    arrow(248, 222, 316, 160, "search")
    arrow(440, 178, 440, 198)
    arrow(562, 104, 630, 104)
    arrow(562, 234, 630, 222)
    s.save("how-it-works.svg")


# ---------------------------------------------------------------------------- keys


def keys():
    groups = [
        ("Move", [("↑↓ jk", "move"), ("g G", "top / bottom"), ("Tab h l", "switch pane"), ("Enter", "open · play"), ("Esc", "back"), ("w", "to what's playing")]),
        ("Listen", [("Space", "play / pause"), ("← →", "−15 s / +30 s"), ("p", "from the start"), ("[ ]", "speed ∓0.1×"), ("+ -", "volume ±5%")]),
        ("Queue", [("a", "add / take out"), ("A", "play next"), ("n", "next in queue"), ("m", "played / not")]),
        ("Shows", [("/", "search · URL · OPML"), ("f", "follow"), ("o O", "genres"), ("L", "show notes"), ("R", "refresh now"), ("D D", "unsubscribe"), ("t i", "theme · cover")]),
    ]
    h = 318
    s = Svg(W, h, "Every key, grouped as in hark's help: " + "; ".join(f"{g}: " + ", ".join(f"{k} {d}" for k, d in ks) for g, ks in groups))
    s.card()
    s.pane(24, 26, W - 49, h - 53, "keys", "? in hark shows them all")
    cw = (W - 88) / 4
    for i, (g, ks) in enumerate(groups):
        x = 52 + i * cw
        s.text(x, 64, g, 13, 600, ACC)
        for j, (k, d) in enumerate(ks):
            y = 92 + j * 28
            kw = len(k) * 12 * 0.6 + 12
            s.rect(x + 0.5, y - 14.5, kw, 20, TILE, rx=3, stroke=PANE)
            s.text(x + kw / 2, y, k, 12, 600, TXT, "middle")
            s.text(x + kw + 8, y, d, 11, 400, MID)
    s.save("keys.svg")


if __name__ == "__main__":
    if not JB[400].exists():
        raise SystemExit("set HARK_FONTS to JetBrains Mono's fonts/ttf directory (see the top of this file)")
    hero()
    features()
    progress()
    numbers()
    how()
    keys()
    screenshot("screen-library.svg", "screen-main.ansi", "hark playing Terminal Delirium from Radiolab at 1.5×: the Library sidebar with the Inbox, In progress and Queue counts, eight shows and the episode's cover art; the Inbox lists new episodes from every show with progress marks, queued episodes and real lengths; the now-playing bar shows 15:33 left at this speed.")
    screenshot("screen-discover.svg", "screen-discover.ansi", "hark's Discover: a strip of genres, then shows For you picked from the genres of the shows you follow, each with its author, genre, number of episodes and when its latest episode came out.")
    screenshot("screen-notes.svg", "screen-notes.ansi", "hark showing an episode's show notes as clean text: paragraphs and bulleted lists of guests and sources.")
