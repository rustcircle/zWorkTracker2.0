//! V2-DP1 of the visual review (docs/visual/REVIEW.md): the day picker's 14 chips share the strip beside the rail,
//! which keeps its 420 px, so the chips narrow with the window: "Today" is cut first, then the dates, and in the
//! narrowest window (960 px, app.slint) both lines of every chip were "…". When the chips are narrower than the widest
//! label a chip writes and its padding, every chip is compact: the weekday's initial over the day's number, today's
//! number in the accent colour. A chip's accessible name stays the whole date.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

mod common;

use chrono::{Datelike, NaiveDate};
use common::{Frame, Harness, rect_of};
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, LogicalSize, Model};
use zwt_app::{ChipData, Strings};

/// A chip of the page on screen, found by its accessible name: the whole date ("Wed 4 Nov 2026").
fn chip_on_screen(h: &Harness, chip: &ChipData) -> ElementHandle {
    let found = h.find_all(AccessibleRole::RadioButton, chip.title.as_str());
    assert_eq!(found.len(), 1, "one chip named {:?}", chip.title);
    found.into_iter().next().unwrap()
}

/// The texts of a chip, top to bottom.
fn texts_of(chip: &ElementHandle) -> Vec<ElementHandle> {
    let mut texts = chip
        .query_descendants()
        .match_accessible_role(AccessibleRole::Text)
        .find_all();
    texts.sort_by(|a, b| a.absolute_position().y.total_cmp(&b.absolute_position().y));
    texts
}

fn label(e: &ElementHandle) -> String {
    e.accessible_label().unwrap_or_default().to_string()
}

fn date_of(chip: &ChipData) -> NaiveDate {
    NaiveDate::parse_from_str(chip.date.as_str(), "%Y-%m-%d").unwrap()
}

/// The box of the pixels of `rect` that are ink on `paper`: further than a quarter of the range from it in a channel.
fn ink_box(
    frame: &Frame,
    rect: (f32, f32, f32, f32),
    paper: [u8; 3],
) -> Option<(u32, u32, u32, u32)> {
    let (x, y, w, h) = rect;
    let (x0, y0) = (x.floor().max(0.0) as u32, y.floor().max(0.0) as u32);
    let x1 = ((x + w).ceil() as u32).min(frame.width());
    let y1 = ((y + h).ceil() as u32).min(frame.height());
    let mut found: Option<(u32, u32, u32, u32)> = None;
    for py in y0..y1 {
        for px in x0..x1 {
            if frame.distance(px, py, paper) > 64 {
                found = Some(match found {
                    None => (px, py, px, py),
                    Some((a, b, c, d)) => (a.min(px), b.min(py), c.max(px), d.max(py)),
                });
            }
        }
    }
    found
}

/// The two lines a compact chip writes (the weekday's initial over the day's number), if it is compact.
fn compact_lines(chip: &ElementHandle) -> Vec<ElementHandle> {
    ["DayChip::initial", "DayChip::number"]
        .into_iter()
        .filter_map(|id| chip.query_descendants().match_id(id).find_first())
        .collect()
}

/// What is wrong with the chips of the page at `width` x 900: each must be compact, its two lines drawn as a letter
/// and digits (an ellipsis is a row of dots on the baseline, 1 to 3 px tall; a letter or a digit is as tall as a
/// capital) within the chip, and its accessible name must be its whole date.
fn faults(h: &Harness, width: f32) -> Vec<String> {
    h.app.window().set_size(LogicalSize::new(width, 900.0));
    h.settle();
    let frame = h.snapshot();
    let chips: Vec<ChipData> = h.state().get_chips().iter().collect();
    assert_eq!(chips.len(), 14, "a page of 14 days");
    let mut faults = Vec::new();
    for chip in &chips {
        let on_screen = chip_on_screen(h, chip);
        let day = date_of(chip);
        if chip.title.as_str() != format!("{} {} {}", chip.weekday, chip.short, day.year()) {
            faults.push(format!(
                "{width} px: the accessible name {:?} is not the whole date",
                chip.title
            ));
        }
        let mut lines = compact_lines(&on_screen);
        if lines.len() != 2 {
            faults.push(format!(
                "{width} px, {}: not compact, writes {:?}",
                chip.title,
                texts_of(&on_screen).iter().map(label).collect::<Vec<_>>()
            ));
            lines = texts_of(&on_screen);
        }
        let (cx, cy, cw, _) = rect_of(&on_screen);
        // The chip's own colour, in its top padding: under its border (2 px at most) and over its first line.
        let paper = frame.rgb((cx + cw / 2.0) as u32, (cy + 4.0) as u32);
        for (line, what) in lines.iter().zip(["first line", "second line"]) {
            let Some((left, top, right, bottom)) = ink_box(&frame, rect_of(line), paper) else {
                faults.push(format!(
                    "{width} px, {}: the {what} is not drawn",
                    chip.title
                ));
                continue;
            };
            if bottom - top + 1 < 5 {
                faults.push(format!(
                    "{width} px, {}: the {what} is drawn {} px tall: an ellipsis",
                    chip.title,
                    bottom - top + 1
                ));
            }
            if (left as f32) < cx || right as f32 >= cx + cw {
                faults.push(format!(
                    "{width} px, {}: the {what} runs out of the chip",
                    chip.title
                ));
            }
        }
    }
    faults
}

/// The narrowest window (960 px: chips of 22.7 px) and 1,280 px (45.6 px), the two widths the review checks.
#[test]
fn narrow_windows_draw_every_chip_compact_and_whole() {
    let h = Harness::rendered(LogicalSize::new(960.0, 900.0));
    let mut found = faults(&h, 960.0);
    found.extend(faults(&h, 1280.0));
    assert!(found.is_empty(), "{}", found.join("\n"));
}

/// In the 1,360 px window of the screenshots the chips are as they were: "Today" or the weekday over the date.
#[test]
fn the_chips_of_the_wide_window_keep_their_names_and_dates() {
    let h = Harness::rendered(LogicalSize::new(1360.0, 900.0));
    let today = h.app.global::<Strings>().get_day_picker_today();
    let today_date = h.state().get_today_date();
    for chip in h.state().get_chips().iter() {
        let first = if chip.date == today_date {
            today.to_string()
        } else {
            chip.weekday.to_string()
        };
        assert_eq!(
            texts_of(&chip_on_screen(&h, &chip))
                .iter()
                .map(label)
                .collect::<Vec<_>>(),
            vec![first, chip.short.to_string()],
            "{}",
            chip.title
        );
    }
}

/// Whether the chips of the page are compact at `width` x 900, all of them or none.
fn compact_at(h: &Harness, width: u32) -> bool {
    h.app
        .window()
        .set_size(LogicalSize::new(width as f32, 900.0));
    h.settle();
    let compact = ElementHandle::find_by_element_id(&h.app, "DayChip::initial").count();
    let chips = h.state().get_chips().row_count();
    assert!(
        compact == 0 || compact == chips,
        "{width} px: every chip of the page or none is compact, not {compact} of {chips}"
    );
    compact == chips
}

/// The chips turn compact where the window narrows past one width, and back where it widens past it, and paging does
/// not move that width: "Today" counts as a label on a page without today too, so a page of older days at the same
/// width is drawn the same way.
#[test]
fn the_chips_turn_compact_at_one_width_on_every_page() {
    let mut h = Harness::rendered(LogicalSize::new(1360.0, 900.0));
    let widths: Vec<u32> = (960..=1400).step_by(2).collect();
    let states: Vec<bool> = widths.iter().map(|w| compact_at(&h, *w)).collect();
    let wide = states
        .iter()
        .position(|c| !c)
        .expect("a wide window draws the chips whole");
    assert!(wide > 0, "compact at 960 px");
    assert!(
        states[wide..].iter().all(|c| !c),
        "not compact from {} px on: {states:?}",
        widths[wide]
    );
    let (narrow, wide) = (widths[wide - 1], widths[wide]);

    // A page of older days: an hour on 15 October lets the left arrow show 8 to 21 October.
    h.log_by_hand("2026-10-15T09:00:00Z", "2026-10-15T10:00:00Z");
    h.find(AccessibleRole::Button, "Previous days")
        .mock_single_click(PointerEventButton::Left);
    h.pump();
    let first = h.state().get_chips().row_data(0).unwrap();
    assert_eq!(first.date.as_str(), "2026-10-08", "the page before");
    assert!(
        compact_at(&h, narrow),
        "{narrow} px: compact on the older page too"
    );
    assert!(
        !compact_at(&h, wide),
        "{wide} px: whole on the older page too"
    );
}
