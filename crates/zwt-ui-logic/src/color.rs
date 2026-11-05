//! Colour arithmetic of 1.x: WCAG contrast and the OKLCH conversions behind the shades of a project colour.
//!
//! Ports of `lib/color.ts` (`normalizeHex`, `hexToRgb`, `luminance`, `contrast`, `fromOklch`, `THEME_BACKGROUNDS`)
//! and of `toOklch` in `features/analysis/shareSlices.ts`. The arithmetic follows the TypeScript expression for
//! expression, so a shade comes out as the same `#rrggbb` (pinned by the `golden` tests against 1.x's own output).

use std::f64::consts::PI;

use crate::js;

/// Page and card backgrounds of the four themes, the backgrounds a colour must stay visible on. The core's
/// `colors.rs` and 1.x's `lib/color.ts` hold the same values.
pub const THEME_BACKGROUNDS: [&str; 7] = [
    "#f3f4f6", "#ffffff", "#fdf6e3", "#002b36", "#17181b", "#212226", "#073642",
];

/// `normalizeHex`: reads `#rgb`, `rgb`, `#rrggbb` or `rrggbb` (any case, surrounding space ignored) and returns the
/// canonical lowercase `#rrggbb`, or `None` for anything else.
#[must_use]
pub fn normalize_hex(input: &str) -> Option<String> {
    let t = input.trim();
    let t = t.strip_prefix('#').unwrap_or(t);
    if !t.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match t.len() {
        3 => Some(
            std::iter::once('#')
                .chain(t.chars().flat_map(|c| [c, c]))
                .collect::<String>()
                .to_ascii_lowercase(),
        ),
        6 => Some(format!("#{}", t.to_ascii_lowercase())),
        _ => None,
    }
}

/// `hexToRgb`: the three channels of a colour [`normalize_hex`] accepts.
#[must_use]
pub fn hex_to_rgb(input: &str) -> Option<[u8; 3]> {
    let hex = normalize_hex(input)?;
    let channel =
        |range: std::ops::Range<usize>| hex.get(range).and_then(|p| u8::from_str_radix(p, 16).ok());
    Some([channel(1..3)?, channel(3..5)?, channel(5..7)?])
}

/// `parseHex`: the channels of `#rrggbb`. 1.x reads anything else as black (its `parseInt` gives `NaN`), and so
/// does this, for the colours it is ever given (shades and theme backgrounds, always `#rrggbb`).
fn parse_hex(hex: &str) -> [u8; 3] {
    hex_to_rgb(hex).unwrap_or([0, 0, 0])
}

/// `luminance`: WCAG relative luminance.
#[must_use]
pub fn luminance(hex: &str) -> f64 {
    let [r, g, b] = parse_hex(hex).map(|v| {
        let c = f64::from(v) / 255.0;
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    });
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// `contrast`: the WCAG contrast ratio of two colours, 1 to 21.
#[must_use]
pub fn contrast(a: &str, b: &str) -> f64 {
    let la = luminance(a);
    let lb = luminance(b);
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// A colour in OKLCH.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Oklch {
    /// Lightness 0 to 1.
    pub l: f64,
    /// Chroma, about 0 to 0.4.
    pub c: f64,
    /// Hue in degrees, 0 to 360.
    pub h: f64,
}

fn to_linear(v: u8) -> f64 {
    let c = f64::from(v) / 255.0;
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn to_gamma(c: f64) -> f64 {
    if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// `toOklch`: the OKLCH values of a hex colour, or `None` when it is not one.
#[must_use]
pub fn to_oklch(hex: &str) -> Option<Oklch> {
    let [r, g, b] = hex_to_rgb(hex)?.map(to_linear);
    let l = (0.412_221_47 * r + 0.536_332_54 * g + 0.051_445_99 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_55 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_84 * g + 0.629_978_7 * b).cbrt();
    let a = 1.977_998_5 * l - 2.428_592_21 * m + 0.450_593_71 * s;
    let bb = 0.025_904_04 * l + 0.782_771_77 * m - 0.808_675_77 * s;
    let c = a.hypot(bb);
    let h = if c < 1e-4 {
        0.0
    } else {
        ((bb.atan2(a) * 180.0) / PI % 360.0 + 360.0) % 360.0
    };
    Some(Oklch {
        l: 0.210_454_26 * l + 0.793_617_79 * m - 0.004_072_05 * s,
        c,
        h,
    })
}

/// `fromOklch`: the sRGB colour of these OKLCH values as `#rrggbb`, or `None` when they fall outside sRGB.
#[must_use]
pub fn from_oklch(l: f64, c: f64, hue_degrees: f64) -> Option<String> {
    let rad = (hue_degrees * PI) / 180.0;
    let a = c * rad.cos();
    let b = c * rad.sin();
    let lp = (l + 0.396_337_78 * a + 0.215_803_76 * b).powf(3.0);
    let mp = (l - 0.105_561_35 * a - 0.063_854_17 * b).powf(3.0);
    let sp = (l - 0.089_484_18 * a - 1.291_485_55 * b).powf(3.0);
    let rgb = [
        4.076_741_66 * lp - 3.307_711_59 * mp + 0.230_969_93 * sp,
        -1.268_438 * lp + 2.609_757_4 * mp - 0.341_319_4 * sp,
        -0.004_196_09 * lp - 0.703_418_61 * mp + 1.707_614_7 * sp,
    ];
    // `contains` is false for NaN, like the `!(v >= -0.0001 && v <= 1.0001)` of 1.x.
    if rgb.iter().any(|v| !(-0.0001..=1.0001).contains(v)) {
        return None;
    }
    let mut out = String::with_capacity(7);
    out.push('#');
    for v in rgb {
        // In 0..=255 after the clamp, so the cast neither truncates nor wraps.
        let byte = js::round(to_gamma(v.clamp(0.0, 1.0)) * 255.0) as u8;
        out.push_str(&format!("{byte:02x}"));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_the_forms_1x_accepts() {
        assert_eq!(normalize_hex("#ABC").as_deref(), Some("#aabbcc"));
        assert_eq!(normalize_hex(" 2563EB ").as_deref(), Some("#2563eb"));
        assert_eq!(normalize_hex("#2563eb").as_deref(), Some("#2563eb"));
        assert_eq!(normalize_hex("var(--accent)"), None);
        assert_eq!(normalize_hex("#12345"), None);
        assert_eq!(hex_to_rgb("#2563eb"), Some([0x25, 0x63, 0xeb]));
    }

    #[test]
    fn contrast_is_wcag() {
        assert!((contrast("#000000", "#ffffff") - 21.0).abs() < 1e-9);
        assert!((contrast("#777777", "#777777") - 1.0).abs() < 1e-12);
    }

    #[test]
    fn oklch_round_trips_a_palette_colour() {
        for hex in ["#2563eb", "#dc2626", "#059669", "#d97706"] {
            let o = to_oklch(hex).unwrap();
            assert_eq!(from_oklch(o.l, o.c, o.h).as_deref(), Some(hex));
        }
        assert_eq!(to_oklch("#808080").map(|o| o.h), Some(0.0));
        assert_eq!(to_oklch("nope"), None);
    }
}
