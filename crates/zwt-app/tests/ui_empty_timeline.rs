//! The owner's rule for 2.0 (2026-11-04): "when there's no tasks the timeline should display the days the same just
//! empty". With no task at all, and with tasks but no time on the page shown, the Timeline shows the same day picker
//! page (14 days ending today, its arrows on and off as with data), the same day axis with its hour labels, hour lines
//! and sub-grid, and no block: never a collapsed or placeholder-only layout.
//!
//! 1.x lists logged days only, so an empty store shows its "Nothing is logged yet" line and no day at all
//! (TimelineTab.tsx L97-98, L140; model.ts `allDays`): a 2.0 change.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

mod common;

use common::{Harness, hex, rect_of, xs_of};
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, LogicalSize, Model};
use z_work_core::Ts;
use z_work_core::adjust::ManualEntryInput;
use z_work_core::machine::{ProjectInput, TaskInput};

const PREVIOUS: &str = "Previous days";
const NEXT: &str = "Next days";
const TODAY_CARD: &str = "Today, Wed 4 Nov 2026";
/// 1.x's line for an empty Timeline (Strings.timeline-empty), which 2.0 does not show in place of the days.
const NOTHING_LOGGED: &str = concat!(
    "Nothing is logged yet. Create a task, press Start, and its time appears here as a block on ",
    "today's axis."
);

fn on(h: &Harness, label: &str) -> bool {
    h.find(AccessibleRole::Button, label).accessible_enabled() == Some(true)
}

/// The chips' dates and accessible names, in the model's order.
fn chips(h: &Harness) -> Vec<(String, String)> {
    h.state()
        .get_chips()
        .iter()
        .map(|c| (c.date.to_string(), c.title.to_string()))
        .collect()
}

/// `first` to `last` (both `2026-mm-dd`), the oldest first.
fn days(first: &str, last: &str) -> Vec<String> {
    let parse = |s: &str| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
    parse(first)
        .iter_days()
        .take_while(|d| *d <= parse(last))
        .map(|d| d.format("%Y-%m-%d").to_string())
        .collect()
}

/// The hour labels under a 24-hour axis: every third hour.
fn hour_labels() -> Vec<String> {
    (0..8).map(|k| format!("{:02}:00", k * 3)).collect()
}

/// What a page with no time shows, as a page with time shows it: 14 chips ending today, each on screen and left of
/// the next, none with time; the arrows as `older` and `newer` say; today's card with its axis, the hour labels, the
/// hour lines and the sub-grid (the default increment, 15 minutes: 3 sub-lines an hour), and no block nor 1.x's line.
fn shows_the_days_empty(h: &Harness, older: bool, newer: bool) {
    let chips = chips(h);
    let dates: Vec<String> = chips.iter().map(|(d, _)| d.clone()).collect();
    assert_eq!(
        dates,
        days("2026-10-22", "2026-11-04"),
        "14 days ending today"
    );
    assert!(
        h.state()
            .get_chips()
            .iter()
            .all(|c| c.parts.row_count() == 0),
        "no chip has time"
    );
    let xs: Vec<f32> = chips
        .iter()
        .map(|(_, title)| {
            let (x, _, w, height) = rect_of(&h.find(AccessibleRole::RadioButton, title));
            assert!(w > 40.0 && height > 40.0, "{title}: {w} x {height}");
            x
        })
        .collect();
    assert!(
        xs.windows(2).all(|w| w[0] < w[1]),
        "chips left to right: {xs:?}"
    );
    assert_eq!(on(h, PREVIOUS), older, "the left arrow");
    assert_eq!(on(h, NEXT), newer, "the right arrow");

    let card = h.find(AccessibleRole::Groupbox, TODAY_CARD);
    let (_, _, card_w, card_h) = rect_of(&card);
    assert!(
        card_w > 800.0 && card_h > 100.0,
        "the day's card: {card_w} x {card_h}"
    );
    for label in hour_labels() {
        assert_eq!(
            h.find_all(AccessibleRole::Text, &label).len(),
            1,
            "the hour label {label}"
        );
    }
    assert_eq!(
        xs_of(h, "HourLine").len(),
        23,
        "an hour line at 01:00 .. 23:00"
    );
    assert_eq!(
        xs_of(h, "SubLine").len(),
        24 * 3,
        "the sub-grid, 3 lines an hour"
    );
    assert!(h.of_type("Block").is_empty(), "no block");
    assert!(
        h.find_all(AccessibleRole::Text, NOTHING_LOGGED).is_empty(),
        "no placeholder in place of the days"
    );
}

#[test]
fn an_empty_store_shows_the_days_the_same_just_empty() {
    let h = Harness::empty();
    assert_eq!(h.tasks().len(), 0, "no task");
    // Nothing before the page: the left arrow is off; the page ends today: the right one is off.
    shows_the_days_empty(&h, false, false);
}

/// With tasks but no time on the page shown: a draft never started, then time a month ago, before the page.
#[test]
fn tasks_without_time_on_the_page_show_the_days_the_same_just_empty() {
    let mut h = Harness::empty();
    h.mutate("a project and a draft", |svc| {
        let project_id = svc.create_project(&ProjectInput {
            name: "Brand refresh".to_owned(),
            color: "#2563eb".to_owned(),
            billable: true,
            budget_hours: None,
            client_zone: None,
            billing_type: Default::default(),
            fixed_fee_cents: None,
            fee_currency: None,
        })?;
        svc.create_task(&TaskInput {
            name: "Logo concepts".to_owned(),
            project_id,
            description: Default::default(),
            continued_from: None,
            tags: Vec::new(),
        })?;
        Ok(())
    });
    assert_eq!(h.tasks().len(), 1, "one task");
    shows_the_days_empty(&h, false, false);

    let task_id = h.tasks()[0].id.to_string();
    h.mutate("an hour a month ago", move |svc| {
        svc.add_manual_entry(&ManualEntryInput {
            task_id,
            start: Ts::parse("2026-10-01T09:00:00Z").unwrap(),
            end: Ts::parse("2026-10-01T10:00:00Z").unwrap(),
            reason: "Forgot to start the timer".to_owned(),
            subtask_id: None,
            allow_overlap: false,
        })
        .map(|_| ())
    });
    // A day with time lies before the page: the left arrow is on, as with data; the page ends today: the right is off.
    shows_the_days_empty(&h, true, false);

    // The left arrow pages back as with data: 8 to 21 October, empty too, and now the right arrow is on.
    h.find(AccessibleRole::Button, PREVIOUS)
        .mock_single_click(PointerEventButton::Left);
    h.pump();
    let dates: Vec<String> = chips(&h).into_iter().map(|(d, _)| d).collect();
    assert_eq!(dates, days("2026-10-08", "2026-10-21"));
    assert!(on(&h, PREVIOUS), "1 October lies before this page too");
    assert!(on(&h, NEXT), "back towards today");
    assert_eq!(
        h.state().get_day_date(),
        "2026-11-04",
        "the axis keeps today"
    );
}

/// The boxes of an element found by `role` and `label` in both apps.
fn both(a: &Harness, b: &Harness, role: AccessibleRole, label: &str) -> [(f32, f32, f32, f32); 2] {
    [rect_of(&a.find(role, label)), rect_of(&b.find(role, label))]
}

fn first_of(h: &Harness, type_name: &str) -> ElementHandle {
    h.of_type(type_name)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no {type_name}"))
}

/// Side by side with the sample's Timeline at the same size: the same picker, chips, arrows, day card, axis, hour
/// labels and grid, to the pixel; only the blocks differ.
#[test]
fn the_empty_timeline_is_laid_out_like_a_full_one() {
    let empty = Harness::empty();
    let full = Harness::open(LogicalSize::new(1360.0, 3600.0));
    assert_eq!(
        full.state().get_day_lanes(),
        1,
        "the sample's today has one lane"
    );
    for type_name in ["DayPicker", "DayCard", "DayAxis"] {
        assert_eq!(
            rect_of(&first_of(&empty, type_name)),
            rect_of(&first_of(&full, type_name)),
            "{type_name}"
        );
    }
    let mut labels: Vec<(AccessibleRole, String)> = vec![
        (AccessibleRole::Button, PREVIOUS.to_owned()),
        (AccessibleRole::Button, NEXT.to_owned()),
        (AccessibleRole::Groupbox, TODAY_CARD.to_owned()),
    ];
    labels.extend(
        chips(&full)
            .into_iter()
            .map(|(_, t)| (AccessibleRole::RadioButton, t)),
    );
    labels.extend(hour_labels().into_iter().map(|l| (AccessibleRole::Text, l)));
    for (role, label) in labels {
        let [a, b] = both(&empty, &full, role, &label);
        assert_eq!(a, b, "{role:?} {label}");
    }
    assert_eq!(xs_of(&empty, "HourLine"), xs_of(&full, "HourLine"));
    assert_eq!(xs_of(&empty, "SubLine"), xs_of(&full, "SubLine"));
    assert!(!xs_of(&empty, "SubLine").is_empty(), "with a sub-grid");
    assert!(empty.of_type("Block").is_empty());
    assert!(!full.of_type("Block").is_empty());
}

/// As drawn by the software renderer: the empty track shows its hour lines and sub-grid in the theme's grid colour
/// (an hour line at 55 %, every third at full strength, a sub-line fainter), not a blank bar.
#[test]
fn the_empty_axis_draws_its_grid() {
    let h = Harness::rendered_empty(LogicalSize::new(1360.0, 900.0));
    let frame = h.snapshot();
    let theme = h.app.global::<zwt_app::Theme>();
    let rgb = |c: slint::Color| [c.red(), c.green(), c.blue()];
    let (grid, surface) = (rgb(theme.get_grid()), rgb(theme.get_surface()));
    assert_eq!(surface, hex("#ffffff"), "Light");
    let hours = h.of_type("HourLine");
    let subs = h.of_type("SubLine");
    assert_eq!((hours.len(), subs.len()), (23, 72));
    // Half way down the track; the now line (10:30, 2 px wide) covers the sub-line under it.
    let (_, y, _, height) = rect_of(&hours[0]);
    let row = (y + height / 2.0) as u32;
    let (now_x, _, now_w, _) = rect_of(&h.find(AccessibleRole::Image, "Now"));
    let (mut drawn, mut covered) = (0, 0);
    for line in hours.iter().chain(&subs) {
        let x = rect_of(line).0;
        if x + 1.0 > now_x && x < now_x + now_w {
            covered += 1;
            continue;
        }
        let c = frame.rgb(x.round() as u32, row);
        assert!(
            c[1] < surface[1] && c[1] >= grid[1],
            "the line at {x}: {c:?}, between the grid {grid:?} and the surface"
        );
        drawn += 1;
    }
    assert_eq!(covered, 1, "the 10:30 sub-line under the now line");
    assert_eq!(drawn, 23 + 72 - 1);
}
