//! Bridge rows (zwt-bridge, zwt-ui-logic) to the Slint structs of `ui/state.slint`. Pure functions: a row converts the
//! same way every time, so the hub's "write only what changed" holds for the Slint models too.

use std::fmt::Write as _;

use chrono::{Datelike, NaiveDate};
use slint::{Color, ModelRc, SharedString, VecModel};
use z_work_core::model::TaskState;
use zwt_bridge::views::{Control, TaskRow};
use zwt_bridge::{DayChip, DayPiece, PieceKind, ShareRow, format};
use zwt_ui_logic::color::{contrast, hex_to_rgb};
use zwt_ui_logic::day;
use zwt_ui_logic::share::{CONTEXT_SEPARATOR, OTHER_COLOR};
use zwt_ui_logic::stripes::{self, Polygon, Ring};

use crate::{ChipData, ChipPart, PieceData, ShareRowData, SubGrid, TagData, TaskRowData};

/// Unix seconds (or any count) as a Slint `int`, saturating instead of wrapping.
#[must_use]
pub fn int(value: i64) -> i32 {
    i32::try_from(value).unwrap_or(if value < 0 { i32::MIN } else { i32::MAX })
}

/// A stored `#rrggbb` colour; when it does not parse, the grey 1.x draws an unknown colour in (`Axis.tsx`,
/// [`day::UNKNOWN_PROJECT_COLOR`]).
#[must_use]
pub fn color(hex: &str) -> Color {
    let [r, g, b] = hex_to_rgb(hex)
        .or_else(|| hex_to_rgb(day::UNKNOWN_PROJECT_COLOR))
        .unwrap_or_default();
    Color::from_rgb_u8(r, g, b)
}

/// Black or white, whichever reads better on `hex` (1.x `lib/color.ts` `readableText`).
#[must_use]
pub fn readable_text(hex: &str) -> Color {
    if contrast(hex, "#000000") >= contrast(hex, "#ffffff") {
        Color::from_rgb_u8(0, 0, 0)
    } else {
        Color::from_rgb_u8(0xff, 0xff, 0xff)
    }
}

fn text(s: &str) -> SharedString {
    SharedString::from(s)
}

fn model<T: Clone + 'static>(rows: impl IntoIterator<Item = T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from_iter(rows))
}

/// 0 draft, 1 running, 2 paused, 3 stopped (`TaskRowData.state`).
#[must_use]
pub const fn state_index(state: TaskState) -> i32 {
    match state {
        TaskState::Draft => 0,
        TaskState::Running => 1,
        TaskState::Paused => 2,
        TaskState::Stopped => 3,
    }
}

fn control(c: &Control) -> (bool, SharedString) {
    (c.enabled, text(&c.why))
}

/// One task card.
#[must_use]
pub fn task_row(r: &TaskRow) -> TaskRowData {
    let (start_on, start_why) = control(&r.controls.start);
    let (pause_on, pause_why) = control(&r.controls.pause);
    let (resume_on, resume_why) = control(&r.controls.resume);
    let (stop_on, stop_why) = control(&r.controls.stop);
    TaskRowData {
        id: text(&r.id),
        name: text(&r.name),
        state: state_index(r.state),
        has_project: r.has_project,
        project_name: text(&r.project_name),
        project_color: color(&r.project_color),
        project_text: readable_text(&r.project_color),
        project_archived: r.project_archived,
        tags: model(r.tags.iter().map(|t| TagData {
            name: text(&t.name),
            color: color(&t.color),
            text_color: readable_text(&t.color),
        })),
        logged_text: text(&r.logged_text),
        running_here: r.open_since.is_some(),
        open_since: int(r.open_since.unwrap_or(0)),
        note: text(&r.note),
        note_is_reason: r.note_is_reason,
        cold_note: text(&r.cold_note),
        start_on,
        start_why,
        pause_on,
        pause_why,
        resume_on,
        resume_why,
        stop_on,
        stop_why,
    }
}

/// One block of the day axis, labelled with its task's name (the row's own `label`, so a rename rewrites the row).
#[must_use]
pub fn piece(p: &DayPiece) -> PieceData {
    PieceData {
        id: text(&p.id),
        label: text(&p.label),
        start: int(p.start),
        end: int(p.end.unwrap_or(p.start)),
        open: p.end.is_none(),
        color: color(&p.color),
        text_color: readable_text(&p.color),
        outline: p.kind == PieceKind::BillablePause,
        lane: int(i64::from(p.lane)),
    }
}

/// The day axis' sub-grid (`AppState.sub-grid`): `increment` minutes from the day's head, the axis drawn `minute_px`
/// pixels a minute. A negative increment, or a width not yet laid out, draws none.
#[must_use]
pub fn sub_grid(increment: i32, minute_px: f32) -> SubGrid {
    let grid = day::sub_grid(u32::try_from(increment).unwrap_or(0), f64::from(minute_px));
    SubGrid {
        step: int(i64::from(grid.step)),
        per_hour: int(i64::from(grid.per_hour)),
    }
}

/// A day as `yyyy-mm-dd`, as the UI compares days and names them to `select-day`.
#[must_use]
pub fn iso(day: NaiveDate) -> SharedString {
    SharedString::from(day.format("%Y-%m-%d").to_string())
}

/// One chip of the day picker: "Wed", "4 Nov", "Wed 4 Nov 2026", what it writes when compact ("W", "4"), and its closed
/// time by project.
#[must_use]
pub fn chip(c: &DayChip) -> ChipData {
    let weekday = format::weekday_short(c.date);
    ChipData {
        date: iso(c.date),
        weekday: text(weekday),
        short: text(&format::short_day(c.date)),
        title: text(&format::day_label(c.date)),
        initial: weekday.chars().take(1).collect::<String>().into(),
        day: text(&c.date.day().to_string()),
        parts: model(c.by_project.iter().map(|p| ChipPart {
            color: color(&p.color),
            secs: int(p.secs),
        })),
    }
}

/// What a legend row writes after its name, muted, as one text: " · Logo concepts · Brand refresh", each name after a
/// middle dot as in [`ShareRow::written`]; empty when nothing follows. One text, so a row too narrow for all of it is
/// cut once, at the end of its line, and the name stays whole (1.x charts.tsx `ShareLegend`, review V2-SS1).
#[must_use]
pub fn context_line(context: &[String]) -> String {
    let mut line = String::new();
    for name in context {
        line.push(' ');
        line.push_str(CONTEXT_SEPARATOR);
        line.push(' ');
        line.push_str(name);
    }
    line
}

/// One slice and legend row. The figure is the row's own `text` ("16h 50m (47%)", written from its share of the total
/// minutes, so the rows add up to the middle); its two columns are the row's `hm` and `pct()`, the parts of `text`.
#[must_use]
pub fn share_row(r: &ShareRow) -> ShareRowData {
    let spoken = r.spoken_name().unwrap_or_else(|| r.written());
    ShareRowData {
        label: text(&r.label),
        context: text(&context_line(&r.context)),
        full_path: text(&r.full_path),
        spoken: text(&spoken),
        text: text(&r.text),
        hm: text(&r.hm),
        pct: text(&r.pct()),
        color: color(&r.color),
        other: r.color == OTHER_COLOR,
        stripes: model(r.stripes.iter().map(|c| color(c))),
        swatch_stripes: model(swatch_stripes(r.stripes.len())),
    }
}

/// Per colour, the SVG path of that colour's stripes; the first colour is drawn under the whole shape (no seam between
/// two stripes), so its entry is empty.
fn stripe_paths(pieces: &[Vec<Polygon>]) -> Vec<SharedString> {
    pieces
        .iter()
        .enumerate()
        .map(|(i, list)| {
            if i == 0 {
                SharedString::default()
            } else {
                text(&stripes::svg_path(list))
            }
        })
        .collect()
}

/// The stripes of a legend swatch for a combination of `colors` tags (charts.tsx `stripesBackground`); none for one.
#[must_use]
pub fn swatch_stripes(colors: usize) -> Vec<SharedString> {
    if colors < 2 {
        return Vec::new();
    }
    stripe_paths(&stripes::swatch_stripes(colors))
}

/// The stripes of a combination's slice of the donut, from `from` to `to` degrees (charts.tsx `StripePattern`); none
/// for a slice of one colour.
#[must_use]
pub fn arc_stripes(from: f32, to: f32, colors: usize) -> ModelRc<SharedString> {
    if colors < 2 {
        return ModelRc::default();
    }
    let pieces = stripes::sector_stripes(&Ring::DONUT, f64::from(from), f64::from(to), colors);
    model(stripe_paths(&pieces))
}

/// `count` lines falling to the right ("\"), `step` px apart horizontally, across a box `width` x `height`, starting
/// `height` px to the left so the box is covered from its left edge.
fn diagonal_lines(width: u32, height: u32, step: u32, rising: bool) -> String {
    let mut out = String::new();
    let h = height;
    let mut x = 0;
    while x <= width + h {
        let (y0, y1) = if rising { (h, 0) } else { (0, h) };
        let x0 = i64::from(x) - i64::from(h);
        let _ = write!(out, "M{x0} {y0}L{x} {y1}");
        x += step.max(1);
    }
    out
}

/// The running block's stripes (timeline.css `.block-live`: 135deg, 6 px of colour every 12 px): rising lines
/// 12 * sqrt(2) = 17 px apart; drawn 6 px thick.
#[must_use]
pub fn live_lines() -> String {
    diagonal_lines(2400, 32, 17, true)
}

/// A billable pause's hatching (timeline.css `.k-billablePause`: 45deg, 3 px every 8 px): falling lines
/// 8 * sqrt(2) = 11 px apart; drawn 3 px thick.
#[must_use]
pub fn pause_lines() -> String {
    diagonal_lines(2400, 32, 11, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::Model;

    #[test]
    fn colours_parse_and_fall_back_to_grey() {
        assert_eq!(color("#2563eb"), Color::from_rgb_u8(0x25, 0x63, 0xeb));
        assert_eq!(
            color("var(--border-strong)"),
            Color::from_rgb_u8(0x6b, 0x72, 0x80)
        );
    }

    #[test]
    fn text_on_a_colour_is_black_or_white_as_1x_picks_it() {
        // lib/color.ts readableText: Brand refresh blue reads white, a pale yellow reads black.
        assert_eq!(readable_text("#2563eb"), Color::from_rgb_u8(255, 255, 255));
        assert_eq!(readable_text("#fde68a"), Color::from_rgb_u8(0, 0, 0));
    }

    #[test]
    fn the_sub_grid_crosses_into_slint_as_the_logic_works_it_out() {
        let at = |step, per_hour| SubGrid { step, per_hour };
        assert_eq!(sub_grid(15, 0.5625), at(15, 3));
        assert_eq!(sub_grid(1, 0.5625), at(6, 9));
        assert_eq!(
            sub_grid(0, 0.5625),
            at(0, 0),
            "rounding off is mapped before"
        );
        assert_eq!(sub_grid(-15, 0.5625), at(0, 0));
        assert_eq!(sub_grid(15, 0.0), at(0, 0), "not laid out yet");
        assert_eq!(sub_grid(15, f32::NAN), at(0, 0));
    }

    #[test]
    fn what_follows_a_legend_name_is_one_text_after_middle_dots() {
        assert_eq!(context_line(&[]), "");
        assert_eq!(
            context_line(&["Logo concepts".to_owned(), "Brand refresh".to_owned()]),
            " · Logo concepts · Brand refresh"
        );
    }

    #[test]
    fn a_chip_carries_what_its_compact_form_writes() {
        let november = |d| DayChip {
            date: NaiveDate::from_ymd_opt(2026, 11, d).unwrap_or_default(),
            closed_secs: 0,
            by_project: Vec::new(),
        };
        let wed = chip(&november(4));
        assert_eq!(
            (wed.initial.as_str(), wed.day.as_str()),
            ("W", "4"),
            "Wed 4 Nov"
        );
        assert_eq!(
            wed.title, "Wed 4 Nov 2026",
            "the accessible name stays whole"
        );
        let sat = chip(&november(28));
        assert_eq!((sat.initial.as_str(), sat.day.as_str()), ("S", "28"));
    }

    #[test]
    fn seconds_saturate_instead_of_wrapping() {
        assert_eq!(int(1_793_779_200), 1_793_779_200);
        assert_eq!(int(i64::MAX), i32::MAX);
        assert_eq!(int(i64::MIN), i32::MIN);
    }

    #[test]
    fn stripe_paths_cover_their_box() {
        let live = live_lines();
        assert!(live.starts_with("M-32 32L0 0"), "{live}");
        assert!(swatch_stripes(1).is_empty());
        // Three colours: the first is the background, the others one path each, of closed pieces in the swatch.
        let three = swatch_stripes(3);
        assert!(
            matches!(three.as_slice(), [first, second, third]
                if first.is_empty()
                    && [second, third].iter().all(|p| p.starts_with('M') && p.ends_with('Z'))),
            "{three:?}"
        );
        assert_eq!(arc_stripes(10.0, 20.0, 1).row_count(), 0);
        let arc = arc_stripes(10.0, 46.0, 2);
        assert_eq!(arc.row_count(), 2);
        assert!(arc.row_data(1).is_some_and(|p| p.contains('Z')));
    }
}
