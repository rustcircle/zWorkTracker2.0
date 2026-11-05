//! The analysis sheets of a selected Excel export, worked out here from the selected task-days alone,
//! since the backend takes no totals from the UI. This is a port of the parts of the Analysis tab that
//! `src/features/analysis/sheets.ts` reads, for the same cases: no filters, no comparison, Billable as
//! the basis. `fixtures/analytics-parity.*.json` pins both sides to the same figures.
//!
//! A whole-period export still takes its sheets from the UI, so they match the screen it was made from.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Days, NaiveDate, Offset, TimeZone};
use chrono_tz::{OffsetName, Tz};
use serde_json::{Value, json};

use crate::billing::{
    ROUNDED_ROWS_NOTE, TaskDay, cents_from_numerator, day_of, money, split_by_day, split_by_rate,
    start_of_day, task_days_of,
};
use crate::error::CoreError;
use crate::model::{BillingType, Shared, ShiftVersion, TaskState};
use crate::pieces::{Piece, PieceKind};
use crate::report::{AnalyticsColumn, AnalyticsSheet, CellKind, ExportSource};
use crate::time::Ts;
use crate::validate::parse_zone;

const WEEKDAY_NAMES: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];
const WEEKDAY_SHORT: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTH_SHORT: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const BANDS: [&str; 4] = ["Night", "Morning", "Afternoon", "Evening"];
/// Sessions shorter than this count as fragmented.
const FRAGMENT_SECS: i64 = 25 * 60;
/// Two sessions this close count as one uninterrupted stretch.
const STREAK_GAP_SECS: i64 = 1;
const LATE_TOLERANCE_SECS: i64 = 60;
const DAY_SECS: i64 = 86_400;

// ---------------------------------------------------------------- days and zones

fn day_num(d: NaiveDate) -> i64 {
    i64::from(chrono::Datelike::num_days_from_ce(&d)) - 719_163
}

fn day_from(n: i64) -> NaiveDate {
    i32::try_from(n + 719_163)
        .ok()
        .and_then(NaiveDate::from_num_days_from_ce_opt)
        .unwrap_or_default()
}

/// 0 = Monday .. 6 = Sunday.
fn weekday_index(n: i64) -> usize {
    usize::try_from((n + 3).rem_euclid(7)).unwrap_or(0)
}

fn offset_at(sec: i64, tz: Tz) -> i64 {
    let off = tz.offset_from_utc_datetime(&Ts(sec).datetime().naive_utc());
    i64::from(off.fix().local_minus_utc())
}

fn day_of_instant(sec: i64, tz: Tz) -> i64 {
    (sec + offset_at(sec, tz)).div_euclid(DAY_SECS)
}

/// Calls `cb(start, end, day, hour)` for each piece of `[start, end)` inside one wall-clock hour.
fn walk_hours(start: i64, end: i64, tz: Tz, mut cb: impl FnMut(i64, i64, i64, usize)) {
    let mut cur = start;
    while cur < end {
        let off = offset_at(cur, tz);
        let local = cur + off;
        let day = local.div_euclid(DAY_SECS);
        let in_day = local - day * DAY_SECS;
        let hour = in_day / 3600;
        let mut next = (cur + 3600 - (in_day - hour * 3600)).min(end);
        if next - 1 > cur && offset_at(next - 1, tz) != off {
            let (mut lo, mut hi) = (cur, next - 1);
            while hi - lo > 1 {
                let mid = lo + (hi - lo) / 2;
                if offset_at(mid, tz) == off {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            next = hi;
        }
        cb(cur, next, day, usize::try_from(hour).unwrap_or(0));
        cur = next;
    }
}

/// The first instant whose wall clock reaches `minutes` past midnight on `day`. In a fall-back
/// overlap, the earlier pass. In a spring-forward gap, the instant the clocks jump.
fn instant_of_wall(day: NaiveDate, minutes: i64, tz: Tz) -> i64 {
    let target = day_num(day) * DAY_SECS + minutes * 60;
    let before = offset_at(target - 172_800, tz);
    let after = offset_at(target + 172_800, tz);
    let (a, b) = (target - before, target - after);
    let (ok_a, ok_b) = (offset_at(a, tz) == before, offset_at(b, tz) == after);
    match (ok_a, ok_b) {
        (true, true) => a.min(b),
        (true, false) => a,
        (false, true) => b,
        (false, false) => {
            let (mut lo, mut hi) = (a.min(b), a.max(b));
            let from = offset_at(lo, tz);
            while hi - lo > 1 {
                let mid = lo + (hi - lo) / 2;
                if offset_at(mid, tz) == from {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            hi
        }
    }
}

fn hhmm_minutes(text: &str) -> i64 {
    let mut parts = text.split(':').map(|p| p.parse::<i64>().unwrap_or(0));
    let h = parts.next().unwrap_or(0);
    h * 60 + parts.next().unwrap_or(0)
}

fn format_offset(offset: i64) -> String {
    let sign = if offset < 0 { '-' } else { '+' };
    let abs = offset.abs();
    let (h, m) = (abs / 3600, (abs % 3600) / 60);
    if m > 0 {
        format!("UTC{sign}{h}:{m:02}")
    } else {
        format!("UTC{sign}{h}")
    }
}

/// "New York, EDT, UTC-4".
fn zone_label(name: &str, tz: Tz, sec: i64) -> String {
    if name == "UTC" {
        return "UTC".to_owned();
    }
    let off = offset_at(sec, tz);
    let period = tz.offset_from_utc_datetime(&Ts(sec).datetime().naive_utc());
    let abbr = period
        .abbreviation()
        .map_or_else(|| format_offset(off), str::to_owned);
    let city = name.rsplit('/').next().unwrap_or(name).replace('_', " ");
    format!("{city}, {abbr}, {}", format_offset(off))
}

/// Minutes after midnight as "09:05". Past 24:00 it keeps counting ("25:10").
fn fmt_clock(minutes: f64) -> String {
    let m = (minutes + 0.5).floor() as i64;
    format!("{:02}:{:02}", m.div_euclid(60), m.rem_euclid(60))
}

/// Close to the screen's `localeCompare` without a collation library: case-insensitive, lower case
/// first on a tie. Accents aren't folded, so "Émile" sorts after "Frank". That only moves rows: shifts,
/// which sort by name, and rows with exactly equal time.
fn cmp_names(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| b.cmp(a))
}

fn ratio(part: f64, whole: f64) -> Option<f64> {
    (whole > 0.0).then(|| part / whole)
}

/// Quantile of an ascending list, linear between ranks (R type 7).
fn quantile(sorted: &[f64], q: f64) -> Option<f64> {
    let n = sorted.len();
    if n == 0 {
        return None;
    }
    let pos = (n - 1) as f64 * q;
    let (lo, hi) = (pos.floor(), pos.ceil());
    let a = *sorted.get(lo as usize)?;
    let b = *sorted.get(hi as usize)?;
    Some(a + (b - a) * (pos - lo))
}

fn bump<K: Ord>(map: &mut BTreeMap<K, i64>, key: K, by: i64) {
    *map.entry(key).or_insert(0) += by;
}

fn add<const N: usize>(list: &mut [i64; N], at: usize, by: i64) {
    if let Some(x) = list.get_mut(at) {
        *x += by;
    }
}

// ---------------------------------------------------------------- the period

struct Period {
    from_day: NaiveDate,
    to_day: NaiveDate,
    from: i64,
    to: i64,
    days: i64,
    label: String,
}

fn period_of(from_day: NaiveDate, to_day: NaiveDate, tz: Tz) -> Period {
    let short = |d: NaiveDate| {
        let month = usize::try_from(chrono::Datelike::month0(&d)).unwrap_or(0);
        format!(
            "{} {}",
            chrono::Datelike::day(&d),
            MONTH_SHORT.get(month).copied().unwrap_or("")
        )
    };
    let (y, ty) = (
        chrono::Datelike::year(&from_day),
        chrono::Datelike::year(&to_day),
    );
    let year = if y == ty {
        String::new()
    } else {
        format!(" {y}")
    };
    Period {
        from_day,
        to_day,
        from: start_of_day(from_day, tz).secs(),
        to: start_of_day(from_day.max(to_day) + Days::new(1), tz).secs(),
        days: day_num(to_day) - day_num(from_day) + 1,
        label: format!("{}{year} – {} {ty}", short(from_day), short(to_day)),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Unit {
    Hour,
    Day,
    Week,
    Month,
}

fn unit_for(days: i64) -> Unit {
    match days {
        1 => Unit::Hour,
        2..=62 => Unit::Day,
        63..=400 => Unit::Week,
        _ => Unit::Month,
    }
}

struct Bucket {
    label: String,
    from_day: i64,
    to_day: i64,
}

/// The rows of the earnings sheet: one per bucket. A single day is one bucket of a day, since hour
/// buckets have no Billable figure.
fn buckets_of(period: &Period, unit: Unit) -> Vec<Bucket> {
    let (first, last) = (day_num(period.from_day), day_num(period.to_day));
    let month_name = |d: NaiveDate| {
        let month = usize::try_from(chrono::Datelike::month0(&d)).unwrap_or(0);
        MONTH_SHORT.get(month).copied().unwrap_or("")
    };
    let mut out = Vec::new();
    let mut n = first;
    while n <= last {
        let d = day_from(n);
        let (label, end) = match unit {
            Unit::Hour | Unit::Day => {
                let date = chrono::Datelike::day(&d);
                let label = if period.days <= 7 {
                    format!(
                        "{} {date}",
                        WEEKDAY_SHORT.get(weekday_index(n)).copied().unwrap_or("")
                    )
                } else {
                    date.to_string()
                };
                (label, n)
            }
            Unit::Week => {
                let monday = n - i64::try_from(weekday_index(n)).unwrap_or(0);
                (
                    format!("{} {}", chrono::Datelike::day(&d), month_name(d)),
                    last.min(monday + 6),
                )
            }
            Unit::Month => {
                let (y, m) = (chrono::Datelike::year(&d), chrono::Datelike::month(&d));
                let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
                let next = NaiveDate::from_ymd_opt(ny, nm, 1).map_or(last + 1, day_num);
                let single = y == chrono::Datelike::year(&period.to_day)
                    && y == chrono::Datelike::year(&period.from_day);
                let label = if single {
                    month_name(d).to_owned()
                } else {
                    format!("{} {:02}", month_name(d), y.rem_euclid(100))
                };
                (label, last.min(next - 1))
            }
        };
        out.push(Bucket {
            label,
            from_day: n,
            to_day: end,
        });
        n = end + 1;
    }
    out
}

// ---------------------------------------------------------------- stretches and spans

/// Adjacent pieces of one recorded segment and kind, joined so a session stays one session.
#[derive(Clone)]
struct Stretch {
    task: String,
    start: i64,
    end: i64,
    /// Open on this device, so its end is now.
    live: bool,
}

/// Worked stretches, closed first, the way the screen's engine holds them. Billable pauses are left
/// out: they feed the task-day rows and nothing else.
fn stretches_of(pieces: &[Piece], own_device: &str) -> Vec<Stretch> {
    let mut groups: BTreeMap<(&str, &str, u8), Vec<&Piece>> = BTreeMap::new();
    for p in pieces.iter().filter(|p| p.kind != PieceKind::BillablePause) {
        let kind = u8::from(p.kind == PieceKind::Thinking);
        groups
            .entry((p.task_id.as_str(), p.source_id.as_str(), kind))
            .or_default()
            .push(p);
    }
    let mut closed = Vec::new();
    let mut live = Vec::new();
    for list in groups.values_mut() {
        list.sort_by_key(|p| (p.start, p.end));
        let mut cur: Option<(Stretch, bool)> = None;
        for p in list.iter() {
            let is_live = p.open && p.device_id.as_deref() == Some(own_device);
            match &mut cur {
                Some((s, open)) if !*open && p.start.secs() == s.end => {
                    s.end = p.end.secs();
                    s.live = is_live;
                    *open = p.open;
                }
                _ => {
                    if let Some((s, _)) = cur.take() {
                        if s.live { live.push(s) } else { closed.push(s) }
                    }
                    cur = Some((
                        Stretch {
                            task: p.task_id.clone(),
                            start: p.start.secs(),
                            end: p.end.secs(),
                            live: is_live,
                        },
                        p.open,
                    ));
                }
            }
        }
        if let Some((s, _)) = cur {
            if s.live { live.push(s) } else { closed.push(s) }
        }
    }
    closed.sort_by_key(|s| (s.start, s.end));
    live.sort_by_key(|s| (s.start, s.end));
    closed.extend(live);
    closed
}

/// A stretch of logged time cut to the period.
struct Span {
    task: String,
    project: String,
    start: i64,
    end: i64,
    /// The stretch's own bounds, for start and finish times.
    own_start: i64,
    own_end: i64,
    day: i64,
}

#[derive(Default)]
struct DayAgg {
    secs: i64,
    /// Earliest start and latest end, in minutes after midnight, of spans that started this day.
    first_min: f64,
    last_min: f64,
    started: i64,
}

#[derive(Default)]
struct Agg {
    total: i64,
    days: BTreeMap<i64, DayAgg>,
    /// In the order projects first appear, which breaks ties in the Bands sheet.
    projects_seen: Vec<String>,
    hours: BTreeMap<String, [i64; 24]>,
    bands: BTreeMap<String, [i64; 4]>,
    weekdays: [i64; 7],
    by_task: BTreeMap<String, i64>,
    by_project: BTreeMap<String, i64>,
}

fn accumulate(spans: &[Span], tz: Tz, from: i64, to: i64) -> Agg {
    let mut agg = Agg::default();
    for sp in spans {
        if !agg.hours.contains_key(&sp.project) {
            agg.projects_seen.push(sp.project.clone());
            agg.hours.insert(sp.project.clone(), [0; 24]);
            agg.bands.insert(sp.project.clone(), [0; 4]);
        }
        walk_hours(sp.start, sp.end, tz, |s, e, day, hour| {
            let len = e - s;
            if len <= 0 {
                return;
            }
            agg.total += len;
            agg.days
                .entry(day)
                .or_insert_with(|| DayAgg {
                    first_min: f64::INFINITY,
                    last_min: f64::NEG_INFINITY,
                    ..DayAgg::default()
                })
                .secs += len;
            if let Some(h) = agg.hours.get_mut(&sp.project) {
                add(h, hour, len);
            }
            if let Some(b) = agg.bands.get_mut(&sp.project) {
                add(b, hour / 6, len);
            }
            add(&mut agg.weekdays, weekday_index(day), len);
            bump(&mut agg.by_task, sp.task.clone(), len);
            bump(&mut agg.by_project, sp.project.clone(), len);
        });
        if sp.own_start >= from && sp.own_start < to {
            let local = sp.own_start + offset_at(sp.own_start, tz);
            let day = local.div_euclid(DAY_SECS);
            let d = agg.days.entry(day).or_insert_with(|| DayAgg {
                first_min: f64::INFINITY,
                last_min: f64::NEG_INFINITY,
                ..DayAgg::default()
            });
            let end_local = sp.own_end + offset_at(sp.own_end, tz);
            d.first_min = d.first_min.min((local - day * DAY_SECS) as f64 / 60.0);
            d.last_min = d.last_min.max((end_local - day * DAY_SECS) as f64 / 60.0);
            d.started += 1;
        }
    }
    agg
}

/// Paused time: the gap between consecutive stretches of one task, plus the last stretch's end to the
/// task's stop, kept only when it begins and ends on one calendar day and begins in the period.
fn pause_gaps(data: &Shared, stretches: &[Stretch], period: &Period, tz: Tz) -> Vec<(i64, i64)> {
    let mut by_task: BTreeMap<&str, Vec<(i64, i64)>> = BTreeMap::new();
    for s in stretches {
        // A stretch still running has no end yet as far as pauses go.
        let end = if s.live { s.start } else { s.end };
        by_task
            .entry(s.task.as_str())
            .or_default()
            .push((s.start, end));
    }
    let mut out = Vec::new();
    for (task, list) in &mut by_task {
        list.sort_by_key(|x| x.0);
        let mut spans = list.iter();
        let Some(first) = spans.next() else { continue };
        let mut max_end = first.1;
        for (start, end) in spans {
            if *start > max_end {
                out.push((max_end, *start));
            }
            max_end = max_end.max(*end);
        }
        if let Some(t) = data.tasks.get(*task)
            && t.state == TaskState::Stopped
            && let Some(stop) = t.stopped_at
            && stop.secs() > max_end
        {
            out.push((max_end, stop.secs()));
        }
    }
    out.retain(|(s, e)| {
        *s >= period.from && *s < period.to && day_of_instant(*s, tz) == day_of_instant(*e - 1, tz)
    });
    out
}

// ---------------------------------------------------------------- shifts

#[derive(Clone)]
struct ShiftWindow {
    shift_id: String,
    name: String,
    zone: String,
    start: i64,
    end: i64,
    scheduled: i64,
}

/// The version in force on a date, or none if there is none or the shift was retired by then.
fn version_on<'a>(versions: &[&'a ShiftVersion], day: &str) -> Option<&'a ShiftVersion> {
    versions
        .iter()
        .filter(|v| !v.deleted && v.effective_from.as_str() <= day)
        .max_by(|a, b| {
            a.effective_from
                .cmp(&b.effective_from)
                .then_with(|| crate::machine::stamp_order(&a.stamp, &a.id, &b.stamp, &b.id))
        })
        .filter(|v| !v.retired)
        .copied()
}

/// Every window overlapping `[from, to)`, sorted by start then shift id.
fn windows_for(data: &Shared, from: i64, to: i64) -> Vec<ShiftWindow> {
    let mut groups: BTreeMap<&str, Vec<&ShiftVersion>> = BTreeMap::new();
    for v in data.shifts.values().filter(|v| !v.deleted) {
        groups.entry(v.shift_id.as_str()).or_default().push(v);
    }
    let mut out = Vec::new();
    for (shift_id, versions) in groups {
        let Some(scan) = versions.first().and_then(|v| parse_zone(&v.zone).ok()) else {
            continue;
        };
        let last = day_num(day_of(Ts(to), scan)) + 1;
        let mut n = day_num(day_of(Ts(from), scan)) - 2;
        while n <= last {
            let day = day_from(n);
            n += 1;
            let text = day.format("%Y-%m-%d").to_string();
            let Some(v) = version_on(&versions, &text) else {
                continue;
            };
            let iso =
                u8::try_from(chrono::Datelike::weekday(&day).number_from_monday()).unwrap_or(0);
            let Ok(zone) = parse_zone(&v.zone) else {
                continue;
            };
            if !v.weekdays.contains(&iso) {
                continue;
            }
            let (start_min, end_min) = (hhmm_minutes(&v.start), hhmm_minutes(&v.end));
            let start = instant_of_wall(day, start_min, zone);
            let end_day = if end_min <= start_min {
                day + Days::new(1)
            } else {
                day
            };
            let end = instant_of_wall(end_day, end_min, zone);
            if end <= from || start >= to || end <= start {
                continue;
            }
            out.push(ShiftWindow {
                shift_id: shift_id.to_owned(),
                name: v.name.clone(),
                zone: v.zone.clone(),
                start,
                end,
                scheduled: (end - start - i64::from(v.break_minutes) * 60).max(0),
            });
        }
    }
    out.sort_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then_with(|| a.shift_id.cmp(&b.shift_id))
    });
    out
}

/// Cuts `[start, end)` at shift boundaries. Where windows overlap, the time goes to the one that
/// started earliest. `None` is time outside any shift.
fn attribute(start: i64, end: i64, windows: &[ShiftWindow]) -> Vec<(i64, i64, Option<usize>)> {
    let mut out: Vec<(i64, i64, Option<usize>)> = Vec::new();
    let mut cur = start;
    while cur < end {
        let mut owner = None;
        let mut next_edge = end;
        for (i, w) in windows.iter().enumerate() {
            if w.start >= end {
                break;
            }
            if w.end <= cur {
                continue;
            }
            if w.start <= cur {
                if owner.is_none() {
                    owner = Some(i);
                }
                if w.end < next_edge && owner == Some(i) {
                    next_edge = w.end;
                }
            } else if w.start < next_edge && owner.is_none() {
                next_edge = w.start;
            }
        }
        match out.last_mut() {
            Some(last) if last.2 == owner => last.1 = next_edge,
            _ => out.push((cur, next_edge, owner)),
        }
        cur = next_edge;
    }
    out
}

#[derive(Clone, Copy, Default)]
struct WindowWork {
    worked: i64,
    first: i64,
    last: i64,
}

struct ShiftStat {
    name: String,
    zone: String,
    windows: i64,
    scheduled: f64,
    worked: i64,
    attainment: Option<f64>,
    coverage: Option<f64>,
    late_starts: i64,
    avg_late: Option<f64>,
    early_finishes: i64,
    avg_early: Option<f64>,
}

struct Shifts {
    configured: bool,
    stats: Vec<ShiftStat>,
    scheduled: f64,
    worked: i64,
    outside: i64,
    outside_share: Option<f64>,
    /// Scheduled seconds per day, for the part of each window already due.
    schedule_by_day: BTreeMap<i64, f64>,
}

fn union_length(mut pairs: Vec<(i64, i64)>) -> i64 {
    pairs.sort_unstable();
    let mut total = 0;
    let mut current: Option<(i64, i64)> = None;
    for (s, e) in pairs {
        match &mut current {
            Some(c) if s <= c.1 => c.1 = c.1.max(e),
            _ => {
                if let Some(c) = current {
                    total += c.1 - c.0;
                }
                current = Some((s, e));
            }
        }
    }
    total + current.map_or(0, |c| c.1 - c.0)
}

fn shifts_of(
    data: &Shared,
    spans: &[Span],
    windows: &[ShiftWindow],
    period: &Period,
    now: i64,
    tz: Tz,
) -> Shifts {
    let configured = data.shifts.values().any(|v| !v.deleted && !v.retired);
    let max_window = windows.iter().map(|w| w.end - w.start).max().unwrap_or(0);
    let mut work: BTreeMap<usize, (WindowWork, Vec<(i64, i64)>)> = BTreeMap::new();
    let mut outside = 0;
    if !windows.is_empty() {
        for sp in spans {
            // The windows near this span, keeping each one's index for the running total.
            let lo = windows.partition_point(|w| w.start < sp.start - max_window);
            let hi = windows.partition_point(|w| w.start < sp.end);
            let near = windows.get(lo..hi.max(lo)).unwrap_or(&[]);
            for (s, e, owner) in attribute(sp.start, sp.end, near) {
                match owner {
                    Some(i) => {
                        let entry = work.entry(lo + i).or_insert_with(|| {
                            (
                                WindowWork {
                                    worked: 0,
                                    first: i64::MAX,
                                    last: i64::MIN,
                                },
                                Vec::new(),
                            )
                        });
                        entry.0.worked += e - s;
                        entry.0.first = entry.0.first.min(s);
                        entry.0.last = entry.0.last.max(e);
                        entry.1.push((s, e));
                    }
                    None => outside += e - s,
                }
            }
        }
    }
    let due = |w: &ShiftWindow| {
        let (s, e) = (w.start.max(period.from), w.end.min(period.to).min(now));
        (e > s).then_some((s, e))
    };
    let mut stats: Vec<(String, ShiftStat, i64, i64, i64, Option<i64>)> = Vec::new();
    let mut schedule_by_day: BTreeMap<i64, f64> = BTreeMap::new();
    for (i, w) in windows.iter().enumerate() {
        if let Some((s, e)) = due(w) {
            let weight = w.scheduled as f64 / (w.end - w.start) as f64;
            for (day, a, b) in split_by_day(Ts(s), Ts(e), tz) {
                *schedule_by_day.entry(day_num(day)).or_insert(0.0) +=
                    (b.secs() - a.secs()) as f64 * weight;
            }
        }
        if w.end <= period.from || w.start >= period.to {
            continue;
        }
        let at = match stats.iter().position(|x| x.0 == w.shift_id) {
            Some(at) => at,
            None => {
                stats.push((
                    w.shift_id.clone(),
                    ShiftStat {
                        name: w.name.clone(),
                        zone: w.zone.clone(),
                        windows: 0,
                        scheduled: 0.0,
                        worked: 0,
                        attainment: None,
                        coverage: None,
                        late_starts: 0,
                        avg_late: None,
                        early_finishes: 0,
                        avg_early: None,
                    },
                    0,
                    0,
                    0,
                    None,
                ));
                stats.len() - 1
            }
        };
        let Some((_, st, due_len, late_sum, early_sum, covered)) = stats.get_mut(at) else {
            continue;
        };
        st.windows += 1;
        if let Some((s, e)) = due(w) {
            st.scheduled += w.scheduled as f64 * (e - s) as f64 / (w.end - w.start) as f64;
            *due_len += e - s;
        }
        if let Some((wa, ivs)) = work.get(&i) {
            st.worked += wa.worked;
            *covered = Some(covered.unwrap_or(0) + union_length(ivs.clone()));
            if w.start >= period.from && w.end <= period.to.min(now) {
                let late = wa.first - w.start;
                if late > LATE_TOLERANCE_SECS {
                    st.late_starts += 1;
                    *late_sum += late;
                }
                let early = w.end - wa.last;
                if early > LATE_TOLERANCE_SECS {
                    st.early_finishes += 1;
                    *early_sum += early;
                }
            }
        }
    }
    let mut list: Vec<ShiftStat> = stats
        .into_iter()
        .map(|(_, mut st, due_len, late_sum, early_sum, covered)| {
            st.attainment = ratio(st.worked as f64, st.scheduled);
            st.coverage = ratio(covered.unwrap_or(0) as f64, due_len as f64);
            st.avg_late = (st.late_starts > 0).then(|| late_sum as f64 / st.late_starts as f64);
            st.avg_early =
                (st.early_finishes > 0).then(|| early_sum as f64 / st.early_finishes as f64);
            st
        })
        .collect();
    list.sort_by(|a, b| cmp_names(&a.name, &b.name));
    let scheduled = list.iter().map(|s| s.scheduled).sum();
    let worked: i64 = list.iter().map(|s| s.worked).sum();
    Shifts {
        configured,
        stats: list,
        scheduled,
        worked,
        outside,
        outside_share: ratio(outside as f64, (outside + worked) as f64),
        schedule_by_day,
    }
}

/// The shift column of a selected export's details: how each stretch splits across shift windows, by
/// the same rules as the UI's `segmentShiftText`, for the selected part alone. Works from the windows
/// of `[from, to)`, which should cover every stretch asked about.
pub(crate) struct ShiftText {
    windows: Vec<ShiftWindow>,
    max_window: i64,
}

impl ShiftText {
    pub(crate) fn new(data: &Shared, from: Ts, to: Ts) -> Self {
        let windows = windows_for(data, from.secs(), to.secs());
        let max_window = windows.iter().map(|w| w.end - w.start).max().unwrap_or(0);
        Self {
            windows,
            max_window,
        }
    }

    /// "Morning 1:00:00; Outside 0:30:00", or nothing when no shift is set up or no time passed.
    pub(crate) fn of(&self, data: &Shared, start: Ts, end: Ts) -> String {
        if end <= start || !data.shifts.values().any(|v| !v.deleted) {
            return String::new();
        }
        let (start, end) = (start.secs(), end.secs());
        let lo = self
            .windows
            .partition_point(|w| w.start < start - self.max_window);
        let hi = self.windows.partition_point(|w| w.start < end);
        let near = self.windows.get(lo..hi.max(lo)).unwrap_or(&[]);
        let mut totals: Vec<(&str, i64)> = Vec::new();
        for (s, e, owner) in attribute(start, end, near) {
            let name = owner
                .and_then(|i| near.get(i))
                .map_or("Outside", |w| w.name.as_str());
            match totals.iter_mut().find(|t| t.0 == name) {
                Some(t) => t.1 += e - s,
                None => totals.push((name, e - s)),
            }
        }
        totals
            .iter()
            .map(|(name, secs)| format!("{name} {}", crate::report::hms(*secs)))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Seconds of logged time inside the client's working hours, by the rule of `src/lib/clientHours.ts`: every
/// day of the week counts, hours that end at or before they start run overnight into the next day, and the
/// same start and end cover the whole day.
fn client_overlap(data: &Shared, spans: &[Span], client: Tz, from: i64, to: i64) -> i64 {
    let setting = |key: &str, default: &str| {
        data.settings
            .get(key)
            .and_then(|e| e.value.as_str())
            .unwrap_or(default)
            .to_owned()
    };
    let start = hhmm_minutes(&setting("clientWorkStart", "09:00"));
    let end = hhmm_minutes(&setting("clientWorkEnd", "17:00"));
    let mut work = Vec::new();
    // One date back, for hours that began the evening before and run past midnight into the period.
    for n in (day_of_instant(from, client) - 1)..=(day_of_instant(to, client) + 1) {
        let finish = if end <= start { n + 1 } else { n };
        work.push((
            instant_of_wall(day_from(n), start, client),
            instant_of_wall(day_from(finish), end, client),
        ));
    }
    let mut total = 0;
    for sp in spans {
        for (ws, we) in &work {
            total += ((*we).min(sp.end) - (*ws).max(sp.start)).max(0);
        }
    }
    total
}

// ---------------------------------------------------------------- the sheets

/// The same figures the UI's `analyze` gives, for one period. Names follow `sheets.ts`.
struct Metrics<'a> {
    data: &'a Shared,
    period: Period,
    today: i64,
    now: i64,
    agg: Agg,
    spans: Vec<Span>,
    gaps: Vec<(i64, i64)>,
    rows: Vec<TaskDay>,
    day_totals: BTreeMap<i64, i64>,
    shifts: Shifts,
    /// Logged time inside client hours, when a client zone is set.
    overlap: Option<i64>,
    selected_days: Option<BTreeSet<i64>>,
    place: String,
}

fn number(v: f64) -> Value {
    serde_json::Number::from_f64(v).map_or(Value::Null, Value::Number)
}
fn opt(v: Option<f64>) -> Value {
    v.map_or(Value::Null, number)
}
fn int(v: i64) -> Value {
    json!(v)
}
fn text(v: &str) -> Value {
    Value::String(v.to_owned())
}

fn columns(list: &[(&str, CellKind)]) -> Vec<AnalyticsColumn> {
    list.iter()
        .map(|(header, kind)| AnalyticsColumn {
            header: (*header).to_owned(),
            kind: *kind,
        })
        .collect()
}

/// Whether a project is billable, on a fixed fee, or has a rate at this moment. Never a price.
fn rate_text(data: &Shared, id: &str, at: i64) -> &'static str {
    let Some(p) = data.projects.get(id) else {
        return "";
    };
    if !p.billable {
        "Not billable"
    } else if p.billing_type == BillingType::FixedFee {
        "Fixed fee"
    } else if split_by_rate(data, id, Ts(at), Ts(at + 1))
        .first()
        .and_then(|r| r.rate_cents)
        .is_none()
    {
        "No rate set"
    } else {
        ""
    }
}

struct ProjectRow {
    id: String,
    name: String,
    exact: i64,
    billable: i64,
    share: Option<f64>,
    tasks: i64,
    adj: i64,
}

struct TaskRow {
    id: String,
    name: String,
    exact: i64,
    billable: i64,
    days: usize,
}

impl Metrics<'_> {
    fn exact(&self) -> i64 {
        self.agg.total
    }
    fn billable(&self) -> i64 {
        self.rows.iter().map(|r| r.billable_secs).sum()
    }
    fn active_days(&self) -> i64 {
        self.agg.days.values().filter(|d| d.secs > 0).count() as i64
    }
    fn elapsed_days(&self) -> i64 {
        (self.today - day_num(self.period.from_day) + 1).clamp(0, self.period.days)
    }
    fn worked(&self, day: i64) -> i64 {
        self.day_totals.get(&day).copied().unwrap_or(0)
    }
    fn setting_hours(&self, key: &str) -> Option<f64> {
        self.data
            .settings
            .get(key)
            .and_then(|e| e.value.as_f64())
            .filter(|h| *h > 0.0)
    }

    fn billable_by_project(&self) -> BTreeMap<&str, i64> {
        let mut out = BTreeMap::new();
        for r in &self.rows {
            bump(&mut out, r.project_id.as_str(), r.billable_secs);
        }
        out
    }

    /// A day holding only a billable pause has a row and no worked time. In a selection it still counts
    /// as a project and a task, so every sheet adds up to the total. The Analysis tab leaves them out.
    fn counted_ids<'s>(
        &'s self,
        worked: &'s BTreeMap<String, i64>,
        of_row: impl Fn(&'s TaskDay) -> &'s str,
    ) -> BTreeSet<&'s str> {
        let mut ids: BTreeSet<&str> = worked.keys().map(String::as_str).collect();
        if self.selected_days.is_some() {
            ids.extend(self.rows.iter().map(of_row));
        }
        ids
    }

    fn projects(&self) -> Vec<ProjectRow> {
        let billable = self.billable_by_project();
        let mut adj: BTreeMap<&str, i64> = BTreeMap::new();
        for r in &self.rows {
            bump(
                &mut adj,
                r.project_id.as_str(),
                r.billable_secs - r.exact_secs,
            );
        }
        let mut tasks: BTreeMap<&str, i64> = BTreeMap::new();
        for t in self.counted_ids(&self.agg.by_task, |r| r.task_id.as_str()) {
            if let Some(task) = self.data.tasks.get(t) {
                bump(&mut tasks, task.project_id.as_str(), 1);
            }
        }
        let of = |m: &BTreeMap<&str, i64>, id: &str| m.get(id).copied().unwrap_or(0);
        let ids = self.counted_ids(&self.agg.by_project, |r| r.project_id.as_str());
        let total: i64 = ids.iter().map(|id| of(&billable, id)).sum();
        let mut list: Vec<ProjectRow> = ids
            .iter()
            .filter_map(|id| {
                let p = self.data.projects.get(*id)?;
                let secs = of(&billable, id);
                Some(ProjectRow {
                    id: (*id).to_owned(),
                    name: p.name.clone(),
                    exact: self.agg.by_project.get(*id).copied().unwrap_or(0),
                    billable: secs,
                    share: ratio(secs as f64, total as f64),
                    tasks: of(&tasks, id),
                    adj: of(&adj, id),
                })
            })
            .collect();
        list.sort_by(|a, b| {
            b.billable
                .cmp(&a.billable)
                .then_with(|| cmp_names(&a.name, &b.name))
        });
        list
    }

    fn tasks(&self) -> Vec<TaskRow> {
        let mut billable: BTreeMap<&str, i64> = BTreeMap::new();
        let mut days: BTreeMap<&str, BTreeSet<NaiveDate>> = BTreeMap::new();
        for r in &self.rows {
            bump(&mut billable, r.task_id.as_str(), r.billable_secs);
            days.entry(r.task_id.as_str()).or_default().insert(r.day);
        }
        let mut list: Vec<TaskRow> = self
            .counted_ids(&self.agg.by_task, |r| r.task_id.as_str())
            .iter()
            .filter_map(|id| {
                let t = self.data.tasks.get(*id)?;
                Some(TaskRow {
                    id: (*id).to_owned(),
                    name: t.name.clone(),
                    exact: self.agg.by_task.get(*id).copied().unwrap_or(0),
                    billable: billable.get(*id).copied().unwrap_or(0),
                    days: days.get(*id).map_or(0, BTreeSet::len),
                })
            })
            .collect();
        list.sort_by(|a, b| {
            b.billable
                .cmp(&a.billable)
                .then_with(|| cmp_names(&a.name, &b.name))
        });
        list
    }

    /// First starts and last ends of the days that fall on a weekday, or on all days.
    fn spread(&self, weekday: Option<usize>, finish: bool) -> Option<(f64, f64, f64)> {
        let mut xs: Vec<f64> = self
            .agg
            .days
            .iter()
            .filter(|(d, a)| a.started > 0 && weekday.is_none_or(|w| weekday_index(**d) == w))
            .map(|(_, a)| if finish { a.last_min } else { a.first_min })
            .collect();
        if xs.len() < 3 {
            return None;
        }
        xs.sort_by(f64::total_cmp);
        Some((
            quantile(&xs, 0.5)?,
            quantile(&xs, 0.25)?,
            quantile(&xs, 0.75)?,
        ))
    }

    fn kpi_sheet(&self) -> AnalyticsSheet {
        let mut rows: Vec<Vec<Value>> = Vec::new();
        const FOLLOWS: &str = "Follows the Basis switch";
        let time =
            |name: &str, secs: Option<f64>, id: &str, basis: &str, rows: &mut Vec<Vec<Value>>| {
                rows.push(vec![
                    text(name),
                    opt(secs),
                    Value::Null,
                    Value::Null,
                    text(basis),
                    text(id),
                ]);
            };
        let count = |name: &str, n: i64, id: &str, basis: &str, rows: &mut Vec<Vec<Value>>| {
            rows.push(vec![
                text(name),
                Value::Null,
                int(n),
                Value::Null,
                text(basis),
                text(id),
            ]);
        };
        let share =
            |name: &str, f: Option<f64>, id: &str, basis: &str, rows: &mut Vec<Vec<Value>>| {
                rows.push(vec![
                    text(name),
                    Value::Null,
                    Value::Null,
                    opt(f),
                    text(basis),
                    text(id),
                ]);
            };
        let (exact, billable) = (self.exact(), self.billable());
        let (exact_f, billable_f) = (exact as f64, billable as f64);
        let active = self.active_days();
        let elapsed = self.elapsed_days();
        time(
            "Total time (Exact)",
            Some(exact_f),
            "volume.total",
            FOLLOWS,
            &mut rows,
        );
        time(
            "Total time (Billable)",
            Some(billable_f),
            "volume.total",
            FOLLOWS,
            &mut rows,
        );
        if billable >= exact {
            time(
                "Rounding adjustment (Billable minus Exact)",
                Some(billable_f - exact_f),
                "money.rounding",
                "Billable",
                &mut rows,
            );
        } else {
            rows.push(vec![
                text("Rounding adjustment (Billable minus Exact), negative: see Projects sheet"),
                Value::Null,
                Value::Null,
                Value::Null,
                text("Billable"),
                text("money.rounding"),
            ]);
        }
        let pause: i64 = self.rows.iter().map(|r| r.pause_secs).sum();
        let thinking: i64 = self.rows.iter().map(|r| r.thinking_secs).sum();
        if pause > 0 {
            time(
                "Billable pauses (in Billable, not in Exact)",
                Some(pause as f64),
                "pauses.billable",
                "Billable",
                &mut rows,
            );
        }
        if thinking > 0 {
            time(
                "Thinking time (in Exact and Billable)",
                Some(thinking as f64),
                "time.thinking",
                "Exact",
                &mut rows,
            );
        }
        count(
            "Active days",
            active,
            "volume.activeDays",
            "Exact",
            &mut rows,
        );
        count(
            "Calendar days elapsed",
            elapsed,
            "volume.avgCalendarDay",
            FOLLOWS,
            &mut rows,
        );
        time(
            "Average per active day",
            (active > 0).then(|| billable_f / active as f64),
            "volume.avgActiveDay",
            FOLLOWS,
            &mut rows,
        );
        time(
            "Average per calendar day",
            (elapsed > 0).then(|| billable_f / elapsed as f64),
            "volume.avgCalendarDay",
            FOLLOWS,
            &mut rows,
        );
        time(
            "Average per week",
            (elapsed >= 7).then(|| billable_f / (elapsed as f64 / 7.0)),
            "volume.perWeek",
            FOLLOWS,
            &mut rows,
        );

        // Focus: sessions are the spans, in time order.
        let mut lens: Vec<f64> = self
            .spans
            .iter()
            .map(|s| (s.end - s.start) as f64)
            .collect();
        lens.sort_by(f64::total_cmp);
        let mut frag = 0;
        let mut switches = 0;
        let (mut run_start, mut run_end) = (0, 0);
        let mut streak: Option<i64> = None;
        let mut last: Option<(i64, &str)> = None;
        for (i, s) in self.spans.iter().enumerate() {
            let len = s.end - s.start;
            if len < FRAGMENT_SECS {
                frag += len;
            }
            if i == 0 || s.start > run_end + STREAK_GAP_SECS {
                if run_end > run_start && streak.is_none_or(|b| run_end - run_start > b) {
                    streak = Some(run_end - run_start);
                }
                run_start = s.start;
                run_end = s.end;
            } else {
                run_end = run_end.max(s.end);
            }
            if last.is_some_and(|(d, t)| d == s.day && t != s.task) {
                switches += 1;
            }
            last = Some((s.day, s.task.as_str()));
        }
        if run_end > run_start && streak.is_none_or(|b| run_end - run_start > b) {
            streak = Some(run_end - run_start);
        }
        let sessions = self.spans.len() as i64;
        let pause_secs: i64 = self.gaps.iter().map(|(s, e)| e - s).sum();
        count("Sessions", sessions, "focus.sessions", "Exact", &mut rows);
        time(
            "Average session",
            (sessions > 0).then(|| exact_f / sessions as f64),
            "focus.avgSession",
            "Exact",
            &mut rows,
        );
        time(
            "Median session",
            quantile(&lens, 0.5),
            "focus.avgSession",
            "Exact",
            &mut rows,
        );
        time(
            "Longest session",
            lens.last().copied(),
            "focus.avgSession",
            "Exact",
            &mut rows,
        );
        time(
            "Longest uninterrupted stretch",
            streak.map(|s| s as f64),
            "focus.streak",
            "Exact",
            &mut rows,
        );
        count(
            "Pauses",
            self.gaps.len() as i64,
            "focus.pauses",
            "Exact",
            &mut rows,
        );
        time(
            "Paused time",
            Some(pause_secs as f64),
            "focus.pausedTime",
            "Exact",
            &mut rows,
        );
        share(
            "Focus ratio",
            ratio(exact_f, (exact + pause_secs) as f64),
            "focus.ratio",
            "Exact",
            &mut rows,
        );
        count(
            "Context switches",
            switches,
            "focus.switches",
            "Exact",
            &mut rows,
        );
        share(
            "Fragmentation (time in sessions under 25 min)",
            ratio(frag as f64, exact_f),
            "focus.fragmentation",
            "Exact",
            &mut rows,
        );

        let tasks = self.tasks();
        let completed = self
            .data
            .tasks
            .values()
            .filter(|t| {
                t.state == TaskState::Stopped
                    && t.is_live()
                    && t.stopped_at
                        .is_some_and(|s| s.secs() >= self.period.from && s.secs() < self.period.to)
            })
            .count() as i64;
        let in_progress = tasks
            .iter()
            .filter(|t| {
                self.data
                    .tasks
                    .get(&t.id)
                    .is_some_and(|x| matches!(x.state, TaskState::Running | TaskState::Paused))
            })
            .count() as i64;
        let multi_day = tasks.iter().filter(|t| t.days >= 2).count() as i64;
        count(
            "Tasks completed",
            completed,
            "tasks.completed",
            "Neither",
            &mut rows,
        );
        count(
            "Tasks in progress",
            in_progress,
            "tasks.inProgress",
            "Neither",
            &mut rows,
        );
        count(
            "Tasks spanning several days",
            multi_day,
            "tasks.multiDay",
            "Neither",
            &mut rows,
        );

        let first = day_num(self.period.from_day);
        let to = day_num(self.period.to_day);
        let end_day = to.min(self.today);
        let mut n = end_day;
        if self.worked(n) <= 0 && n == self.today {
            n -= 1;
        }
        let mut current = 0;
        while self.worked(n - current) > 0 && n - current >= first - 400 {
            current += 1;
        }
        count(
            "Day streak",
            current,
            "consistency.streak",
            "Exact",
            &mut rows,
        );
        let clock_row = |name: &str, s: (f64, f64, f64), id: &str, rows: &mut Vec<Vec<Value>>| {
            rows.push(vec![
                text(&format!(
                    "{name}: {} (middle half {}-{})",
                    fmt_clock(s.0),
                    fmt_clock(s.1),
                    fmt_clock(s.2)
                )),
                Value::Null,
                Value::Null,
                Value::Null,
                text("Exact"),
                text(id),
            ]);
        };
        if let Some(s) = self.spread(None, false) {
            clock_row("Typical start", s, "consistency.start", &mut rows);
        }
        if let Some(s) = self.spread(None, true) {
            clock_row("Typical finish", s, "consistency.finish", &mut rows);
        }

        let daily = self.setting_hours("dailyTargetHours");
        let over = |target: &dyn Fn(i64) -> f64| {
            (first..=to.min(self.today))
                .map(|d| (self.worked(d) as f64 - target(d)).max(0.0))
                .sum::<f64>()
        };
        if self.shifts.configured {
            let secs = over(&|d| self.shifts.schedule_by_day.get(&d).copied().unwrap_or(0.0));
            time(
                "Overtime against shifts",
                Some(secs),
                "consistency.overtime",
                "Exact",
                &mut rows,
            );
        } else if let Some(hours) = daily {
            let secs = over(&|_| hours * 3600.0);
            time(
                "Overtime against the daily target",
                Some(secs),
                "consistency.overtime",
                "Exact",
                &mut rows,
            );
        }
        if let Some(hours) = daily {
            share(
                "Daily target attainment (last elapsed day)",
                Some(self.worked(end_day) as f64 / (hours * 3600.0)),
                "goals.daily",
                "Exact",
                &mut rows,
            );
        }
        if let Some(hours) = self.setting_hours("weeklyTargetHours") {
            let start = end_day - i64::try_from(weekday_index(end_day)).unwrap_or(0);
            let secs: i64 = (start..start + 7).map(|d| self.worked(d)).sum();
            share(
                "Weekly target attainment (last elapsed week)",
                Some(secs as f64 / (hours * 3600.0)),
                "goals.weekly",
                "Exact",
                &mut rows,
            );
        }
        let flagged: i64 = self
            .rows
            .iter()
            .filter(|r| {
                self.data
                    .projects
                    .get(&r.project_id)
                    .is_some_and(|p| p.billable)
            })
            .map(|r| r.billable_secs)
            .sum();
        share(
            "Share of Billable time on billable projects",
            ratio(flagged as f64, billable_f),
            "money.billable",
            "Billable",
            &mut rows,
        );
        if self.shifts.configured && !self.shifts.stats.is_empty() {
            time(
                "Scheduled shift hours",
                Some(self.shifts.scheduled),
                "shifts.scheduled",
                "Exact",
                &mut rows,
            );
            time(
                "Worked inside shifts",
                Some(self.shifts.worked as f64),
                "shifts.attainment",
                "Exact",
                &mut rows,
            );
            time(
                "Outside any shift",
                Some(self.shifts.outside as f64),
                "shifts.outside",
                "Exact",
                &mut rows,
            );
        }
        if let Some(overlap) = self.overlap {
            share(
                "Logged time inside client working hours",
                ratio(overlap as f64, exact_f),
                "zones.overlap",
                "Exact",
                &mut rows,
            );
        }
        // A projection only makes sense for a whole period still under way, never for picked entries.
        if self.selected_days.is_none()
            && self.period.days >= 7
            && self.now >= self.period.from
            && self.now < self.period.to
        {
            let fraction =
                (self.now - self.period.from) as f64 / (self.period.to - self.period.from) as f64;
            if active >= 2 && fraction >= 0.2 {
                time(
                    "Projected period total (projection)",
                    Some(billable_f / fraction),
                    "forecast.projected",
                    FOLLOWS,
                    &mut rows,
                );
            }
        }
        AnalyticsSheet {
            name: "KPI summary".into(),
            title: format!("Summary: {}", self.place),
            columns: columns(&[
                ("Metric", CellKind::Text),
                ("Time", CellKind::Duration),
                ("Count", CellKind::Int),
                ("Share", CellKind::Percent),
                ("Time basis", CellKind::Text),
                ("Glossary id", CellKind::Text),
            ]),
            rows,
            contains_prices: false,
        }
    }

    fn bands_sheet(&self) -> AnalyticsSheet {
        let total = self.exact() as f64;
        let active = self.active_days();
        let avg = |secs: i64| (active > 0).then(|| secs as f64 / active as f64);
        let share = |secs: i64| opt(ratio(secs as f64, total));
        let name_of = |id: &str| {
            self.data
                .projects
                .get(id)
                .map_or_else(|| id.to_owned(), |p| p.name.clone())
        };
        let mut rows = Vec::new();
        for (b, band) in BANDS.iter().enumerate() {
            let mut by: Vec<(&String, i64)> = self
                .agg
                .projects_seen
                .iter()
                .filter_map(|p| {
                    let v = self.agg.bands.get(p)?.get(b).copied()?;
                    (v > 0).then_some((p, v))
                })
                .collect();
            let secs: i64 = by.iter().map(|x| x.1).sum();
            rows.push(vec![
                text("Band"),
                text(band),
                text("All projects"),
                int(secs),
                share(secs),
                opt(avg(secs)),
            ]);
            by.sort_by_key(|x| std::cmp::Reverse(x.1));
            for (p, v) in by {
                rows.push(vec![
                    text("Band"),
                    text(band),
                    text(&name_of(p)),
                    int(v),
                    share(v),
                    Value::Null,
                ]);
            }
        }
        for h in 0..24 {
            let secs: i64 = self.agg.hours.values().filter_map(|a| a.get(h)).sum();
            if secs > 0 {
                rows.push(vec![
                    text("Hour"),
                    text(&format!("{h:02}:00")),
                    text("All projects"),
                    int(secs),
                    share(secs),
                    opt(avg(secs)),
                ]);
            }
        }
        AnalyticsSheet {
            name: "Bands".into(),
            title: format!("Time of day (Exact): {}", self.place),
            columns: columns(&[
                ("Kind", CellKind::Text),
                ("Band or hour", CellKind::Text),
                ("Project", CellKind::Text),
                ("Exact time", CellKind::Duration),
                ("Share", CellKind::Percent),
                ("Average per active day", CellKind::Duration),
            ]),
            rows,
            contains_prices: false,
        }
    }

    fn ref_sec(&self) -> i64 {
        self.now.min(self.period.to - 1)
    }

    fn projects_sheet(&self) -> AnalyticsSheet {
        let rows = self
            .projects()
            .iter()
            .map(|p| {
                vec![
                    text(&p.name),
                    int(p.exact),
                    int(p.billable),
                    opt(p.share),
                    int(p.tasks),
                    opt((p.tasks > 0).then(|| p.billable as f64 / p.tasks as f64)),
                    number(p.adj as f64 / 3600.0),
                    Value::Null,
                    Value::Null,
                    text(rate_text(self.data, &p.id, self.ref_sec())),
                ]
            })
            .collect();
        AnalyticsSheet {
            name: "Projects".into(),
            title: format!("Projects (Billable shares): {}", self.place),
            columns: columns(&[
                ("Project", CellKind::Text),
                ("Exact time", CellKind::Duration),
                ("Billable time", CellKind::Duration),
                ("Share", CellKind::Percent),
                ("Tasks with time", CellKind::Int),
                ("Average per task", CellKind::Duration),
                ("Rounding adjustment (hours)", CellKind::Number),
                ("Previous period", CellKind::Duration),
                ("Change", CellKind::Percent),
                ("Rate", CellKind::Text),
            ]),
            rows,
            contains_prices: false,
        }
    }

    fn weekdays_sheet(&self) -> AnalyticsSheet {
        let mut occurrences = [0i64; 7];
        for n in day_num(self.period.from_day)..=day_num(self.period.to_day).min(self.today) {
            add(&mut occurrences, weekday_index(n), 1);
        }
        let rows = WEEKDAY_NAMES
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let secs = self.agg.weekdays.get(i).copied().unwrap_or(0);
                let occ = occurrences.get(i).copied().unwrap_or(0);
                let clock = |finish| {
                    self.spread(Some(i), finish)
                        .map_or(Value::Null, |s| text(&fmt_clock(s.0)))
                };
                vec![
                    text(name),
                    int(secs),
                    int(occ),
                    opt((occ > 0).then(|| secs as f64 / occ as f64)),
                    clock(false),
                    clock(true),
                ]
            })
            .collect();
        AnalyticsSheet {
            name: "Weekdays".into(),
            title: format!("Weekdays (Exact): {}", self.place),
            columns: columns(&[
                ("Weekday", CellKind::Text),
                ("Exact time", CellKind::Duration),
                ("Occurrences so far", CellKind::Int),
                ("Average per occurrence", CellKind::Duration),
                ("Typical start", CellKind::Text),
                ("Typical finish", CellKind::Text),
            ]),
            rows,
            contains_prices: false,
        }
    }

    /// `None` when no task with time carries a tag.
    fn tags_sheet(&self) -> Option<AnalyticsSheet> {
        #[derive(Default)]
        struct Row {
            exact: i64,
            billable: i64,
            tasks: i64,
        }
        let mut by: BTreeMap<&str, Row> = BTreeMap::new();
        let mut untagged: Option<Row> = None;
        let mut total = 0;
        let tasks = self.tasks();
        for t in &tasks {
            total += t.billable;
            let tags: Vec<&str> = self
                .data
                .tasks
                .get(&t.id)
                .map(|x| {
                    x.tags
                        .iter()
                        .filter(|id| self.data.tags.get(*id).is_some_and(|g| !g.deleted))
                        .map(String::as_str)
                        .collect()
                })
                .unwrap_or_default();
            let count = |r: &mut Row| {
                r.exact += t.exact;
                r.billable += t.billable;
                r.tasks += 1;
            };
            if tags.is_empty() {
                count(untagged.get_or_insert_with(Row::default));
            }
            for id in tags {
                count(by.entry(id).or_default());
            }
        }
        if by.is_empty() {
            return None;
        }
        let mut list: Vec<(String, Row)> = by
            .into_iter()
            .filter_map(|(id, r)| Some((self.data.tags.get(id)?.name.clone(), r)))
            .collect();
        list.sort_by(|a, b| {
            b.1.billable
                .cmp(&a.1.billable)
                .then_with(|| cmp_names(&a.0, &b.0))
        });
        if let Some(r) = untagged {
            list.push(("No tag".to_owned(), r));
        }
        let rows = list
            .iter()
            .map(|(name, r)| {
                vec![
                    text(name),
                    int(r.exact),
                    int(r.billable),
                    opt(ratio(r.billable as f64, total as f64)),
                    int(r.tasks),
                ]
            })
            .collect();
        Some(AnalyticsSheet {
            name: "Tags".into(),
            title: format!(
                "Time by tag (a task with two tags counts fully in each, so rows overlap and have no total): {}",
                self.place
            ),
            columns: columns(&[
                ("Tag", CellKind::Text),
                ("Exact time", CellKind::Duration),
                ("Billable time", CellKind::Duration),
                (
                    "Share of total time (Billable, overlaps)",
                    CellKind::Percent,
                ),
                ("Tasks", CellKind::Int),
            ]),
            rows,
            contains_prices: false,
        })
    }

    fn shifts_sheet(&self) -> AnalyticsSheet {
        let s = &self.shifts;
        let mut rows: Vec<Vec<Value>> = s
            .stats
            .iter()
            .map(|st| {
                vec![
                    text(&st.name),
                    text(&st.zone),
                    int(st.windows),
                    number(st.scheduled),
                    int(st.worked),
                    opt(st.attainment),
                    opt(st.coverage),
                    int(st.late_starts),
                    opt(st.avg_late),
                    int(st.early_finishes),
                    opt(st.avg_early),
                ]
            })
            .collect();
        let mut outside = vec![Value::Null; 11];
        if let Some(c) = outside.get_mut(0) {
            *c = text("Outside any shift");
        }
        if let Some(c) = outside.get_mut(4) {
            *c = int(s.outside);
        }
        if let Some(c) = outside.get_mut(5) {
            *c = opt(s.outside_share);
        }
        rows.push(outside);
        AnalyticsSheet {
            name: "Shifts".into(),
            title: format!("Shifts (Exact): {}", self.place),
            columns: columns(&[
                ("Shift", CellKind::Text),
                ("Zone", CellKind::Text),
                ("Windows", CellKind::Int),
                ("Scheduled", CellKind::Duration),
                ("Worked inside", CellKind::Duration),
                (
                    "Attainment (outside row: share of logged time)",
                    CellKind::Percent,
                ),
                ("Coverage", CellKind::Percent),
                ("Started after shift start", CellKind::Int),
                ("Average later start", CellKind::Duration),
                ("Finished before shift end", CellKind::Int),
                ("Average earlier finish", CellKind::Duration),
            ]),
            rows,
            contains_prices: false,
        }
    }

    fn earnings_period_sheet(&self) -> AnalyticsSheet {
        let unit = unit_for(self.period.days);
        let buckets = buckets_of(
            &self.period,
            if unit == Unit::Hour { Unit::Day } else { unit },
        );
        let mut eur_seen = false;
        let mut usd_seen = false;
        let mut per: Vec<(i64, i64, i64)> = vec![(0, 0, 0); buckets.len()];
        let mut totals: BTreeMap<&str, i64> = BTreeMap::new();
        for r in &self.rows {
            let n = day_num(r.day);
            let at = buckets
                .iter()
                .position(|b| n >= b.from_day && n <= b.to_day);
            if let Some(p) = at.and_then(|i| per.get_mut(i)) {
                p.0 += r.billable_secs;
            }
            for b in &r.buckets {
                let (Some(rate), Some(cur)) = (b.rate_cents, b.currency.as_deref()) else {
                    continue;
                };
                let num = rate * b.billable_secs;
                let is_eur = cur == "EUR";
                bump(&mut totals, if is_eur { "EUR" } else { "USD" }, num);
                if is_eur {
                    eur_seen = true;
                } else {
                    usd_seen = true;
                }
                if let Some(p) = at.and_then(|i| per.get_mut(i)) {
                    if is_eur {
                        p.1 += num;
                    } else {
                        p.2 += num;
                    }
                }
            }
        }
        let cents = |seen: bool, num: i64| {
            if seen {
                int(cents_from_numerator(num))
            } else {
                Value::Null
            }
        };
        let mut rows = Vec::new();
        for (b, p) in buckets.iter().zip(&per) {
            // A selection lists only the days it holds, so no row stands for a day it left out.
            if let Some(days) = &self.selected_days
                && !(b.from_day..=b.to_day).any(|d| days.contains(&d))
            {
                continue;
            }
            let exact: i64 = if unit == Unit::Hour {
                self.exact()
            } else {
                (b.from_day..=b.to_day)
                    .filter_map(|d| self.agg.days.get(&d))
                    .map(|a| a.secs)
                    .sum()
            };
            rows.push(vec![
                text(&b.label),
                int(p.0),
                int(exact),
                cents(eur_seen, p.1),
                cents(usd_seen, p.2),
            ]);
        }
        let total = |c: &str| {
            totals
                .get(c)
                .map_or(Value::Null, |n| int(cents_from_numerator(*n)))
        };
        rows.push(vec![
            text("Total"),
            int(self.billable()),
            int(self.exact()),
            total("EUR"),
            total("USD"),
        ]);
        AnalyticsSheet {
            name: "Earnings by period".into(),
            title: format!(
                "Earnings by period (Billable time, rate in force when logged). {ROUNDED_ROWS_NOTE} {}",
                self.place
            ),
            columns: columns(&[
                ("Period", CellKind::Text),
                ("Billable time", CellKind::Duration),
                ("Exact time", CellKind::Duration),
                ("Earnings EUR", CellKind::Eur),
                ("Earnings USD", CellKind::Usd),
            ]),
            rows,
            contains_prices: true,
        }
    }

    fn earnings_project_sheet(&self) -> AnalyticsSheet {
        let mut by: BTreeMap<(&str, &str), i64> = BTreeMap::new();
        for r in &self.rows {
            for b in &r.buckets {
                if let (Some(rate), Some(cur)) = (b.rate_cents, b.currency.as_deref()) {
                    bump(
                        &mut by,
                        (r.project_id.as_str(), cur),
                        rate * b.billable_secs,
                    );
                }
            }
        }
        let rows = self
            .projects()
            .iter()
            .map(|p| {
                let cents = |cur: &str| {
                    by.get(&(p.id.as_str(), cur))
                        .map_or(Value::Null, |n| int(cents_from_numerator(*n)))
                };
                let fixed = self
                    .data
                    .projects
                    .get(&p.id)
                    .filter(|x| x.billing_type == BillingType::FixedFee)
                    .map_or_else(String::new, |x| {
                        // Over the Billable time here only, so a selection shows nothing of the rest.
                        match (x.fixed_fee_cents, x.fee_currency.as_deref()) {
                            (Some(fee), Some(cur)) if p.billable > 0 => {
                                let hourly = i128::from(fee) * 3600 / i128::from(p.billable);
                                format!(
                                    "{} per hour",
                                    money(cur, i64::try_from(hourly).unwrap_or(i64::MAX))
                                )
                            }
                            _ => "Not enough data".to_owned(),
                        }
                    });
                vec![
                    text(&p.name),
                    int(p.billable),
                    cents("EUR"),
                    cents("USD"),
                    text(rate_text(self.data, &p.id, self.ref_sec())),
                    text(&fixed),
                ]
            })
            .collect();
        AnalyticsSheet {
            name: "Earnings by project".into(),
            title: format!("Earnings by project: {}", self.place),
            columns: columns(&[
                ("Project", CellKind::Text),
                ("Billable time", CellKind::Duration),
                ("Earnings EUR", CellKind::Eur),
                ("Earnings USD", CellKind::Usd),
                ("Rate", CellKind::Text),
                ("Effective hourly rate (fixed fee)", CellKind::Text),
            ]),
            rows,
            contains_prices: true,
        }
    }

    fn sheets(&self) -> Vec<AnalyticsSheet> {
        let mut out = vec![
            self.kpi_sheet(),
            self.bands_sheet(),
            self.projects_sheet(),
            self.weekdays_sheet(),
        ];
        out.extend(self.tags_sheet());
        if self.shifts.configured && !self.shifts.stats.is_empty() {
            out.push(self.shifts_sheet());
        }
        out.push(self.earnings_period_sheet());
        out.push(self.earnings_project_sheet());
        out
    }
}

/// Works out the figures of one period from what `source` counts.
fn metrics<'a>(
    source: &'a ExportSource,
    zone: &str,
    tz: Tz,
    from_day: NaiveDate,
    to_day: NaiveDate,
    selected_days: Option<BTreeSet<i64>>,
) -> Metrics<'a> {
    let data = &source.data;
    let now = source.now.secs();
    let period = period_of(from_day, to_day, tz);
    let pieces = source.counted();
    let project_of = |task: &str| data.tasks.get(task).map(|t| t.project_id.clone());
    let stretches = stretches_of(&pieces, &source.device_id);
    let cut_to = |from: i64, to: i64| -> Vec<Span> {
        let mut list: Vec<Span> = stretches
            .iter()
            .filter_map(|s| {
                let (a, b) = (s.start.max(from), s.end.min(to));
                (b > a).then(|| {
                    Some(Span {
                        project: project_of(&s.task)?,
                        task: s.task.clone(),
                        start: a,
                        end: b,
                        own_start: s.start,
                        own_end: s.end,
                        day: day_of_instant(a, tz),
                    })
                })?
            })
            .collect();
        list.sort_by_key(|s| (s.start, s.end));
        list
    };
    let spans = cut_to(period.from, period.to);
    // Goals and streaks look at the days around the period too, as the screen does.
    let year_start = NaiveDate::from_ymd_opt(chrono::Datelike::year(&from_day), 1, 1)
        .and_then(|d| d.checked_sub_days(Days::new(7)))
        .unwrap_or(from_day);
    let year_end = NaiveDate::from_ymd_opt(chrono::Datelike::year(&to_day), 12, 31)
        .and_then(|d| d.checked_add_days(Days::new(8)))
        .unwrap_or(to_day);
    let ext = (
        period.from.min(start_of_day(year_start, tz).secs()),
        period.to.max(start_of_day(year_end, tz).secs()),
    );
    let mut day_totals = BTreeMap::new();
    for sp in cut_to(ext.0, ext.1) {
        for (day, a, b) in split_by_day(Ts(sp.start), Ts(sp.end), tz) {
            bump(&mut day_totals, day_num(day), b.secs() - a.secs());
        }
    }
    let row_pieces: Vec<Piece> = pieces
        .iter()
        .filter_map(|p| {
            let (a, b) = (p.start.secs().max(period.from), p.end.secs().min(period.to));
            (b > a).then(|| Piece {
                start: Ts(a),
                end: Ts(b),
                ..p.clone()
            })
        })
        .collect();
    let rows = task_days_of(data, tz, row_pieces);
    let mut windows = windows_for(data, ext.0, ext.1);
    // A selection counts only the shifts of the days it holds, so scheduled hours and attainment are
    // against those days, and a shift with no window on them is not named.
    if let Some(days) = &selected_days {
        let bounds: Vec<(i64, i64)> = days
            .iter()
            .map(|d| {
                let day = day_from(*d);
                (
                    start_of_day(day, tz).secs(),
                    start_of_day(day + Days::new(1), tz).secs(),
                )
            })
            .collect();
        windows.retain(|w| bounds.iter().any(|(s, e)| w.start < *e && w.end > *s));
    }
    let shifts = shifts_of(data, &spans, &windows, &period, now, tz);
    let client = data
        .settings
        .get("clientZone")
        .and_then(|e| e.value.as_str())
        .and_then(|z| parse_zone(z).ok());
    let overlap = client.map(|c| client_overlap(data, &spans, c, period.from, period.to));
    let agg = accumulate(&spans, tz, period.from, period.to);
    let gaps = pause_gaps(data, &stretches, &period, tz);
    let where_text = format!(
        "{}, {}",
        period.label,
        zone_label(zone, tz, now.min(period.to - 1))
    );
    let place = if selected_days.is_some() {
        format!("Selected entries, {where_text}")
    } else {
        where_text
    };
    Metrics {
        data,
        today: day_of_instant(now, tz),
        now,
        agg,
        spans,
        gaps,
        rows,
        day_totals,
        shifts,
        overlap,
        selected_days,
        place,
        period,
    }
}

/// The analysis sheets of a narrowed source: its first to last selected day, those entries only.
pub fn selection_sheets(
    source: &ExportSource,
    zone: &str,
) -> Result<Vec<AnalyticsSheet>, CoreError> {
    let tz = parse_zone(zone)?;
    let days: BTreeSet<i64> = source
        .only
        .iter()
        .flat_map(|o| o.keys.iter().map(|k| day_num(k.1)))
        .collect();
    let (Some(first), Some(last)) = (days.first().copied(), days.last().copied()) else {
        return Err(CoreError::Validation(
            "select at least one entry to export".into(),
        ));
    };
    Ok(metrics(
        source,
        zone,
        tz,
        day_from(first),
        day_from(last),
        Some(days),
    )
    .sheets())
}

#[cfg(test)]
pub(crate) fn period_sheets(
    source: &ExportSource,
    zone: &str,
    from: NaiveDate,
    to: NaiveDate,
) -> Vec<AnalyticsSheet> {
    let tz = parse_zone(zone).unwrap();
    metrics(source, zone, tz, from, to, None).sheets()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::billing::tests::seg;
    use crate::machine::RateInput;
    use crate::machine::tests::{DEV, ctx, project_input, task_input};
    use crate::model::{
        BillablePause, GapDecision, GapKind, IdleGap, RoundMethod, RoundingVersion, Segment,
        SettingEntry, Stamp, Tag,
    };
    use crate::report::SelectedEntry;
    use crate::report::tests::source;
    use crate::summary::project_figures;
    use crate::xlsx;

    const ZONE: &str = "America/New_York";
    const PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/analytics-parity.json"
    );

    fn date(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }
    fn stamp() -> Stamp {
        Stamp {
            at: Ts::parse("2026-01-01T00:00:00Z").unwrap(),
            by: "x".into(),
        }
    }
    fn at(s: &str) -> Ts {
        Ts::parse(s).unwrap()
    }

    /// Fixed ids, so the fixture file is stable.
    fn project(
        s: &mut Shared,
        id: &str,
        color: &str,
        change: impl Fn(&mut crate::machine::ProjectInput),
    ) {
        let c = ctx("2026-01-01T00:00:00Z");
        let mut input = project_input(id);
        input.color = color.into();
        change(&mut input);
        let made = s.create_project(&c, &input).unwrap();
        let mut p = s.projects.remove(&made).unwrap();
        p.id = id.into();
        s.projects.insert(id.into(), p);
    }
    fn task(s: &mut Shared, id: &str, project: &str, tags: &[&str]) {
        let c = ctx("2026-01-01T00:00:00Z");
        let made = s.create_task(&c, &task_input(id, project)).unwrap();
        let mut t = s.tasks.remove(&made).unwrap();
        t.id = id.into();
        t.tags = tags.iter().map(|x| (*x).to_owned()).collect();
        s.tasks.insert(id.into(), t);
    }
    fn rate(s: &mut Shared, p: &str, cents: i64, cur: &str, from: &str) {
        let c = ctx("2026-01-01T00:00:00Z");
        s.set_rate(
            &c,
            &RateInput {
                project_id: p.into(),
                rate_cents: Some(cents),
                currency: cur.into(),
                effective_from: Some(from.into()),
                confirm_retroactive: true,
            },
        )
        .unwrap();
        let ids: Vec<String> = s.rates.keys().cloned().collect();
        let mut rates: Vec<_> = ids.iter().map(|k| s.rates.remove(k).unwrap()).collect();
        rates.sort_by_key(|r| (r.project_id.clone(), r.effective_from));
        for (i, mut r) in rates.into_iter().enumerate() {
            r.id = format!("rate{i}");
            s.rates.insert(r.id.clone(), r);
        }
    }
    #[expect(clippy::too_many_arguments, reason = "one table of shifts in one test")]
    fn shift(
        s: &mut Shared,
        id: &str,
        shift_id: &str,
        name: &str,
        from: &str,
        hours: (&str, &str),
        days: &[u8],
        zone: &str,
        brk: u32,
        retired: bool,
    ) {
        s.shifts.insert(
            id.into(),
            ShiftVersion {
                id: id.into(),
                shift_id: shift_id.into(),
                effective_from: from.into(),
                name: name.into(),
                color: "#0ea5e9".into(),
                start: hours.0.into(),
                end: hours.1.into(),
                weekdays: days.to_vec(),
                zone: zone.into(),
                break_minutes: brk,
                retired,
                deleted: false,
                stamp: stamp(),
            },
        );
    }
    fn setting(s: &mut Shared, key: &str, value: Value) {
        s.settings.insert(
            key.into(),
            SettingEntry {
                value,
                stamp: stamp(),
            },
        );
    }

    /// Every kind of thing the sheets read: rates that change, rounding per project, a fixed fee, a
    /// project with no rate, tags, shifts with a new version, a retired one and an overnight one,
    /// targets, a client zone, a billable pause, thinking and dropped away time, a segment over
    /// midnight, one over the spring clock change, and a task with the word Secret that is never selected.
    fn world() -> Shared {
        let mut s = Shared::default();
        project(&mut s, "alpha", "#111111", |_| {});
        project(&mut s, "beta", "#222222", |_| {});
        project(&mut s, "fixed", "#333333", |p| {
            p.billing_type = BillingType::FixedFee;
            p.fixed_fee_cents = Some(100_000);
            p.fee_currency = Some("EUR".into());
        });
        project(&mut s, "internal", "#444444", |p| p.billable = false);
        project(&mut s, "norate", "#555555", |_| {});
        project(&mut s, "secretproject", "#666666", |_| {});
        rate(&mut s, "alpha", 6000, "EUR", "2026-01-01T00:00:00Z");
        rate(&mut s, "alpha", 7200, "EUR", "2026-03-03T14:00:00Z");
        rate(&mut s, "beta", 8050, "USD", "2026-01-01T00:00:00Z");
        rate(
            &mut s,
            "secretproject",
            99_900,
            "USD",
            "2026-01-01T00:00:00Z",
        );
        for (id, name) in [
            ("tag-a", "Client work"),
            ("tag-b", "Deep"),
            ("tag-z", "Hidden tag"),
        ] {
            s.tags.insert(
                id.into(),
                Tag {
                    id: id.into(),
                    name: name.into(),
                    color: "#abcdef".into(),
                    deleted: false,
                    stamp: stamp(),
                },
            );
        }
        s.rounding.insert(
            "r-global".into(),
            RoundingVersion {
                id: "r-global".into(),
                project_id: None,
                effective_from: "2000-01-01".into(),
                increment_minutes: 15,
                method: RoundMethod::Up,
                deleted: false,
                stamp: stamp(),
            },
        );
        s.rounding.insert(
            "r-beta".into(),
            RoundingVersion {
                id: "r-beta".into(),
                project_id: Some("beta".into()),
                effective_from: "2000-01-01".into(),
                increment_minutes: 6,
                method: RoundMethod::Nearest,
                deleted: false,
                stamp: stamp(),
            },
        );
        task(&mut s, "write", "alpha", &["tag-a", "tag-b"]);
        task(&mut s, "review", "alpha", &[]);
        task(&mut s, "design", "beta", &["tag-a"]);
        task(&mut s, "support", "fixed", &[]);
        task(&mut s, "admin", "internal", &[]);
        task(&mut s, "spike", "norate", &[]);
        task(&mut s, "secret", "secretproject", &["tag-z"]);
        for (t, id, a, b) in [
            (
                "write",
                "w1",
                "2026-03-02T14:00:00Z",
                "2026-03-02T15:10:00Z",
            ),
            (
                "write",
                "w2",
                "2026-03-02T15:30:00Z",
                "2026-03-02T16:00:00Z",
            ),
            (
                "design",
                "d1",
                "2026-03-02T16:05:00Z",
                "2026-03-02T17:00:00Z",
            ),
            // Over midnight in New York.
            (
                "write",
                "w3",
                "2026-03-03T04:30:00Z",
                "2026-03-03T05:40:00Z",
            ),
            (
                "review",
                "r1",
                "2026-03-03T13:00:00Z",
                "2026-03-03T13:07:00Z",
            ),
            (
                "design",
                "d2",
                "2026-03-03T13:10:00Z",
                "2026-03-03T15:00:00Z",
            ),
            (
                "admin",
                "a1",
                "2026-03-03T15:00:00Z",
                "2026-03-03T15:30:00Z",
            ),
            (
                "secret",
                "s1",
                "2026-03-03T16:00:00Z",
                "2026-03-03T18:00:00Z",
            ),
            (
                "support",
                "u1",
                "2026-03-04T14:00:00Z",
                "2026-03-04T18:00:00Z",
            ),
            (
                "spike",
                "k1",
                "2026-03-04T18:10:00Z",
                "2026-03-04T18:20:00Z",
            ),
            (
                "write",
                "w4",
                "2026-03-04T19:00:00Z",
                "2026-03-04T20:30:00Z",
            ),
            (
                "secret",
                "s2",
                "2026-03-05T14:00:00Z",
                "2026-03-05T22:00:00Z",
            ),
            (
                "write",
                "w5",
                "2026-03-06T14:00:00Z",
                "2026-03-06T15:00:00Z",
            ),
            // Over the clock change in New York, 2:00 to 3:00 on 8 March.
            (
                "admin",
                "a2",
                "2026-03-08T06:30:00Z",
                "2026-03-08T08:30:00Z",
            ),
            (
                "write",
                "w6",
                "2026-03-10T13:00:00Z",
                "2026-03-10T16:45:30Z",
            ),
            (
                "write",
                "w7",
                "2026-03-11T13:30:00Z",
                "2026-03-11T14:00:00Z",
            ),
            (
                "design",
                "d3",
                "2026-03-11T14:00:00Z",
                "2026-03-11T15:00:00Z",
            ),
            (
                "write",
                "w8",
                "2026-03-12T13:15:00Z",
                "2026-03-12T13:20:00Z",
            ),
            (
                "review",
                "r2",
                "2026-05-14T13:00:00Z",
                "2026-05-14T15:00:00Z",
            ),
            (
                "design",
                "d4",
                "2026-11-20T15:00:00Z",
                "2026-11-20T15:30:00Z",
            ),
        ] {
            seg(&mut s, t, id, a, b);
        }
        s.pauses.insert(
            "p1".into(),
            BillablePause {
                id: "p1".into(),
                task_id: "write".into(),
                start: at("2026-03-02T15:10:00Z"),
                end: at("2026-03-02T15:20:00Z"),
                note: String::new(),
                deleted: false,
                stamp: stamp(),
            },
        );
        for (id, decision, a, b) in [
            (
                "g1",
                GapDecision::Thinking,
                "2026-03-04T19:20:00Z",
                "2026-03-04T19:30:00Z",
            ),
            (
                "g2",
                GapDecision::Drop,
                "2026-03-04T19:50:00Z",
                "2026-03-04T20:00:00Z",
            ),
        ] {
            s.gaps.insert(
                id.into(),
                IdleGap {
                    id: id.into(),
                    segment_id: "w4".into(),
                    task_id: "write".into(),
                    device_id: DEV.into(),
                    start: at(a),
                    end: at(b),
                    kind: GapKind::Idle,
                    decision,
                    decision_stamp: stamp(),
                },
            );
        }
        let stopped = |s: &mut Shared, id: &str, when: &str| {
            let t = s.tasks.get_mut(id).unwrap();
            t.state = TaskState::Stopped;
            t.stopped_at = Some(at(when));
        };
        stopped(&mut s, "write", "2026-03-12T13:40:00Z");
        stopped(&mut s, "support", "2026-03-04T18:05:00Z");
        s.tasks.get_mut("design").unwrap().state = TaskState::Paused;
        s.tasks.get_mut("review").unwrap().state = TaskState::Paused;
        shift(
            &mut s,
            "v1",
            "day",
            "Day",
            "2020-01-01",
            ("09:00", "17:00"),
            &[1, 2, 3, 4, 5],
            ZONE,
            30,
            false,
        );
        shift(
            &mut s,
            "v2",
            "day",
            "Day",
            "2026-03-10",
            ("08:00", "16:00"),
            &[1, 2, 3, 4, 5],
            ZONE,
            60,
            false,
        );
        shift(
            &mut s,
            "v3",
            "late",
            "Late",
            "2020-01-01",
            ("22:00", "02:00"),
            &[1, 3],
            "UTC",
            0,
            false,
        );
        shift(
            &mut s,
            "v4",
            "old",
            "Old",
            "2020-01-01",
            ("12:00", "13:00"),
            &[2],
            ZONE,
            0,
            false,
        );
        shift(
            &mut s,
            "v5",
            "old",
            "Old",
            "2026-03-05",
            ("12:00", "13:00"),
            &[2],
            ZONE,
            0,
            true,
        );
        setting(&mut s, "clientZone", json!("Europe/Lisbon"));
        setting(&mut s, "clientWorkStart", json!("10:00"));
        setting(&mut s, "clientWorkEnd", json!("16:00"));
        setting(&mut s, "dailyTargetHours", json!(4));
        setting(&mut s, "weeklyTargetHours", json!(20));
        s
    }

    fn entry(task: &str, day: &str) -> SelectedEntry {
        SelectedEntry {
            task_id: task.into(),
            day: day.into(),
        }
    }

    /// Selected on every day of its period, so the UI's engine, given the same pieces, sees the same
    /// days. Unselected on those days: `secret`, `review`, `admin`, `spike`.
    fn selected(src: &ExportSource) -> ExportSource {
        src.narrowed(
            parse_zone(ZONE).unwrap(),
            &[
                entry("write", "2026-03-02"),
                entry("write", "2026-03-03"),
                entry("write", "2026-03-04"),
                entry("design", "2026-03-03"),
                entry("support", "2026-03-04"),
            ],
        )
        .unwrap()
    }

    /// The fields of the app's snapshot that the UI's analytics read, made the way `service` makes them.
    fn snapshot_of(src: &ExportSource) -> Value {
        let tz = parse_zone(ZONE).unwrap();
        let end = |s: &Segment| src.end_of(s);
        let figures = project_figures(&src.data, tz, &end);
        let rows = src.rows(tz);
        let projects: Vec<Value> = src
            .data
            .projects
            .values()
            .map(|p| {
                let mut f = figures.get(&p.id).cloned().unwrap_or_default();
                // A selection's rate is over its own time: `project_figures` sees whole tasks.
                if src.only.is_some() {
                    let secs: i64 = rows
                        .iter()
                        .filter(|r| r.row.project_id == p.id)
                        .map(|r| r.row.billable_secs)
                        .sum();
                    f.effective_hourly_cents = p
                        .fixed_fee_cents
                        .filter(|_| secs > 0 && p.billing_type == BillingType::FixedFee)
                        .map(|fee| fee * 3600 / secs);
                }
                f.budget = None;
                json!({
                    "id": p.id, "name": p.name, "color": p.color, "archived": p.archived,
                    "billable": p.billable, "budgetHours": p.budget_hours, "billingType": p.billing_type,
                    "fixedFeeCents": p.fixed_fee_cents, "feeCurrency": p.fee_currency, "figures": f,
                })
            })
            .collect();
        let tasks: Vec<Value> = src
            .data
            .tasks
            .values()
            .map(|t| {
                json!({
                    "id": t.id, "name": t.name, "projectId": t.project_id, "state": t.state,
                    "stoppedAt": t.stopped_at, "tags": t.tags,
                })
            })
            .collect();
        let mut pieces = src.counted();
        pieces.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| a.id.cmp(&b.id)));
        let segments: Vec<Value> = pieces
            .iter()
            .map(|p| {
                let device = p.device_id.clone().unwrap_or_default();
                json!({
                    "id": p.id, "taskId": p.task_id, "deviceId": device, "start": p.start,
                    "end": (!p.open).then_some(p.end),
                    "provisionalEnd": (p.open && device != src.device_id).then_some(p.end),
                    "segmentId": p.source_id, "kind": p.kind,
                })
            })
            .collect();
        let settings: BTreeMap<&String, &Value> = src
            .data
            .settings
            .iter()
            .map(|(k, v)| (k, &v.value))
            .collect();
        json!({
            "systemZone": "UTC",
            "projects": projects,
            "tasks": tasks,
            "segments": segments,
            "shifts": src.data.shifts.values().collect::<Vec<_>>(),
            "rates": src.data.rates.values().collect::<Vec<_>>(),
            "rounding": src.data.rounding.values().collect::<Vec<_>>(),
            "settings": settings,
            "tags": src.data.tags.values().collect::<Vec<_>>(),
            "gaps": [],
            "pauses": [],
        })
    }

    fn case(name: &str, src: &ExportSource, from: &str, to: &str) -> Value {
        let sheets = if src.only.is_some() {
            selection_sheets(src, ZONE).unwrap()
        } else {
            period_sheets(src, ZONE, date(from), date(to))
        };
        json!({
            "name": name, "zone": ZONE, "now": src.now.secs(), "from": from, "to": to,
            "selection": src.only.is_some(),
            "snapshot": snapshot_of(src), "sheets": sheets,
        })
    }

    /// The figures the UI's analytics must also give for these inputs: `analyticsParity.test.ts` runs
    /// them through `buildAnalyticsSheets` and compares. Written with `ZWT_WRITE_PARITY=1` and committed.
    #[test]
    fn the_sheets_match_the_fixture_the_ui_is_checked_against() {
        let full = source(world(), "2027-02-01T00:00:00Z");
        let mut running = source(world(), "2026-03-20T15:40:00Z");
        running.data.segments.insert(
            "open".into(),
            Segment {
                id: "open".into(),
                task_id: "review".into(),
                device_id: DEV.into(),
                start: at("2026-03-20T15:00:00Z"),
                end: None,
                ..Segment::default()
            },
        );
        running.data.tasks.get_mut("review").unwrap().state = TaskState::Running;
        let mut plain = world();
        plain.shifts.clear();
        plain.tags.clear();
        plain.settings.remove("clientZone");
        let plain = source(plain, "2027-02-01T00:00:00Z");
        // Client hours hold on every day of the week, and run overnight when they end before they start.
        let mut weekend = world();
        seg(
            &mut weekend,
            "write",
            "sat",
            "2026-03-07T11:00:00Z",
            "2026-03-07T12:30:00Z",
        );
        seg(
            &mut weekend,
            "review",
            "sun",
            "2026-03-15T15:00:00Z",
            "2026-03-15T17:00:00Z",
        );
        let weekend = source(weekend, "2027-02-01T00:00:00Z");
        let client_hours = |start: &str, end: &str| {
            let mut data = world();
            setting(&mut data, "clientWorkStart", json!(start));
            setting(&mut data, "clientWorkEnd", json!(end));
            // Saturday night into Sunday, over the clock change in Lisbon.
            seg(
                &mut data,
                "review",
                "night",
                "2026-03-28T20:00:00Z",
                "2026-03-29T06:00:00Z",
            );
            source(data, "2027-02-01T00:00:00Z")
        };
        let overnight = client_hours("22:00", "06:00");
        let all_day = client_hours("00:00", "00:00");
        let got = json!({
            "comment": "Written by the Rust test in analytics.rs, and read by that test and by analyticsParity.test.ts, so both sides give these figures.",
            "cases": [
                case("a month", &full, "2026-03-01", "2026-03-31"),
                case("one day", &full, "2026-03-03", "2026-03-03"),
                case("a week", &full, "2026-03-02", "2026-03-08"),
                case("weeks", &full, "2026-01-01", "2026-06-30"),
                case("months", &full, "2026-01-01", "2027-01-31"),
                case("a month still running", &running, "2026-03-01", "2026-03-31"),
                case("no shifts, tags or client zone", &plain, "2026-03-01", "2026-03-31"),
                case("a selection", &selected(&full), "2026-03-02", "2026-03-04"),
                case("client hours on a weekend", &weekend, "2026-03-01", "2026-03-31"),
                case("client hours overnight", &overnight, "2026-03-01", "2026-03-31"),
                case("client hours overnight, one day", &overnight, "2026-03-03", "2026-03-03"),
                case("client hours all day", &all_day, "2026-03-02", "2026-03-08"),
                case("a selection with client hours overnight", &selected(&overnight), "2026-03-02", "2026-03-04"),
            ],
        });
        let body = serde_json::to_string_pretty(&got).unwrap() + "\n";
        if std::env::var_os("ZWT_WRITE_PARITY").is_some() {
            std::fs::write(PATH, &body).unwrap();
        }
        // A checkout may turn line endings into CRLF. Only the content is pinned.
        let want = std::fs::read_to_string(PATH).unwrap().replace("\r\n", "\n");
        assert_eq!(
            body, want,
            "run with ZWT_WRITE_PARITY=1, then check it with vitest"
        );
    }

    fn sheet<'a>(all: &'a [AnalyticsSheet], name: &str) -> &'a AnalyticsSheet {
        all.iter().find(|s| s.name == name).unwrap()
    }
    fn kpi(all: &[AnalyticsSheet], metric: &str, column: usize) -> Value {
        sheet(all, "KPI summary")
            .rows
            .iter()
            .find(|r| r[0].as_str() == Some(metric))
            .unwrap()[column]
            .clone()
    }

    #[test]
    fn r7_the_earnings_by_period_sheet_says_its_rows_can_differ_from_its_total() {
        let full = source(world(), "2027-02-01T00:00:00Z");
        let sheets = selection_sheets(&selected(&full), ZONE).unwrap();
        let title = &sheet(&sheets, "Earnings by period").title;
        assert!(
            title.contains(
                "Row amounts are rounded; the total is worked out from the exact amounts."
            ),
            "{title}"
        );
    }

    #[test]
    fn a_selections_totals_are_those_of_its_own_rows() {
        let full = source(world(), "2027-02-01T00:00:00Z");
        let only = selected(&full);
        let sheets = selection_sheets(&only, ZONE).unwrap();
        let tz = parse_zone(ZONE).unwrap();
        let rows = only.rows(tz);
        let exact: i64 = rows.iter().map(|r| r.row.exact_secs).sum();
        let billable: i64 = rows.iter().map(|r| r.row.billable_secs).sum();
        assert_eq!(
            kpi(&sheets, "Total time (Exact)", 1).as_f64(),
            Some(exact as f64)
        );
        assert_eq!(
            kpi(&sheets, "Total time (Billable)", 1).as_f64(),
            Some(billable as f64)
        );
        let last = sheet(&sheets, "Earnings by period")
            .rows
            .last()
            .unwrap()
            .clone();
        assert_eq!(last[0], "Total");
        assert_eq!(last[1].as_i64(), Some(billable));
        let mut eur = 0;
        for r in &rows {
            eur += r.row.numerators().get("EUR").copied().unwrap_or(0);
        }
        assert_eq!(last[3].as_i64(), Some(cents_from_numerator(eur)));
        // Projects add up to the same total.
        let sum: i64 = sheet(&sheets, "Projects")
            .rows
            .iter()
            .map(|r| r[2].as_i64().unwrap())
            .sum();
        assert_eq!(sum, billable);
        // `write` stopped on a day left out, so only `support` counts as completed.
        assert_eq!(kpi(&sheets, "Tasks completed", 2), json!(1));
    }

    #[test]
    fn nothing_unselected_reaches_a_sheet() {
        let full = source(world(), "2027-02-01T00:00:00Z");
        let sheets = selection_sheets(&selected(&full), ZONE).unwrap();
        let all = serde_json::to_string(&sheets).unwrap();
        for leak in [
            "secret",
            "Secret",
            "Hidden tag",
            "review",
            "admin",
            "spike",
            "norate",
            "internal",
        ] {
            assert!(!all.contains(leak), "{leak}");
        }
        assert!(all.contains("Selected entries, 2 Mar – 4 Mar 2026"));
    }

    #[test]
    fn gaps_in_a_selection_get_no_row_and_no_shift_of_their_own() {
        let full = source(world(), "2027-02-01T00:00:00Z");
        let only = full
            .narrowed(
                parse_zone(ZONE).unwrap(),
                &[entry("write", "2026-03-02"), entry("write", "2026-03-06")],
            )
            .unwrap();
        let sheets = selection_sheets(&only, ZONE).unwrap();
        let labels: Vec<&str> = sheet(&sheets, "Earnings by period")
            .rows
            .iter()
            .map(|r| r[0].as_str().unwrap())
            .collect();
        assert_eq!(labels, ["Mon 2", "Fri 6", "Total"]);
        // Old runs on Tuesdays only, so a Monday and a Friday never name it, though the days between do.
        let shifts = sheet(&sheets, "Shifts");
        let names: Vec<&str> = shifts.rows.iter().map(|r| r[0].as_str().unwrap()).collect();
        assert_eq!(names, ["Day", "Late", "Outside any shift"]);
        // The Day shift counts two windows, 7.5 hours each, not those of the days between.
        assert_eq!(shifts.rows[0][2], json!(2));
        assert_eq!(shifts.rows[0][3].as_f64(), Some(2.0 * 7.5 * 3600.0));
    }

    #[test]
    fn a_task_cut_from_one_day_loses_the_session_on_the_other() {
        let full = source(world(), "2027-02-01T00:00:00Z");
        // w3 runs 23:30 to 00:40 in New York: only its first half is on 2 March.
        let only = full
            .narrowed(parse_zone(ZONE).unwrap(), &[entry("write", "2026-03-03")])
            .unwrap();
        let sheets = selection_sheets(&only, ZONE).unwrap();
        assert_eq!(kpi(&sheets, "Total time (Exact)", 1).as_f64(), Some(2400.0));
        assert_eq!(kpi(&sheets, "Sessions", 2), json!(1));
        assert_eq!(kpi(&sheets, "Pauses", 2), json!(0));
    }

    #[test]
    fn the_selected_workbook_holds_every_sheet_and_ignores_what_the_ui_sends() {
        let full = source(world(), "2027-02-01T00:00:00Z");
        let request = crate::report::ExportRequest {
            day_zone: ZONE.into(),
            my_zone: ZONE.into(),
            client_zone: None,
            include_earnings: false,
            segment_shifts: BTreeMap::new(),
            analytics: vec![AnalyticsSheet {
                name: "From the UI".into(),
                title: "Secret".into(),
                columns: Vec::new(),
                rows: Vec::new(),
                contains_prices: false,
            }],
            selection: Some(vec![entry("write", "2026-03-02")]),
        };
        let bytes = xlsx::build(&full, &request).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("x.xlsx");
        std::fs::write(&zip, &bytes).unwrap();
        let out = std::process::Command::new("unzip")
            .arg("-p")
            .arg(&zip)
            .output()
            .unwrap();
        let all = String::from_utf8_lossy(&out.stdout);
        for wanted in [
            "About this selection",
            "KPI summary",
            "Bands",
            "Projects",
            "Weekdays",
            "Tags",
        ] {
            assert!(all.contains(wanted), "{wanted}");
        }
        for leak in ["From the UI", "Secret", "Earnings", "EUR", "USD"] {
            assert!(!all.contains(leak), "{leak}");
        }
    }

    #[test]
    fn a_cut_segment_shows_the_shifts_of_its_selected_part_and_nothing_from_the_ui() {
        // w3 runs 23:30 on 2 March to 00:40 on 3 March in New York. Only 3 March is selected, and a
        // shift runs on 2 March alone, covering its half.
        let mut data = world();
        shift(
            &mut data,
            "v9",
            "eve",
            "LeakEvening",
            "2020-01-01",
            ("23:00", "23:59"),
            &[1],
            ZONE,
            0,
            false,
        );
        let full = source(data, "2027-02-01T00:00:00Z");
        let mut sent = BTreeMap::new();
        sent.insert(
            "w3".to_owned(),
            "LeakEvening 0:30:00; Outside 0:40:00".to_owned(),
        );
        let request = crate::report::ExportRequest {
            day_zone: ZONE.into(),
            my_zone: ZONE.into(),
            client_zone: None,
            include_earnings: true,
            segment_shifts: sent,
            analytics: Vec::new(),
            selection: Some(vec![entry("write", "2026-03-03")]),
        };
        let bytes = xlsx::build(&full, &request).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("x.xlsx");
        std::fs::write(&zip, &bytes).unwrap();
        let out = std::process::Command::new("unzip")
            .arg("-p")
            .arg(&zip)
            .output()
            .unwrap();
        let all = String::from_utf8_lossy(&out.stdout);
        assert!(!all.contains("LeakEvening"), "a shift of the day left out");
        assert!(all.contains("0:40:00"), "the selected part's own split");
    }

    #[test]
    fn a_task_whose_latest_day_was_left_out_is_not_in_progress() {
        let mut data = world();
        data.tasks.get_mut("design").unwrap().state = TaskState::Running;
        let full = source(data, "2027-02-01T00:00:00Z");
        let tz = parse_zone(ZONE).unwrap();
        let earlier = full.narrowed(tz, &[entry("design", "2026-03-03")]).unwrap();
        let sheets = selection_sheets(&earlier, ZONE).unwrap();
        assert_eq!(kpi(&sheets, "Tasks in progress", 2), json!(0));
        let latest = full.narrowed(tz, &[entry("design", "2026-11-20")]).unwrap();
        let sheets = selection_sheets(&latest, ZONE).unwrap();
        assert_eq!(kpi(&sheets, "Tasks in progress", 2), json!(1));
    }

    #[test]
    fn a_day_holding_only_a_billable_pause_still_adds_up_across_the_sheets() {
        let mut data = world();
        data.pauses.insert(
            "p2".into(),
            BillablePause {
                id: "p2".into(),
                task_id: "design".into(),
                start: at("2026-03-25T14:00:00Z"),
                end: at("2026-03-25T15:00:00Z"),
                note: String::new(),
                deleted: false,
                stamp: stamp(),
            },
        );
        let full = source(data, "2027-02-01T00:00:00Z");
        let only = full
            .narrowed(parse_zone(ZONE).unwrap(), &[entry("design", "2026-03-25")])
            .unwrap();
        let sheets = selection_sheets(&only, ZONE).unwrap();
        assert_eq!(
            kpi(&sheets, "Total time (Billable)", 1).as_f64(),
            Some(3600.0)
        );
        let projects = &sheet(&sheets, "Projects").rows;
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0][2], json!(3600));
        assert_eq!(projects[0][4], json!(1));
        assert_eq!(
            sheet(&sheets, "Earnings by project").rows[0][3],
            json!(8050)
        );
        assert_eq!(sheet(&sheets, "Tags").rows[0][2], json!(3600));
    }

    #[test]
    fn the_wall_clock_is_right_through_a_clock_change() {
        let ny = parse_zone(ZONE).unwrap();
        // Spring forward: 02:30 does not exist, so it is the instant the clocks jump.
        assert_eq!(
            instant_of_wall(date("2026-03-08"), 150, ny),
            at("2026-03-08T07:00:00Z").secs()
        );
        // Fall back: 01:30 happens twice, and the first is the one.
        assert_eq!(
            instant_of_wall(date("2026-11-01"), 90, ny),
            at("2026-11-01T05:30:00Z").secs()
        );
        assert_eq!(
            instant_of_wall(date("2026-06-01"), 540, ny),
            at("2026-06-01T13:00:00Z").secs()
        );
    }

    /// The rule is `src/lib/clientHours.ts`, and these are the cases its own tests name.
    #[test]
    fn client_hours_hold_every_day_and_run_overnight_as_the_ui_says() {
        let lisbon = parse_zone("Europe/Lisbon").unwrap();
        let inside = |start: &str, end: &str, from: &str, to: &str| {
            let mut data = Shared::default();
            setting(&mut data, "clientWorkStart", json!(start));
            setting(&mut data, "clientWorkEnd", json!(end));
            let (from, to) = (at(from).secs(), at(to).secs());
            let span = Span {
                task: String::new(),
                project: String::new(),
                start: from,
                end: to,
                own_start: from,
                own_end: to,
                day: 0,
            };
            client_overlap(&data, &[span], lisbon, from, to)
        };
        // Lisbon is on UTC until 29 March 2026. 7 March is a Saturday, 8 March a Sunday, 9 March a Monday.
        for (day, secs) in [
            ("2026-03-07", 6 * 3600),
            ("2026-03-08", 6 * 3600),
            ("2026-03-09", 6 * 3600),
        ] {
            let (from, to) = (format!("{day}T00:00:00Z"), format!("{day}T23:59:59Z"));
            assert_eq!(inside("10:00", "16:00", &from, &to), secs, "{day}");
        }
        // From 22:00 to 06:00: the night before the range counts, and so does the one starting in it.
        assert_eq!(
            inside(
                "22:00",
                "06:00",
                "2026-03-03T04:30:00Z",
                "2026-03-03T07:00:00Z"
            ),
            5400,
            "the end of the night before"
        );
        assert_eq!(
            inside(
                "22:00",
                "06:00",
                "2026-03-03T21:00:00Z",
                "2026-03-04T01:00:00Z"
            ),
            3 * 3600,
            "over midnight"
        );
        // A weekend night is a night like any other.
        assert_eq!(
            inside(
                "22:00",
                "06:00",
                "2026-03-07T21:00:00Z",
                "2026-03-08T07:00:00Z"
            ),
            8 * 3600,
            "Saturday into Sunday"
        );
        // The same start and end cover the whole day, which is how to say round the clock.
        for (start, end) in [("00:00", "00:00"), ("09:00", "09:00")] {
            assert_eq!(
                inside(start, end, "2026-03-06T21:00:00Z", "2026-03-08T03:00:00Z"),
                30 * 3600,
                "{start} to {end}"
            );
        }
        // The night the clocks go forward is an hour short, and the night they go back an hour long.
        assert_eq!(
            inside(
                "22:00",
                "06:00",
                "2026-03-28T20:00:00Z",
                "2026-03-29T12:00:00Z"
            ),
            7 * 3600,
            "clocks forward"
        );
        assert_eq!(
            inside(
                "22:00",
                "06:00",
                "2026-10-24T19:00:00Z",
                "2026-10-25T12:00:00Z"
            ),
            9 * 3600,
            "clocks back"
        );
    }

    #[test]
    fn a_task_deleted_with_its_time_reaches_no_sheet() {
        let tz = parse_zone(ZONE).unwrap();
        let from = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let to = NaiveDate::from_ymd_opt(2026, 12, 31).unwrap();
        let text = |data: Shared| {
            let src = source(data, "2027-02-01T00:00:00Z");
            let sheets = metrics(&src, ZONE, tz, from, to, None).sheets();
            serde_json::to_string(&sheets).unwrap()
        };
        let all = text(world());
        assert!(all.contains("secretproject") && all.contains("Hidden tag"));
        let mut data = world();
        data.tasks.get_mut("secret").unwrap().discarded = Some(stamp());
        let all = text(data);
        assert!(!all.contains("secretproject"), "its project's row");
        assert!(!all.contains("Hidden tag"), "its tag's row");
    }
}
