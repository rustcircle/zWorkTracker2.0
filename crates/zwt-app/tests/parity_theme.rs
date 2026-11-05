//! Drift test: ui/theme.slint against the 1.x design tokens (PLAN.md 7.2).
//!
//! Checks, for every theme in 1.x `src/lib/themes.ts`, that each token (heat ramp as heat-0..heat-5, plus bg-solid =
//! bg) has the same colour in theme.slint, that THEME_LABELS and the dark themes match, that each typography line still
//! matches the 1.x CSS rule it names, and that the sha256 in the header is the sha256 of the current themes.ts. The
//! stamp is a real SHA-256, implemented here with std only (no crates), equal to `shasum -a 256 src/lib/themes.ts` and to
//! Python's hashlib in scripts/gen_theme.py.
//!
//! The 1.x repo is `$ZWT_1X_DIR`, else the first ancestor A of this crate with `A/z-work-tracker/src/lib/themes.ts`,
//! else the first ancestor A with `A/reference/z-work-tracker/src/lib/themes.ts` (the gitignored read-only copy in the
//! main checkout). Without it the test prints a skip message and passes.

mod common;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use common::one_x_dir;

const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");
const THEMES_TS: &str = "src/lib/themes.ts";
const FIX: &str = "run `python3 -I scripts/gen_theme.py` from the repo root";

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

/// SHA-256 (FIPS 180-4) of `data`, as lowercase hex.
fn sha256_hex(data: &[u8]) -> String {
    #[rustfmt::skip]
    const K: [u32; 64] = [
        0x428a_2f98, 0x7137_4491, 0xb5c0_fbcf, 0xe9b5_dba5, 0x3956_c25b, 0x59f1_11f1, 0x923f_82a4, 0xab1c_5ed5,
        0xd807_aa98, 0x1283_5b01, 0x2431_85be, 0x550c_7dc3, 0x72be_5d74, 0x80de_b1fe, 0x9bdc_06a7, 0xc19b_f174,
        0xe49b_69c1, 0xefbe_4786, 0x0fc1_9dc6, 0x240c_a1cc, 0x2de9_2c6f, 0x4a74_84aa, 0x5cb0_a9dc, 0x76f9_88da,
        0x983e_5152, 0xa831_c66d, 0xb003_27c8, 0xbf59_7fc7, 0xc6e0_0bf3, 0xd5a7_9147, 0x06ca_6351, 0x1429_2967,
        0x27b7_0a85, 0x2e1b_2138, 0x4d2c_6dfc, 0x5338_0d13, 0x650a_7354, 0x766a_0abb, 0x81c2_c92e, 0x9272_2c85,
        0xa2bf_e8a1, 0xa81a_664b, 0xc24b_8b70, 0xc76c_51a3, 0xd192_e819, 0xd699_0624, 0xf40e_3585, 0x106a_a070,
        0x19a4_c116, 0x1e37_6c08, 0x2748_774c, 0x34b0_bcb5, 0x391c_0cb3, 0x4ed8_aa4a, 0x5b9c_ca4f, 0x682e_6ff3,
        0x748f_82ee, 0x78a5_636f, 0x84c8_7814, 0x8cc7_0208, 0x90be_fffa, 0xa450_6ceb, 0xbef9_a3f7, 0xc671_78f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    let (blocks, _) = msg.as_chunks::<64>();
    for block in blocks {
        let (words, _) = block.as_chunks::<4>();
        let mut w: Vec<u32> = words.iter().map(|b| u32::from_be_bytes(*b)).collect();
        for t in 16..64 {
            let at = |i: usize| w.get(i).copied().unwrap_or(0);
            let (w15, w2) = (at(t - 15), at(t - 2));
            let s0 = w15.rotate_right(7) ^ w15.rotate_right(18) ^ (w15 >> 3);
            let s1 = w2.rotate_right(17) ^ w2.rotate_right(19) ^ (w2 >> 10);
            let next = at(t - 16)
                .wrapping_add(s0)
                .wrapping_add(at(t - 7))
                .wrapping_add(s1);
            w.push(next);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for (k, wt) in K.iter().zip(&w) {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(*k)
                .wrapping_add(*wt);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(v);
        }
    }
    h.iter().fold(String::with_capacity(64), |mut s, x| {
        let _ = write!(s, "{x:08x}");
        s
    })
}

/// The text between the first line starting with `start` and the next line starting with `end`.
fn section<'a>(text: &'a str, start: &str, end: &str) -> Result<&'a str, String> {
    let from = text
        .find(start)
        .ok_or_else(|| format!("themes.ts: `{start}` not found"))?;
    let rest = text.get(from..).unwrap_or_default();
    let body_from = rest.find('\n').map_or(rest.len(), |i| i + 1);
    let body = rest.get(body_from..).unwrap_or_default();
    let to = body
        .find(end)
        .ok_or_else(|| format!("themes.ts: `{end}` not found after `{start}`"))?;
    Ok(body.get(..to).unwrap_or_default())
}

/// `surfaceAlt` -> `surface-alt`, as kebab() in themes.ts.
fn kebab(name: &str) -> String {
    name.chars().fold(String::new(), |mut s, c| {
        if c.is_ascii_uppercase() {
            s.push('-');
            s.push(c.to_ascii_lowercase());
        } else {
            s.push(c);
        }
        s
    })
}

type Themes = BTreeMap<String, BTreeMap<String, String>>;

/// Every theme of THEMES with its tokens (kebab-case, heat expanded) as lowercase hex.
fn ts_themes(ts: &str) -> Result<Themes, String> {
    let mut consts = BTreeMap::new();
    for item in section(ts, "const S = {", "} as const;")?.split(',') {
        if let Some((k, v)) = item.split_once(':') {
            consts.insert(
                k.trim().to_owned(),
                v.trim().trim_matches('\'').to_ascii_lowercase(),
            );
        }
    }
    let value = |v: &str| -> Result<String, String> {
        let v = v.trim();
        if let Some(name) = v.strip_prefix("S.") {
            return consts
                .get(name)
                .cloned()
                .ok_or_else(|| format!("themes.ts: unknown S.{name}"));
        }
        let hex = v.trim_matches('\'');
        if hex.len() == 7
            && hex.starts_with('#')
            && hex.chars().skip(1).all(|c| c.is_ascii_hexdigit())
        {
            Ok(hex.to_ascii_lowercase())
        } else {
            Err(format!("themes.ts: cannot read value `{v}`"))
        }
    };
    let mut themes = Themes::new();
    let mut current: Option<String> = None;
    for line in section(ts, "export const THEMES:", "\n};")?.lines() {
        if let Some(name) = line.strip_prefix("  ").and_then(|l| l.strip_suffix(": {")) {
            current = Some(name.trim().to_owned());
            continue;
        }
        let Some(theme) = current.clone() else {
            continue;
        };
        let tokens = themes.entry(theme).or_default();
        let mut rest = line.trim();
        while !rest.is_empty() {
            let Some((key, after)) = rest.split_once(':') else {
                break;
            };
            let key = key.trim().trim_start_matches(',').trim();
            let after = after.trim_start();
            if let Some(arr) = after.strip_prefix('[') {
                let (items, tail) = arr
                    .split_once(']')
                    .ok_or("themes.ts: unclosed heat array")?;
                for (i, item) in items
                    .split(',')
                    .filter(|s| !s.trim().is_empty())
                    .enumerate()
                {
                    tokens.insert(format!("{}-{i}", kebab(key)), value(item)?);
                }
                rest = tail.trim_start_matches(',').trim();
            } else {
                let (v, tail) = after.split_once(',').unwrap_or((after, ""));
                if !v.trim().is_empty() && v.trim() != "{" {
                    tokens.insert(kebab(key), value(v)?);
                }
                rest = tail.trim();
            }
        }
    }
    themes.retain(|_, t| !t.is_empty());
    if themes.is_empty() {
        return Err("themes.ts: no themes found in THEMES".to_owned());
    }
    Ok(themes)
}

/// `{ "tokens-<kebab id>": {token: hex} }` from the per-theme struct literals of theme.slint.
fn slint_themes(slint: &str) -> Themes {
    let mut themes = Themes::new();
    let mut current: Option<String> = None;
    for line in slint.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("out property <ThemeTokens> ") {
            current = rest.strip_suffix(": {").map(str::to_owned);
            continue;
        }
        if line == "};" {
            current = None;
            continue;
        }
        if let (Some(theme), Some((k, v))) = (current.as_ref(), line.split_once(':')) {
            let v = v.trim().trim_end_matches(',').to_ascii_lowercase();
            themes
                .entry(theme.clone())
                .or_default()
                .insert(k.trim().to_owned(), v);
        }
    }
    themes
}

/// `(selector, body)` of each plain CSS rule; @-rule wrappers are entered, comments dropped.
fn css_rules(css: &str) -> Vec<(String, String)> {
    let mut clean = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(i) = rest.find("/*") {
        clean.push_str(rest.get(..i).unwrap_or_default());
        rest = rest
            .get(i..)
            .and_then(|r| r.find("*/").and_then(|j| r.get(j + 2..)))
            .unwrap_or_default();
    }
    clean.push_str(rest);
    let mut out = Vec::new();
    let mut rest = clean.as_str();
    while let Some(open) = rest.find(['{', '}']) {
        let head = rest.get(..open).unwrap_or_default();
        let tail = rest.get(open + 1..).unwrap_or_default();
        let selector = head
            .rsplit(['{', '}'])
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if rest.get(open..=open) == Some("}") || selector.starts_with('@') {
            rest = tail;
            continue;
        }
        let close = tail.find('}').unwrap_or(tail.len());
        out.push((selector, tail.get(..close).unwrap_or_default().to_owned()));
        rest = tail.get(close + 1..).unwrap_or_default();
    }
    out
}

/// The value of `prop` in the first rule of `css` whose selector is `selector`, whitespace collapsed.
fn css_value(css: &str, selector: &str, prop: &str) -> Option<String> {
    css_rules(css)
        .into_iter()
        .filter(|(s, _)| s == selector)
        .find_map(|(_, body)| {
            body.split(';').find_map(|decl| {
                let (name, value) = decl.split_once(':')?;
                (name.trim() == prop)
                    .then(|| value.split_whitespace().collect::<Vec<_>>().join(" "))
            })
        })
}

#[test]
fn theme_slint_matches_1x_themes_ts() {
    let Some(one_x) = one_x_dir() else {
        println!(
            "SKIP parity_theme: 1.x repo not found (set ZWT_1X_DIR, or put z-work-tracker or reference/z-work-tracker in an ancestor)"
        );
        return;
    };
    let ts_path = one_x.join(THEMES_TS);
    let slint_path = Path::new(MANIFEST_DIR).join("ui/theme.slint");
    let ts_bytes = std::fs::read(&ts_path);
    assert!(ts_bytes.is_ok(), "cannot read {}", ts_path.display());
    let ts_bytes = ts_bytes.unwrap_or_default();
    let ts = String::from_utf8_lossy(&ts_bytes);
    let slint = read(&slint_path);
    assert!(slint.is_ok(), "{slint:?}");
    let slint = slint.unwrap_or_default();

    // 1. The header names the sha256 of the current themes.ts.
    let sha = sha256_hex(&ts_bytes);
    let header = format!(
        "// GENERATED by scripts/gen_theme.py from src/lib/themes.ts (sha256 {sha}) - do not edit"
    );
    assert_eq!(
        slint.lines().next().unwrap_or_default(),
        header,
        "theme.slint was generated from another version of {}; {FIX}",
        ts_path.display()
    );

    // 2. Every token of every theme has the same colour.
    let want = ts_themes(&ts);
    assert!(want.is_ok(), "{want:?}");
    let want = want.unwrap_or_default();
    let have = slint_themes(&slint);
    let mut checked = 0usize;
    for (theme, tokens) in &want {
        let prop = format!("tokens-{}", kebab(theme));
        let got = have.get(&prop);
        assert!(
            got.is_some(),
            "theme.slint has no `{prop}` for 1.x theme {theme}; {FIX}"
        );
        let got = got.cloned().unwrap_or_default();
        for (token, hex) in tokens {
            assert_eq!(
                got.get(token),
                Some(hex),
                "{theme}.{token}: themes.ts says {hex}; {FIX}"
            );
            checked += 1;
        }
        assert_eq!(
            got.get("bg-solid"),
            tokens.get("bg"),
            "{theme}.bg-solid must equal bg (applyTheme); {FIX}"
        );
        assert_eq!(
            got.len(),
            tokens.len() + 1,
            "{theme}: theme.slint has tokens 1.x does not: {got:?}; {FIX}"
        );
    }
    assert_eq!(
        have.len(),
        want.len(),
        "theme.slint has themes 1.x does not: {:?}; {FIX}",
        have.keys()
    );

    // 3. Labels and dark themes.
    for line in section(&ts, "export const THEME_LABELS", "\n};")
        .unwrap_or_default()
        .lines()
    {
        if let Some((id, label)) = line.trim().trim_end_matches(',').split_once(": ") {
            let label = label.trim_matches('\'');
            let dark = if id == "dark" || id == "solarizedDark" {
                "true"
            } else {
                "false"
            };
            let option = format!("{{ id: \"{id}\", label: \"{label}\", is-dark: {dark} }},");
            assert!(
                slint.contains(&option),
                "theme.slint lacks `{option}`; {FIX}"
            );
        }
    }
    assert!(
        ts.contains("theme === 'dark' || theme === 'solarizedDark' ? 'dark' : 'light'"),
        "1.x changed which themes are dark (applyTheme); update this test and {FIX}"
    );
    assert!(slint.contains(
        "out property <bool> is-dark: current == \"dark\" || current == \"solarizedDark\";"
    ));

    // 4. Typography lines: `out property <kind> name: value; // file selector { prop: css }`.
    let mut type_lines = 0usize;
    for line in slint.lines().map(str::trim) {
        let Some((decl, note)) = line.split_once("; // src/styles/") else {
            continue;
        };
        let Some((file, rule)) = note.split_once(' ') else {
            continue;
        };
        let Some((selector, inner)) = rule.split_once(" { ") else {
            continue;
        };
        let Some((prop, css)) = inner.trim_end_matches(" }").split_once(": ") else {
            continue;
        };
        let Some((head, value)) = decl.split_once(": ") else {
            continue;
        };
        let path = one_x.join("src/styles").join(file);
        let text = read(&path);
        assert!(text.is_ok(), "{text:?}");
        let found = css_value(&text.unwrap_or_default(), selector, prop);
        assert_eq!(
            found.as_deref(),
            Some(css),
            "{head}: {} `{selector}` `{prop}` changed in 1.x; {FIX}",
            path.display()
        );
        let expect = if head.contains("<string>") {
            format!(
                "\"{}\"",
                css.split(',')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .trim_matches(['\'', '"'])
            )
        } else {
            css.to_owned()
        };
        assert_eq!(
            value, expect,
            "{head} does not follow its CSS `{css}`; {FIX}"
        );
        type_lines += 1;
    }
    assert!(
        type_lines >= 3,
        "theme.slint has only {type_lines} typography lines; {FIX}"
    );

    println!(
        "parity_theme: {} themes, {checked} token values, {type_lines} typography lines match {} (sha256 {sha})",
        want.len(),
        ts_path.display()
    );
}

#[test]
fn sha256_known_answers() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha256_hex(&[b'a'; 1000]),
        "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
    );
}
