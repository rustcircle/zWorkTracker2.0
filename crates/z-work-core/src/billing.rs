//! Derived figures for reports: task-day rows, rounding and earnings. Nothing here is stored.
//! The same rules are implemented in `src/lib/billing.ts` and pinned by `fixtures/billing-fixtures.json`.
//!
//! Rules:
//! - Raw UTC segments are never altered. A row is one task on one day in the reporting zone.
//! - The row's exact time is rounded once. Larger totals of time are sums of the rounded rows.
//! - Rounding version: the project's own version in force on the row's day, else the global one,
//!   else the default (15 minutes, up). Timestamps are whole seconds, so sub-second timer noise can
//!   never push a row over an exact multiple: 15:00 stays 15:00 and 15:01 bills as 30:00.
//! - Earnings are `seconds * rate_cents / 3600`, on billable seconds. Row buckets keep the exact numerator.
//!   A row shows its own amount rounded, and a total is the exact numerators of its rows added up, then
//!   rounded once (half up to whole cents) by `total_cents`, never a sum of rounded rows, so every document
//!   comes to the same total. Currencies are never added together.
//! - The rounding difference goes to the last rate in force that day. If that bucket would go below
//!   zero, the remainder moves to the previous bucket.

use std::collections::BTreeMap;

use chrono::{Datelike, Days, NaiveDate, NaiveTime, TimeZone};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::machine::stamp_order;
use crate::model::{RateVersion, RoundMethod, Segment, Shared};
use crate::pieces::{Piece, PieceKind, pieces};
use crate::time::Ts;
use crate::validate::is_currency;

pub const DEFAULT_INCREMENT_MINUTES: u32 = 15;

/// Local calendar day of an instant.
#[must_use]
pub fn day_of(t: Ts, tz: Tz) -> NaiveDate {
    t.datetime().with_timezone(&tz).date_naive()
}

/// First instant whose wall clock reaches `date` at 00:00. Skips forward over a DST gap.
#[must_use]
pub fn start_of_day(date: NaiveDate, tz: Tz) -> Ts {
    let mut wall = date.and_time(NaiveTime::MIN);
    for _ in 0..(4 * 26) {
        if let Some(t) = tz.from_local_datetime(&wall).earliest() {
            return Ts(t.timestamp());
        }
        wall += chrono::Duration::minutes(15);
    }
    Ts(wall.and_utc().timestamp())
}

/// Splits `[start, end)` at local midnights. A zero-length span yields one zero-length piece.
#[must_use]
pub fn split_by_day(start: Ts, end: Ts, tz: Tz) -> Vec<(NaiveDate, Ts, Ts)> {
    let mut out = Vec::new();
    let mut cur = start;
    loop {
        let day = day_of(cur, tz);
        let next = day
            .checked_add_days(Days::new(1))
            .map_or(end, |d| start_of_day(d, tz));
        if next >= end {
            out.push((day, cur, end));
            return out;
        }
        out.push((day, cur, next));
        cur = next;
    }
}

/// Rounds exact seconds to the increment. Exact halves round up under `Nearest`.
#[must_use]
pub fn round_seconds(exact: i64, increment_minutes: u32, method: RoundMethod) -> i64 {
    if increment_minutes == 0 {
        return exact;
    }
    let inc = i64::from(increment_minutes) * 60;
    let down = exact.div_euclid(inc) * inc;
    let rem = exact.rem_euclid(inc);
    match method {
        RoundMethod::Down => down,
        RoundMethod::Up => {
            if rem == 0 {
                down
            } else {
                down + inc
            }
        }
        RoundMethod::Nearest => {
            if rem * 2 >= inc {
                down + inc
            } else {
                down
            }
        }
    }
}

/// Seconds of further counted time until `round_seconds` next gives a different result, or `None` when
/// rounding is off and every second counts.
#[must_use]
pub fn secs_to_next_step(counted: i64, increment_minutes: u32, method: RoundMethod) -> Option<i64> {
    if increment_minutes == 0 {
        return None;
    }
    let inc = i64::from(increment_minutes) * 60;
    let rem = counted.rem_euclid(inc);
    Some(match method {
        RoundMethod::Down => inc - rem,
        // The next increment is reached one second past a whole one.
        RoundMethod::Up if rem == 0 => 1,
        RoundMethod::Up => inc - rem + 1,
        RoundMethod::Nearest => {
            let half = (inc + 1) / 2;
            if rem < half {
                half - rem
            } else {
                inc - rem + half
            }
        }
    })
}

/// `round(numerator / 3600)` half up, in whole cents, with no floating point involved.
#[must_use]
pub fn cents_from_numerator(numerator: i64) -> i64 {
    numerator.saturating_add(1800).div_euclid(3600)
}

/// What a document prints where its rows don't add up to its total.
pub const ROUNDED_ROWS_NOTE: &str =
    "Row amounts are rounded; the total is worked out from the exact amounts.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoundingRule {
    pub increment_minutes: u32,
    pub method: RoundMethod,
}

/// The rule in force for a project on a day.
#[must_use]
pub fn rounding_rule(shared: &Shared, project_id: &str, day: NaiveDate) -> RoundingRule {
    let day_str = day.format("%Y-%m-%d").to_string();
    let pick = |project: Option<&str>| {
        shared
            .rounding
            .values()
            .filter(|r| {
                !r.deleted && r.project_id.as_deref() == project && r.effective_from <= day_str
            })
            .max_by(|a, b| {
                a.effective_from
                    .cmp(&b.effective_from)
                    .then_with(|| stamp_order(&a.stamp, &a.id, &b.stamp, &b.id))
            })
    };
    let chosen = pick(Some(project_id)).or_else(|| pick(None));
    chosen.map_or(
        RoundingRule {
            increment_minutes: DEFAULT_INCREMENT_MINUTES,
            method: RoundMethod::default(),
        },
        |r| RoundingRule {
            increment_minutes: r.increment_minutes,
            method: r.method,
        },
    )
}

fn rates_for<'a>(shared: &'a Shared, project_id: &str) -> Vec<&'a RateVersion> {
    // A fixed-fee project earns its fee, not time at a rate.
    let billable = shared
        .projects
        .get(project_id)
        .is_some_and(|p| p.billable && p.billing_type == crate::model::BillingType::Hourly);
    if !billable {
        return Vec::new();
    }
    let mut rates: Vec<&RateVersion> = shared
        .rates
        .values()
        .filter(|r| !r.deleted && r.project_id == project_id && is_currency(&r.currency))
        .collect();
    rates.sort_by(|a, b| {
        a.effective_from
            .cmp(&b.effective_from)
            .then_with(|| stamp_order(&a.stamp, &a.id, &b.stamp, &b.id))
    });
    rates
}

/// The first moment after `after` at which a new rate for this project takes effect.
#[must_use]
pub fn next_rate_change(shared: &Shared, project_id: &str, after: Ts) -> Option<Ts> {
    rates_for(shared, project_id)
        .iter()
        .map(|r| r.effective_from)
        .filter(|t| *t > after)
        .min()
}

/// A slice of a segment under a single rate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RatePiece {
    pub start: Ts,
    pub end: Ts,
    pub rate_cents: Option<i64>,
    pub currency: Option<String>,
}

/// Splits `[start, end)` at rate changes.
#[must_use]
pub fn split_by_rate(shared: &Shared, project_id: &str, start: Ts, end: Ts) -> Vec<RatePiece> {
    let rates = rates_for(shared, project_id);
    let in_force = |t: Ts| rates.iter().rev().find(|r| r.effective_from <= t);
    let mut out = Vec::new();
    let mut cur = start;
    loop {
        let version = in_force(cur);
        let next = rates
            .iter()
            .map(|r| r.effective_from)
            .filter(|t| *t > cur)
            .min();
        let piece_end = next.map_or(end, |n| n.min(end));
        out.push(RatePiece {
            start: cur,
            end: piece_end,
            rate_cents: version.and_then(|r| r.rate_cents),
            currency: version
                .filter(|r| r.rate_cents.is_some())
                .map(|r| r.currency.clone()),
        });
        if piece_end >= end {
            return out;
        }
        cur = piece_end;
    }
}

/// Time under one rate within a row. `rate_cents: None` is unpriced time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bucket {
    pub rate_cents: Option<i64>,
    pub currency: Option<String>,
    pub exact_secs: i64,
    pub billable_secs: i64,
}

/// Which of a row's seconds are not plain tracked time.
#[derive(Clone, Copy, Debug, Default)]
struct Extra {
    pause: i64,
    thinking: i64,
}

/// One task on one day in the reporting zone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskDay {
    pub task_id: String,
    pub project_id: String,
    pub day: NaiveDate,
    pub first_start: Ts,
    pub exact_secs: i64,
    /// Pauses marked as still working. Part of billable, never of exact.
    pub pause_secs: i64,
    /// Away time marked as thinking. Part of exact and billable, reported separately.
    pub thinking_secs: i64,
    pub billable_secs: i64,
    pub rule: RoundingRule,
    /// True when real time rounded down to nothing (Nearest or Down only). The row is kept.
    pub rounded_to_zero: bool,
    /// True when the row holds no exact time at all, such as an instant start and pause.
    pub zero_exact: bool,
    pub buckets: Vec<Bucket>,
}

impl TaskDay {
    /// Exact `cents * seconds` numerators per currency, from billable time.
    #[must_use]
    pub fn numerators(&self) -> BTreeMap<String, i64> {
        let mut out = BTreeMap::new();
        for b in &self.buckets {
            if let (Some(rate), Some(cur)) = (b.rate_cents, &b.currency) {
                let total = out.entry(cur.clone()).or_insert(0i64);
                *total = total.saturating_add(rate.saturating_mul(b.billable_secs));
            }
        }
        out
    }
}

/// Adds a row's exact numerators to a running total, per currency.
pub fn add_numerators(total: &mut BTreeMap<String, i64>, row: &TaskDay) {
    for (cur, n) in row.numerators() {
        let sum = total.entry(cur).or_insert(0);
        *sum = sum.saturating_add(n);
    }
}

/// Exact numerators as whole cents, per currency.
#[must_use]
pub fn cents_of(numerators: &BTreeMap<String, i64>) -> BTreeMap<String, i64> {
    numerators
        .iter()
        .map(|(cur, n)| (cur.clone(), cents_from_numerator(*n)))
        .collect()
}

/// The money these rows come to, per currency. Every total shown is this: their exact amounts added up,
/// and rounded once.
#[must_use]
pub fn total_cents<'a>(rows: impl IntoIterator<Item = &'a TaskDay>) -> BTreeMap<String, i64> {
    let mut numerators = BTreeMap::new();
    for row in rows {
        add_numerators(&mut numerators, row);
    }
    cents_of(&numerators)
}

/// Whether the amounts rows show add up to something other than their total, in any currency.
#[must_use]
pub fn rows_differ_from_total<'a>(
    rows: impl IntoIterator<Item = &'a BTreeMap<String, i64>>,
    totals: &BTreeMap<String, i64>,
) -> bool {
    let mut shown: BTreeMap<&str, i64> = BTreeMap::new();
    for row in rows {
        for (cur, cents) in row {
            let sum = shown.entry(cur).or_insert(0);
            *sum = sum.saturating_add(*cents);
        }
    }
    shown
        .into_iter()
        .any(|(cur, sum)| totals.get(cur).is_some_and(|t| *t != sum))
}

/// Moves the rounding difference onto the buckets, starting from the last rate in force.
fn allocate_rounding(buckets: &mut [Bucket], mut diff: i64) {
    for b in buckets.iter_mut().rev() {
        let applied = diff.max(-b.billable_secs);
        b.billable_secs += applied;
        diff -= applied;
        if diff == 0 {
            return;
        }
    }
}

/// Builds every task-day row. `end_of` decides the end of an open segment.
#[must_use]
pub fn task_days(shared: &Shared, tz: Tz, end_of: &dyn Fn(&Segment) -> Ts) -> Vec<TaskDay> {
    task_days_of(shared, tz, pieces(shared, end_of))
}

/// The task-day rows of these pieces, using the projects, rates and rounding in `shared`.
pub fn task_days_of(shared: &Shared, tz: Tz, pieces: Vec<Piece>) -> Vec<TaskDay> {
    task_days_billed(shared, tz, pieces, &|_, _, counted, rule| {
        round_seconds(counted, rule.increment_minutes, rule.method)
    })
}

/// As `task_days_of`, with `bill` giving each row's billable seconds from its task, its day, its counted
/// seconds and the rule in force. The difference from the counted seconds goes onto the buckets as usual.
pub(crate) fn task_days_billed(
    shared: &Shared,
    tz: Tz,
    pieces: Vec<Piece>,
    bill: &dyn Fn(&str, NaiveDate, i64, RoundingRule) -> i64,
) -> Vec<TaskDay> {
    #[derive(Default)]
    struct Acc {
        first: Option<Ts>,
        pieces: Vec<(RatePiece, bool)>,
        extra: Extra,
    }
    let mut acc: BTreeMap<(String, NaiveDate), Acc> = BTreeMap::new();
    let mut project_of: BTreeMap<&str, &str> = BTreeMap::new();
    for t in shared.tasks.values().filter(|t| t.is_live()) {
        project_of.insert(t.id.as_str(), t.project_id.as_str());
    }
    for piece in pieces {
        let Some(project_id) = project_of.get(piece.task_id.as_str()).copied() else {
            continue;
        };
        let is_pause = piece.kind == PieceKind::BillablePause;
        for (day, s, e) in split_by_day(piece.start, piece.end, tz) {
            let row = acc.entry((piece.task_id.clone(), day)).or_default();
            if !is_pause {
                row.first = Some(row.first.map_or(s, |f| f.min(s)));
            }
            let secs = e.secs() - s.secs();
            match piece.kind {
                PieceKind::BillablePause => row.extra.pause += secs,
                PieceKind::Thinking => row.extra.thinking += secs,
                PieceKind::Work => {}
            }
            row.pieces.extend(
                split_by_rate(shared, project_id, s, e)
                    .into_iter()
                    .map(|p| (p, is_pause)),
            );
        }
    }
    let mut rows = Vec::with_capacity(acc.len());
    for ((task_id, day), mut row) in acc {
        let Some(project_id) = project_of.get(task_id.as_str()).copied() else {
            continue;
        };
        row.pieces.sort_by_key(|(p, _)| p.start);
        let mut buckets: Vec<Bucket> = Vec::new();
        for (p, is_pause) in &row.pieces {
            let secs = p.end.secs() - p.start.secs();
            let exact = if *is_pause { 0 } else { secs };
            let same = buckets
                .last()
                .is_some_and(|b| b.rate_cents == p.rate_cents && b.currency == p.currency);
            match buckets.last_mut() {
                Some(b) if same => {
                    b.exact_secs += exact;
                    b.billable_secs += secs;
                }
                _ => buckets.push(Bucket {
                    rate_cents: p.rate_cents,
                    currency: p.currency.clone(),
                    exact_secs: exact,
                    billable_secs: secs,
                }),
            }
        }
        let exact_secs: i64 = buckets.iter().map(|b| b.exact_secs).sum();
        let rule = rounding_rule(shared, project_id, day);
        let counted = exact_secs + row.extra.pause;
        let billable_secs = bill(&task_id, day, counted, rule);
        allocate_rounding(&mut buckets, billable_secs - counted);
        rows.push(TaskDay {
            task_id,
            project_id: project_id.to_owned(),
            day,
            first_start: row.first.unwrap_or_default(),
            exact_secs,
            pause_secs: row.extra.pause,
            thinking_secs: row.extra.thinking,
            billable_secs,
            rule,
            rounded_to_zero: counted > 0 && billable_secs == 0,
            zero_exact: counted == 0,
            buckets,
        });
    }
    rows
}

#[must_use]
pub fn money(currency: &str, cents: i64) -> String {
    let symbol = if currency == "EUR" { "\u{20ac}" } else { "$" };
    let whole = cents.div_euclid(100);
    let frac = cents.rem_euclid(100);
    let digits = whole.to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!("{symbol}{grouped}.{frac:02}")
}

/// `weekday` as 1 = Monday .. 7 = Sunday, for the tests and reports.
#[must_use]
pub fn iso_weekday(d: NaiveDate) -> u32 {
    d.weekday().number_from_monday()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::machine::tests::{DEV, ctx, project_input, task_input};
    use crate::machine::{RateInput, RoundingInput};
    use crate::validate::parse_zone;

    pub fn fixture(rule: Option<(u32, RoundMethod)>) -> (Shared, String, String) {
        let mut s = Shared::default();
        let c = ctx("2026-01-01T00:00:00Z");
        let p = s.create_project(&c, &project_input("Alpha")).unwrap();
        let t = s.create_task(&c, &task_input("Write", &p)).unwrap();
        if let Some((m, method)) = rule {
            s.set_rounding(
                &c,
                &RoundingInput {
                    project_id: None,
                    effective_from: "2000-01-01".into(),
                    increment_minutes: m,
                    method,
                    confirm_retroactive: true,
                },
                "2026-01-01",
            )
            .unwrap();
        }
        (s, p, t)
    }

    pub fn seg(s: &mut Shared, task: &str, id: &str, a: &str, b: &str) {
        s.segments.insert(
            id.into(),
            Segment {
                id: id.into(),
                task_id: task.into(),
                device_id: DEV.into(),
                start: Ts::parse(a).unwrap(),
                end: Ts::parse(b),
                ..Segment::default()
            },
        );
    }

    fn rate(s: &mut Shared, p: &str, cents: Option<i64>, cur: &str, from: &str) {
        s.set_rate(
            &ctx("2026-06-01T00:00:00Z"),
            &RateInput {
                project_id: p.into(),
                rate_cents: cents,
                currency: cur.into(),
                effective_from: Some(from.into()),
                confirm_retroactive: true,
            },
        )
        .unwrap();
    }

    fn rows(s: &Shared, zone: &str) -> Vec<TaskDay> {
        task_days(s, parse_zone(zone).unwrap(), &|x: &Segment| {
            x.end.unwrap_or(x.start)
        })
    }

    #[test]
    fn rounding_methods_and_exact_halves() {
        use RoundMethod::{Down, Nearest, Up};
        for (exact, inc, method, want) in [
            (450, 15, Nearest, 900),
            (449, 15, Nearest, 0),
            (450, 15, Down, 0),
            (1, 15, Up, 900),
            (900, 15, Up, 900),
            (900, 15, Down, 900),
            (900, 15, Nearest, 900),
            (0, 15, Up, 0),
            (0, 15, Nearest, 0),
            (1234, 0, Nearest, 1234),
            (30, 1, Nearest, 60),
            (29, 1, Nearest, 0),
            (150, 5, Nearest, 300),
            (180, 6, Nearest, 360),
            (299, 10, Nearest, 0),
            (900, 30, Nearest, 1800),
            (1799, 60, Nearest, 0),
            (1800, 60, Nearest, 3600),
            (3700, 7, Down, 3360),
            (3700, 7, Up, 3780),
        ] {
            assert_eq!(
                round_seconds(exact, inc, method),
                want,
                "{exact} {inc} {method:?}"
            );
        }
    }

    #[test]
    fn row_rounds_once_so_short_pauses_do_not_inflate() {
        let (mut s, _, t) = fixture(Some((15, RoundMethod::Up)));
        // Three 1-minute segments: per-segment rounding would give 45 min, per-row gives 15.
        seg(
            &mut s,
            &t,
            "a",
            "2026-02-02T09:00:00Z",
            "2026-02-02T09:01:00Z",
        );
        seg(
            &mut s,
            &t,
            "b",
            "2026-02-02T10:00:00Z",
            "2026-02-02T10:01:00Z",
        );
        seg(
            &mut s,
            &t,
            "c",
            "2026-02-02T11:00:00Z",
            "2026-02-02T11:01:00Z",
        );
        let r = rows(&s, "UTC");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].exact_secs, 180);
        assert_eq!(r[0].billable_secs, 900);
    }

    #[test]
    fn multi_day_and_midnight_crossing_give_one_row_per_day() {
        let (mut s, _, t) = fixture(Some((0, RoundMethod::Nearest)));
        seg(
            &mut s,
            &t,
            "a",
            "2026-02-02T23:30:00Z",
            "2026-02-03T00:45:00Z",
        );
        seg(
            &mut s,
            &t,
            "b",
            "2026-02-05T10:00:00Z",
            "2026-02-05T11:00:00Z",
        );
        let r = rows(&s, "UTC");
        assert_eq!(
            r.iter()
                .map(|x| (x.day.to_string(), x.exact_secs))
                .collect::<Vec<_>>(),
            [
                ("2026-02-02".to_string(), 1800),
                ("2026-02-03".to_string(), 2700),
                ("2026-02-05".to_string(), 3600),
            ]
        );
    }

    #[test]
    fn exact_totals_are_zone_invariant_but_rounded_may_differ() {
        let (mut s, _, t) = fixture(Some((15, RoundMethod::Nearest)));
        // One 20-minute row in UTC, but two 10-minute rows in New York, where midnight falls inside it.
        seg(
            &mut s,
            &t,
            "a",
            "2026-02-02T04:50:00Z",
            "2026-02-02T05:10:00Z",
        );
        let utc = rows(&s, "UTC");
        let ny = rows(&s, "America/New_York"); // midnight NY = 05:00Z: splits the segment
        assert_eq!(
            utc.iter().map(|x| x.exact_secs).sum::<i64>(),
            ny.iter().map(|x| x.exact_secs).sum::<i64>()
        );
        assert_eq!(utc.iter().map(|x| x.billable_secs).sum::<i64>(), 900); // 20 min rounds to 15
        assert_eq!(ny.len(), 2);
        assert_eq!(ny.iter().map(|x| x.billable_secs).sum::<i64>(), 1800); // 10 + 10 each round to 15
    }

    #[test]
    fn sub_increment_row_is_kept_and_flagged() {
        let (mut s, _, t) = fixture(Some((15, RoundMethod::Nearest)));
        seg(
            &mut s,
            &t,
            "a",
            "2026-02-02T09:00:00Z",
            "2026-02-02T09:05:00Z",
        );
        let r = rows(&s, "UTC");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].billable_secs, 0);
        assert!(r[0].rounded_to_zero);
    }

    #[test]
    fn zero_exact_row_stays_at_zero_and_is_flagged() {
        let (mut s, _, t) = fixture(None);
        seg(
            &mut s,
            &t,
            "a",
            "2026-02-02T09:00:00Z",
            "2026-02-02T09:00:00Z",
        );
        let r = rows(&s, "UTC");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].billable_secs, 0);
        assert!(r[0].zero_exact);
        assert!(!r[0].rounded_to_zero);
    }

    #[test]
    fn default_rule_is_fifteen_minutes_up_applied_once_per_row() {
        let (mut s, _, t) = fixture(None);
        seg(
            &mut s,
            &t,
            "a",
            "2026-02-02T09:00:00Z",
            "2026-02-02T09:01:00Z",
        );
        seg(
            &mut s,
            &t,
            "b",
            "2026-02-03T09:00:00Z",
            "2026-02-03T09:15:00Z",
        );
        seg(
            &mut s,
            &t,
            "c",
            "2026-02-04T09:00:00Z",
            "2026-02-04T09:15:01Z",
        );
        let r = rows(&s, "UTC");
        assert_eq!(
            r[0].rule,
            RoundingRule {
                increment_minutes: 15,
                method: RoundMethod::Up
            }
        );
        assert_eq!(
            r.iter().map(|x| x.billable_secs).collect::<Vec<_>>(),
            [900, 900, 1800]
        );
    }

    #[test]
    fn sub_second_noise_stays_within_the_exact_multiple() {
        // Stored timestamps are whole seconds, so 15:00.001 is 15:00, not a whole extra increment.
        let (mut s, _, t) = fixture(None);
        seg(
            &mut s,
            &t,
            "a",
            "2026-02-02T09:00:00.000Z",
            "2026-02-02T09:15:00.001Z",
        );
        let r = rows(&s, "UTC");
        assert_eq!((r[0].exact_secs, r[0].billable_secs), (900, 900));
    }

    #[test]
    fn rounding_versions_apply_by_day_and_project_override_wins() {
        let (mut s, p, t) = fixture(Some((15, RoundMethod::Nearest)));
        let c = ctx("2026-06-01T00:00:00Z");
        s.set_rounding(
            &c,
            &RoundingInput {
                project_id: None,
                effective_from: "2026-03-01".into(),
                increment_minutes: 30,
                method: RoundMethod::Up,
                confirm_retroactive: true,
            },
            "2026-06-01",
        )
        .unwrap();
        seg(
            &mut s,
            &t,
            "a",
            "2026-02-28T09:00:00Z",
            "2026-02-28T09:20:00Z",
        );
        seg(
            &mut s,
            &t,
            "b",
            "2026-03-01T09:00:00Z",
            "2026-03-01T09:20:00Z",
        );
        let r = rows(&s, "UTC");
        assert_eq!(r[0].billable_secs, 900);
        assert_eq!(r[1].billable_secs, 1800);
        s.set_rounding(
            &c,
            &RoundingInput {
                project_id: Some(p),
                effective_from: "2026-03-01".into(),
                increment_minutes: 0,
                method: RoundMethod::Nearest,
                confirm_retroactive: true,
            },
            "2026-06-01",
        )
        .unwrap();
        let r = rows(&s, "UTC");
        assert_eq!(r[1].billable_secs, 1200);
    }

    #[test]
    fn backdated_changes_need_confirmation() {
        let (mut s, p, _) = fixture(None);
        let c = ctx("2026-06-01T00:00:00Z");
        assert!(
            s.set_rounding(
                &c,
                &RoundingInput {
                    project_id: None,
                    effective_from: "2026-05-01".into(),
                    increment_minutes: 5,
                    method: RoundMethod::Up,
                    confirm_retroactive: false
                },
                "2026-06-01"
            )
            .is_err()
        );
        assert!(
            s.set_rate(
                &c,
                &RateInput {
                    project_id: p,
                    rate_cents: Some(1),
                    currency: "EUR".into(),
                    effective_from: Some("2026-05-01T00:00:00Z".into()),
                    confirm_retroactive: false
                }
            )
            .is_err()
        );
    }

    #[test]
    fn same_date_versions_from_two_devices_resolve_to_the_newest_edit() {
        let (mut s, p, t) = fixture(Some((0, RoundMethod::Nearest)));
        seg(
            &mut s,
            &t,
            "a",
            "2026-02-02T10:00:00Z",
            "2026-02-02T11:00:00Z",
        );
        for (id, cents, at) in [
            ("zzz", 5000, "2026-01-02T00:00:00Z"),
            ("aaa", 7000, "2026-01-03T00:00:00Z"),
        ] {
            s.rates.insert(
                id.into(),
                RateVersion {
                    id: id.into(),
                    project_id: p.clone(),
                    effective_from: Ts::parse("2026-01-01T00:00:00Z").unwrap(),
                    rate_cents: Some(cents),
                    currency: "EUR".into(),
                    deleted: false,
                    stamp: crate::model::Stamp {
                        at: Ts::parse(at).unwrap(),
                        by: "x".into(),
                    },
                },
            );
        }
        assert_eq!(
            cents_from_numerator(rows(&s, "UTC")[0].numerators()["EUR"]),
            7000
        );
        // An edit updates the winner, and takes effect.
        rate(&mut s, &p, Some(9000), "EUR", "2026-01-01T00:00:00Z");
        assert_eq!(s.rates.len(), 2);
        assert_eq!(
            cents_from_numerator(rows(&s, "UTC")[0].numerators()["EUR"]),
            9000
        );
    }

    #[test]
    fn time_logged_on_a_draft_deleted_elsewhere_is_not_lost() {
        let (mut s, _, t) = fixture(Some((0, RoundMethod::Nearest)));
        seg(
            &mut s,
            &t,
            "a",
            "2026-02-02T10:00:00Z",
            "2026-02-02T11:00:00Z",
        );
        let task = s.tasks.get_mut(&t).unwrap();
        task.deleted = true;
        task.state = crate::model::TaskState::Paused;
        assert_eq!(rows(&s, "UTC")[0].exact_secs, 3600);
    }

    #[test]
    fn time_on_a_task_deleted_with_it_counts_nowhere() {
        let (mut s, _, t) = fixture(Some((0, RoundMethod::Nearest)));
        seg(
            &mut s,
            &t,
            "a",
            "2026-02-02T10:00:00Z",
            "2026-02-02T11:00:00Z",
        );
        assert_eq!(rows(&s, "UTC")[0].exact_secs, 3600);
        let task = s.tasks.get_mut(&t).unwrap();
        task.deleted = true;
        task.discarded = Some(task.deleted_stamp.clone());
        task.state = crate::model::TaskState::Paused;
        assert!(rows(&s, "UTC").is_empty());
    }

    #[test]
    fn rate_change_mid_segment_splits_at_the_instant() {
        let (mut s, p, t) = fixture(Some((0, RoundMethod::Nearest)));
        rate(&mut s, &p, Some(6000), "EUR", "2026-01-01T00:00:00Z");
        rate(&mut s, &p, Some(12000), "EUR", "2026-02-02T10:00:00Z");
        seg(
            &mut s,
            &t,
            "a",
            "2026-02-02T09:30:00Z",
            "2026-02-02T10:30:00Z",
        );
        let r = rows(&s, "UTC");
        assert_eq!(r[0].buckets.len(), 2);
        assert_eq!(cents_from_numerator(r[0].numerators()["EUR"]), 3000 + 6000);
    }

    #[test]
    fn rounding_difference_goes_to_the_last_rate_in_force() {
        let (mut s, p, t) = fixture(Some((15, RoundMethod::Up)));
        rate(&mut s, &p, Some(6000), "EUR", "2026-01-01T00:00:00Z");
        rate(&mut s, &p, Some(12000), "EUR", "2026-02-02T10:00:00Z");
        seg(
            &mut s,
            &t,
            "a",
            "2026-02-02T09:50:00Z",
            "2026-02-02T10:05:00Z",
        ); // 10 + 5, 15: no adjustment
        seg(
            &mut s,
            &t,
            "b",
            "2026-02-02T11:00:00Z",
            "2026-02-02T11:05:00Z",
        ); // now 20 total, up to 30
        let r = rows(&s, "UTC");
        assert_eq!(r[0].billable_secs, 1800);
        assert_eq!(r[0].buckets[0].billable_secs, 600);
        assert_eq!(r[0].buckets[1].billable_secs, 1200); // exact 600 + 600 adjustment
        assert_eq!(cents_from_numerator(r[0].numerators()["EUR"]), 1000 + 4000);
    }

    #[test]
    fn negative_rounding_carries_back_across_buckets() {
        let (mut s, p, t) = fixture(Some((30, RoundMethod::Down)));
        rate(&mut s, &p, Some(6000), "USD", "2026-01-01T00:00:00Z");
        rate(&mut s, &p, Some(12000), "USD", "2026-02-02T10:00:00Z");
        seg(
            &mut s,
            &t,
            "a",
            "2026-02-02T09:35:00Z",
            "2026-02-02T10:05:00Z",
        ); // 25 + 5 = 30: no change
        seg(
            &mut s,
            &t,
            "b",
            "2026-02-02T11:00:00Z",
            "2026-02-02T11:10:00Z",
        ); // 25 + 15 = 40: down to 30
        let r = rows(&s, "UTC");
        assert_eq!(r[0].billable_secs, 1800);
        assert_eq!(r[0].buckets[1].billable_secs, 300); // lost 10 of 15 minutes
        assert_eq!(r[0].buckets[0].billable_secs, 1500);
        let big = &mut r.clone()[0].buckets;
        big[1].billable_secs = 900;
        big[0].billable_secs = 1500;
        allocate_rounding(big, -1000);
        assert_eq!((big[0].billable_secs, big[1].billable_secs), (1400, 0));
    }

    #[test]
    fn currencies_are_never_mixed_and_blank_differs_from_zero() {
        let (mut s, p, t) = fixture(Some((0, RoundMethod::Nearest)));
        seg(
            &mut s,
            &t,
            "z",
            "2026-02-01T10:00:00Z",
            "2026-02-01T11:00:00Z",
        ); // before any rate
        rate(&mut s, &p, Some(0), "EUR", "2026-02-02T00:00:00Z");
        seg(
            &mut s,
            &t,
            "a",
            "2026-02-02T10:00:00Z",
            "2026-02-02T11:00:00Z",
        );
        rate(&mut s, &p, Some(5000), "USD", "2026-02-03T00:00:00Z");
        seg(
            &mut s,
            &t,
            "b",
            "2026-02-03T10:00:00Z",
            "2026-02-03T11:00:00Z",
        );
        rate(&mut s, &p, None, "USD", "2026-02-04T00:00:00Z");
        seg(
            &mut s,
            &t,
            "c",
            "2026-02-04T10:00:00Z",
            "2026-02-04T11:00:00Z",
        );
        let r = rows(&s, "UTC");
        assert!(r[0].numerators().is_empty());
        assert_eq!(r[1].numerators().get("EUR"), Some(&0));
        assert_eq!(r[2].numerators().get("USD"), Some(&(5000 * 3600)));
        assert!(!r[2].numerators().contains_key("EUR"));
        assert!(r[3].numerators().is_empty());
    }

    #[test]
    fn non_billable_project_and_unknown_currency_earn_nothing() {
        let (mut s, p, t) = fixture(Some((0, RoundMethod::Nearest)));
        rate(&mut s, &p, Some(6000), "EUR", "2026-01-01T00:00:00Z");
        seg(
            &mut s,
            &t,
            "a",
            "2026-02-02T10:00:00Z",
            "2026-02-02T11:00:00Z",
        );
        let id = s.rates.keys().next().unwrap().clone();
        s.rates.get_mut(&id).unwrap().currency = "GBP".into();
        assert!(rows(&s, "UTC")[0].numerators().is_empty());
        s.rates.get_mut(&id).unwrap().currency = "EUR".into();
        s.projects.get_mut(&p).unwrap().billable = false;
        assert!(rows(&s, "UTC")[0].numerators().is_empty());
        let bad = RateInput {
            project_id: p,
            rate_cents: Some(1),
            currency: "GBP".into(),
            effective_from: None,
            confirm_retroactive: false,
        };
        assert!(s.set_rate(&ctx("2026-06-01T00:00:00Z"), &bad).is_err());
    }

    #[test]
    fn a_total_is_the_exact_amounts_added_up_and_rounded_once() {
        let (mut s, p, t) = fixture(None);
        rate(&mut s, &p, Some(3333), "EUR", "2026-01-01T00:00:00Z");
        for day in 2..=5 {
            seg(
                &mut s,
                &t,
                &format!("s{day}"),
                &format!("2026-02-{day:02}T09:00:00Z"),
                &format!("2026-02-{day:02}T09:15:00Z"),
            );
        }
        let got = rows(&s, "UTC");
        let total = total_cents(&got);
        assert_eq!(total["EUR"], 3333, "4 x 833.25");
        let shown: Vec<BTreeMap<String, i64>> = got.iter().map(|r| total_cents([r])).collect();
        assert_eq!(shown[0]["EUR"], 833);
        assert!(rows_differ_from_total(&shown, &total));
        assert!(
            !rows_differ_from_total(&shown[..0], &total),
            "no rows, nothing to add up"
        );
        let other = BTreeMap::from([("USD".to_owned(), 1)]);
        assert!(
            !rows_differ_from_total(&shown, &other),
            "no total in that currency"
        );
        assert!(total_cents(&got[..0]).is_empty());
        assert_eq!(
            cents_from_numerator(i64::MAX),
            i64::MAX / 3600,
            "never overflows"
        );
    }

    #[test]
    fn half_cents_round_up_at_the_total_and_money_formats() {
        // 18 s at 100 cents/h = 0.5 cent exactly.
        assert_eq!(cents_from_numerator(100 * 18), 1);
        assert_eq!(cents_from_numerator(100 * 17), 0);
        // Two rows of 0.5 cents total 1 cent; rounding per row would give 2.
        assert_eq!(cents_from_numerator(100 * 18 + 100 * 18), 1);
        assert_eq!(money("EUR", 123_456_789), "\u{20ac}1,234,567.89");
        assert_eq!(money("USD", 5), "$0.05");
    }

    #[test]
    fn dst_days_split_on_the_real_midnight() {
        let ny = parse_zone("America/New_York").unwrap();
        // Spring forward day is 23 hours long.
        let d = NaiveDate::from_ymd_opt(2026, 3, 8).unwrap();
        let next = d.succ_opt().unwrap();
        assert_eq!(
            start_of_day(next, ny).secs() - start_of_day(d, ny).secs(),
            23 * 3600
        );
        let d = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
        assert_eq!(
            start_of_day(d.succ_opt().unwrap(), ny).secs() - start_of_day(d, ny).secs(),
            25 * 3600
        );
        // Havana: midnight itself is skipped on the spring change.
        let hav = parse_zone("America/Havana").unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 3, 8).unwrap();
        assert_eq!(start_of_day(d, hav).iso(), "2026-03-08T05:00:00Z");
    }

    /// Shared with `src/lib/parity.test.ts`, so Rust and TypeScript cannot drift apart.
    mod shared_fixtures {
        use super::*;
        use chrono::{Offset, TimeZone};
        use serde_json::Value;

        fn load(name: &str) -> Value {
            let path = format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
        }

        #[test]
        fn tz_conversions_days_and_splits() {
            let f = load("tz-fixtures.json");
            for c in f["conversions"].as_array().unwrap() {
                let tz = parse_zone(c["zone"].as_str().unwrap()).unwrap();
                let t = Ts::parse(c["at"].as_str().unwrap()).unwrap();
                let local = t.datetime().with_timezone(&tz);
                assert_eq!(
                    local.format("%Y-%m-%d").to_string(),
                    c["day"].as_str().unwrap(),
                    "{c}"
                );
                assert_eq!(
                    local.format("%H:%M").to_string(),
                    c["clock"].as_str().unwrap(),
                    "{c}"
                );
                let off = tz
                    .offset_from_utc_datetime(&t.datetime().naive_utc())
                    .fix()
                    .local_minus_utc()
                    / 60;
                assert_eq!(i64::from(off), c["offsetMin"].as_i64().unwrap(), "{c}");
            }
            for c in f["days"].as_array().unwrap() {
                let tz = parse_zone(c["zone"].as_str().unwrap()).unwrap();
                let d = NaiveDate::parse_from_str(c["day"].as_str().unwrap(), "%Y-%m-%d").unwrap();
                let start = start_of_day(d, tz);
                assert_eq!(start.iso(), c["start"].as_str().unwrap(), "{c}");
                let next = start_of_day(d.succ_opt().unwrap(), tz);
                assert_eq!(
                    next.secs() - start.secs(),
                    c["hours"].as_i64().unwrap() * 3600,
                    "{c}"
                );
            }
            for c in f["splits"].as_array().unwrap() {
                let tz = parse_zone(c["zone"].as_str().unwrap()).unwrap();
                let got: Vec<(String, i64)> = split_by_day(
                    Ts::parse(c["start"].as_str().unwrap()).unwrap(),
                    Ts::parse(c["end"].as_str().unwrap()).unwrap(),
                    tz,
                )
                .into_iter()
                .map(|(d, a, b)| (d.to_string(), b.secs() - a.secs()))
                .collect();
                let want: Vec<(String, i64)> = c["pieces"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|p| {
                        (
                            p["day"].as_str().unwrap().to_owned(),
                            p["secs"].as_i64().unwrap(),
                        )
                    })
                    .collect();
                assert_eq!(got, want, "{c}");
            }
        }

        #[test]
        fn billing_cases() {
            let f = load("billing-fixtures.json");
            for case in f["cases"].as_array().unwrap() {
                let name = case["name"].as_str().unwrap();
                let (mut s, p, t) = fixture(None);
                let c = ctx("2026-06-01T00:00:00Z");
                for r in case["rounding"].as_array().unwrap() {
                    let method = match r["method"].as_str().unwrap() {
                        "up" => RoundMethod::Up,
                        "down" => RoundMethod::Down,
                        _ => RoundMethod::Nearest,
                    };
                    s.set_rounding(
                        &c,
                        &RoundingInput {
                            project_id: None,
                            effective_from: r["from"].as_str().unwrap().into(),
                            increment_minutes: r["minutes"].as_u64().unwrap() as u32,
                            method,
                            confirm_retroactive: true,
                        },
                        "2026-06-01",
                    )
                    .unwrap();
                }
                for r in case["rates"].as_array().unwrap() {
                    rate(
                        &mut s,
                        &p,
                        r["cents"].as_i64(),
                        r["currency"].as_str().unwrap(),
                        r["from"].as_str().unwrap(),
                    );
                }
                if case["fixedFee"].as_bool().unwrap_or(false) {
                    s.projects.get_mut(&p).unwrap().billing_type =
                        crate::model::BillingType::FixedFee;
                }
                for (i, pair) in case["pauses"].as_array().into_iter().flatten().enumerate() {
                    let id = format!("pause{i}");
                    s.pauses.insert(
                        id.clone(),
                        crate::model::BillablePause {
                            id,
                            task_id: t.clone(),
                            start: Ts::parse(pair[0].as_str().unwrap()).unwrap(),
                            end: Ts::parse(pair[1].as_str().unwrap()).unwrap(),
                            note: String::new(),
                            deleted: false,
                            stamp: crate::model::Stamp::default(),
                        },
                    );
                }
                for (i, pair) in case["thinking"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    // A segment covering it, with the whole of it marked as thinking.
                    let sid = format!("t{i}");
                    seg(
                        &mut s,
                        &t,
                        &sid,
                        pair[0].as_str().unwrap(),
                        pair[1].as_str().unwrap(),
                    );
                    let gid = format!("g{i}");
                    s.gaps.insert(
                        gid.clone(),
                        crate::model::IdleGap {
                            id: gid,
                            segment_id: sid,
                            task_id: t.clone(),
                            device_id: DEV.into(),
                            start: Ts::parse(pair[0].as_str().unwrap()).unwrap(),
                            end: Ts::parse(pair[1].as_str().unwrap()).unwrap(),
                            kind: crate::model::GapKind::Idle,
                            decision: crate::model::GapDecision::Thinking,
                            decision_stamp: crate::model::Stamp::default(),
                        },
                    );
                }
                for (i, seg_pair) in case["segments"].as_array().unwrap().iter().enumerate() {
                    seg(
                        &mut s,
                        &t,
                        &format!("s{i}"),
                        seg_pair[0].as_str().unwrap(),
                        seg_pair[1].as_str().unwrap(),
                    );
                }
                let got = rows(&s, case["zone"].as_str().unwrap());
                let want = case["rows"].as_array().unwrap();
                assert_eq!(got.len(), want.len(), "{name}");
                if let Some(totals) = case.get("totals") {
                    let totals: BTreeMap<String, i64> = totals
                        .as_object()
                        .unwrap()
                        .iter()
                        .map(|(k, v)| (k.clone(), v.as_i64().unwrap()))
                        .collect();
                    assert_eq!(total_cents(&got), totals, "{name} totals");
                }
                for (g, w) in got.iter().zip(want) {
                    assert_eq!(g.day.to_string(), w["day"].as_str().unwrap(), "{name}");
                    assert_eq!(g.exact_secs, w["exact"].as_i64().unwrap(), "{name}");
                    assert_eq!(g.pause_secs, w["pause"].as_i64().unwrap_or(0), "{name}");
                    assert_eq!(g.billable_secs, w["billable"].as_i64().unwrap(), "{name}");
                    assert_eq!(
                        g.rounded_to_zero,
                        w["roundedToZero"].as_bool().unwrap_or(false),
                        "{name}"
                    );
                    let amounts: BTreeMap<String, i64> = g
                        .numerators()
                        .iter()
                        .map(|(k, n)| (k.clone(), cents_from_numerator(*n)))
                        .collect();
                    let want_amounts: BTreeMap<String, i64> = w["amounts"]
                        .as_object()
                        .unwrap()
                        .iter()
                        .map(|(k, v)| (k.clone(), v.as_i64().unwrap()))
                        .collect();
                    assert_eq!(amounts, want_amounts, "{name}");
                    let mut exact: BTreeMap<String, i64> = BTreeMap::new();
                    for b in &g.buckets {
                        if let (Some(r), Some(cur)) = (b.rate_cents, &b.currency) {
                            *exact.entry(cur.clone()).or_insert(0) += r * b.exact_secs;
                        }
                    }
                    let exact: BTreeMap<String, i64> = exact
                        .iter()
                        .map(|(k, n)| (k.clone(), cents_from_numerator(*n)))
                        .collect();
                    let want_exact: BTreeMap<String, i64> = w["exactAmounts"]
                        .as_object()
                        .unwrap()
                        .iter()
                        .map(|(k, v)| (k.clone(), v.as_i64().unwrap()))
                        .collect();
                    assert_eq!(exact, want_exact, "{name} exact");
                }
            }
        }

        /// The same steps run in `src/mocks/summaryRounding.test.ts`.
        #[test]
        fn summary_cases() {
            let f = load("billing-fixtures.json");
            let cents = |v: &Value| -> BTreeMap<String, i64> {
                v.as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, n)| (k.clone(), n.as_i64().unwrap()))
                    .collect()
            };
            for case in f["summaries"].as_array().unwrap() {
                let name = case["name"].as_str().unwrap();
                let (mut s, p, t) = fixture(None);
                let c = ctx("2026-06-01T00:00:00Z");
                for r in case["rates"].as_array().unwrap() {
                    rate(
                        &mut s,
                        &p,
                        r["cents"].as_i64(),
                        r["currency"].as_str().unwrap(),
                        r["from"].as_str().unwrap(),
                    );
                }
                let mut made: Vec<String> = Vec::new();
                for (i, step) in case["steps"].as_array().unwrap().iter().enumerate() {
                    if let Some(r) = step.get("rule") {
                        let method = match r["method"].as_str().unwrap() {
                            "up" => RoundMethod::Up,
                            "down" => RoundMethod::Down,
                            _ => RoundMethod::Nearest,
                        };
                        s.set_rounding(
                            &c,
                            &RoundingInput {
                                project_id: None,
                                effective_from: r["from"].as_str().unwrap().into(),
                                increment_minutes: r["minutes"].as_u64().unwrap() as u32,
                                method,
                                confirm_retroactive: true,
                            },
                            "2026-06-01",
                        )
                        .unwrap();
                    }
                    for fix in step["corrected"].as_array().into_iter().flatten() {
                        s.segments.get_mut(fix[0].as_str().unwrap()).unwrap().edit =
                            Some(crate::model::SegmentEdit {
                                start: Ts::parse(fix[1].as_str().unwrap()).unwrap(),
                                end: Ts::parse(fix[2].as_str().unwrap()).unwrap(),
                                voided: false,
                                reason: "corrected on another device".into(),
                                settled: false,
                            });
                    }
                    for (n, pair) in step["pauses"].as_array().into_iter().flatten().enumerate() {
                        let id = format!("p{i}-{n}");
                        s.pauses.insert(
                            id.clone(),
                            crate::model::BillablePause {
                                id,
                                task_id: t.clone(),
                                start: Ts::parse(pair[0].as_str().unwrap()).unwrap(),
                                end: Ts::parse(pair[1].as_str().unwrap()).unwrap(),
                                note: String::new(),
                                deleted: false,
                                stamp: crate::model::Stamp::default(),
                            },
                        );
                    }
                    for (n, pair) in step["segments"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .enumerate()
                    {
                        seg(
                            &mut s,
                            &t,
                            &format!("s{i}-{n}"),
                            pair[0].as_str().unwrap(),
                            pair[1].as_str().unwrap(),
                        );
                    }
                    for v in step["void"].as_array().into_iter().flatten() {
                        let id = made[v.as_u64().unwrap() as usize].clone();
                        s.void_summary(&c, &id).unwrap();
                    }
                    let request = crate::summary::SummaryRequest {
                        project_id: p.clone(),
                        from: step["from"].as_str().unwrap_or("2026-01-01").into(),
                        to: step["to"].as_str().unwrap_or("2026-12-31").into(),
                        zone: step["zone"].as_str().unwrap_or("UTC").into(),
                    };
                    let preview = s.summary_preview(&request).unwrap();
                    let want = step["rows"].as_array().unwrap();
                    assert_eq!(preview.rows.len(), want.len(), "{name} step {i}");
                    for (g, w) in preview.rows.iter().zip(want) {
                        assert_eq!(g.day, w["day"].as_str().unwrap(), "{name} step {i}");
                        assert_eq!(
                            g.exact_secs,
                            w["exact"].as_i64().unwrap(),
                            "{name} step {i}"
                        );
                        assert_eq!(
                            g.billable_secs,
                            w["billable"].as_i64().unwrap(),
                            "{name} step {i}"
                        );
                        assert_eq!(
                            g.pause_secs,
                            w["pause"].as_i64().unwrap_or(0),
                            "{name} step {i}"
                        );
                        assert_eq!(g.cents, cents(&w["amounts"]), "{name} step {i}");
                    }
                    assert_eq!(preview.totals, cents(&step["totals"]), "{name} step {i}");
                    assert_eq!(
                        preview.partly_billed_rows as u64,
                        step["partlyBilled"].as_u64().unwrap(),
                        "{name} step {i}"
                    );
                    if step["create"].as_bool().unwrap() {
                        let id = s.create_summary(&c, &request).unwrap();
                        if let Some(older) = step.get("asOlderBuild") {
                            let b = s.summaries.get_mut(&id).unwrap();
                            b.rows[0].billable_secs = older["billable"].as_i64().unwrap();
                            b.rows[0].cents = cents(&older["amounts"]);
                            b.totals = cents(&older["amounts"]);
                        }
                        made.push(id);
                    }
                }
                let billed: i64 = s
                    .summaries
                    .values()
                    .filter(|b| !b.voided)
                    .flat_map(|b| b.rows.iter().map(|r| r.billable_secs))
                    .sum();
                let report: i64 = rows(&s, case["reportZone"].as_str().unwrap_or("UTC"))
                    .iter()
                    .map(|r| r.billable_secs)
                    .sum();
                assert_eq!(billed, case["billed"].as_i64().unwrap(), "{name} billed");
                assert_eq!(report, case["report"].as_i64().unwrap(), "{name} report");
            }
        }
    }

    #[test]
    fn the_next_step_is_exactly_where_rounding_next_changes() {
        for method in [RoundMethod::Up, RoundMethod::Down, RoundMethod::Nearest] {
            for minutes in [1u32, 5, 6, 7, 15] {
                let inc = i64::from(minutes) * 60;
                for counted in 0..(2 * inc + 3) {
                    let now = round_seconds(counted, minutes, method);
                    let expected = (1..=inc + 1)
                        .find(|s| round_seconds(counted + s, minutes, method) != now)
                        .unwrap();
                    assert_eq!(
                        secs_to_next_step(counted, minutes, method),
                        Some(expected),
                        "{minutes} min {method:?} from {counted} s"
                    );
                }
            }
        }
        assert_eq!(secs_to_next_step(42, 0, RoundMethod::Up), None);
    }
}
