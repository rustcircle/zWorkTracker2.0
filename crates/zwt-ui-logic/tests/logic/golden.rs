//! The ports against 1.x's own output: `golden.json` holds what 1.x's `taskShades` and `apportion` return and how
//! `localeCompare` orders a list of names, computed by node on a verbatim JavaScript transcription of
//! `lib/color.ts` and `features/analysis/shareSlices.ts` (generator: `.stack/out/orchestrator/<run>/nodes/B2/golden.mjs`).

use std::cmp::Ordering;

use serde_json::Value;
use zwt_ui_logic::collate::locale_compare;
use zwt_ui_logic::share::{apportion, task_shades};

fn golden() -> Value {
    serde_json::from_str(include_str!("golden.json")).unwrap()
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn shades_are_the_colours_1x_draws() {
    let g = golden();
    let shades = g["shades"].as_object().unwrap();
    // The 12 colours of the palette and 12 more (`#7c3aed` is in both lists): hex, short hex, upper case, grey,
    // black, white and one that is not a colour.
    assert_eq!(shades.len(), 24);
    let mut checked = 0;
    for (colour, by_count) in shades {
        for (n, want) in by_count.as_array().unwrap().iter().enumerate() {
            assert_eq!(task_shades(colour, n), strings(want), "{colour} x{n}");
            checked += 1;
        }
    }
    assert_eq!(checked, shades.len() * 10);
}

#[test]
fn apportion_gives_what_1x_gives() {
    let g = golden();
    let cases = g["apportionCases"].as_array().unwrap();
    assert_eq!(cases.len(), 400);
    for case in cases {
        let weights: Vec<f64> = case["weights"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w.as_f64().unwrap())
            .collect();
        let units = case["units"].as_i64().unwrap();
        let want: Vec<i64> = case["out"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_i64().unwrap())
            .collect();
        assert_eq!(apportion(&weights, units), want, "{weights:?} into {units}");
    }
}

#[test]
fn names_sort_as_locale_compare_sorts_them() {
    let g = golden();
    let want = strings(&g["sorted"]);
    let mut got = want.clone();
    got.reverse();
    got.sort_by(|a, b| locale_compare(a, b));
    assert_eq!(got, want);
    for pair in want.windows(2) {
        assert_eq!(
            locale_compare(&pair[0], &pair[1]),
            Ordering::Less,
            "{} < {}",
            pair[0],
            pair[1]
        );
    }
}
