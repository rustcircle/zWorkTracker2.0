//! One day of the Timeline: the pieces of time drawn on its day axis, the sub-grid that splits its hours, and the
//! chips of the day picker.
//!
//! Follows how 1.x cuts and colours a day (`features/timeline/model.ts`: `timelineSegs`, `buildClosed`,
//! `buildOverlay`, `blocksOf`; `axisGeometry.ts`: `assignLanes`; `Axis.tsx`: block colour). PARITY rows 124-127:
//! a day has its real length in the zone (23 or 25 hours across a clock change), and a piece that crosses midnight
//! is cut at it, each day drawing its own part. The sub-grid is new in 2.0 (owner, 2026-11-04): 1.x draws hour lines
//! only.

use std::collections::BTreeMap;

use chrono::{Days, NaiveDate};
use chrono_tz::Tz;
use z_work_core::Ts;
use z_work_core::billing::{DEFAULT_INCREMENT_MINUTES, rounding_rule, split_by_day, start_of_day};
use z_work_core::model::LocalFile;
use z_work_core::pieces::Piece;

pub use z_work_core::pieces::PieceKind;

use crate::counted::{is_live, pieces_at};

/// The colour of a block whose project 1.x cannot find (`project?.color ?? '#6b7280'` in `Axis.tsx`): the grey of
/// "No tag", [`crate::share::UNTAGGED_COLOR`].
pub const UNKNOWN_PROJECT_COLOR: &str = crate::share::UNTAGGED_COLOR;

/// One block of the day axis: one piece of counted time, cut to the day.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DayPiece {
    pub task_id: String,
    /// The block's text and accessible name: its task's name, empty when the task is not found. A row carries it, so
    /// a renamed task's blocks differ from the ones the UI holds and are rewritten (review R3).
    pub label: String,
    pub project_id: String,
    /// The project's colour, `#rrggbb` as stored, or [`UNKNOWN_PROJECT_COLOR`].
    pub color: String,
    /// Unix seconds, no earlier than the day's first instant.
    pub start: i64,
    /// Unix seconds, no later than the next day's first instant. `None` while the piece is still running on this
    /// device: it ends at now (the UI's `now-sec`), and on a day before the one now is in, at that day's end.
    pub end: Option<i64>,
    /// The core's piece id (`<segment id>`, `<segment id>:<n>`, `<pause id>:<n>`): stable while the data does not
    /// change, unique within a day, so a keyed list can diff by it.
    pub id: String,
    /// Work and thinking are drawn solid in the project's colour; a billable pause as an outline in it.
    pub kind: PieceKind,
    /// `assignLanes`: the row of the axis the block sits in, 0 first; pieces that overlap (two devices) never share a
    /// lane. The axis is as tall as the highest lane plus one.
    pub lane: u32,
}

/// A project's seconds on one day.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectSecs {
    pub project_id: String,
    /// The project's colour, as [`DayPiece::color`].
    pub color: String,
    pub secs: i64,
}

/// One chip of the day picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DayChip {
    pub date: NaiveDate,
    /// Worked seconds (work and thinking) of the closed pieces only, so a running timer never changes a chip: the UI
    /// adds the running piece from `now-sec` if it wants it.
    pub closed_secs: i64,
    /// The same seconds per project, most first, then by project id (as 1.x orders `projectTotals`).
    pub by_project: Vec<ProjectSecs>,
}

/// The first instant of `day` and of the day after, in `tz`.
fn day_bounds(day: NaiveDate, tz: Tz) -> (Ts, Ts) {
    let end = day
        .checked_add_days(Days::new(1))
        .map_or(Ts::MAX, |next| start_of_day(next, tz));
    (start_of_day(day, tz), end)
}

fn project_color(file: &LocalFile, project_id: &str) -> String {
    file.data
        .projects
        .get(project_id)
        .map_or_else(|| UNKNOWN_PROJECT_COLOR.to_owned(), |p| p.color.clone())
}

/// Every counted piece, an open segment of this device left open (its end is far in the future, where no day axis
/// reaches) and another device's ending at its last heartbeat.
fn open_ended_pieces(file: &LocalFile) -> Vec<Piece> {
    pieces_at(file, Ts::MAX)
}

/// `assignLanes`: greedy interval partitioning by start, then end; a running piece runs to the end of time.
fn assign_lanes(pieces: &mut [DayPiece]) {
    let mut order: Vec<usize> = (0..pieces.len()).collect();
    let end_of = |p: &DayPiece| p.end.unwrap_or(i64::MAX);
    order.sort_by_key(|&i| pieces.get(i).map(|p| (p.start, end_of(p))));
    let mut ends: Vec<i64> = Vec::new();
    for i in order {
        let Some(piece) = pieces.get_mut(i) else {
            continue;
        };
        let lane = match ends.iter().position(|&e| e <= piece.start) {
            Some(lane) => lane,
            None => {
                ends.push(0);
                ends.len() - 1
            }
        };
        if let Some(e) = ends.get_mut(lane) {
            *e = end_of(piece);
        }
        piece.lane = u32::try_from(lane).unwrap_or(u32::MAX);
    }
}

/// The pieces of counted time of `day` in `tz` (work, thinking and billable pauses of the live tasks), each cut to
/// the day, ordered by start (then id), with their lanes.
///
/// A piece still running on this device has `end: None` on every day from the one it started on; the UI draws it
/// to `now-sec` and never past the axis. Only days up to the one now is in are asked for. Another device's open piece
/// ends at that device's last heartbeat. Pieces of no length are left out, as in 1.x.
#[must_use]
pub fn day_pieces(file: &LocalFile, day: NaiveDate, tz: Tz) -> Vec<DayPiece> {
    let (day_start, day_end) = day_bounds(day, tz);
    let mut out = Vec::new();
    for p in open_ended_pieces(file) {
        let live = is_live(&p, file);
        let start = p.start.max(day_start);
        let end = if live {
            if p.start >= day_end {
                continue;
            }
            None
        } else {
            let end = p.end.min(day_end);
            if end <= start {
                continue;
            }
            Some(end.secs())
        };
        let task = file.data.tasks.get(&p.task_id);
        let project_id = task.map(|t| t.project_id.clone()).unwrap_or_default();
        out.push(DayPiece {
            color: project_color(file, &project_id),
            project_id,
            label: task.map(|t| t.name.clone()).unwrap_or_default(),
            task_id: p.task_id,
            start: start.secs(),
            end,
            id: p.id,
            kind: p.kind,
            lane: 0,
        });
    }
    out.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| a.id.cmp(&b.id)));
    assign_lanes(&mut out);
    out
}

/// The minutes the day axis' sub-grid splits the hours of `day` by: the increment of the global rounding rule in force
/// on that day (1.x Settings > Rounding, "Increment", read as its Settings page reads it: `ruleFor(rounding, '', day)`;
/// a project's own rule leaves the axis alone), or the core's default of 15 when no rule is saved or rounding is off
/// (0, exact time). The owner, 2026-11-04: "a sub grid in the timeline of hours that splits time in the increments
/// choosen".
#[must_use]
pub fn day_increment(file: &LocalFile, day: NaiveDate) -> u32 {
    match rounding_rule(&file.data, "", day).increment_minutes {
        0 => DEFAULT_INCREMENT_MINUTES,
        minutes => minutes,
    }
}

/// The least distance between two lines of the day axis' grid, in pixels: closer, they would merge into a tint.
pub const MIN_LINE_GAP_PX: f64 = 3.0;

/// The sub-grid of a day axis: in each hour, a line `step` minutes into it, then at each further `step`,
/// `per_hour` lines in all; none on the hour, where the hour line is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SubGrid {
    /// Minutes between two lines; 0 with no sub-grid.
    pub step: u32,
    /// Lines in each hour; 0 with no sub-grid.
    pub per_hour: u32,
}

/// The sub-grid for an increment of `increment_minutes` on an axis drawn `minute_px` pixels a minute: a line at each
/// multiple of the increment into each hour while the lines stay [`MIN_LINE_GAP_PX`] apart; else at each multiple of
/// the least multiple of the increment that keeps them so and, when the increment splits the hour evenly, still splits
/// it (1 minute marks every 2, 3, 4, 5, 6, 10, 12, 15, 20 or 30 minutes as the width allows; 5 every 10, 15, 20 or 30,
/// never every 25). The last line of an increment that does not split the hour is left out when it is too near the
/// next hour's line (7: :07 to :49, not :56). None for an increment of an hour or more: the hour lines are the grid.
#[must_use]
pub fn sub_grid(increment_minutes: u32, minute_px: f64) -> SubGrid {
    if increment_minutes == 0 || increment_minutes >= 60 || minute_px.is_nan() || minute_px <= 0.0 {
        return SubGrid::default();
    }
    let apart = |minutes: u32| f64::from(minutes) * minute_px >= MIN_LINE_GAP_PX;
    let splits_hour = 60_u32.is_multiple_of(increment_minutes);
    let Some(step) = (1..)
        .map(|k| k * increment_minutes)
        .take_while(|&minutes| minutes < 60)
        .filter(|&minutes| !splits_hour || 60_u32.is_multiple_of(minutes))
        .find(|&minutes| apart(minutes))
    else {
        return SubGrid::default();
    };
    // The multiples of the step inside the hour; the last one goes when it is too near the next hour's line, which
    // only happens when the step does not split the hour (60 - last < step).
    let mut per_hour = 59 / step;
    if !apart(60 - per_hour * step) {
        per_hour -= 1;
    }
    if per_hour == 0 {
        return SubGrid::default();
    }
    SubGrid { step, per_hour }
}

/// The first day in `tz` with a block on its axis: the day the earliest piece of counted time starts on (a piece still
/// running here counts from its start, as [`day_pieces`] draws it), or `None` with no such piece. The day picker shows
/// older days only while one of them has time (zwt-bridge `views::DayPage::older`).
#[must_use]
pub fn first_day(file: &LocalFile, tz: Tz) -> Option<NaiveDate> {
    open_ended_pieces(file)
        .into_iter()
        .filter(|p| is_live(p, file) || p.end > p.start)
        .map(|p| p.start)
        .min()
        .map(|start| start.datetime().with_timezone(&tz).date_naive())
}

/// The chips of the day picker: `days` days ending with `last` in `tz`, the oldest first (2.0 runs the Timeline from
/// the past to the future, owner 2026-11-04; 1.x lists its days newest first), every day listed even with no time,
/// each with its closed worked seconds in all and per project.
#[must_use]
pub fn day_chips(file: &LocalFile, last: NaiveDate, days: u32, tz: Tz) -> Vec<DayChip> {
    if days == 0 {
        return Vec::new();
    }
    // Near the calendar's start there are fewer days to list, as there are no earlier dates.
    let oldest = last
        .checked_sub_days(Days::new(u64::from(days - 1)))
        .unwrap_or(NaiveDate::MIN);
    let dates: Vec<NaiveDate> = oldest.iter_days().take_while(|d| *d <= last).collect();
    let (window_start, _) = day_bounds(oldest, tz);
    let (_, window_end) = day_bounds(last, tz);
    let mut secs: BTreeMap<NaiveDate, BTreeMap<String, i64>> = BTreeMap::new();
    for p in open_ended_pieces(file) {
        if is_live(&p, file) || p.kind == PieceKind::BillablePause {
            continue;
        }
        let (a, b) = (p.start.max(window_start), p.end.min(window_end));
        if b <= a {
            continue;
        }
        let Some(task) = file.data.tasks.get(&p.task_id) else {
            continue;
        };
        for (date, s, e) in split_by_day(a, b, tz) {
            *secs
                .entry(date)
                .or_default()
                .entry(task.project_id.clone())
                .or_default() += e.secs() - s.secs();
        }
    }
    dates
        .into_iter()
        .map(|date| {
            let mut by_project: Vec<ProjectSecs> = secs
                .remove(&date)
                .unwrap_or_default()
                .into_iter()
                .filter(|(_, s)| *s > 0)
                .map(|(project_id, secs)| ProjectSecs {
                    color: project_color(file, &project_id),
                    project_id,
                    secs,
                })
                .collect();
            by_project.sort_by(|a, b| {
                b.secs
                    .cmp(&a.secs)
                    .then_with(|| a.project_id.cmp(&b.project_id))
            });
            DayChip {
                date,
                closed_secs: by_project.iter().map(|p| p.secs).sum(),
                by_project,
            }
        })
        .collect()
}
