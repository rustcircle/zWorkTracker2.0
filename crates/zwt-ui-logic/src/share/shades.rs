//! Shades of one project colour for that project's tasks or subtasks. Port of `taskShades` and its helpers in
//! `features/analysis/shareSlices.ts`.

use crate::color::{Oklch, THEME_BACKGROUNDS, contrast, from_oklch, to_oklch};

/// A shade must keep this much of its project's chroma, or it stops reading as the same hue.
const KEEP_CHROMA: f64 = 0.6;
/// Contrast against every theme's page and card that a shade must keep.
const MIN_CONTRAST: f64 = 2.0;
/// The widest step in lightness between two neighbouring shades.
const MAX_STEP: f64 = 0.07;
/// Lightness values tried when looking for the range a hue stays visible in.
const SCAN_FROM: f64 = 0.3;
const SCAN_STEP: f64 = 0.005;
const SCAN_COUNT: usize = 131;

fn visible(hex: &str) -> bool {
    THEME_BACKGROUNDS
        .iter()
        .all(|bg| contrast(hex, bg) >= MIN_CONTRAST)
}

/// `shadeAt`: the colour at lightness `l` with the base hue and as much of the base chroma as sRGB allows, or `None`
/// when it would be too washed out or too hard to see on one of the themes.
fn shade_at(base: &Oklch, l: f64) -> Option<String> {
    let mut c = base.c;
    for _ in 0..40 {
        if let Some(hex) = from_oklch(l, c, base.h) {
            return (c >= base.c * KEEP_CHROMA && visible(&hex)).then_some(hex);
        }
        c *= 0.93;
    }
    None
}

fn scan_lightness(k: usize) -> f64 {
    SCAN_FROM + k as f64 * SCAN_STEP
}

/// `visibleBand`: the run of lightness values where the hue is visible on every theme: the one holding the base,
/// else the longest. 1.x keeps a cache of these per colour; the result is the same without it.
fn visible_band(base: &Oklch) -> Option<(f64, f64)> {
    let mut best = None;
    let mut best_score = f64::NEG_INFINITY;
    let mut from: Option<usize> = None;
    for k in 0..=SCAN_COUNT {
        let ok = k < SCAN_COUNT && shade_at(base, scan_lightness(k)).is_some();
        if ok && from.is_none() {
            from = Some(k);
        }
        if !ok && let Some(start) = from {
            let lo = scan_lightness(start);
            let hi = scan_lightness(k.saturating_sub(1));
            let holds_base = base.l >= lo - SCAN_STEP && base.l <= hi + SCAN_STEP;
            let bonus = if holds_base { 10.0 } else { 0.0 };
            let score = bonus + (hi - lo);
            if score > best_score {
                best_score = score;
                best = Some((lo, hi));
            }
            from = None;
        }
    }
    best
}

/// `taskShades`: `count` shades of one project colour for that project's rows, darkest first: the same hue,
/// lightness stepped evenly inside the range where the hue stays visible on every theme, centred on the project's own
/// lightness. A single row has the project colour itself, and so does every row of a colour that is not a hex value.
/// Each step is at most 0.07 in OKLCH lightness, less when the visible range is too narrow for that many.
#[must_use]
pub fn task_shades(base: &str, count: usize) -> Vec<String> {
    if count == 0 {
        return Vec::new();
    }
    let lch = to_oklch(base);
    let band = match lch {
        Some(ref l) if count > 1 => visible_band(l),
        _ => None,
    };
    let (Some(lch), Some((lo, hi))) = (lch, band) else {
        return vec![base.to_owned(); count];
    };
    let gaps = (count - 1) as f64;
    let step = MAX_STEP.min((hi - lo) / gaps);
    let span = step * gaps;
    let start = (hi - span).min(lo.max(lch.l - span / 2.0));
    (0..count)
        .map(|i| shade_at(&lch, start + i as f64 * step).unwrap_or_else(|| base.to_owned()))
        .collect()
}
