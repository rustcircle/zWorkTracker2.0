//! Name order for ties: an approximation of 1.x's `a.localeCompare(b)` without an ICU dependency. 1.x passes no
//! locale argument, so there the webview's locale decides; this module approximates the ICU root collation.
//!
//! The rules of the root collation that decide the order of names, compared in three passes like ICU:
//! 1. base characters: spaces, then punctuation and symbols, then digits, then letters A to Z with accented Latin
//!    letters under their base letter (`É` with `E`, `ß` as `ss`, `æ` as `ae`), then other letters;
//! 2. accents, an unaccented letter first (`e` < `é` < `è`);
//! 3. case, lower case first (`task` < `Task`).
//!
//! For ASCII and Latin-1 names this approximates the root collation, with known differences from 1.x (review N2,
//! PLAN §12): `æ` and `Æ` (also `œ` and `Œ`) compare equal, `STRASSE` sorts after `Straße`, and typographic
//! punctuation sorts after all ASCII punctuation. Other scripts fall back to code point order inside their group, so
//! some of them sort differently from 1.x, as does the dotless `ı`. It only orders rows whose seconds are equal, and
//! the TypeScript breaks the remaining ties by id the same way.

use std::cmp::Ordering;

/// One collation element: the base character's group and order, its accent and its case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Element {
    group: u8,
    order: u32,
    accent: u16,
    upper: u8,
}

const GROUP_SPACE: u8 = 0;
const GROUP_PUNCT: u8 = 1;
const GROUP_DIGIT: u8 = 2;
const GROUP_LATIN: u8 = 3;
const GROUP_LETTER: u8 = 4;
const GROUP_LAST: u8 = 5;

/// ASCII punctuation and symbols in the order of the root collation; others follow by code point.
const PUNCT_ORDER: &str = "_-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";

/// Accents of the root collation's secondary order (DUCET): acute, grave, breve, circumflex, caron, ring, diaeresis,
/// double acute, tilde, dot, stroke, cedilla, ogonek, macron.
const ACUTE: u16 = 1;
const GRAVE: u16 = 2;
const CIRCUMFLEX: u16 = 4;
const RING: u16 = 6;
const DIAERESIS: u16 = 7;
const TILDE: u16 = 9;
const STROKE: u16 = 11;
const CEDILLA: u16 = 12;
/// Letters of Latin Extended-A: an accent of their own, after the Latin-1 ones.
const EXTENDED: u16 = 32;

/// Base letters of U+0100..=U+017F (Latin Extended-A), lower case, one per code point.
const EXTENDED_A: &[u8] = concat!(
    "aaaaaa",       // U+0100..=U+0105
    "cccccccc",     // U+0106..=U+010D
    "dddd",         // U+010E..=U+0111
    "eeeeeeeeee",   // U+0112..=U+011B
    "gggggggg",     // U+011C..=U+0123
    "hhhh",         // U+0124..=U+0127
    "iiiiiiiiiiii", // U+0128..=U+0133 (dotted, dotless, the ligature IJ)
    "jj",           // U+0134..=U+0135
    "kkk",          // U+0136..=U+0138 (with kra)
    "llllllllll",   // U+0139..=U+0142
    "nnnnnnnnn",    // U+0143..=U+014B (with eng)
    "oooooooo",     // U+014C..=U+0153 (with the ligature OE)
    "rrrrrr",       // U+0154..=U+0159
    "ssssssss",     // U+015A..=U+0161
    "tttttt",       // U+0162..=U+0167
    "uuuuuuuuuuuu", // U+0168..=U+0173
    "ww",           // U+0174..=U+0175
    "yyy",          // U+0176..=U+0178
    "zzzzzz",       // U+0179..=U+017E
    "s",            // U+017F, long s
)
.as_bytes();
const _: () = assert!(
    EXTENDED_A.len() == 128,
    "one base letter per code point of U+0100..=U+017F"
);

fn latin(base: u8, accent: u16, upper: bool) -> Element {
    Element {
        group: GROUP_LATIN,
        order: u32::from(base.to_ascii_lowercase().wrapping_sub(b'a')),
        accent,
        upper: u8::from(upper),
    }
}

/// The Latin-1 Supplement letter `c` (U+00C0..=U+00FF) as a base letter and accent, or `None` for the two signs.
fn latin1(c: char) -> Option<(u8, u16)> {
    let lower = c.to_lowercase().next().unwrap_or(c);
    Some(match lower {
        'à' => (b'a', GRAVE),
        'á' => (b'a', ACUTE),
        'â' => (b'a', CIRCUMFLEX),
        'ã' => (b'a', TILDE),
        'ä' => (b'a', DIAERESIS),
        'å' => (b'a', RING),
        'ç' => (b'c', CEDILLA),
        'è' => (b'e', GRAVE),
        'é' => (b'e', ACUTE),
        'ê' => (b'e', CIRCUMFLEX),
        'ë' => (b'e', DIAERESIS),
        'ì' => (b'i', GRAVE),
        'í' => (b'i', ACUTE),
        'î' => (b'i', CIRCUMFLEX),
        'ï' => (b'i', DIAERESIS),
        'ð' => (b'd', STROKE),
        'ñ' => (b'n', TILDE),
        'ò' => (b'o', GRAVE),
        'ó' => (b'o', ACUTE),
        'ô' => (b'o', CIRCUMFLEX),
        'õ' => (b'o', TILDE),
        'ö' => (b'o', DIAERESIS),
        'ø' => (b'o', STROKE),
        'ù' => (b'u', GRAVE),
        'ú' => (b'u', ACUTE),
        'û' => (b'u', CIRCUMFLEX),
        'ü' => (b'u', DIAERESIS),
        'ý' | 'ÿ' => (b'y', if lower == 'ý' { ACUTE } else { DIAERESIS }),
        _ => return None,
    })
}

fn push_elements(c: char, out: &mut Vec<Element>) {
    let upper = c.is_uppercase();
    match c {
        'a'..='z' | 'A'..='Z' => out.push(latin(c as u8, 0, upper)),
        '0'..='9' => out.push(Element {
            group: GROUP_DIGIT,
            order: u32::from(c) - u32::from('0'),
            accent: 0,
            upper: 0,
        }),
        // Expansions: the root collation reads these as two letters, set apart by case-level weight.
        'ß' => {
            out.push(latin(b's', 0, false));
            out.push(Element {
                upper: 2,
                ..latin(b's', 0, false)
            });
        }
        'æ' | 'Æ' => {
            out.push(Element {
                upper: 2,
                ..latin(b'a', 0, upper)
            });
            out.push(Element {
                upper: 2,
                ..latin(b'e', 0, upper)
            });
        }
        'œ' | 'Œ' => {
            out.push(Element {
                upper: 2,
                ..latin(b'o', 0, upper)
            });
            out.push(Element {
                upper: 2,
                ..latin(b'e', 0, upper)
            });
        }
        'þ' | 'Þ' => out.push(Element {
            group: GROUP_LATIN,
            order: 26,
            accent: 0,
            upper: u8::from(upper),
        }),
        '\u{c0}'..='\u{ff}' if latin1(c).is_some() => {
            if let Some((base, accent)) = latin1(c) {
                out.push(latin(base, accent, upper));
            }
        }
        '\u{100}'..='\u{17f}' => {
            if let Some((base, accent)) = latin1(c) {
                // U+0178, the capital of the Latin-1 `ÿ`.
                out.push(latin(base, accent, upper));
            } else {
                let i = (u32::from(c) - 0x100) as usize;
                let base = EXTENDED_A.get(i).copied().unwrap_or(b'z');
                // One accent per letter, shared by its two cases.
                let lower = u32::from(c.to_lowercase().next().unwrap_or(c));
                let accent =
                    EXTENDED.saturating_add(u16::try_from(lower.saturating_sub(0xff)).unwrap_or(0));
                out.push(latin(base, accent, upper));
            }
        }
        // Noncharacters and private use sort after every letter, as unassigned code points do in ICU.
        '\u{f0000}'..='\u{10ffff}' => out.push(Element {
            group: GROUP_LAST,
            order: u32::from(c),
            accent: 0,
            upper: 0,
        }),
        _ if c.is_whitespace() => out.push(Element {
            group: GROUP_SPACE,
            order: u32::from(c),
            accent: 0,
            upper: 0,
        }),
        _ if c.is_alphabetic() => out.push(Element {
            group: GROUP_LETTER,
            order: u32::from(c.to_lowercase().next().unwrap_or(c)),
            accent: 0,
            upper: u8::from(upper),
        }),
        _ if c.is_numeric() => out.push(Element {
            group: GROUP_DIGIT,
            order: 10 + u32::from(c),
            accent: 0,
            upper: 0,
        }),
        _ => {
            let order = PUNCT_ORDER
                .find(c)
                .and_then(|i| u32::try_from(i).ok())
                .unwrap_or(0x100 + u32::from(c));
            out.push(Element {
                group: GROUP_PUNCT,
                order,
                accent: 0,
                upper: 0,
            });
        }
    }
}

fn elements(s: &str) -> Vec<Element> {
    let mut out = Vec::with_capacity(s.len());
    for c in s.chars() {
        push_elements(c, &mut out);
    }
    out
}

/// `a.localeCompare(b)` of 1.x, approximated (see the module docs). `Equal` only for names that differ in nothing
/// the three passes look at, which for the names here means equal names.
#[must_use]
pub fn locale_compare(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let (x, y) = (elements(a), elements(b));
    let primary = |e: &Element| (e.group, e.order);
    x.iter()
        .map(primary)
        .cmp(y.iter().map(primary))
        .then_with(|| x.iter().map(|e| e.accent).cmp(y.iter().map(|e| e.accent)))
        .then_with(|| x.iter().map(|e| e.upper).cmp(y.iter().map(|e| e.upper)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extended_a_table_has_one_base_letter_per_code_point() {
        let at = |c: char| EXTENDED_A[(u32::from(c) - 0x100) as usize];
        assert_eq!(EXTENDED_A.len(), 128);
        assert_eq!(at('Ā'), b'a');
        assert_eq!(at('Č'), b'c');
        assert_eq!(at('Đ'), b'd');
        assert_eq!(at('Ę'), b'e');
        assert_eq!(at('Ğ'), b'g');
        assert_eq!(at('Ħ'), b'h');
        assert_eq!(at('ı'), b'i');
        assert_eq!(at('Ł'), b'l');
        assert_eq!(at('ň'), b'n');
        assert_eq!(at('Ő'), b'o');
        assert_eq!(at('Ř'), b'r');
        assert_eq!(at('Š'), b's');
        assert_eq!(at('ţ'), b't');
        assert_eq!(at('Ů'), b'u');
        assert_eq!(at('ŵ'), b'w');
        assert_eq!(at('Ÿ'), b'y');
        assert_eq!(at('Ž'), b'z');
        assert_eq!(at('ſ'), b's');
    }

    #[test]
    fn orders_like_the_root_collation() {
        use Ordering::Less;
        for (a, b) in [
            ("Alpha", "Beta"),
            ("alpha", "Beta"),
            ("task", "Task"),
            ("Elan", "Émile"),
            ("Émile", "Fred"),
            ("e", "é"),
            ("é", "è"),
            ("a b", "ab"),
            ("Task 10", "Task 2"),
            ("9 lives", "Alpha"),
            ("(no subtask)", "Alpha"),
            ("Ølfeld", "Oslo"),
            ("Oslo", "Østby"),
            ("Logo", "\u{10ffff}Logo"),
        ] {
            assert_eq!(locale_compare(a, b), Less, "{a} < {b}");
            assert_eq!(locale_compare(b, a), Ordering::Greater, "{b} > {a}");
        }
        assert_eq!(locale_compare("Same", "Same"), Ordering::Equal);
    }
}
