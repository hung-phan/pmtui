#!/usr/bin/env python3
"""Turn the UI gallery's ANSI captures into one self-contained HTML page.

The page is 1:1 WITH THE SHIPPED DASHBOARD BY CONSTRUCTION. It carries no hand-written
markup for any widget: every cell comes from a frame `pmtui` actually painted, so the
page cannot drift from the product the way a hand-drawn prototype does. Re-run it after
a UI change and the document is current again.

    scripts/pmtui-dogfood.sh --size 120x32 --quiet
    scripts/ansi-frames-to-html.py target/pmtui-ui/<stamp> docs/PMTUI-WORKBENCH-PROTOTYPE.html

Only truecolor SGR is handled (`38;2;r;g;b` / `48;2;r;g;b`) plus bold, dim, reverse and
reset, because that is all ratatui emits for these themes.
"""

from __future__ import annotations

import html
import re
import sys
from pathlib import Path

SGR = re.compile(r"\x1b\[([0-9;]*)m")

# Frames worth showing, in the order a reader should meet them, with what each one is for.
FRAMES: list[tuple[str, str, str]] = [
    (
        "main",
        "Sessions",
        "The session rail grouped by who drives each row, the selected session's live "
        "transcript beside it, and an open decision waiting on a human.",
    ),
    (
        "tasks",
        "Tasks",
        "The same sessions as cards, grouped into the columns that tell you what to do "
        "next: Paused, Needs You, Pending, Autopilot and Working.",
    ),
    (
        "task-inspector",
        "Task inspector",
        "One card opened up: its goal, its engine and tier, and the actions available on it.",
    ),
    (
        "answer",
        "Answering a decision",
        "The answer surface for an open stop, reached with s. The agent's own question and "
        "options are carried through verbatim.",
    ),
    (
        "help",
        "Key reference",
        "Every binding, from ?. This is the source of truth for the keyboard contract.",
    ),
]


def ansi_to_html(text: str) -> str:
    """One frame of ANSI into HTML spans, preserving every cell."""
    out: list[str] = []
    fg: str | None = None
    bg: str | None = None
    bold = dim = reverse = False
    open_span = False

    def style() -> str:
        f, b = (bg, fg) if reverse else (fg, bg)
        parts = []
        if f:
            parts.append(f"color:{f}")
        if b:
            parts.append(f"background:{b}")
        if bold:
            parts.append("font-weight:600")
        if dim:
            parts.append("opacity:.62")
        return ";".join(parts)

    def close() -> None:
        nonlocal open_span
        if open_span:
            out.append("</span>")
            open_span = False

    def emit(chunk: str) -> None:
        nonlocal open_span
        if not chunk:
            return
        css = style()
        if css:
            out.append(f'<span style="{css}">')
            open_span = True
        out.append(html.escape(chunk))
        close()

    pos = 0
    for m in SGR.finditer(text):
        emit(text[pos : m.start()])
        pos = m.end()
        codes = [c for c in m.group(1).split(";") if c != ""] or ["0"]
        i = 0
        while i < len(codes):
            c = codes[i]
            if c == "0":
                fg = bg = None
                bold = dim = reverse = False
            elif c == "1":
                bold = True
            elif c == "2":
                dim = True
            elif c == "7":
                reverse = True
            elif c == "22":
                bold = dim = False
            elif c == "27":
                reverse = False
            elif c == "39":
                fg = None
            elif c == "49":
                bg = None
            elif c in ("38", "48") and codes[i + 1 : i + 2] == ["2"]:
                r, g, b = (int(x) for x in codes[i + 2 : i + 5])
                colour = f"#{r:02x}{g:02x}{b:02x}"
                if c == "38":
                    fg = colour
                else:
                    bg = colour
                i += 4
            i += 1
    emit(text[pos:])
    close()
    return "".join(out)


def page(sections: list[tuple[str, str, str]], size: str) -> str:
    body = "\n".join(
        f"""  <section class="frame">
    <h2>{html.escape(title)}</h2>
    <p>{html.escape(blurb)}</p>
    <pre>{frame}</pre>
  </section>"""
        for title, blurb, frame in sections
    )
    return f"""<!doctype html>
<!--
  GENERATED - do not hand-edit. Every frame below is a capture of the real `pmtui`
  binary, so this document cannot drift from the shipped dashboard. Regenerate with:

    scripts/pmtui-dogfood.sh --size {size} --quiet
    scripts/ansi-frames-to-html.py target/pmtui-ui/<stamp> docs/PMTUI-WORKBENCH-PROTOTYPE.html

  The normative behaviour is docs/SPEC.md. Where this and the spec differ, the spec wins.
-->
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>pmtui - the shipped dashboard</title>
<style>
  :root {{ color-scheme: dark; }}
  body {{
    margin: 0; padding: 3rem 1.5rem 5rem;
    background: #1a1b26; color: #c0caf5;
    font: 15px/1.6 ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif;
  }}
  main {{ max-width: 1180px; margin: 0 auto; }}
  h1 {{ font-size: 1.6rem; margin: 0 0 .35rem; letter-spacing: -.01em; }}
  .lede {{ margin: 0 0 2.75rem; color: #9aa5ce; max-width: 62ch; }}
  .frame {{ margin: 0 0 3rem; }}
  .frame h2 {{ font-size: 1.05rem; margin: 0 0 .3rem; color: #c0caf5; }}
  .frame p {{ margin: 0 0 .9rem; color: #9aa5ce; max-width: 72ch; font-size: .92rem; }}
  pre {{
    margin: 0; padding: 1.1rem 1.25rem; overflow-x: auto;
    background: #16161e; border: 1px solid #292e42; border-radius: 10px;
    font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
    font-size: 12.5px; line-height: 1.34; white-space: pre; tab-size: 8;
  }}
  footer {{ margin-top: 3.5rem; color: #565f89; font-size: .85rem; }}
  a {{ color: #7aa2f7; }}
</style>
</head>
<body>
<main>
  <h1>pmtui - the shipped dashboard</h1>
  <p class="lede">Captured from the real binary at {html.escape(size)}, not drawn by hand, so
  every glyph, colour and column here is what the terminal actually paints. The normative
  behaviour lives in <a href="SPEC.md">docs/SPEC.md</a>.</p>
{body}
  <footer>Generated from a <code>pmtui</code> UI-gallery capture at {html.escape(size)}.</footer>
</main>
</body>
</html>
"""


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    src, dest = Path(sys.argv[1]), Path(sys.argv[2])
    sizes = sorted({p.name.split("-", 1)[0] for p in src.glob("*.ansi")})
    if not sizes:
        print(f"no .ansi frames in {src}", file=sys.stderr)
        return 1
    # The widest capture carries the most detail; a narrower one elides preview text.
    size = max(sizes, key=lambda s: int(s.split("x")[0]))

    sections = []
    for name, title, blurb in FRAMES:
        f = src / f"{size}-{name}.ansi"
        if not f.exists():
            print(f"skipping absent frame {f.name}", file=sys.stderr)
            continue
        sections.append((title, blurb, ansi_to_html(f.read_text())))
    if not sections:
        print("no requested frames were present", file=sys.stderr)
        return 1

    dest.write_text(page(sections, size))
    print(f"wrote {dest} from {len(sections)} frames at {size}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
