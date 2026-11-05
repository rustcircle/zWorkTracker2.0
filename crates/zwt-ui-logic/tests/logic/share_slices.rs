//! `features/analysis/shareSlices.test.ts`: rounding that always adds up, the Other group, the order of the tasks,
//! the text of a legend row (in `format.rs`) and the shades of a project's colour.

use std::collections::BTreeSet;

use zwt_ui_logic::color::{THEME_BACKGROUNDS, contrast, to_oklch};
use zwt_ui_logic::share::{
    OTHER_COLOR, RowKind, SHOW_ALL_UP_TO, ShareProject, ShareTask, TOP_ROWS, apportion,
    project_shares, task_shades, task_shares,
};

use crate::fixtures::{below, color_of, expect_consistent, lcg, sum, t};

fn w(xs: &[i64]) -> Vec<f64> {
    xs.iter().map(|&x| x as f64).collect()
}

// ---- apportion: the largest-remainder method

#[test]
fn splits_three_equal_parts_into_34_33_and_33() {
    assert_eq!(apportion(&w(&[1, 1, 1]), 100), [34, 33, 33]);
}

#[test]
fn keeps_a_1_1_98_split_as_it_is() {
    assert_eq!(apportion(&w(&[1, 1, 98]), 100), [1, 1, 98]);
}

#[test]
fn gives_tiny_values_nothing_and_the_big_row_the_rest() {
    assert_eq!(apportion(&w(&[1, 1, 98_000]), 100), [0, 0, 100]);
}

#[test]
fn adds_up_seven_equal_parts_two_rows_get_15_and_five_get_14() {
    let seven = apportion(&w(&[1; 7]), 100);
    assert_eq!(seven, [15, 15, 14, 14, 14, 14, 14]);
    assert_eq!(sum(seven), 100);
}

#[test]
fn gives_the_spare_unit_to_the_largest_remainder_where_plain_rounding_would_fall_one_short() {
    // 47.4 + 26.3 + 26.3 rounds to 47 + 26 + 26 = 99.
    assert_eq!(apportion(&w(&[474, 263, 263]), 100), [48, 26, 26]);
}

#[test]
fn breaks_a_tie_in_the_remainder_by_the_larger_weight_then_by_the_earlier_row() {
    // 0.5 and 1.5: the same remainder, so the larger weight wins.
    assert_eq!(apportion(&w(&[1, 3]), 2), [0, 2]);
    // All four have the same weight and the same remainder: the first two rows win.
    assert_eq!(apportion(&w(&[2, 2, 2, 2]), 6), [2, 2, 1, 1]);
}

#[test]
fn leaves_a_row_with_no_weight_at_0_even_when_it_is_first_in_line() {
    assert_eq!(apportion(&w(&[0, 5, 0, 5]), 3), [0, 2, 0, 1]);
}

#[test]
fn has_nothing_to_split_when_there_are_no_rows_no_weight_or_no_units() {
    assert_eq!(apportion(&[], 100), Vec::<i64>::new());
    assert_eq!(apportion(&w(&[0, 0]), 100), [0, 0]);
    assert_eq!(apportion(&w(&[1, 2]), 0), [0, 0]);
    assert_eq!(apportion(&[f64::NAN, -1.0, 3.0], 4), [0, 0, 4]);
}

#[test]
fn always_adds_up_to_the_units_and_every_row_stays_within_one_unit_of_its_exact_share() {
    let mut rnd = lcg(20_261_103);
    for _ in 0..600 {
        let rows = 1 + below(&mut rnd, 12);
        let weights: Vec<i64> = (0..rows)
            .map(|_| {
                if rnd() < 0.15 {
                    0
                } else {
                    1 + below(&mut rnd, 5_000_000)
                }
            })
            .collect();
        let units = if rnd() < 0.5 {
            100
        } else {
            1 + below(&mut rnd, 5000)
        };
        let got = apportion(&w(&weights), units);
        let total = sum(weights.iter().copied());
        if total == 0 {
            assert_eq!(got, vec![0; weights.len()]);
            continue;
        }
        assert_eq!(sum(got.iter().copied()), units);
        for (i, &wt) in weights.iter().enumerate() {
            let exact = (wt as f64 * units as f64) / total as f64;
            assert!(got[i] >= (exact + 1e-9).floor() as i64);
            assert!(got[i] <= (exact - 1e-9).ceil() as i64);
            if wt == 0 {
                assert_eq!(got[i], 0);
            }
        }
    }
}

// ---- projectShares

fn p(id: &str, name: &str, secs: i64, color: &str) -> ShareProject {
    ShareProject {
        id: id.to_owned(),
        name: name.to_owned(),
        color: color.to_owned(),
        secs,
    }
}

fn pb(id: &str, name: &str, secs: i64) -> ShareProject {
    p(id, name, secs, "#2563eb")
}

#[test]
fn lists_the_projects_with_time_most_first_then_by_name_each_in_its_own_colour() {
    let m = project_shares(&[
        p("c", "Charlie", 600, "#059669"),
        pb("z", "Idle", 0),
        p("a", "Alpha", 1200, "#dc2626"),
        p("b", "Bravo", 600, "#2563eb"),
    ]);
    assert_eq!(m.level, zwt_ui_logic::share::ShareLevel::Projects);
    let got: Vec<(&str, &str, Option<&str>, Option<&str>)> = m
        .rows
        .iter()
        .map(|r| (r.label.as_str(), r.color.as_str(), r.id(), r.project_id()))
        .collect();
    assert_eq!(
        got,
        [
            ("Alpha", "#dc2626", Some("a"), Some("a")),
            ("Bravo", "#2563eb", Some("b"), Some("b")),
            ("Charlie", "#059669", Some("c"), Some("c")),
        ]
    );
    assert!(
        m.rows
            .iter()
            .all(|r| matches!(r.kind, RowKind::Project { .. }))
    );
    expect_consistent(&m);
}

#[test]
fn adds_three_equal_projects_up_to_100_percent_the_extra_point_going_to_the_first_by_name() {
    let m = project_shares(&[
        pb("g", "Gamma", 3600),
        pb("a", "Alpha", 3600),
        pb("b", "Beta", 3600),
    ]);
    let got: Vec<(&str, &str)> = m
        .rows
        .iter()
        .map(|r| (r.label.as_str(), r.text.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("Alpha", "1h 00m (34%)"),
            ("Beta", "1h 00m (33%)"),
            ("Gamma", "1h 00m (33%)")
        ]
    );
}

#[test]
fn adds_the_minutes_up_to_the_total_the_centre_shows_where_rounding_each_row_would_drift() {
    let minutes =
        |m: &zwt_ui_logic::share::ShareModel| m.rows.iter().map(|r| r.minutes).collect::<Vec<_>>();
    let alone = |m: &zwt_ui_logic::share::ShareModel| {
        m.rows
            .iter()
            .map(|r| (r.secs + 30).div_euclid(60))
            .collect::<Vec<_>>()
    };
    // 30 s each: every row alone rounds up to a minute (4 in all), the 2 minutes of the total is what the centre shows.
    let halves = project_shares(&[
        pb("a", "A", 30),
        pb("b", "B", 30),
        pb("c", "C", 30),
        pb("d", "D", 30),
    ]);
    assert_eq!(alone(&halves), [1, 1, 1, 1]);
    assert_eq!(halves.total_minutes, 2);
    assert_eq!(minutes(&halves), [1, 1, 0, 0]);
    // 89 s twice: each alone rounds down to 1 minute (2 in all) but the total of 178 s is 3 minutes.
    let down = project_shares(&[pb("a", "A", 89), pb("b", "B", 89)]);
    assert_eq!(alone(&down), [1, 1]);
    assert_eq!(down.total_minutes, 3);
    assert_eq!(minutes(&down), [2, 1]);
    // 100 s three times: each alone rounds up to 2 minutes (6 in all) but the total of 300 s is 5.
    let up = project_shares(&[pb("a", "A", 100), pb("b", "B", 100), pb("c", "C", 100)]);
    assert_eq!(alone(&up), [2, 2, 2]);
    assert_eq!(up.total_minutes, 5);
    assert_eq!(minutes(&up), [2, 2, 1]);
    for m in [&halves, &down, &up] {
        expect_consistent(m);
    }
}

#[test]
fn writes_less_than_one_percent_for_a_project_with_a_sliver_of_time() {
    let m = project_shares(&[pb("a", "Big", 99_999), pb("b", "Sliver", 1)]);
    let got: Vec<&str> = m.rows.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(got, ["27h 47m (100%)", "0h 00m (<1%)"]);
    assert_eq!(m.rows[1].percent, "<1%");
    expect_consistent(&m);
}

#[test]
fn shows_a_project_that_fills_the_whole_donut_at_100_percent() {
    let m = project_shares(&[pb("a", "Only", 2700)]);
    assert_eq!(m.rows[0].text, "0h 45m (100%)");
}

#[test]
fn has_no_rows_when_there_is_no_time() {
    let m = project_shares(&[pb("a", "A", 0)]);
    assert!(m.rows.is_empty());
    assert_eq!(m.total_secs, 0);
    assert_eq!(m.total_minutes, 0);
}

#[test]
fn projects_stay_consistent_for_random_data() {
    let mut rnd = lcg(7);
    for _ in 0..300 {
        let rows = 1 + below(&mut rnd, 14);
        let projects: Vec<ShareProject> = (0..rows)
            .map(|i| {
                let secs = if rnd() < 0.2 {
                    0
                } else {
                    1 + below(&mut rnd, 400_000)
                };
                pb(&format!("p{i}"), &format!("Project {i}"), secs)
            })
            .collect();
        expect_consistent(&project_shares(&projects));
    }
}

// ---- taskShares

/// `n` tasks of one project with distinct, falling times.
fn many(n: i64, project_id: &str) -> Vec<ShareTask> {
    (0..n)
        .map(|i| t(&format!("t{i:02}"), project_id, 10_000 - i * 100))
        .collect()
}

fn ids(m: &zwt_ui_logic::share::ShareModel) -> Vec<Option<String>> {
    m.rows.iter().map(|r| r.id().map(str::to_owned)).collect()
}

#[test]
fn gives_every_task_a_slice_when_there_are_nine_or_fewer() {
    assert_eq!(SHOW_ALL_UP_TO, 9);
    let m = task_shares(&many(9, "A"), color_of);
    assert_eq!(m.rows.len(), 9);
    assert!(
        m.rows
            .iter()
            .all(|r| matches!(r.kind, RowKind::Task { .. }))
    );
    assert!(!m.rows.iter().any(|r| r.label.starts_with("Other")));
    expect_consistent(&m);
}

#[test]
fn groups_all_but_the_top_eight_as_other_2_tasks_when_there_are_ten() {
    assert_eq!(TOP_ROWS, 8);
    let tasks = many(10, "A");
    let m = task_shares(&tasks, color_of);
    assert_eq!(m.rows.len(), 9);
    let other = &m.rows[8];
    assert_eq!(
        other.kind,
        RowKind::Other {
            grouped: 2,
            unit: zwt_ui_logic::share::OtherUnit::Tasks
        }
    );
    assert_eq!((other.id(), other.project_id()), (None, None));
    assert_eq!(other.label, "Other (2 tasks)");
    assert_eq!(other.color, OTHER_COLOR);
    assert_eq!(other.secs, tasks[8].secs + tasks[9].secs);
    let first: Vec<Option<String>> = tasks[..8].iter().map(|t| Some(t.id.clone())).collect();
    assert_eq!(ids(&m)[..8], first[..]);
    expect_consistent(&m);
}

#[test]
fn counts_the_grouped_tasks_in_the_label_12_tasks_make_other_4_tasks() {
    let m = task_shares(&many(12, "A"), color_of);
    let last = m.rows.last().unwrap();
    assert_eq!(last.label, "Other (4 tasks)");
    assert!(matches!(last.kind, RowKind::Other { grouped: 4, .. }));
}

#[test]
fn never_writes_other_1_tasks_whatever_the_number_of_tasks() {
    for n in 1..=40 {
        let m = task_shares(&many(n, "A"), color_of);
        let labels: Vec<&str> = m.rows.iter().map(|r| r.label.as_str()).collect();
        assert!(
            !labels
                .iter()
                .any(|l| *l == "Other (1 tasks)" || *l == "Other (1 task)")
        );
        let other = labels.iter().find(|l| l.starts_with("Other"));
        if n <= 9 {
            assert_eq!(other, None);
        } else {
            assert_eq!(
                other.map(|s| s.to_string()),
                Some(format!("Other ({} tasks)", n - 8))
            );
        }
    }
}

#[test]
fn does_not_count_a_task_with_no_time_towards_the_nine() {
    let mut tasks = many(9, "A");
    tasks.push(t("idle1", "A", 0));
    tasks.push(t("idle2", "B", 0));
    let m = task_shares(&tasks, color_of);
    assert_eq!(m.rows.len(), 9);
    assert!(
        !m.rows
            .iter()
            .any(|r| matches!(r.kind, RowKind::Other { .. }))
    );
}

#[test]
fn groups_the_shown_tasks_by_project_the_projects_by_their_time_each_projects_tasks_by_time_other_last()
 {
    let m = task_shares(
        &[
            t("t1", "A", 10),
            t("t2", "A", 3),
            t("t3", "B", 8),
            t("t4", "B", 7),
        ],
        color_of,
    );
    // B has 15 against A's 13, although the biggest single task is A's.
    let got: Vec<&str> = m.rows.iter().map(|r| r.id().unwrap()).collect();
    assert_eq!(got, ["t3", "t4", "t1", "t2"]);
    let projects: Vec<&str> = m.rows.iter().map(|r| r.project_id().unwrap()).collect();
    assert_eq!(projects, ["B", "B", "A", "A"]);
}

#[test]
fn counts_the_time_of_the_grouped_tasks_when_it_puts_the_projects_in_order() {
    // A: one task of 130. B: 90 and nine of 5, so B has 135 in all but only 120 of it shown.
    let mut tasks = vec![t("a1", "A", 130), t("b1", "B", 90)];
    tasks.extend((0..9).map(|i| t(&format!("b{}", i + 2), "B", 5)));
    let m = task_shares(&tasks, color_of);
    assert_eq!(m.rows.last().unwrap().label, "Other (3 tasks)");
    assert_eq!(m.rows[0].project_id(), Some("B"));
    assert!(m.rows.iter().any(|r| r.id() == Some("a1")));
    let projects: Vec<Option<&str>> = m.rows.iter().map(|r| r.project_id()).collect();
    let mut want = vec![Some("B"); 7];
    want.extend([Some("A"), None]);
    assert_eq!(projects, want);
}

#[test]
fn puts_projects_with_the_same_time_in_the_order_of_their_best_task() {
    let m = task_shares(&[t("t1", "B", 5), t("t2", "A", 5)], color_of);
    // The same time: the project of the task that ranks first goes first.
    let got: Vec<&str> = m.rows.iter().map(|r| r.id().unwrap()).collect();
    assert_eq!(got, ["t1", "t2"]);
    let swapped = task_shares(&[t("x", "B", 9), t("y", "A", 4), t("z", "A", 5)], color_of);
    let got: Vec<&str> = swapped.rows.iter().map(|r| r.id().unwrap()).collect();
    assert_eq!(got, ["x", "z", "y"]);
}

#[test]
fn is_the_same_whatever_order_the_tasks_come_in() {
    let mut tasks = vec![
        t("t1", "A", 10),
        t("t2", "B", 10),
        t("t3", "A", 10),
        t("t4", "C", 1),
    ];
    tasks.extend(many(12, "B"));
    let mut reversed = tasks.clone();
    reversed.reverse();
    let view = |m: zwt_ui_logic::share::ShareModel| {
        m.rows
            .into_iter()
            .map(|r| (r.key, r.color, r.text))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        view(task_shares(&reversed, color_of)),
        view(task_shares(&tasks, color_of))
    );
}

#[test]
fn gives_a_projects_only_task_the_projects_own_colour_and_several_tasks_different_shades_of_its_hue()
 {
    let alone = task_shares(&[t("t1", "A", 5), t("t2", "B", 4)], color_of);
    let got: Vec<&str> = alone.rows.iter().map(|r| r.color.as_str()).collect();
    assert_eq!(got, ["#2563eb", "#dc2626"]);
    let shared = task_shares(
        &[
            t("t1", "A", 9),
            t("t2", "A", 5),
            t("t3", "A", 2),
            t("t4", "B", 1),
        ],
        color_of,
    );
    let colours: Vec<&str> = shared.rows.iter().map(|r| r.color.as_str()).collect();
    assert_eq!(colours.iter().collect::<BTreeSet<_>>().len(), 4);
    assert_eq!(colours[3], "#dc2626");
}

#[test]
fn adds_up_in_minutes_and_percent_with_other_included_for_random_data() {
    let mut rnd = lcg(99);
    for _ in 0..200 {
        let rows = 1 + below(&mut rnd, 30);
        let tasks: Vec<ShareTask> = (0..rows)
            .map(|i| {
                let project = ["A", "B", "C"][below(&mut rnd, 3) as usize];
                let secs = if rnd() < 0.1 {
                    0
                } else {
                    1 + below(&mut rnd, 90_000)
                };
                t(&format!("t{i}"), project, secs)
            })
            .collect();
        let m = task_shares(&tasks, color_of);
        expect_consistent(&m);
        assert_eq!(m.total_secs, sum(tasks.iter().map(|t| t.secs)));
    }
}

// ---- taskShades: the shades of a project colour

/// The angle between two hues, 0 to 180 degrees.
fn hue_gap(a: f64, b: f64) -> f64 {
    (((a - b + 540.0) % 360.0) - 180.0).abs()
}

fn lightness(hex: &str) -> f64 {
    to_oklch(hex).map_or(f64::NAN, |o| o.l)
}

const COLOURS: [&str; 14] = [
    "#2563eb", "#dc2626", "#059669", "#d97706", "#7c3aed", "#db2777", "#0891b2", "#65a30d",
    "#ea580c", "#4f46e5", "#0d9488", "#be123c", "#0ea5e9", "#7c5cbf",
];

#[test]
fn gives_a_single_task_the_project_colour_itself() {
    for c in COLOURS {
        assert_eq!(task_shades(c, 1), [c]);
    }
}

#[test]
fn gives_none_for_no_tasks() {
    assert!(task_shades("#2563eb", 0).is_empty());
}

#[test]
fn keeps_the_hue_of_the_project_colour_in_every_shade_for_every_palette_colour_and_up_to_eight_tasks()
 {
    for c in COLOURS {
        let base = to_oklch(c).unwrap();
        assert!(base.c > 0.09);
        for n in 2..=8 {
            let shades = task_shades(c, n);
            assert_eq!(shades.len(), n);
            for s in &shades {
                let h = to_oklch(s).map_or(999.0, |o| o.h);
                assert!(hue_gap(h, base.h) < 3.0, "{c} x{n} {s}");
            }
        }
    }
}

#[test]
fn steps_the_lightness_darkest_first_every_shade_different_neighbours_far_enough_apart_to_tell() {
    for c in COLOURS {
        for n in 2..=8 {
            let l: Vec<f64> = task_shades(c, n).iter().map(|s| lightness(s)).collect();
            let smallest = l
                .windows(2)
                .map(|p| p[1] - p[0])
                .fold(f64::INFINITY, f64::min);
            let floor = if n <= 3 {
                0.05
            } else if n <= 5 {
                0.04
            } else {
                0.025
            };
            assert!(smallest > floor, "{c} x{n}");
        }
    }
}

#[test]
fn keeps_every_shade_visible_against_the_page_and_the_card_of_all_four_themes() {
    for c in COLOURS {
        for n in 2..=8 {
            for s in task_shades(c, n) {
                let weakest = THEME_BACKGROUNDS
                    .iter()
                    .map(|bg| contrast(&s, bg))
                    .fold(f64::INFINITY, f64::min);
                assert!(weakest >= 2.0, "{c} x{n} {s}");
            }
        }
    }
}

#[test]
fn is_the_same_every_time() {
    assert_eq!(task_shades("#7c3aed", 5), task_shades("#7c3aed", 5));
}

#[test]
fn leaves_a_colour_that_is_not_a_hex_value_as_it_is_for_each_task() {
    assert_eq!(task_shades("var(--accent)", 3), ["var(--accent)"; 3]);
}

// ---- the colour of the grouped rest

#[test]
fn is_the_strong_border_token() {
    assert_eq!(OTHER_COLOR, "var(--border-strong)");
    // `lib/themes.ts`: every theme's strong border on its card.
    for (theme, border_strong, surface) in crate::share_rows::THEMES {
        assert!(contrast(border_strong, surface) >= 3.0, "{theme}");
    }
}

// ---- what the glossary and the README say about the rounding (the figures; the texts are 1.x's docs)

#[test]
fn reserves_less_than_one_percent_for_a_slice_whose_share_rounds_to_0() {
    // 0.7% is given a spare point, so it reads 1%. 0.4% is not, so it reads "<1%".
    let pct = |a: i64, b: i64| {
        project_shares(&[pb("a", "Big", a), pb("b", "Sliver", b)])
            .rows
            .iter()
            .map(|r| r.pct())
            .collect::<Vec<_>>()
    };
    assert_eq!(pct(9930, 70), ["(99%)", "(1%)"]);
    assert_eq!(pct(9960, 40), ["(100%)", "(<1%)"]);
}

#[test]
fn some_slices_not_one_can_differ_by_a_point_from_plain_rounding() {
    // Plain rounding of seven equal parts is 14 each, 98 in all: two slices take the spare points.
    let seven: Vec<ShareProject> = (0..7)
        .map(|i| pb(&format!("p{i}"), &format!("Project {i}"), 600))
        .collect();
    let got: Vec<i64> = project_shares(&seven)
        .rows
        .iter()
        .map(|r| r.whole_percent)
        .collect();
    assert_eq!(got, [15, 15, 14, 14, 14, 14, 14]);
    // Twelve equal parts: 8 each is 96 in all, so four slices differ.
    let twelve: Vec<ShareProject> = (0..12)
        .map(|i| pb(&format!("p{i:02}"), &format!("Project {i:02}"), 600))
        .collect();
    let m = project_shares(&twelve);
    let differ = m
        .rows
        .iter()
        .filter(|r| r.whole_percent != (r.secs as f64 / m.total_secs as f64 * 100.0).round() as i64)
        .count();
    assert_eq!(differ, 4);
}
