#!/usr/bin/env python3
"""Generate crates/zwt-app/ui/theme.slint from the 1.x design tokens. Standard library only.

    python3 -I scripts/gen_theme.py [ONE_X_DIR] [--check] [--out PATH]

Colours, theme ids, labels and the dark flag come from the 1.x file src/lib/themes.ts (THEMES, THEME_LABELS,
applyTheme); the file's sha256 goes into the header, and crates/zwt-app/tests/parity_theme.rs checks it. Fonts, the
ticker clock and the radii come from the 1.x CSS rules named in TYPE_TOKENS; each generated line names its rule, and
the same test re-reads it.

The 1.x repo is ONE_X_DIR, else $ZWT_1X_DIR, else the first ancestor A of this repo (itself included) that has
A/z-work-tracker/src/lib/themes.ts, else the first that has A/reference/z-work-tracker/src/lib/themes.ts (the gitignored
read-only copy in the main checkout). --check writes nothing and exits 1 when theme.slint differs from what this script
would write. Output is deterministic: run it twice, get the same bytes.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
OUT = REPO / "crates" / "zwt-app" / "ui" / "theme.slint"
THEMES_TS = Path("src/lib/themes.ts")

# (token, Slint type, 1.x CSS file, selector, CSS property). A family token takes the first family of the stack:
# Slint's font-family names one family, and on a system without it Slint falls back to its default font.
TYPE_TOKENS: list[tuple[str, str, str, str, str]] = [
    ("font-ui", "string", "src/styles/base.css", "body", "font-family"),
    ("font-display", "string", "src/styles/base.css", "h1, h2, h3, h4", "font-family"),
    ("font-mono", "string", "src/styles/ui.css", "code", "font-family"),
    ("font-size-base", "length", "src/styles/base.css", "body", "font-size"),
    ("font-weight-heading", "int", "src/styles/base.css", "h1, h2, h3, h4", "font-weight"),
    ("font-family-ticker-clock", "string", "src/styles/tasks.css", ".ticker-clock", "font-family"),
    ("font-size-ticker-clock", "length", "src/styles/tasks.css", ".ticker-clock", "font-size"),
    ("font-weight-ticker-clock", "int", "src/styles/tasks.css", ".ticker-clock", "font-weight"),
    ("font-size-task-name", "length", "src/styles/tasks.css", ".task-name", "font-size"),
    ("font-size-rail-title", "length", "src/styles/shell.css", ".rail-head h2", "font-size"),
    ("font-size-rail-group-title", "length", "src/styles/shell.css", ".rail-group-title", "font-size"),
    ("font-weight-rail-group-title", "int", "src/styles/shell.css", ".rail-group-title", "font-weight"),
    ("font-weight-tab", "int", "src/styles/shell.css", ".tab", "font-weight"),
    ("font-size-summary-headline", "length", "src/styles/timeline.css", ".summary-headline", "font-size"),
    ("font-size-summary-total", "length", "src/styles/timeline.css", ".summary-headline strong", "font-size"),
    ("font-size-day-title", "length", "src/styles/timeline.css", ".day-title h3", "font-size"),
    ("font-size-axis-label", "length", "src/styles/timeline.css", ".axis-label", "font-size"),
    ("font-size-block-text", "length", "src/styles/timeline.css", ".block-text", "font-size"),
    ("font-weight-block-text", "int", "src/styles/timeline.css", ".block-text", "font-weight"),
    ("font-size-legend", "length", "src/styles/timeline.css", ".legend ul", "font-size"),
    ("radius", "length", "src/styles/ui.css", ":root", "--radius"),
    ("radius-sm", "length", "src/styles/ui.css", ":root", "--radius-sm"),
]


class SourceError(Exception):
    pass


def kebab(name: str) -> str:
    """The same rule as kebab() in themes.ts: surfaceAlt -> surface-alt."""
    return re.sub(r"[A-Z]", lambda m: "-" + m.group(0).lower(), name)


def find_1x(arg: str | None) -> Path:
    if arg:
        p = Path(arg).expanduser().resolve()
        return p.parents[2] if p.is_file() and p.name == "themes.ts" else p
    env = os.environ.get("ZWT_1X_DIR")
    if env:
        return Path(env).expanduser().resolve()
    # Two passes, as in the drift tests: a 1.x checkout beside any ancestor wins over the reference copy.
    # os.path.isfile is False on any OSError (an unreadable folder too), like Rust's Path::is_file.
    for rel in (Path("z-work-tracker"), Path("reference") / "z-work-tracker"):
        for a in (REPO, *REPO.parents):
            if os.path.isfile(a / rel / THEMES_TS):
                return a / rel
    raise SourceError(
        "1.x repo not found: pass its path, set ZWT_1X_DIR, or put z-work-tracker or reference/z-work-tracker in an"
        " ancestor of this repo"
    )


def block(text: str, start_pat: str, end_pat: str) -> str:
    m = re.search(start_pat, text, re.M)
    if not m:
        raise SourceError(f"themes.ts: no match for {start_pat!r}")
    e = re.search(end_pat, text[m.end():], re.M)
    if not e:
        raise SourceError(f"themes.ts: no end {end_pat!r} after {start_pat!r}")
    return text[m.end(): m.end() + e.start()]


HEX = r"#[0-9a-fA-F]{6}"


def parse_themes(ts: str) -> tuple[list[str], dict[str, dict[str, str]], dict[str, str], list[str]]:
    """Token order, {theme: {kebab token: #hex}} in THEMES order, labels, dark theme ids."""
    iface = block(ts, r"^export interface Tokens \{", r"^\}")
    order = re.findall(r"^\s+(\w+): (?:string|readonly \[)", iface, re.M)
    s_consts = dict(re.findall(r"(\w+): '(" + HEX + r")'", block(ts, r"^const S = \{", r"^\} as const;")))
    themes_src = block(ts, r"^export const THEMES: Record<Theme, Tokens> = \{", r"^\};")
    themes: dict[str, dict[str, str]] = {}
    for m in re.finditer(r"^  (\w+): \{\n(.*?)\n  \},", themes_src, re.M | re.S):
        theme, body = m.group(1), m.group(2)

        def value(v: str) -> str:
            v = v.strip()
            if re.fullmatch("'" + HEX + "'", v):
                return v.strip("'").lower()
            if v.startswith("S.") and v[2:] in s_consts:
                return s_consts[v[2:]].lower()
            raise SourceError(f"themes.ts: {theme}: cannot read value {v!r}")

        tokens: dict[str, str] = {}
        for key, arr, val in re.findall(r"(\w+): (?:\[([^\]]*)\]|([^,\n\]]+))", body):
            if kebab(key) in tokens or f"{kebab(key)}-0" in tokens:
                raise SourceError(f"themes.ts: {theme}.{key} twice")
            if arr:
                for i, item in enumerate(x for x in arr.split(",") if x.strip()):
                    tokens[f"{kebab(key)}-{i}"] = value(item)
            else:
                tokens[kebab(key)] = value(val)
        themes[theme] = tokens
    if not themes:
        raise SourceError("themes.ts: no themes in THEMES")
    flat: list[str] = []
    first = next(iter(themes.values()))
    for key in order:
        k = kebab(key)
        flat += [k] if k in first else sorted((t for t in first if t.startswith(k + "-")), key=lambda t: int(t.rsplit("-", 1)[1]))
    for theme, tokens in themes.items():
        if sorted(tokens) != sorted(flat):
            raise SourceError(f"themes.ts: {theme} has tokens {sorted(set(tokens) ^ set(flat))} that differ from interface Tokens")
    labels = dict(re.findall(r"^  (\w+): '([^']*)',", block(ts, r"^export const THEME_LABELS: Record<Theme, string> = \{", r"^\};"), re.M))
    if sorted(labels) != sorted(themes):
        raise SourceError("themes.ts: THEME_LABELS and THEMES name different themes")
    scheme = re.search(r"colorScheme = (.*?) \? 'dark' : 'light';", ts)
    if not scheme:
        raise SourceError("themes.ts: applyTheme colorScheme line not found")
    dark = re.findall(r"theme === '(\w+)'", scheme.group(1))
    if not dark or any(d not in themes for d in dark):
        raise SourceError("themes.ts: dark theme ids not found in applyTheme")
    return flat, themes, labels, dark


def css_rules(css: str) -> list[tuple[str, str]]:
    """(selector, body) of every plain rule; @-rule wrappers are entered, not returned."""
    css = re.sub(r"/\*.*?\*/", "", css, flags=re.S)
    out: list[tuple[str, str]] = []
    start = 0
    i = 0
    while i < len(css):
        c = css[i]
        if c == "{":
            sel = " ".join(css[start:i].split())
            if sel.startswith("@"):
                start = i + 1
            else:
                j = css.find("}", i)
                if j < 0:
                    break
                out.append((sel, css[i + 1: j]))
                i = j
                start = j + 1
        elif c == "}":
            start = i + 1
        i += 1
    return out


def css_value(one_x: Path, file: str, selector: str, prop: str) -> str:
    path = one_x / file
    try:
        css = path.read_text(encoding="utf-8")
    except OSError as e:
        raise SourceError(f"{file}: {e}") from e
    want = " ".join(selector.split())
    for sel, body in css_rules(css):
        if sel != want:
            continue
        for decl in body.split(";"):
            name, sep, val = decl.partition(":")
            if sep and name.strip() == prop:
                return " ".join(val.split())
    raise SourceError(f"{file}: no '{prop}' in rule '{selector}'")


def slint_value(kind: str, css: str, where: str) -> str:
    if kind == "string":
        first = css.split(",")[0].strip().strip("'\"")
        return '"' + first + '"'
    if kind == "length":
        if not re.fullmatch(r"\d+(\.\d+)?px", css):
            raise SourceError(f"{where}: expected a px length, got {css!r}")
        return css
    if kind == "int":
        if not re.fullmatch(r"\d+", css):
            raise SourceError(f"{where}: expected an integer, got {css!r}")
        return css
    raise SourceError(f"unknown kind {kind}")


def generate(one_x: Path) -> str:
    ts_path = one_x / THEMES_TS
    try:
        raw = ts_path.read_bytes()
    except OSError as e:
        raise SourceError(f"{ts_path}: {e}") from e
    digest = hashlib.sha256(raw).hexdigest()
    flat, themes, labels, dark = parse_themes(raw.decode("utf-8"))
    ids = list(themes)
    prop = {t: "tokens-" + kebab(t) for t in ids}
    width = max(len(k) for k in flat + ["bg-solid"])
    L: list[str] = []
    w = L.append
    w(f"// GENERATED by scripts/gen_theme.py from src/lib/themes.ts (sha256 {digest}) - do not edit")
    w("// Regenerate: python3 -I scripts/gen_theme.py [<1.x repo>]. Check: python3 -I scripts/gen_theme.py --check.")
    w("// Colours: THEMES, THEME_LABELS and applyTheme() of 1.x src/lib/themes.ts; token names are its CSS variables")
    w("// without the leading --, heat ramp as heat-0..heat-5, plus bg-solid (= bg, set by applyTheme). Every value is an")
    w("// opaque colour. Typography and radii: the 1.x CSS rule named at the end of each line (tests/parity_theme.rs reads")
    w("// it again); a family token is the first family of that stack, since Slint's font-family takes one name.")
    w("")
    w("/// The colour tokens of one theme.")
    w("export struct ThemeTokens {")
    for k in flat + ["bg-solid"]:
        w(f"    {k}: color,")
    w("}")
    w("")
    w("/// One theme as a picker lists it, in 1.x THEMES order.")
    w("export struct ThemeOption {")
    w("    id: string,")
    w("    label: string,")
    w("    is-dark: bool,")
    w("}")
    w("")
    w("export global Theme {")
    w(f'    /// The active theme id: {", ".join(chr(34) + t + chr(34) for t in ids)}. Light is the default; an unknown')
    w("    /// id shows Light. The OS theme is not followed (1.x rule).")
    w(f'    in-out property <string> current: "{ids[0]}";')
    for t in ids:
        w("")
        w(f"    // theme {t}: {labels[t]}")
        w(f"    out property <ThemeTokens> {prop[t]}: {{")
        for k in flat:
            w(f"        {k + ':':<{width + 1}} {themes[t][k]},")
        w(f"        {'bg-solid:':<{width + 1}} {themes[t]['bg']},")
        w("    };")
    w("")
    chain = " : ".join(f'current == "{t}" ? {prop[t]}' for t in reversed(ids[1:]))
    w("    /// The tokens of the active theme.")
    w(f"    out property <ThemeTokens> tokens: {chain} : {prop[ids[0]]};")
    chain = " : ".join(f'current == "{t}" ? "{labels[t]}"' for t in reversed(ids[1:]))
    w("    /// The active theme's name as 1.x shows it (THEME_LABELS).")
    w(f'    out property <string> label: {chain} : "{labels[ids[0]]}";')
    w("    /// True for the themes 1.x gives colorScheme 'dark'.")
    w("    out property <bool> is-dark: " + " || ".join(f'current == "{t}"' for t in dark) + ";")
    w("    out property <[ThemeOption]> options: [")
    for t in ids:
        w(f'        {{ id: "{t}", label: "{labels[t]}", is-dark: {"true" if t in dark else "false"} }},')
    w("    ];")
    w("")
    w("    // The active theme's tokens, one property each: Theme.bg, Theme.surface-alt, Theme.c-worked, Theme.heat-0 ...")
    for k in flat + ["bg-solid"]:
        w(f"    out property <color> {k}: tokens.{k};")
    heat = [k for k in flat if k.startswith("heat-")]
    w("    /// The heat ramp of the active theme, low to high.")
    w("    out property <[color]> heat: [" + ", ".join(f"tokens.{k}" for k in heat) + "];")
    w("")
    w("    // Typography and radii, the same in every theme.")
    for name, kind, file, selector, css_prop in TYPE_TOKENS:
        css = css_value(one_x, file, selector, css_prop)
        val = slint_value(kind, css, f"{file} {selector} {css_prop}")
        w(f"    out property <{kind}> {name}: {val}; // {file} {selector} {{ {css_prop}: {css} }}")
    w("}")
    return "\n".join(L) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("one_x", nargs="?", help="1.x repo root (default: $ZWT_1X_DIR, else z-work-tracker, then reference/z-work-tracker, in an ancestor)")
    ap.add_argument("--check", action="store_true", help="exit 1 if theme.slint differs from the generated text")
    ap.add_argument("--out", type=Path, default=OUT, help=f"output file (default: {OUT.relative_to(REPO)})")
    a = ap.parse_args()
    try:
        one_x = find_1x(a.one_x)
        text = generate(one_x)
    except SourceError as e:
        print(f"gen_theme.py: {e}", file=sys.stderr)
        return 2
    if a.check:
        try:
            current = a.out.read_text(encoding="utf-8")
        except OSError:
            current = None
        if current != text:
            print(f"gen_theme.py: {a.out} is out of date with {one_x / THEMES_TS} and its CSS; run python3 -I scripts/gen_theme.py", file=sys.stderr)
            return 1
        print(f"gen_theme.py: {a.out.name} is up to date with {one_x / THEMES_TS}")
        return 0
    a.out.parent.mkdir(parents=True, exist_ok=True)
    a.out.write_text(text, encoding="utf-8")
    print(f"gen_theme.py: wrote {a.out} from {one_x / THEMES_TS}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
