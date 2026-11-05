//! The day axis' sub-grid (owner, 2026-11-04): "there should be a sub grid in the timeline of hours that splits time
//! in the increments choosen", "for instance 0, 15, 30, 45, 60". The increment chosen is 1.x's rounding increment
//! (Settings > Rounding, "Increment"): the global rule in force on the day shown, or the core's default of 15 minutes
//! when none is saved or rounding is off. The hour lines stay as 1.x draws them (the grid colour, every third at full
//! strength, the others at 55 %); between them, a fainter sub-line at each increment from the day's start, none on an
//! hour line and none closer than 3 px to the next (a small increment then marks every few increments). The lines are
//! static: drawn from the day, its increment and the axis' width, so a clock tick moves none.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

mod common;

use std::cell::Cell;
use std::rc::Rc;

use common::{Harness, rect_of, xs_of};
use i_slint_backend_testing::AccessibleRole;
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, LogicalSize};
use z_work_core::machine::RoundingInput;
use z_work_core::model::RoundMethod;

/// Saves a rounding rule of `minutes` from the day `from` (`yyyy-mm-dd`), for every project or for `project`, and lets
/// the window catch up before its next frame.
fn rounding(h: &mut Harness, minutes: u32, from: &str, project: Option<&str>) {
    let input = RoundingInput {
        project_id: project.map(str::to_owned),
        effective_from: from.to_owned(),
        increment_minutes: minutes,
        method: RoundMethod::Up,
        confirm_retroactive: true,
    };
    h.mutate("rounding", move |svc| {
        svc.set_rounding(&input, "2026-11-04")
    });
    h.settle();
}

/// Picks the day chip named `title` and lets the window catch up.
fn pick(h: &mut Harness, title: &str) {
    h.find(AccessibleRole::RadioButton, title)
        .mock_single_click(PointerEventButton::Left);
    h.pump();
    h.settle();
}

/// The sub-lines strictly between each two consecutive hour lines.
fn between_hours(h: &Harness) -> Vec<Vec<f32>> {
    let (hours, subs) = (xs_of(h, "HourLine"), xs_of(h, "SubLine"));
    assert!(hours.len() >= 22, "hour lines: {hours:?}");
    hours
        .windows(2)
        .map(|pair| {
            subs.iter()
                .copied()
                .filter(|&x| x > pair[0] && x < pair[1])
                .collect()
        })
        .collect()
}

/// Each hour is cut into `parts` equal parts: `parts - 1` sub-lines between each two hour lines, evenly spaced.
fn hours_split_into(h: &Harness, parts: usize, what: &str) {
    let hours = xs_of(h, "HourLine");
    for (i, inside) in between_hours(h).iter().enumerate() {
        let (a, b) = (hours[i], hours[i + 1]);
        assert_eq!(
            inside.len(),
            parts - 1,
            "{what}: between the hour lines at {a} and {b}: {inside:?}"
        );
        for (k, x) in inside.iter().enumerate() {
            let want = a + (b - a) * (k + 1) as f32 / parts as f32;
            assert!(
                (x - want).abs() < 0.01,
                "{what}: sub-line {k} at {x}, want {want}"
            );
        }
    }
}

#[test]
fn fifteen_minutes_put_three_sub_lines_between_consecutive_hour_lines() {
    // The sample saves no rounding rule: the default increment, 15 minutes.
    let h = Harness::new();
    let hours = xs_of(&h, "HourLine");
    let subs = xs_of(&h, "SubLine");
    assert_eq!(hours.len(), 23, "an hour line at 01:00 .. 23:00");
    hours_split_into(&h, 4, "15 minutes");
    // The first hour (from the track's start) and the last (to its end) have their three too.
    assert_eq!(subs.iter().filter(|&&x| x < hours[0]).count(), 3);
    assert_eq!(subs.iter().filter(|&&x| x > hours[22]).count(), 3);
    assert_eq!(subs.len(), 24 * 3);
}

#[test]
fn each_hour_splits_into_sixty_over_the_increment_chosen() {
    let mut h = Harness::new();
    for (minutes, parts) in [(30, 2), (20, 3), (12, 5), (10, 6), (6, 10), (15, 4)] {
        rounding(&mut h, minutes, "2026-11-04", None);
        hours_split_into(&h, parts, &format!("{minutes} minutes"));
    }
    // An hour or more: the hour lines are the grid.
    rounding(&mut h, 60, "2026-11-04", None);
    assert!(
        xs_of(&h, "SubLine").is_empty(),
        "no sub-line for 60 minutes"
    );
    assert_eq!(xs_of(&h, "HourLine").len(), 23, "the hour lines stay");
    rounding(&mut h, 90, "2026-11-04", None);
    assert!(
        xs_of(&h, "SubLine").is_empty(),
        "no sub-line for 90 minutes"
    );
    // Rounding off (exact time): the default of 15 minutes.
    rounding(&mut h, 0, "2026-11-04", None);
    hours_split_into(&h, 4, "rounding off");
}

/// The rule in force on the day shown draws its grid: a rule from today on leaves yesterday on the default, a
/// project's own rule leaves the axis alone, and a rule from tomorrow on leaves today alone.
#[test]
fn the_axis_follows_the_rule_in_force_on_the_day_shown() {
    let mut h = Harness::new();
    rounding(&mut h, 30, "2026-11-04", None);
    hours_split_into(&h, 2, "today, 30 minutes from today");

    pick(&mut h, "Tue 3 Nov 2026");
    assert_eq!(h.state().get_day_date(), "2026-11-03");
    hours_split_into(&h, 4, "yesterday, before the rule: the default");

    pick(&mut h, "Wed 4 Nov 2026");
    hours_split_into(&h, 2, "today again");

    rounding(&mut h, 10, "2026-11-04", Some("p-brand"));
    hours_split_into(&h, 2, "a project's own rule");
    rounding(&mut h, 6, "2026-11-05", None);
    hours_split_into(&h, 2, "a rule from tomorrow on");
}

/// Lines closer than 3 px would merge into a tint: a small increment marks every few increments instead, on a
/// multiple of it that still splits the hour evenly when the increment does. An increment that does not split the
/// hour marks its multiples into each hour (45: :45; 25: :25 and :50), leaving out a last one too near the next hour
/// line, or every line when that leaves none (55).
#[test]
fn small_increments_keep_their_lines_apart() {
    let mut h = Harness::new();
    let hours = xs_of(&h, "HourLine");
    // The track starts an hour before the first hour line.
    let (hour_px, start) = (hours[1] - hours[0], hours[0] - (hours[1] - hours[0]));
    let minute_px = hour_px / 60.0;
    assert!(
        (minute_px - 0.5625).abs() < 0.001,
        "810 px for 1440 minutes at 1360 px: {minute_px}"
    );
    // At 0.5625 px a minute, 3 px is 5.3 minutes. (increment, minutes between lines, lines in each hour)
    let cases = [
        (1, 6, 9),
        (2, 6, 9),
        (4, 12, 4),
        (5, 10, 5),
        (7, 7, 7),
        (8, 8, 6),
        (25, 25, 2),
        (45, 45, 1),
        (55, 0, 0),
    ];
    for (minutes, step, per_hour) in cases {
        rounding(&mut h, minutes, "2026-11-04", None);
        let got = xs_of(&h, "SubLine");
        let expected: Vec<f32> = (0..24)
            .flat_map(|hour| (1..=per_hour).map(move |k| hour * 60 + k * step))
            .map(|m| start + m as f32 * minute_px)
            .collect();
        assert_eq!(
            got.len(),
            expected.len(),
            "{minutes} min: {per_hour} lines an hour, {step} min apart"
        );
        for (x, want) in got.iter().zip(&expected) {
            assert!(
                (x - want).abs() < 0.01,
                "{minutes} min: a sub-line at {x}, want {want}"
            );
        }
        let mut lines = got;
        lines.extend(xs_of(&h, "HourLine"));
        lines.sort_by(f32::total_cmp);
        for pair in lines.windows(2) {
            assert!(
                pair[1] - pair[0] >= 3.0 - 0.001,
                "{minutes} min: lines at {} and {} are closer than 3 px",
                pair[0],
                pair[1]
            );
        }
    }
}

/// `over` at alpha `a` on `under`, per channel, as the renderer blends.
fn mix(over: [u8; 3], under: [u8; 3], a: f32) -> [u8; 3] {
    [0, 1, 2].map(|i| (f32::from(over[i]) * a + f32::from(under[i]) * (1.0 - a)) as u8)
}

/// The theme's grid colour draws every line, over the track's surface: an hour line at 55 %, every third hour at
/// full strength (timeline.css `.grid-line`, `.major`), a sub-line at 25 %, in Light and in Solarized Dark.
#[test]
fn the_grid_is_drawn_in_the_themes_colours() {
    let h = Harness::rendered_empty(LogicalSize::new(1360.0, 900.0));
    let rgb = |c: slint::Color| [c.red(), c.green(), c.blue()];
    let (now_x, _, now_w, _) = rect_of(&h.find(AccessibleRole::Image, "Now"));
    for theme_id in ["light", "solarizedDark"] {
        zwt_app::set_theme(&h.app, theme_id);
        let theme = h.app.global::<zwt_app::Theme>();
        let (grid, surface) = (rgb(theme.get_grid()), rgb(theme.get_surface()));
        let frame = h.snapshot();
        let hours = h.of_type("HourLine");
        let subs = h.of_type("SubLine");
        assert_eq!((hours.len(), subs.len()), (23, 72), "{theme_id}");
        let (_, y, _, height) = rect_of(&hours[0]);
        let row = (y + height / 2.0) as u32;
        let check = |x: f32, want: [u8; 3], what: &str| {
            let got = frame.rgb(x.round() as u32, row);
            let off = (0..3).map(|i| got[i].abs_diff(want[i])).max().unwrap();
            assert!(
                off <= 2,
                "{theme_id}: {what} at {x}: {got:?}, want {want:?}"
            );
        };
        for (i, line) in hours.iter().enumerate() {
            let major = (i + 1).is_multiple_of(3);
            let want = if major {
                grid
            } else {
                mix(grid, surface, 0.55)
            };
            check(rect_of(line).0, want, "an hour line");
        }
        for line in &subs {
            let x = rect_of(line).0;
            if x + 1.0 > now_x && x < now_x + now_w {
                continue;
            }
            check(x, mix(grid, surface, 0.25), "a sub-line");
        }
        // Between the lines, the surface.
        let first = rect_of(&subs[0]).0;
        check(first + 3.0, surface, "the track");
    }
}

/// A day of 25 hours (Lisbon, Sunday 25 October 2026: clocks go back an hour): 24 hour lines and the sub-grid in
/// all 25 hours, the last one too.
#[test]
fn a_25_hour_day_has_its_sub_grid_in_every_hour() {
    let mut h = Harness::new();
    pick(&mut h, "Sun 25 Oct 2026");
    assert_eq!(
        h.state().get_day_end() - h.state().get_day_start(),
        25 * 3600
    );
    let hours = xs_of(&h, "HourLine");
    assert_eq!(hours.len(), 24);
    hours_split_into(&h, 4, "a 25-hour day");
    let subs = xs_of(&h, "SubLine");
    assert_eq!(subs.len(), 25 * 3);
    assert_eq!(subs.iter().filter(|&&x| x > hours[23]).count(), 3);
}

/// Days half an hour short or long (Lord Howe Island moves its clocks by 30 minutes: 23.5 hours on 4 October 2026,
/// 24.5 on 5 April 2026): the sub-grid fills the whole hours, and the last half hour holds only the line that fits in
/// it, :15, never one past the end of the track.
#[test]
fn a_day_of_part_of_an_hour_ends_its_sub_grid_with_the_day() {
    i_slint_backend_testing::init_no_event_loop();
    let size = LogicalSize::new(1360.0, 3600.0);
    for (now, whole) in [("2026-10-04T12:00:00Z", 23), ("2026-04-05T12:00:00Z", 24)] {
        let h = Harness::open_empty_in("Australia/Lord_Howe", now, size);
        let length = h.state().get_day_end() - h.state().get_day_start();
        assert_eq!(length, whole * 3600 + 1800, "{now}");
        let hours = xs_of(&h, "HourLine");
        assert_eq!(
            hours.len(),
            whole as usize,
            "{now}: a line at each whole hour"
        );
        hours_split_into(&h, 4, now);
        let hour = hours[1] - hours[0];
        let (start, end) = (
            hours[0] - hour,
            hours[0] - hour + hour * length as f32 / 3600.0,
        );
        let subs = xs_of(&h, "SubLine");
        let last: Vec<f32> = subs
            .iter()
            .copied()
            .filter(|&x| x > hours[hours.len() - 1])
            .collect();
        assert_eq!(last.len(), 1, "{now}: the last half hour holds :15 only");
        assert!((last[0] - (hours[hours.len() - 1] + hour / 4.0)).abs() < 0.01);
        assert_eq!(subs.len(), whole as usize * 3 + 1, "{now}");
        assert!(
            subs.iter().all(|&x| x > start && x < end),
            "{now}: inside the track"
        );
    }
}

/// The Timeline tab makes its panel anew each time it comes back (About, then Timeline): the new axis works its grid
/// out when it is made, for the increment of the day shown, before any `changed` handler runs.
#[test]
fn the_sub_grid_comes_back_with_the_timeline_tab() {
    let mut h = Harness::new();
    rounding(&mut h, 30, "2026-11-04", None);
    hours_split_into(&h, 2, "30 minutes");
    h.find(AccessibleRole::Tab, "About")
        .mock_single_click(PointerEventButton::Left);
    assert!(h.of_type("SubLine").is_empty(), "no axis on the About tab");
    h.find(AccessibleRole::Tab, "Timeline")
        .mock_single_click(PointerEventButton::Left);
    hours_split_into(&h, 2, "back on the Timeline");
}

/// The grid is drawn from the day, its increment and the axis' width: a running clock moves no line and writes nothing
/// to the day axis' models, across a minute boundary too (AC d).
#[test]
fn clock_ticks_move_no_grid_line() {
    let mut h = Harness::new();
    assert!(h.ui.clock().is_running(), "a task runs: the clock ticks");
    let (hours, subs) = (xs_of(&h, "HourLine"), xs_of(&h, "SubLine"));
    assert_eq!(subs.len(), 72);
    let lines: Vec<_> = h
        .of_type("SubLine")
        .into_iter()
        .chain(h.of_type("HourLine"))
        .collect();
    let before = h.ui.writes();
    for _ in 0..65 {
        h.second();
        assert!(
            lines.iter().all(|l| l.is_valid()),
            "a tick made the lines anew"
        );
    }
    assert_eq!(xs_of(&h, "HourLine"), hours);
    assert_eq!(xs_of(&h, "SubLine"), subs);
    let after = h.ui.writes();
    assert_eq!(after.pieces, before.pieces, "no write to the day axis");
    assert_eq!(after.chips, before.chips, "no write to the day chips");
}

/// The window asks the Rust side for the sub-grid when the axis' width changes (a new layout), and never on a tick of
/// the running clock, though Slint lays the window out again on each one (the rail's elapsed time is in a layout).
#[test]
fn the_sub_grid_is_worked_out_on_a_new_width_never_on_a_tick() {
    let mut h = Harness::new();
    let asked = Rc::new(Cell::new(0_u32));
    {
        let asked = Rc::clone(&asked);
        h.state().on_sub_grid(move |increment, minute_px| {
            asked.set(asked.get() + 1);
            zwt_app::convert::sub_grid(increment, minute_px)
        });
    }
    // 960 px, the narrowest window: 410 px for 24 hours, so 10 minutes would be 2.8 px apart.
    h.app.window().set_size(LogicalSize::new(960.0, 3600.0));
    h.settle();
    assert_eq!(asked.get(), 1, "a new width asks once");
    rounding(&mut h, 10, "2026-11-04", None);
    assert_eq!(asked.get(), 2, "a new increment asks once");
    hours_split_into(&h, 3, "10 minutes at 960 px: every 20");
    let subs = xs_of(&h, "SubLine");
    asked.set(0);
    for _ in 0..65 {
        h.second();
    }
    assert_eq!(xs_of(&h, "SubLine"), subs);
    assert_eq!(asked.get(), 0, "no tick asks");
}
