//! Drift test: ui/strings.slint against the 1.x sources it was copied from (PLAN.md 7.2 and 8).
//!
//! Each entry `out property <string> name: "text"; // <path> [- note]` must have its literal pieces (the text split at
//! `{placeholder}`s) in the named 1.x file, in that order, with every run of whitespace counted as one space. An entry
//! marked `// 2.0-only: <reason>` is not checked against 1.x but must give a reason. Every entry needs one of the two.
//!
//! The 1.x repo is `$ZWT_1X_DIR`, else the first ancestor A of this crate with `A/z-work-tracker/src/lib/themes.ts`,
//! else the first ancestor A with `A/reference/z-work-tracker/src/lib/themes.ts` (the gitignored read-only copy in the
//! main checkout). Without it the sources are not checked: the test prints a skip message (the format checks still run).

mod common;

use std::collections::BTreeSet;
use std::path::Path;

use common::one_x_dir;

const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");
const PREFIX: &str = "out property <string> ";
const TWO_ONLY: &str = "2.0-only:";

#[derive(Debug)]
struct Entry {
    line: usize,
    name: String,
    text: String,
    comment: String,
}

/// The string literal at the start of `s` (which starts after the opening quote), unescaped, and what follows it.
fn literal(s: &str) -> Result<(String, &str), String> {
    let mut out = String::new();
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Ok((out, s.get(i + 1..).unwrap_or_default())),
            '\\' => match chars.next() {
                Some((_, '"')) => out.push('"'),
                Some((_, '\\')) => out.push('\\'),
                Some((_, 'n')) => out.push('\n'),
                Some((_, '{')) => {
                    return Err(
                        "\\{ starts Slint interpolation; strings.slint must not use it".to_owned(),
                    );
                }
                other => return Err(format!("unsupported escape {other:?}")),
            },
            c => out.push(c),
        }
    }
    Err("unterminated string".to_owned())
}

fn parse(slint: &str) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    for (n, raw) in slint.lines().enumerate() {
        let line = raw.trim();
        let Some(rest) = line.strip_prefix(PREFIX) else {
            if line.contains("property") && !line.starts_with("//") {
                return Err(format!(
                    "line {}: not a single-line `{PREFIX}name: \"text\"; // source`",
                    n + 1
                ));
            }
            continue;
        };
        let (name, after) = rest
            .split_once(": \"")
            .ok_or(format!("line {}: expected `name: \"`", n + 1))?;
        let (text, tail) = literal(after).map_err(|e| format!("line {}: {e}", n + 1))?;
        let comment = tail
            .strip_prefix(';')
            .map(str::trim)
            .and_then(|t| t.strip_prefix("//"))
            .map(str::trim)
            .ok_or(format!(
                "line {}: `{name}` has no `; // <source or 2.0-only: reason>`",
                n + 1
            ))?;
        entries.push(Entry {
            line: n + 1,
            name: name.to_owned(),
            text,
            comment: comment.to_owned(),
        });
    }
    Ok(entries)
}

/// The literal pieces of a template: the text with each `{name}` placeholder taken out.
fn pieces(text: &str) -> Result<Vec<String>, String> {
    let mut out = vec![String::new()];
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let head = rest.get(..open).unwrap_or_default();
        let after = rest.get(open + 1..).unwrap_or_default();
        let close = after
            .find('}')
            .ok_or(format!("unclosed placeholder in `{text}`"))?;
        let name = after.get(..close).unwrap_or_default();
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(format!("bad placeholder `{{{name}}}` in `{text}`"));
        }
        if let Some(last) = out.last_mut() {
            last.push_str(head);
        }
        out.push(String::new());
        rest = after.get(close + 1..).unwrap_or_default();
    }
    if rest.contains('}') {
        return Err(format!("stray `}}` in `{text}`"));
    }
    if let Some(last) = out.last_mut() {
        last.push_str(rest);
    }
    out.retain(|p| !p.is_empty());
    Ok(out)
}

/// Every run of whitespace as one space, ends kept: `" includes  {x}"` stays `" includes {x}"`.
fn squash(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !space {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(c);
            space = false;
        }
    }
    out
}

/// None when every piece is in `source` in order; else the first piece that is not.
fn missing_piece(source: &str, pieces: &[String]) -> Option<String> {
    let mut from = 0usize;
    for p in pieces {
        let piece = squash(p);
        let found = source.get(from..).and_then(|rest| rest.find(&piece));
        match found {
            Some(at) => from += at + piece.len(),
            None => return Some(piece),
        }
    }
    None
}

#[test]
fn strings_slint_matches_1x_sources() {
    let path = Path::new(MANIFEST_DIR).join("ui/strings.slint");
    let slint = std::fs::read_to_string(&path);
    assert!(slint.is_ok(), "cannot read {}: {slint:?}", path.display());
    let entries = parse(&slint.unwrap_or_default());
    assert!(entries.is_ok(), "strings.slint: {entries:?}");
    let entries = entries.unwrap_or_default();
    assert!(!entries.is_empty(), "strings.slint has no entries");

    let mut names = BTreeSet::new();
    for e in &entries {
        assert!(
            names.insert(e.name.clone()),
            "line {}: `{}` twice",
            e.line,
            e.name
        );
        let p = pieces(&e.text);
        assert!(p.is_ok(), "line {}: {p:?}", e.line);
        if let Some(reason) = e.comment.strip_prefix(TWO_ONLY) {
            assert!(
                !reason.trim().is_empty(),
                "line {}: `{}` is 2.0-only without a reason",
                e.line,
                e.name
            );
        } else {
            let file = e.comment.split_whitespace().next().unwrap_or_default();
            assert!(
                file.contains('.') && !file.starts_with('/') && !file.contains(".."),
                "line {}: `{}` needs `// <1.x file>` or `// 2.0-only: <reason>`, got `// {}`",
                e.line,
                e.name,
                e.comment
            );
        }
    }

    let Some(one_x) = one_x_dir() else {
        println!(
            "SKIP parity_strings: 1.x repo not found (set ZWT_1X_DIR, or put z-work-tracker or reference/z-work-tracker in an ancestor); format checked only"
        );
        return;
    };
    let mut checked = 0usize;
    let mut two_only = 0usize;
    let mut failures = Vec::new();
    for e in &entries {
        if e.comment.starts_with(TWO_ONLY) {
            two_only += 1;
            continue;
        }
        let file = e.comment.split_whitespace().next().unwrap_or_default();
        let source = std::fs::read_to_string(one_x.join(file)).map(|s| squash(&s));
        let Ok(source) = source else {
            failures.push(format!(
                "line {} `{}`: cannot read 1.x {file}",
                e.line, e.name
            ));
            continue;
        };
        if let Some(piece) = missing_piece(&source, &pieces(&e.text).unwrap_or_default()) {
            failures.push(format!(
                "line {} `{}`: {:?} is not in 1.x {file} (in order)",
                e.line, e.name, piece
            ));
        }
        checked += 1;
    }
    assert!(
        failures.is_empty(),
        "strings.slint drifted from 1.x at {}:\n  {}\nCopy the 1.x wording verbatim, or mark a string 2.0-only with a reason.",
        one_x.display(),
        failures.join("\n  ")
    );
    println!(
        "parity_strings: {checked} entries match 1.x at {}, {two_only} marked 2.0-only",
        one_x.display()
    );
}

#[test]
fn pieces_split_at_placeholders() {
    assert_eq!(
        pieces("Controls for {task}"),
        Ok(vec!["Controls for ".to_owned()])
    );
    assert_eq!(
        pieces("{h}h {mm}m"),
        Ok(vec!["h ".to_owned(), "m".to_owned()])
    );
    assert!(pieces("{Bad}").is_err());
    let src = squash("a  Paused \"x\"\n   because you started \"y\". Only");
    let p = pieces("Paused \"{a}\" because you started \"{b}\". Only").unwrap_or_default();
    assert_eq!(missing_piece(&src, &p), None);
    assert_eq!(
        missing_piece("b a", &["a".to_owned(), "b".to_owned()]),
        Some("b".to_owned())
    );
}
