//! Picks a colour no active project or tag uses. Perceptual distance in OKLab, readable on every theme.

use crate::validate::clean_color;

/// Page and card backgrounds of the four themes. `src/lib/themes.ts` holds the same values.
const THEME_BACKGROUNDS: [&str; 7] = [
    "#f3f4f6", "#ffffff", "#fdf6e3", "#002b36", "#17181b", "#212226", "#073642",
];
/// Below this the colour is hard to see against a background.
const MIN_CONTRAST: f64 = 2.0;
/// OKLab distance that reads as a different colour. Relaxed only if the store is very crowded.
const DISTINCT: f64 = 0.09;
const TRIES: u32 = 400;

#[derive(Clone, Copy)]
struct Oklab(f64, f64, f64);

fn channels(hex: &str) -> Option<[f64; 3]> {
    let h = hex.strip_prefix('#')?;
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let part = |r: std::ops::Range<usize>| {
        h.get(r)
            .and_then(|p| u8::from_str_radix(p, 16).ok())
            .map(|v| f64::from(v) / 255.0)
    };
    Some([part(0..2)?, part(2..4)?, part(4..6)?])
}

fn linear(c: f64) -> f64 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn gamma(c: f64) -> f64 {
    if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

fn oklab(hex: &str) -> Option<Oklab> {
    let [r, g, b] = channels(hex)?.map(linear);
    let l = (0.412_221_47 * r + 0.536_332_54 * g + 0.051_445_99 * b).cbrt();
    let m = (0.211_903_50 * r + 0.680_699_55 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_84 * g + 0.629_978_70 * b).cbrt();
    Some(Oklab(
        0.210_454_26 * l + 0.793_617_79 * m - 0.004_072_05 * s,
        1.977_998_50 * l - 2.428_592_21 * m + 0.450_593_71 * s,
        0.025_904_04 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ))
}

/// The sRGB colour of these OKLCH values, or `None` when it falls outside sRGB.
#[must_use]
pub fn from_oklch(l: f64, c: f64, hue_degrees: f64) -> Option<String> {
    let (sin, cos) = hue_degrees.to_radians().sin_cos();
    let (a, b) = (c * cos, c * sin);
    let lp = (l + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
    let mp = (l - 0.105_561_35 * a - 0.063_854_17 * b).powi(3);
    let sp = (l - 0.089_484_18 * a - 1.291_485_55 * b).powi(3);
    let rgb = [
        4.076_741_66 * lp - 3.307_711_59 * mp + 0.230_969_93 * sp,
        -1.268_438_00 * lp + 2.609_757_40 * mp - 0.341_319_40 * sp,
        -0.004_196_09 * lp - 0.703_418_61 * mp + 1.707_614_70 * sp,
    ];
    if rgb.iter().any(|v| !(-0.000_1..=1.000_1).contains(v)) {
        return None;
    }
    let mut out = String::from("#");
    for v in rgb {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 0..=255 first"
        )]
        let byte = (gamma(v.clamp(0.0, 1.0)) * 255.0).round().clamp(0.0, 255.0) as u8;
        out.push_str(&format!("{byte:02x}"));
    }
    Some(out)
}

/// Distance between two colours in OKLab. Unreadable input counts as far away.
#[must_use]
pub fn distance(a: &str, b: &str) -> f64 {
    match (oklab(a), oklab(b)) {
        (Some(x), Some(y)) => {
            ((x.0 - y.0).powi(2) + (x.1 - y.1).powi(2) + (x.2 - y.2).powi(2)).sqrt()
        }
        _ => f64::MAX,
    }
}

fn luminance(hex: &str) -> f64 {
    channels(hex).map_or(0.0, |c| {
        let [r, g, b] = c.map(linear);
        0.2126 * r + 0.7152 * g + 0.0722 * b
    })
}

fn contrast(a: &str, b: &str) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

fn readable_everywhere(hex: &str) -> bool {
    THEME_BACKGROUNDS
        .iter()
        .all(|bg| contrast(hex, bg) >= MIN_CONTRAST)
}

fn unit(random: &mut dyn FnMut() -> u64) -> f64 {
    #[allow(clippy::cast_precision_loss, reason = "only a fraction is needed")]
    let v = (random() % 1_000_000) as f64;
    v / 1_000_000.0
}

/// A random colour, readable on all four themes, as far as it can be from every colour in `taken`.
/// It is never exactly one of them.
pub fn distinct_color(taken: &[String], random: &mut dyn FnMut() -> u64) -> String {
    let mut best: Option<(f64, String)> = None;
    for _ in 0..TRIES {
        let l = 0.52 + 0.14 * unit(random);
        let c = 0.09 + 0.09 * unit(random);
        let h = 360.0 * unit(random);
        let Some(hex) = from_oklch(l, c, h).filter(|x| readable_everywhere(x)) else {
            continue;
        };
        if taken.iter().any(|t| t.eq_ignore_ascii_case(&hex)) {
            continue;
        }
        let nearest = taken
            .iter()
            .map(|t| distance(t, &hex))
            .fold(f64::MAX, f64::min);
        if nearest >= DISTINCT {
            return hex;
        }
        if best.as_ref().is_none_or(|(d, _)| nearest > *d) {
            best = Some((nearest, hex));
        }
    }
    best.map_or_else(|| "#7c5cbf".to_owned(), |(_, hex)| hex)
}

/// The colour to save: as asked, unless it exactly matches a colour in `taken`, when a random
/// different one replaces it. Returns whether that happened.
pub fn resolve(
    wanted: &str,
    taken: &[String],
    random: &mut dyn FnMut() -> u64,
) -> Result<(String, bool), crate::error::CoreError> {
    let clean = clean_color(wanted)?;
    if taken.iter().any(|t| t.eq_ignore_ascii_case(&clean)) {
        Ok((distinct_color(taken, random), true))
    } else {
        Ok((clean, false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counter() -> impl FnMut() -> u64 {
        let mut n = 7u64;
        move || {
            n = n.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            n >> 16
        }
    }

    #[test]
    fn conversions_agree_with_known_values() {
        let white = oklab("#ffffff").unwrap();
        assert!((white.0 - 1.0).abs() < 0.001 && white.1.abs() < 0.001);
        assert!(oklab("nope").is_none());
        assert_eq!(from_oklch(0.0, 0.0, 0.0).as_deref(), Some("#000000"));
        assert!(from_oklch(0.6, 0.9, 30.0).is_none(), "outside sRGB");
        assert!(distance("#2563eb", "#2563eb") < 1e-9);
        assert!(distance("#000000", "#ffffff") > 0.9);
    }

    #[test]
    fn a_new_colour_never_repeats_and_stays_readable_as_the_palette_fills() {
        let mut random = counter();
        let mut taken: Vec<String> = Vec::new();
        for _ in 0..60 {
            let hex = distinct_color(&taken, &mut random);
            assert!(!taken.contains(&hex), "{hex} repeated");
            assert!(readable_everywhere(&hex), "{hex}");
            assert!(clean_color(&hex).is_ok());
            taken.push(hex);
        }
    }

    #[test]
    fn the_first_colours_are_clearly_different() {
        let mut random = counter();
        let mut taken: Vec<String> = vec!["#2563eb".into(), "#dc2626".into()];
        for _ in 0..8 {
            let hex = distinct_color(&taken, &mut random);
            let nearest = taken
                .iter()
                .map(|t| distance(t, &hex))
                .fold(f64::MAX, f64::min);
            assert!(nearest >= DISTINCT, "{hex} {nearest}");
            taken.push(hex);
        }
    }

    #[test]
    fn only_an_exact_match_is_replaced() {
        let mut random = counter();
        let taken = vec!["#2563eb".to_owned()];
        assert!(resolve("#2563EB", &taken, &mut random).unwrap().1);
        assert_ne!(
            resolve("#2563EB", &taken, &mut random).unwrap().0,
            "#2563eb"
        );
        assert_eq!(
            resolve("#2563ec", &taken, &mut random).unwrap(),
            ("#2563ec".to_owned(), false),
            "similar colours keep the warning instead"
        );
        assert!(resolve("blue", &taken, &mut random).is_err());
    }

    #[test]
    fn it_terminates_even_if_nothing_random_comes_out() {
        let mut zero = || 0;
        let hex = distinct_color(&[], &mut zero);
        assert!(clean_color(&hex).is_ok());
    }
}
