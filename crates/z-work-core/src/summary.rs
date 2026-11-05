//! Billing summaries for the user's own guidance, budget status and fixed-fee figures. A summary is
//! not an invoice and has no tax fields. It freezes what it listed and marks that time as billed.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::billing::{
    ROUNDED_ROWS_NOTE, add_numerators, cents_of, day_of, round_seconds, rows_differ_from_total,
    split_by_day, task_days, task_days_billed, task_days_of, total_cents,
};
use crate::error::CoreError;
use crate::machine::Ctx;
use crate::model::{
    BillingSummary, BillingType, DevicePrefix, GapDecision, Segment, Shared, SummaryRow, Task,
};
use crate::pieces::{Piece, PieceKind, pieces, pieces_for_summary};
use crate::time::Ts;
use crate::validate::{check_date, parse_zone};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryRequest {
    pub project_id: String,
    /// Inclusive days, `YYYY-MM-DD`, in `zone`.
    pub from: String,
    pub to: String,
    pub zone: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryPreview {
    pub rows: Vec<SummaryRow>,
    pub totals: BTreeMap<String, i64>,
    pub segment_ids: Vec<String>,
    pub pause_ids: Vec<String>,
    pub exact_secs: i64,
    pub pause_secs: i64,
    pub billable_secs: i64,
    /// Time in the period that is still running, so it isn't listed yet.
    pub running_excluded: bool,
    /// Time away inside these segments with no decision yet. It is left out, and a summary freezes that.
    pub unconfirmed_secs: i64,
    /// Rows that bill a difference: their task-day is partly on a summary already, so they don't bill what
    /// their own time rounds to.
    pub partly_billed_rows: usize,
}

fn view_has(ids: &[String], id: &str) -> bool {
    ids.iter().any(|x| x == id)
}

fn parse_day(raw: &str, field: &str) -> Result<NaiveDate, CoreError> {
    check_date(raw, field)?;
    NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .map_err(|_| CoreError::Validation(format!("{field} is not a date")))
}

impl Shared {
    fn billed_sets(&self) -> (BTreeSet<&str>, BTreeSet<&str>) {
        let mut segs = BTreeSet::new();
        let mut pauses = BTreeSet::new();
        for b in self.summaries.values().filter(|b| !b.voided) {
            segs.extend(b.segment_ids.iter().map(String::as_str));
            pauses.extend(b.pause_ids.iter().map(String::as_str));
        }
        (segs, pauses)
    }

    /// The segment ids on a live summary.
    #[must_use]
    pub fn billed_segments(&self) -> BTreeSet<&str> {
        self.billed_sets().0
    }

    /// The references of the live summaries listing any of this task's time, in order.
    pub(crate) fn summaries_listing(&self, task_id: &str) -> Vec<&str> {
        let own_segment = |id: &String| self.segments.get(id).is_some_and(|s| s.task_id == task_id);
        let own_pause = |id: &String| self.pauses.get(id).is_some_and(|p| p.task_id == task_id);
        let mut references: Vec<&str> = self
            .summaries
            .values()
            .filter(|b| !b.voided)
            .filter(|b| {
                b.rows.iter().any(|r| r.task_id == task_id)
                    || b.segment_ids.iter().any(own_segment)
                    || b.pause_ids.iter().any(own_pause)
            })
            .map(|b| b.reference.as_str())
            .collect();
        references.sort_unstable();
        references.dedup();
        references
    }

    /// Unbilled, closed time of a project in a period. Time belongs to the day its segment started.
    pub fn summary_preview(&self, req: &SummaryRequest) -> Result<SummaryPreview, CoreError> {
        let tz: Tz = parse_zone(&req.zone)?;
        let from = parse_day(&req.from, "the start date")?;
        let to = parse_day(&req.to, "the end date")?;
        if to < from {
            return Err(CoreError::Validation(
                "the end date must not be before the start date".into(),
            ));
        }
        let project = self
            .projects
            .get(&req.project_id)
            .ok_or(CoreError::NotFound("project"))?;
        let in_period = |t: Ts| {
            let d = day_of(t, tz);
            d >= from && d <= to
        };
        let (billed_segs, billed_pauses) = self.billed_sets();
        let mut view = Shared {
            projects: BTreeMap::from([(project.id.clone(), project.clone())]),
            rates: self.rates.clone(),
            rounding: self.rounding.clone(),
            ..Shared::default()
        };
        view.tasks = self
            .tasks
            .iter()
            .filter(|(_, t)| t.project_id == req.project_id && t.is_live())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut out = SummaryPreview::default();
        for s in self.segments.values() {
            let Some((start, end)) = s.effective() else {
                continue;
            };
            if !view.tasks.contains_key(&s.task_id) || !in_period(start) {
                continue;
            }
            if end.is_none() {
                out.running_excluded = true;
                continue;
            }
            if !billed_segs.contains(s.id.as_str()) {
                out.segment_ids.push(s.id.clone());
            }
        }
        for p in self.pauses.values().filter(|p| !p.deleted) {
            if view.tasks.contains_key(&p.task_id)
                && in_period(p.start)
                && !billed_pauses.contains(p.id.as_str())
            {
                out.pause_ids.push(p.id.clone());
            }
        }
        out.unconfirmed_secs = self
            .gaps
            .values()
            .filter(|g| {
                g.decision == GapDecision::Pending && view_has(&out.segment_ids, &g.segment_id)
            })
            .map(|g| (g.end.secs() - g.start.secs()).max(0))
            .fold(0, i64::saturating_add);
        // Pieces come from the whole store, so a pause never bills what an already billed segment
        // covers, and are then narrowed to what this summary lists.
        let open_end = |s: &Segment| s.end.unwrap_or(s.start);
        let all = pieces_for_summary(self, &open_end, &out.pause_ids);
        let held = self.held_by_task_day(&view.tasks, tz, &all);
        let (listed, rest): (Vec<Piece>, Vec<Piece>) = all.into_iter().partition(|p| {
            let ids = if p.kind == PieceKind::BillablePause {
                &out.pause_ids
            } else {
                &out.segment_ids
            };
            view_has(ids, &p.source_id)
        });
        // A task-day rounds once, as a whole: what this summary lists and what live summaries hold. A row
        // bills what that comes to less what they already bill for it, and nothing issued goes back.
        let mut whole_day = listed.clone();
        whole_day.extend(rest.into_iter().filter(|p| {
            if p.kind == PieceKind::BillablePause {
                billed_pauses.contains(p.source_id.as_str())
            } else {
                billed_segs.contains(p.source_id.as_str())
            }
        }));
        let whole: BTreeMap<(String, NaiveDate), i64> = task_days_of(&view, tz, whole_day)
            .into_iter()
            .map(|td| ((td.task_id, td.day), td.billable_secs))
            .collect();
        let rows = task_days_billed(&view, tz, listed, &|task, day, counted, _| {
            if counted == 0 {
                return 0;
            }
            let key = (task.to_owned(), day);
            let day_total = whole.get(&key).copied().unwrap_or(counted);
            (day_total - held.get(&key).copied().unwrap_or(0)).max(0)
        });
        out.totals = total_cents(&rows);
        for td in &rows {
            let Some(task) = view.tasks.get(&td.task_id) else {
                continue;
            };
            out.exact_secs += td.exact_secs;
            out.pause_secs += td.pause_secs;
            out.billable_secs += td.billable_secs;
            let counted = td.exact_secs + td.pause_secs;
            if td.billable_secs != round_seconds(counted, td.rule.increment_minutes, td.rule.method)
            {
                out.partly_billed_rows += 1;
            }
            out.rows.push(SummaryRow {
                task_id: td.task_id.clone(),
                task_name: task.name.clone(),
                day: td.day.format("%Y-%m-%d").to_string(),
                exact_secs: td.exact_secs,
                pause_secs: td.pause_secs,
                billable_secs: td.billable_secs,
                cents: total_cents([td]),
            });
        }
        Ok(out)
    }

    /// What live summaries already bill for each task-day, in `tz`: the rows they froze, never worked out again,
    /// under whatever rule and by whichever version they were made. A row's amount goes where its time lies now:
    /// over this zone's days for a summary made in another zone, and along with its time when a correction from a
    /// device that hadn't seen the summary moved it to another day.
    fn held_by_task_day(
        &self,
        tasks: &BTreeMap<String, Task>,
        tz: Tz,
        all: &[Piece],
    ) -> BTreeMap<(String, NaiveDate), i64> {
        let mut pieces_of: BTreeMap<&str, Vec<&Piece>> = BTreeMap::new();
        for p in all {
            pieces_of.entry(p.source_id.as_str()).or_default().push(p);
        }
        let mut held = BTreeMap::new();
        for b in self.summaries.values().filter(|b| !b.voided) {
            let theirs = parse_zone(&b.zone).unwrap_or(tz);
            let ids: BTreeSet<&str> = b
                .segment_ids
                .iter()
                .chain(&b.pause_ids)
                .map(String::as_str)
                .collect();
            // Per task and day of the summary's zone, the seconds it lists now, on each day of this one.
            let mut lies: BTreeMap<&str, DaysIn> = BTreeMap::new();
            for p in ids.into_iter().filter_map(|id| pieces_of.get(id)).flatten() {
                for (their_day, s, e) in split_by_day(p.start, p.end, theirs) {
                    let here = lies
                        .entry(p.task_id.as_str())
                        .or_default()
                        .entry(their_day)
                        .or_default();
                    for (day, a, z) in split_by_day(s, e, tz) {
                        if z > a {
                            *here.entry(day).or_insert(0) += z.secs() - a.secs();
                        }
                    }
                }
            }
            let mut rows: BTreeMap<&str, Vec<(&SummaryRow, NaiveDate)>> = BTreeMap::new();
            for r in b.rows.iter().filter(|r| tasks.contains_key(&r.task_id)) {
                if let Ok(day) = NaiveDate::parse_from_str(&r.day, "%Y-%m-%d") {
                    rows.entry(r.task_id.as_str()).or_default().push((r, day));
                }
            }
            for (task, rows) in rows {
                hold_rows(task, &rows, lies.get(task), &mut held);
            }
        }
        held
    }

    /// This device's prefix, or the first letter no other device uses.
    #[must_use]
    pub fn prefix_of(&self, device: &str) -> String {
        if let Some(p) = self.prefixes.get(device) {
            return p.prefix.clone();
        }
        let used: BTreeSet<&str> = self.prefixes.values().map(|p| p.prefix.as_str()).collect();
        ('A'..='Z')
            .map(String::from)
            .find(|p| !used.contains(p.as_str()))
            .unwrap_or_else(|| "PC".to_owned())
    }

    /// Other devices using the same prefix as this one. Their numbers could collide.
    #[must_use]
    pub fn prefix_clashes(&self, device: &str) -> Vec<String> {
        let mine = self.prefix_of(device);
        self.prefixes
            .iter()
            .filter(|(id, p)| id.as_str() != device && p.prefix == mine)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Raises each prefix's mark to the highest number on any summary here, voided ones included.
    pub fn raise_reference_marks(&self, marks: &mut BTreeMap<String, u32>) {
        for b in self.summaries.values() {
            let mark = marks.entry(b.prefix.clone()).or_insert(0);
            *mark = (*mark).max(b.number);
        }
    }

    /// References on more than one live summary, which offline devices can produce.
    #[must_use]
    pub fn reference_clashes(&self) -> Vec<String> {
        let mut seen: BTreeMap<&str, u32> = BTreeMap::new();
        for b in self.summaries.values().filter(|b| !b.voided) {
            *seen.entry(b.reference.as_str()).or_insert(0) += 1;
        }
        seen.into_iter()
            .filter(|(_, n)| *n > 1)
            .map(|(r, _)| r.to_owned())
            .collect()
    }

    /// The references of live summaries that hold the same segment or pause, one list per set of summaries.
    /// Two devices can each bill a period before their files cross, and both summaries then stand.
    #[must_use]
    pub fn billed_twice(&self) -> Vec<Vec<&str>> {
        let mut holders: BTreeMap<&str, BTreeSet<(&str, &str)>> = BTreeMap::new();
        for b in self.summaries.values().filter(|b| !b.voided) {
            for id in b.segment_ids.iter().chain(&b.pause_ids) {
                holders
                    .entry(id.as_str())
                    .or_default()
                    .insert((b.reference.as_str(), b.id.as_str()));
            }
        }
        let sets: BTreeSet<Vec<&str>> = holders
            .into_values()
            .filter(|h| h.len() > 1)
            .map(|h| h.into_iter().map(|(reference, _)| reference).collect())
            .collect();
        sets.into_iter().collect()
    }

    /// Seconds of pause time each live summary holds and its pauses no longer count for, by summary id. Work
    /// that arrives over a billed pause takes its time, and so does a pause billed elsewhere before the files
    /// crossed, while the summary keeps what it froze. That time is then counted again somewhere else, under
    /// another id, which `billed_twice` can't see.
    ///
    /// Only a fall counts: a pause that held less all along, because work already covered part of it, has lost
    /// nothing. And only a fall across a task's summaries together: with one dated before another it saw, time
    /// moves between them live, but each second is still on one.
    #[must_use]
    pub fn pause_time_short(&self) -> BTreeMap<&str, i64> {
        let holding: Vec<_> = self
            .summaries
            .values()
            .filter(|b| !b.voided && !b.pause_ids.is_empty())
            .collect();
        let mut short = BTreeMap::new();
        if holding.is_empty() {
            return short;
        }
        let open_end = |s: &Segment| s.end.unwrap_or(s.start);
        let mut counted: BTreeMap<String, i64> = BTreeMap::new();
        for p in pieces(self, &open_end) {
            if p.kind == PieceKind::BillablePause {
                *counted.entry(p.source_id.clone()).or_insert(0) += p.secs();
            }
        }
        // Per task and summary: what it froze, and what its pauses count for now.
        let mut by_task: BTreeMap<&str, BTreeMap<&str, (i64, i64)>> = BTreeMap::new();
        for b in holding {
            for r in &b.rows {
                if self.tasks.get(&r.task_id).is_some_and(|t| t.is_live()) {
                    let held = by_task.entry(r.task_id.as_str()).or_default();
                    held.entry(b.id.as_str()).or_default().0 += r.pause_secs;
                }
            }
            for p in b.pause_ids.iter().filter_map(|id| self.pauses.get(id)) {
                let held = by_task.entry(p.task_id.as_str()).or_default();
                held.entry(b.id.as_str()).or_default().1 +=
                    counted.get(&p.id).copied().unwrap_or(0);
            }
        }
        for held in by_task.values() {
            let frozen: i64 = held.values().map(|h| h.0).sum();
            let still: i64 = held.values().map(|h| h.1).sum();
            if frozen > still {
                for (id, (froze, holds)) in held {
                    if froze > holds {
                        *short.entry(*id).or_insert(0) += froze - holds;
                    }
                }
            }
        }
        short
    }

    pub fn set_device_prefix(&mut self, ctx: &Ctx<'_>, raw: &str) -> Result<(), CoreError> {
        let prefix = raw.trim().to_uppercase();
        let ok =
            (1..=4).contains(&prefix.len()) && prefix.bytes().all(|b| b.is_ascii_alphanumeric());
        if !ok {
            return Err(CoreError::Validation(
                "a prefix is 1 to 4 letters or digits".into(),
            ));
        }
        let stamp = self
            .prefixes
            .get(ctx.device)
            .map_or_else(|| ctx.stamp(), |p| ctx.next(&p.stamp));
        self.prefixes
            .insert(ctx.device.to_owned(), DevicePrefix { prefix, stamp });
        Ok(())
    }

    /// Freezes the preview into a summary with this device's next number, and marks the time billed.
    pub fn create_summary(
        &mut self,
        ctx: &Ctx<'_>,
        req: &SummaryRequest,
    ) -> Result<String, CoreError> {
        self.create_summary_above(ctx, req, &BTreeMap::new())
    }

    /// As `create_summary`, with a number above `marks` too: numbers issued before a restore.
    pub fn create_summary_above(
        &mut self,
        ctx: &Ctx<'_>,
        req: &SummaryRequest,
        marks: &BTreeMap<String, u32>,
    ) -> Result<String, CoreError> {
        let preview = self.summary_preview(req)?;
        if preview.rows.is_empty() {
            return Err(CoreError::Validation(
                "there is no unbilled time in that period".into(),
            ));
        }
        let prefix = self.prefix_of(ctx.device);
        if !self.prefixes.contains_key(ctx.device) {
            self.set_device_prefix(ctx, &prefix)?;
        }
        // Across all devices: a prefix can change hands, and the number must not repeat under it.
        let number = self
            .summaries
            .values()
            .filter(|b| b.prefix == prefix)
            .map(|b| b.number)
            .max()
            .unwrap_or(0)
            .max(marks.get(&prefix).copied().unwrap_or(0))
            .checked_add(1)
            .ok_or_else(|| CoreError::Validation("this prefix has run out of numbers".into()))?;
        let id = Uuid::new_v4().to_string();
        let reference = format!("{prefix}-{number:03}");
        self.summaries.insert(
            id.clone(),
            BillingSummary {
                id: id.clone(),
                reference: reference.clone(),
                prefix,
                number,
                device_id: ctx.device.to_owned(),
                project_id: req.project_id.clone(),
                from: req.from.clone(),
                to: req.to.clone(),
                zone: req.zone.clone(),
                created_at: ctx.now,
                segment_ids: preview.segment_ids,
                pause_ids: preview.pause_ids,
                rows: preview.rows,
                totals: preview.totals,
                voided: false,
                stamp: ctx.stamp(),
            },
        );
        self.audit(
            ctx,
            "summary",
            &id,
            "create",
            "",
            Value::Null,
            json!({"reference": reference}),
        );
        Ok(id)
    }

    /// Puts the time back to unbilled. The reference number is not reused.
    pub fn void_summary(&mut self, ctx: &Ctx<'_>, id: &str) -> Result<(), CoreError> {
        let b = self
            .summaries
            .get_mut(id)
            .ok_or(CoreError::NotFound("billing summary"))?;
        if b.voided {
            return Ok(());
        }
        b.voided = true;
        b.stamp = ctx.next(&b.stamp);
        let reference = b.reference.clone();
        self.audit(
            ctx,
            "summary",
            id,
            "void",
            "",
            json!({"reference": reference}),
            Value::Null,
        );
        Ok(())
    }
}

/// By day of a summary's zone, the seconds on each day of the zone being worked in. None of them is zero.
type DaysIn = BTreeMap<NaiveDate, BTreeMap<NaiveDate, i64>>;

/// Puts one summary's rows for one task on the days their time lies on now. Time that moved takes its share of a
/// row's amount to where it was brought, past what the summary froze for that day. Time gone and brought nowhere was
/// cut, so its share stays with the row: over what is left of its time, or on the date it names.
fn hold_rows(
    task: &str,
    rows: &[(&SummaryRow, NaiveDate)],
    lies: Option<&DaysIn>,
    held: &mut BTreeMap<(String, NaiveDate), i64>,
) {
    let counted = |r: &SummaryRow| r.exact_secs + r.pause_secs;
    let froze: BTreeMap<NaiveDate, i64> = rows.iter().map(|(r, day)| (*day, counted(r))).collect();
    let mut brought: BTreeMap<NaiveDate, i64> = BTreeMap::new();
    for (their_day, here) in lies.into_iter().flatten() {
        let mut over = here.values().sum::<i64>() - froze.get(their_day).copied().unwrap_or(0);
        for (day, secs) in here {
            let part = over.clamp(0, *secs);
            if part > 0 {
                *brought.entry(*day).or_insert(0) += part;
            }
            over -= part;
        }
    }
    // What is left goes over the time that is left, so the shares add up to the amount.
    let mut share_out = |amount: i64, days: &BTreeMap<NaiveDate, i64>| {
        let mut left = amount;
        let mut time: i64 = days.values().sum();
        for (day, secs) in days {
            let share = left.saturating_mul(*secs) / time;
            *held.entry((task.to_owned(), *day)).or_insert(0) += share;
            left -= share;
            time -= secs;
        }
    };
    let lost = |r: &SummaryRow, day: &NaiveDate| -> i64 {
        let time: i64 = lies
            .and_then(|l| l.get(day))
            .into_iter()
            .flat_map(|h| h.values())
            .sum();
        (counted(r) - time).max(0)
    };
    let gone: i64 = rows.iter().map(|(r, day)| lost(r, day)).sum();
    // No more can have moved than was brought. Which row it left can't be told, so each gives up its part.
    let came = brought.values().sum::<i64>().min(gone);
    let mut moved = 0;
    for (r, day) in rows {
        let here = lies.and_then(|l| l.get(day)).filter(|h| !h.is_empty());
        let stays = if came == 0 || counted(r) == 0 {
            r.billable_secs
        } else {
            let leaves = lost(r, day).saturating_mul(came) / gone;
            r.billable_secs.saturating_mul(counted(r) - leaves) / counted(r)
        };
        moved += r.billable_secs - stays;
        match here {
            Some(here) => share_out(stays, here),
            None => share_out(stays, &BTreeMap::from([(*day, 1)])),
        }
    }
    share_out(moved, &brought);
}

/// Per project, from the task-day rows of the given zone.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFigures {
    pub exact_secs: i64,
    pub pause_secs: i64,
    pub billable_secs: i64,
    pub thinking_secs: i64,
    /// Fixed-fee projects: fee divided by billable hours, in cents per hour.
    pub effective_hourly_cents: Option<i64>,
    /// Time at the project's rate versions, even on a fixed-fee project, per currency.
    pub time_value_cents: BTreeMap<String, i64>,
    /// The same before rounding, so a figure the page carries forward from this one rounds once too.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub time_value_numerators: BTreeMap<String, i64>,
    pub budget: Option<BudgetStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetStatus {
    /// `hours`, or `fee` when time value is measured against a fixed fee.
    pub basis: &'static str,
    /// Whole percent used, rounded down so 79.9% doesn't alert early.
    pub percent: i64,
    /// 0, 80 or 100: the highest threshold reached.
    pub level: u8,
    /// A fixed fee can't be tracked without a price per hour to value the time.
    pub needs_target_rate: bool,
}

fn level_of(percent: i64) -> u8 {
    match percent {
        p if p >= 100 => 100,
        p if p >= 80 => 80,
        _ => 0,
    }
}

/// Computes the figures for every project in one pass.
#[must_use]
pub fn project_figures(
    shared: &Shared,
    tz: Tz,
    end_of: &dyn Fn(&Segment) -> Ts,
) -> BTreeMap<String, ProjectFigures> {
    let mut out: BTreeMap<String, ProjectFigures> = BTreeMap::new();
    // Rates apply to every project here, so a fixed-fee project's time can be valued.
    let wanted: BTreeSet<&str> = shared
        .projects
        .values()
        .filter(|p| p.budget_hours.is_some() || p.billing_type == BillingType::FixedFee)
        .map(|p| p.id.as_str())
        .collect();
    let mut all_hourly = Shared::default();
    if !wanted.is_empty() {
        all_hourly.rates = shared.rates.clone();
        all_hourly.rounding = shared.rounding.clone();
        all_hourly.gaps = shared.gaps.clone();
        all_hourly.pauses = shared.pauses.clone();
        for p in shared
            .projects
            .values()
            .filter(|p| wanted.contains(p.id.as_str()))
        {
            let mut p = p.clone();
            p.billing_type = BillingType::Hourly;
            p.billable = true;
            all_hourly.projects.insert(p.id.clone(), p);
        }
        for t in shared
            .tasks
            .values()
            .filter(|t| wanted.contains(t.project_id.as_str()))
        {
            all_hourly.tasks.insert(t.id.clone(), t.clone());
        }
        for s in shared
            .segments
            .values()
            .filter(|s| all_hourly.tasks.contains_key(&s.task_id))
        {
            all_hourly.segments.insert(s.id.clone(), s.clone());
        }
    }
    for td in task_days(&all_hourly, tz, end_of) {
        let f = out.entry(td.project_id.clone()).or_default();
        f.exact_secs += td.exact_secs;
        f.pause_secs += td.pause_secs;
        f.thinking_secs += td.thinking_secs;
        f.billable_secs += td.billable_secs;
        add_numerators(&mut f.time_value_numerators, &td);
    }
    for p in shared.projects.values() {
        let f = out.entry(p.id.clone()).or_default();
        f.time_value_cents = cents_of(&f.time_value_numerators);
        let fee = (p.billing_type == BillingType::FixedFee)
            .then_some(p.fixed_fee_cents)
            .flatten();
        if let Some(fee) = fee
            && f.billable_secs > 0
        {
            f.effective_hourly_cents = Some(
                i64::try_from(i128::from(fee) * 3600 / i128::from(f.billable_secs))
                    .unwrap_or(i64::MAX),
            );
        }
        f.budget = if let Some(hours) = p.budget_hours.filter(|h| *h > 0.0) {
            #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
            let percent = (f.billable_secs as f64 / (hours * 3600.0) * 100.0).floor() as i64;
            Some(BudgetStatus {
                basis: "hours",
                percent,
                level: level_of(percent),
                needs_target_rate: false,
            })
        } else if let Some(fee) = fee.filter(|f| *f > 0) {
            let value = p
                .fee_currency
                .as_ref()
                .and_then(|c| f.time_value_cents.get(c))
                .copied();
            let percent = value.map_or(0, |v| v.saturating_mul(100) / fee);
            Some(BudgetStatus {
                basis: "fee",
                percent,
                level: level_of(percent),
                needs_target_rate: value.is_none(),
            })
        } else {
            None
        };
    }
    out
}

/// Per task: seconds on a live summary, and seconds per subtask.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskTotals {
    pub billed_secs: i64,
    pub unbilled_secs: i64,
    pub thinking_secs: i64,
    pub pause_secs: i64,
    pub subtask_secs: BTreeMap<String, i64>,
}

#[must_use]
pub fn task_totals(shared: &Shared, all: &[Piece]) -> BTreeMap<String, TaskTotals> {
    let (segs, pauses) = shared.billed_sets();
    let mut out: BTreeMap<String, TaskTotals> = BTreeMap::new();
    for p in all {
        let t = out.entry(p.task_id.clone()).or_default();
        let billed = match p.kind {
            PieceKind::BillablePause => pauses.contains(p.source_id.as_str()),
            _ => segs.contains(p.source_id.as_str()),
        };
        if billed {
            t.billed_secs += p.secs();
        } else {
            t.unbilled_secs += p.secs();
        }
        match p.kind {
            PieceKind::Thinking => t.thinking_secs += p.secs(),
            PieceKind::BillablePause => t.pause_secs += p.secs(),
            PieceKind::Work => {}
        }
        if let Some(id) = &p.subtask_id {
            *t.subtask_secs.entry(id.clone()).or_insert(0) += p.secs();
        }
    }
    out
}

/// The summary as CSV. User text is neutralised against spreadsheet formula injection.
#[must_use]
pub fn summary_csv(b: &BillingSummary, project_name: &str, hide_earnings: bool) -> String {
    let mut out = String::new();
    let mut line = |cells: &[String]| {
        out.push_str(
            &cells
                .iter()
                .map(|c| crate::csv::quote(c))
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push_str("\r\n");
    };
    line(&["For your own guidance. Not an invoice, not a tax document.".to_owned()]);
    if hide_earnings {
        line(&["Amounts are left out because Hide earnings is on.".to_owned()]);
    }
    if b.voided {
        line(&["Status".to_owned(), "Voided".to_owned()]);
    }
    line(&["Reference".to_owned(), crate::csv::neutralize(&b.reference)]);
    line(&["Project".to_owned(), crate::csv::neutralize(project_name)]);
    line(&[
        "Period".to_owned(),
        crate::csv::neutralize(&format!("{} to {} ({})", b.from, b.to, b.zone)),
    ]);
    // With amounts hidden there are simply no currency columns, and so nothing to read back.
    let currencies: Vec<&String> = if hide_earnings {
        Vec::new()
    } else {
        b.totals.keys().collect()
    };
    let mut head: Vec<String> = [
        "Day",
        "Task",
        "Exact hours",
        "Billable pause hours",
        "Billable hours",
    ]
    .map(str::to_owned)
    .to_vec();
    head.extend(currencies.iter().map(|c| crate::csv::neutralize(c)));
    line(&head);
    #[allow(clippy::cast_precision_loss)]
    let hours = |s: i64| format!("{:.2}", s as f64 / 3600.0);
    #[allow(clippy::cast_precision_loss)]
    let amount = |c: i64| format!("{:.2}", c as f64 / 100.0);
    for r in &b.rows {
        let mut cells = vec![
            crate::csv::neutralize(&r.day),
            crate::csv::neutralize(&r.task_name),
            hours(r.exact_secs),
            hours(r.pause_secs),
            hours(r.billable_secs),
        ];
        cells.extend(
            currencies
                .iter()
                .map(|c| amount(r.cents.get(*c).copied().unwrap_or(0))),
        );
        line(&cells);
    }
    let mut total = vec![
        "Total".to_owned(),
        String::new(),
        hours(
            b.rows
                .iter()
                .map(|r| r.exact_secs)
                .fold(0, i64::saturating_add),
        ),
        hours(
            b.rows
                .iter()
                .map(|r| r.pause_secs)
                .fold(0, i64::saturating_add),
        ),
        hours(
            b.rows
                .iter()
                .map(|r| r.billable_secs)
                .fold(0, i64::saturating_add),
        ),
    ];
    total.extend(
        currencies
            .iter()
            .map(|c| amount(b.totals.get(*c).copied().unwrap_or(0))),
    );
    line(&total);
    if !currencies.is_empty() && rows_differ_from_total(b.rows.iter().map(|r| &r.cents), &b.totals)
    {
        line(&[ROUNDED_ROWS_NOTE.to_owned()]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adjust::{ManualEntryInput, PauseInput};
    use crate::machine::tests::{ctx, project_input, task_input};
    use crate::machine::{ProjectInput, RateInput};

    const NOW: &str = "2026-03-20T12:00:00Z";

    fn at(device: &str) -> Ctx<'_> {
        Ctx::at(Ts::parse("2026-03-20T12:00:00Z").unwrap(), device)
    }
    fn rate_for(s: &mut Shared, project: &str) {
        s.set_rate(
            &at("pc-a"),
            &RateInput {
                project_id: project.to_owned(),
                rate_cents: Some(6000),
                currency: "EUR".into(),
                effective_from: Some("2026-01-01T00:00:00Z".to_owned()),
                confirm_retroactive: true,
            },
        )
        .unwrap();
    }

    fn ts(x: &str) -> Ts {
        Ts::parse(x).unwrap()
    }
    fn world(input: &ProjectInput) -> (Shared, String, String) {
        let mut s = Shared::default();
        let c = ctx("2026-01-01T08:00:00Z");
        let p = s.create_project(&c, input).unwrap();
        s.set_rate(
            &c,
            &RateInput {
                project_id: p.clone(),
                rate_cents: Some(6000),
                currency: "EUR".into(),
                effective_from: Some("2026-01-01T00:00:00Z".to_owned()),
                confirm_retroactive: true,
            },
        )
        .unwrap();
        let t = s.create_task(&c, &task_input("Job", &p)).unwrap();
        (s, p, t)
    }
    fn entry(s: &mut Shared, t: &str, a: &str, b: &str) -> String {
        s.add_manual_entry(
            &ctx(NOW),
            &ManualEntryInput {
                task_id: t.into(),
                start: ts(a),
                end: ts(b),
                reason: String::new(),
                subtask_id: None,
                allow_overlap: true,
            },
        )
        .unwrap()
    }
    fn req(p: &str) -> SummaryRequest {
        SummaryRequest {
            project_id: p.into(),
            from: "2026-03-01".into(),
            to: "2026-03-31".into(),
            zone: "UTC".into(),
        }
    }

    #[test]
    fn a_summary_lists_unbilled_time_marks_it_and_a_second_one_finds_nothing() {
        let (mut s, p, t) = world(&project_input("A"));
        entry(&mut s, &t, "2026-03-02T09:00:00Z", "2026-03-02T10:00:00Z");
        let id = s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        let b = &s.summaries[&id];
        assert_eq!(b.reference, "A-001");
        assert_eq!(b.totals["EUR"], 6000);
        assert_eq!(b.rows[0].billable_secs, 3600);
        assert!(s.create_summary(&ctx(NOW), &req(&p)).is_err());
        let all = crate::pieces::pieces(&s, &|x: &Segment| x.end.unwrap_or(x.start));
        let totals = task_totals(&s, &all);
        assert_eq!(
            (totals[&t].billed_secs, totals[&t].unbilled_secs),
            (3600, 0)
        );
    }

    #[test]
    fn r9_the_same_segment_or_pause_on_two_live_summaries_names_both() {
        let (mut s, p, t) = world(&project_input("A"));
        entry(&mut s, &t, "2026-03-02T09:00:00Z", "2026-03-02T10:00:00Z");
        s.mark_billable_pause(
            &ctx(NOW),
            &crate::adjust::PauseInput {
                task_id: t.clone(),
                start: Ts::parse("2026-03-03T09:00:00Z").unwrap(),
                end: Ts::parse("2026-03-03T10:00:00Z").unwrap(),
                note: String::new(),
            },
        )
        .unwrap();
        let first = s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        assert!(
            s.billed_twice().is_empty(),
            "one summary is the normal case"
        );
        // What a second device writes when the first one's summary hasn't reached it yet.
        let copy = |s: &Shared, id: &str, prefix: &str, keep: &dyn Fn(&mut BillingSummary)| {
            let mut b = s.summaries[&first].clone();
            b.id = id.to_owned();
            b.prefix = prefix.to_owned();
            b.reference = format!("{prefix}-001");
            keep(&mut b);
            b
        };
        let only_segments = copy(&s, "b", "B", &|b| b.pause_ids.clear());
        s.summaries.insert("b".into(), only_segments);
        assert_eq!(s.billed_twice(), vec![vec!["A-001", "B-001"]]);
        let only_pauses = copy(&s, "c", "C", &|b| b.segment_ids.clear());
        s.summaries.insert("c".into(), only_pauses);
        assert_eq!(
            s.billed_twice(),
            vec![vec!["A-001", "B-001"], vec!["A-001", "C-001"]],
            "each set once, in order"
        );
        s.void_summary(&ctx(NOW), &first).unwrap();
        assert!(
            s.billed_twice().is_empty(),
            "a voided summary holds nothing"
        );
    }

    #[test]
    fn billed_time_cannot_be_corrected_until_the_summary_is_voided() {
        let (mut s, p, t) = world(&project_input("A"));
        let seg = entry(&mut s, &t, "2026-03-02T09:00:00Z", "2026-03-02T10:00:00Z");
        let id = s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        assert!(matches!(
            s.void_segment(&ctx(NOW), &seg, "x"),
            Err(CoreError::Conflict(_))
        ));
        s.void_summary(&ctx(NOW), &id).unwrap();
        assert!(s.void_segment(&ctx(NOW), &seg, "x").is_ok());
    }

    #[test]
    fn numbers_count_per_prefix_and_are_not_reused_after_a_void() {
        let (mut s, p, t) = world(&project_input("A"));
        entry(&mut s, &t, "2026-03-02T09:00:00Z", "2026-03-02T10:00:00Z");
        let first = s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        s.void_summary(&ctx(NOW), &first).unwrap();
        let second = s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        assert_eq!(s.summaries[&second].reference, "A-002");
        s.set_device_prefix(&ctx(NOW), " b2 ").unwrap();
        entry(&mut s, &t, "2026-03-03T09:00:00Z", "2026-03-03T10:00:00Z");
        let third = s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        assert_eq!(s.summaries[&third].reference, "B2-001");
        assert!(s.set_device_prefix(&ctx(NOW), "too long").is_err());
    }

    #[test]
    fn two_devices_get_different_prefixes_and_a_forced_clash_is_reported() {
        let mut s = Shared::default();
        let a = Ctx::at(ts(NOW), "pc-a");
        let b = Ctx::at(ts(NOW), "pc-b");
        s.set_device_prefix(&a, &s.prefix_of("pc-a")).unwrap();
        assert_eq!(s.prefix_of("pc-b"), "B");
        s.set_device_prefix(&b, "A").unwrap();
        assert_eq!(s.prefix_clashes("pc-a"), vec!["pc-b".to_owned()]);
    }

    #[test]
    fn a_billable_pause_is_on_the_summary_and_reported_separately() {
        let (mut s, p, t) = world(&project_input("A"));
        entry(&mut s, &t, "2026-03-02T09:00:00Z", "2026-03-02T10:00:00Z");
        entry(&mut s, &t, "2026-03-02T11:00:00Z", "2026-03-02T12:00:00Z");
        s.mark_billable_pause(
            &ctx(NOW),
            &PauseInput {
                task_id: t.clone(),
                start: ts("2026-03-02T10:00:00Z"),
                end: ts("2026-03-02T11:00:00Z"),
                note: String::new(),
            },
        )
        .unwrap();
        let pre = s.summary_preview(&req(&p)).unwrap();
        assert_eq!(
            (pre.exact_secs, pre.pause_secs, pre.billable_secs),
            (7200, 3600, 10800)
        );
        assert_eq!(pre.totals["EUR"], 18000);
    }

    #[test]
    fn budget_levels_on_hours() {
        let input = ProjectInput {
            budget_hours: Some(10.0),
            ..project_input("A")
        };
        let (mut s, p, t) = world(&input);
        // Rounding is off here so the percentages are exact.
        s.set_rounding(
            &ctx(NOW),
            &crate::machine::RoundingInput {
                project_id: None,
                effective_from: "2000-01-01".into(),
                increment_minutes: 0,
                method: crate::model::RoundMethod::Up,
                confirm_retroactive: true,
            },
            "2026-03-20",
        )
        .unwrap();
        let end = |x: &Segment| x.end.unwrap_or(x.start);
        let tz = parse_zone("UTC").unwrap();
        entry(&mut s, &t, "2026-03-02T00:00:00Z", "2026-03-02T07:59:00Z");
        assert_eq!(
            project_figures(&s, tz, &end)[&p]
                .budget
                .as_ref()
                .unwrap()
                .level,
            0
        );
        entry(&mut s, &t, "2026-03-03T00:00:00Z", "2026-03-03T00:01:00Z");
        let b = project_figures(&s, tz, &end)[&p].budget.clone().unwrap();
        assert_eq!((b.percent, b.level, b.basis), (80, 80, "hours"));
        entry(&mut s, &t, "2026-03-04T00:00:00Z", "2026-03-04T02:00:00Z");
        assert_eq!(
            project_figures(&s, tz, &end)[&p]
                .budget
                .as_ref()
                .unwrap()
                .level,
            100
        );
    }

    #[test]
    fn a_fixed_fee_project_earns_no_hourly_money_but_has_an_effective_rate() {
        let input = ProjectInput {
            billing_type: BillingType::FixedFee,
            fixed_fee_cents: Some(100_000),
            fee_currency: Some("EUR".into()),
            ..project_input("A")
        };
        let (mut s, p, t) = world(&input);
        entry(&mut s, &t, "2026-03-02T09:00:00Z", "2026-03-02T19:00:00Z");
        let end = |x: &Segment| x.end.unwrap_or(x.start);
        let tz = parse_zone("UTC").unwrap();
        let rows = task_days(&s, tz, &end);
        assert!(rows[0].numerators().is_empty(), "no hourly earnings");
        let f = &project_figures(&s, tz, &end)[&p];
        assert_eq!(f.effective_hourly_cents, Some(10_000));
        let b = f.budget.clone().unwrap();
        // 10 h at 60.00 is 600.00 of a 1000.00 fee.
        assert_eq!(
            (b.basis, b.percent, b.level, b.needs_target_rate),
            ("fee", 60, 0, false)
        );
    }

    #[test]
    fn a_fixed_fee_without_a_rate_says_it_needs_one() {
        let mut s = Shared::default();
        let c = ctx("2026-01-01T08:00:00Z");
        let p = s
            .create_project(
                &c,
                &ProjectInput {
                    billing_type: BillingType::FixedFee,
                    fixed_fee_cents: Some(100),
                    fee_currency: Some("EUR".into()),
                    ..project_input("A")
                },
            )
            .unwrap();
        let f = &project_figures(&s, parse_zone("UTC").unwrap(), &|x: &Segment| {
            x.end.unwrap_or(x.start)
        })[&p];
        assert!(f.budget.as_ref().unwrap().needs_target_rate);
    }

    #[test]
    fn the_csv_says_it_is_no_tax_document_and_neutralises_names() {
        let (mut s, p, t) = world(&project_input("A"));
        s.tasks.get_mut(&t).unwrap().name = "=HYPERLINK(1)".into();
        entry(&mut s, &t, "2026-03-02T09:00:00Z", "2026-03-02T10:00:00Z");
        let id = s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        let text = summary_csv(&s.summaries[&id], "A", false);
        assert!(text.starts_with("\"For your own guidance. Not an invoice, not a tax document.\""));
        assert!(text.contains("'=HYPERLINK"));
        assert!(text.contains("60.00"));
    }

    #[test]
    fn a_reference_number_never_repeats_when_a_prefix_changes_hands() {
        let (mut s, p, t) = crate::billing::tests::fixture(None);
        rate_for(&mut s, &p);
        crate::billing::tests::seg(
            &mut s,
            &t,
            "s1",
            "2026-03-01T09:00:00Z",
            "2026-03-01T10:00:00Z",
        );
        let a = at("pc-a");
        let first = s.create_summary(&a, &req(&p)).unwrap();
        assert_eq!(s.summaries[&first].reference, "A-001");
        s.set_device_prefix(&a, "X").unwrap();
        let b = at("pc-b");
        s.set_device_prefix(&b, "A").unwrap();
        crate::billing::tests::seg(
            &mut s,
            &t,
            "s2",
            "2026-03-02T09:00:00Z",
            "2026-03-02T10:00:00Z",
        );
        let second = s.create_summary(&b, &req(&p)).unwrap();
        assert_eq!(s.summaries[&second].reference, "A-002");
        assert!(s.reference_clashes().is_empty());
        // Two offline devices can still pick the same one: that is reported.
        let mut copy = s.summaries[&second].clone();
        copy.id = "other".into();
        s.summaries.insert("other".into(), copy);
        assert_eq!(s.reference_clashes(), vec!["A-002".to_owned()]);
        s.summaries.get_mut("other").unwrap().voided = true;
        assert!(s.reference_clashes().is_empty());
    }

    #[test]
    fn a_pause_around_a_billed_segment_keeps_its_unbilled_time_on_the_summary() {
        let (mut s, p, t) =
            crate::billing::tests::fixture(Some((0, crate::model::RoundMethod::Nearest)));
        rate_for(&mut s, &p);
        crate::billing::tests::seg(
            &mut s,
            &t,
            "billed",
            "2026-03-01T10:30:00Z",
            "2026-03-01T11:00:00Z",
        );
        let c = at("pc-a");
        s.create_summary(&c, &req(&p)).unwrap();
        s.pauses.insert(
            "pz".into(),
            crate::model::BillablePause {
                id: "pz".into(),
                task_id: t.clone(),
                start: Ts::parse("2026-03-01T10:00:00Z").unwrap(),
                end: Ts::parse("2026-03-01T12:00:00Z").unwrap(),
                note: String::new(),
                deleted: false,
                stamp: crate::model::Stamp::default(),
            },
        );
        let preview = s.summary_preview(&req(&p)).unwrap();
        assert_eq!(preview.pause_secs, 5400, "both sides of the billed segment");
    }

    #[test]
    fn the_same_pause_marked_twice_bills_once_in_either_order() {
        let (mut s, p, t) =
            crate::billing::tests::fixture(Some((0, crate::model::RoundMethod::Nearest)));
        rate_for(&mut s, &p);
        for id in ["pa", "pb"] {
            s.pauses.insert(
                id.into(),
                crate::model::BillablePause {
                    id: id.into(),
                    task_id: t.clone(),
                    start: Ts::parse("2026-03-01T10:00:00Z").unwrap(),
                    end: Ts::parse("2026-03-01T11:00:00Z").unwrap(),
                    note: String::new(),
                    deleted: false,
                    stamp: crate::model::Stamp::default(),
                },
            );
        }
        let preview = s.summary_preview(&req(&p)).unwrap();
        assert_eq!(preview.pause_secs, 3600);
        assert_eq!(preview.billable_secs, 3600);
    }

    #[test]
    fn a_preview_says_how_much_time_away_it_leaves_out() {
        let (mut s, p, t) = crate::billing::tests::fixture(None);
        rate_for(&mut s, &p);
        crate::billing::tests::seg(
            &mut s,
            &t,
            "s1",
            "2026-03-01T09:00:00Z",
            "2026-03-01T12:00:00Z",
        );
        s.add_gap(
            &at("pc-a"),
            "s1",
            Ts::parse("2026-03-01T10:00:00Z").unwrap(),
            Ts::parse("2026-03-01T11:00:00Z").unwrap(),
            crate::model::GapKind::Sleep,
        )
        .unwrap();
        let preview = s.summary_preview(&req(&p)).unwrap();
        assert_eq!(preview.unconfirmed_secs, 3600);
        assert_eq!(preview.exact_secs, 2 * 3600);
    }

    #[test]
    fn hostile_summary_payloads_are_refused_at_the_sync_check() {
        let (mut s, p, t) = crate::billing::tests::fixture(None);
        rate_for(&mut s, &p);
        crate::billing::tests::seg(
            &mut s,
            &t,
            "s1",
            "2026-03-01T09:00:00Z",
            "2026-03-01T10:00:00Z",
        );
        let id = s.create_summary(&at("pc-a"), &req(&p)).unwrap();
        let now = Ts::parse("2026-06-01T00:00:00Z").unwrap();
        crate::validate::check_shared(&s, now).unwrap();
        type Attack = fn(&mut BillingSummary);
        let attacks: [Attack; 8] = [
            |b| b.number = u32::MAX,
            |b| b.rows[0].exact_secs = i64::MAX,
            |b| b.from = "=HYPERLINK(1)".into(),
            |b| b.rows[0].day = "=1+1".into(),
            |b| b.zone = "=cmd".into(),
            |b| b.reference = "=1+1".into(),
            |b| {
                b.totals.insert("=X".into(), 1);
            },
            |b| {
                b.totals.insert("EUR".into(), i64::MAX);
            },
        ];
        for attack in attacks {
            let mut bad = s.clone();
            attack(bad.summaries.get_mut(&id).unwrap());
            assert!(crate::validate::check_shared(&bad, now).is_err());
        }
        let mut wrong_key = s.clone();
        let moved = wrong_key.summaries.remove(&id).unwrap();
        wrong_key.summaries.insert("elsewhere".into(), moved);
        assert!(crate::validate::check_shared(&wrong_key, now).is_err());
        let mut fee = s.clone();
        fee.projects.get_mut(&p).unwrap().fixed_fee_cents = Some(-5);
        assert!(crate::validate::check_shared(&fee, now).is_err());
    }

    #[test]
    fn a_name_with_a_comma_is_quoted_once_and_a_formula_is_neutralised() {
        let (mut s, p, t) = crate::billing::tests::fixture(None);
        rate_for(&mut s, &p);
        s.tasks.get_mut(&t).unwrap().name = "Logo, phase 2".into();
        crate::billing::tests::seg(
            &mut s,
            &t,
            "s1",
            "2026-03-01T09:00:00Z",
            "2026-03-01T10:00:00Z",
        );
        let id = s.create_summary(&at("pc-a"), &req(&p)).unwrap();
        let csv = summary_csv(&s.summaries[&id], "=evil()", false);
        assert!(csv.contains(",\"Logo, phase 2\","), "{csv}");
        assert!(!csv.contains("\"\"\""), "{csv}");
        assert!(csv.contains("'=evil()"), "{csv}");
    }

    #[test]
    fn a_hidden_csv_has_no_amount_and_a_voided_one_says_so() {
        let (mut s, p, t) = crate::billing::tests::fixture(None);
        rate_for(&mut s, &p);
        crate::billing::tests::seg(
            &mut s,
            &t,
            "s1",
            "2026-03-01T09:00:00Z",
            "2026-03-01T10:00:00Z",
        );
        let id = s.create_summary(&at("pc-a"), &req(&p)).unwrap();
        let shown = summary_csv(&s.summaries[&id], "P", false);
        assert!(shown.contains("EUR") && shown.contains("60.00"), "{shown}");
        assert!(
            shown.starts_with("\"For your own guidance. Not an invoice, not a tax document.\"")
        );
        let hidden = summary_csv(&s.summaries[&id], "P", true);
        assert!(
            !hidden.contains("EUR") && !hidden.contains("60.00"),
            "{hidden}"
        );
        assert!(hidden.contains("left out because Hide earnings"));
        assert!(!shown.contains("Voided"));
        s.void_summary(&at("pc-a"), &id).unwrap();
        assert!(summary_csv(&s.summaries[&id], "P", false).contains("Status,Voided"));
    }

    #[test]
    fn voiding_twice_writes_one_audit_entry() {
        let (mut s, p, t) = crate::billing::tests::fixture(None);
        rate_for(&mut s, &p);
        crate::billing::tests::seg(
            &mut s,
            &t,
            "s1",
            "2026-03-01T09:00:00Z",
            "2026-03-01T10:00:00Z",
        );
        let id = s.create_summary(&at("pc-a"), &req(&p)).unwrap();
        s.void_summary(&at("pc-a"), &id).unwrap();
        let stamp = s.summaries[&id].stamp.clone();
        let audits = s.audit_log.len();
        s.void_summary(&at("pc-a"), &id).unwrap();
        assert_eq!(s.audit_log.len(), audits);
        assert_eq!(s.summaries[&id].stamp, stamp);
    }

    /// The billable time of every live summary, and of the report over the same store.
    fn billed_and_report(s: &Shared, zone: &str) -> (i64, i64) {
        let billed = s
            .summaries
            .values()
            .filter(|b| !b.voided)
            .flat_map(|b| b.rows.iter().map(|r| r.billable_secs))
            .sum();
        let end = |x: &Segment| x.end.unwrap_or(x.start);
        let report = task_days(s, parse_zone(zone).unwrap(), &end)
            .iter()
            .map(|r| r.billable_secs)
            .sum();
        (billed, report)
    }

    fn rule(s: &mut Shared, minutes: u32, method: crate::model::RoundMethod) {
        s.set_rounding(
            &ctx(NOW),
            &crate::machine::RoundingInput {
                project_id: None,
                effective_from: "2000-01-01".into(),
                increment_minutes: minutes,
                method,
                confirm_retroactive: true,
            },
            "2026-03-20",
        )
        .unwrap();
    }

    const NOTE: &str = "Row amounts are rounded; the total is worked out from the exact amounts.";

    /// Forty 15-minute rows at 33.33 an hour, one a day from 2 January.
    fn forty_rows(s: &mut Shared, p: &str, t: &str) {
        s.set_rate(
            &ctx(NOW),
            &RateInput {
                project_id: p.to_owned(),
                rate_cents: Some(3333),
                currency: "EUR".into(),
                effective_from: Some("2026-01-01T00:00:00Z".to_owned()),
                confirm_retroactive: true,
            },
        )
        .unwrap();
        for i in 0..40 {
            let day = NaiveDate::from_ymd_opt(2026, 1, 2).unwrap() + chrono::Duration::days(i);
            entry(
                s,
                t,
                &format!("{day}T09:00:00Z"),
                &format!("{day}T09:15:00Z"),
            );
        }
    }

    fn all_of_it(p: &str) -> SummaryRequest {
        SummaryRequest {
            project_id: p.into(),
            from: "2026-01-01".into(),
            to: "2026-12-31".into(),
            zone: "UTC".into(),
        }
    }

    #[test]
    fn r7_a_project_time_value_is_worked_out_once_from_exact_amounts() {
        let input = ProjectInput {
            billing_type: BillingType::FixedFee,
            fixed_fee_cents: Some(100_000),
            fee_currency: Some("EUR".into()),
            ..project_input("A")
        };
        let (mut s, p, t) = world(&input);
        forty_rows(&mut s, &p, &t);
        let end = |x: &Segment| x.end.unwrap_or(x.start);
        let f = &project_figures(&s, parse_zone("UTC").unwrap(), &end)[&p];
        assert_eq!(
            f.time_value_cents["EUR"], 33330,
            "33320 if the rows were added"
        );
        let sent = serde_json::to_value(f).unwrap();
        assert_eq!(sent["timeValueNumerators"]["EUR"], 3333 * 900 * 40);
        let none = ProjectFigures::default();
        assert!(
            serde_json::to_value(none)
                .unwrap()
                .get("timeValueNumerators")
                .is_none()
        );
    }

    #[test]
    fn r7_a_summary_csv_says_so_only_when_its_rows_do_not_add_up_to_its_total() {
        let (mut s, p, t) = world(&project_input("A"));
        forty_rows(&mut s, &p, &t);
        let id = s.create_summary(&ctx(NOW), &all_of_it(&p)).unwrap();
        let csv = summary_csv(&s.summaries[&id], "A", false);
        assert!(
            csv.contains(",8.33\r\n"),
            "rows keep their rounded amounts: {csv}"
        );
        assert!(csv.contains(",333.30\r\n"), "{csv}");
        assert!(csv.contains(NOTE), "{csv}");
        let hidden = summary_csv(&s.summaries[&id], "A", true);
        assert!(
            !hidden.contains(NOTE),
            "no amounts, so nothing to explain: {hidden}"
        );

        let (mut s, p, t) = world(&project_input("A"));
        entry(&mut s, &t, "2026-03-02T09:00:00Z", "2026-03-02T10:00:00Z");
        let id = s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        assert!(!summary_csv(&s.summaries[&id], "A", false).contains(NOTE));
    }

    /// The total of a summary is stored when it is made and read as stored, so what an older build issued,
    /// the sum of its rounded rows, is never worked out again.
    #[test]
    fn r7_an_issued_summary_keeps_the_total_it_froze() {
        let (mut s, p, t) = world(&project_input("A"));
        forty_rows(&mut s, &p, &t);
        let id = s.create_summary(&ctx(NOW), &all_of_it(&p)).unwrap();
        let older = s.summaries.get_mut(&id).unwrap();
        older.totals = BTreeMap::from([("EUR".to_owned(), 33320)]);
        let issued = older.clone();
        entry(&mut s, &t, "2026-03-02T14:00:00Z", "2026-03-02T15:00:00Z");
        s.create_summary(&ctx(NOW), &all_of_it(&p)).unwrap();
        crate::validate::check_shared(&s, ts(NOW)).unwrap();
        assert_eq!(s.summaries[&id], issued);
        let csv = summary_csv(&s.summaries[&id], "A", false);
        assert!(csv.contains(",333.20\r\n"), "{csv}");
        assert!(
            !csv.contains(NOTE),
            "its rows add up to what it says: {csv}"
        );
    }

    #[test]
    fn r8_two_summaries_on_one_task_day_bill_what_the_report_shows() {
        let (mut s, p, t) = world(&project_input("A"));
        entry(&mut s, &t, "2026-03-02T09:00:00Z", "2026-03-02T09:20:00Z");
        let first = s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        assert_eq!(s.summaries[&first].rows[0].billable_secs, 1800);
        entry(&mut s, &t, "2026-03-02T14:00:00Z", "2026-03-02T14:20:00Z");
        let preview = s.summary_preview(&req(&p)).unwrap();
        assert_eq!(
            (preview.exact_secs, preview.billable_secs),
            (1200, 900),
            "20 + 20 minutes round to 45 as a whole, and 30 are on the first summary"
        );
        assert_eq!(preview.totals["EUR"], 1500);
        assert_eq!(preview.rows[0].cents["EUR"], 1500);
        assert_eq!(preview.partly_billed_rows, 1);
        s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        assert_eq!(billed_and_report(&s, "UTC"), (2700, 2700));
    }

    #[test]
    fn r8_a_day_billed_in_one_summary_says_nothing_of_parts() {
        let (mut s, p, t) = world(&project_input("A"));
        entry(&mut s, &t, "2026-03-02T09:00:00Z", "2026-03-02T09:20:00Z");
        entry(&mut s, &t, "2026-03-02T14:00:00Z", "2026-03-02T14:20:00Z");
        let preview = s.summary_preview(&req(&p)).unwrap();
        assert_eq!(preview.billable_secs, 2700);
        assert_eq!(preview.partly_billed_rows, 0);
    }

    #[test]
    fn r8_voiding_a_summary_lets_a_later_preview_round_what_is_left() {
        let two = || {
            let (mut s, p, t) = world(&project_input("A"));
            entry(&mut s, &t, "2026-03-02T09:00:00Z", "2026-03-02T09:20:00Z");
            let first = s.create_summary(&ctx(NOW), &req(&p)).unwrap();
            entry(&mut s, &t, "2026-03-02T14:00:00Z", "2026-03-02T14:20:00Z");
            let second = s.create_summary(&ctx(NOW), &req(&p)).unwrap();
            (s, p, first, second)
        };
        // The first goes: the second keeps its 15, and the morning takes the 30 the day still lacks.
        let (mut s, p, first, second) = two();
        let kept = s.summaries[&second].clone();
        s.void_summary(&ctx(NOW), &first).unwrap();
        assert_eq!(
            s.summaries[&second].rows, kept.rows,
            "issued rows don't move"
        );
        assert_eq!(s.summaries[&second].totals, kept.totals);
        let preview = s.summary_preview(&req(&p)).unwrap();
        assert_eq!((preview.exact_secs, preview.billable_secs), (1200, 1800));
        assert_eq!(
            preview.partly_billed_rows, 0,
            "what 20 minutes round to alone"
        );
        s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        assert_eq!(billed_and_report(&s, "UTC"), (2700, 2700));
        // The second goes: the afternoon takes its 15 again.
        let (mut s, p, _, second) = two();
        s.void_summary(&ctx(NOW), &second).unwrap();
        let preview = s.summary_preview(&req(&p)).unwrap();
        assert_eq!((preview.exact_secs, preview.billable_secs), (1200, 900));
        // Both go: the day is one row again, with nothing to take off.
        let (mut s, p, first, second) = two();
        s.void_summary(&ctx(NOW), &first).unwrap();
        s.void_summary(&ctx(NOW), &second).unwrap();
        let preview = s.summary_preview(&req(&p)).unwrap();
        assert_eq!((preview.exact_secs, preview.billable_secs), (2400, 2700));
        assert_eq!(preview.partly_billed_rows, 0);
    }

    #[test]
    fn r8_the_whole_day_rounds_under_the_rule_in_force_now() {
        use crate::model::RoundMethod::{Nearest, Up};
        let start = || {
            let (mut s, p, t) = world(&project_input("A"));
            entry(&mut s, &t, "2026-03-02T09:00:00Z", "2026-03-02T09:20:00Z");
            let first = s.create_summary(&ctx(NOW), &req(&p)).unwrap();
            assert_eq!(s.summaries[&first].rows[0].billable_secs, 1800);
            entry(&mut s, &t, "2026-03-02T14:00:00Z", "2026-03-02T14:20:00Z");
            (s, p, t, first)
        };
        // Rounding turned off: 40 minutes in all, and the first summary keeps the 30 it issued.
        let (mut s, p, _, first) = start();
        rule(&mut s, 0, Nearest);
        assert_eq!(s.summary_preview(&req(&p)).unwrap().billable_secs, 600);
        s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        assert_eq!(s.summaries[&first].rows[0].billable_secs, 1800);
        assert_eq!(billed_and_report(&s, "UTC"), (2400, 2400));
        // A coarser rule: the day is an hour now.
        let (mut s, p, _, _) = start();
        rule(&mut s, 60, Up);
        assert_eq!(s.summary_preview(&req(&p)).unwrap().billable_secs, 1800);
        s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        assert_eq!(billed_and_report(&s, "UTC"), (3600, 3600));
        // Less than is already issued: nothing is billed, and nothing is taken back.
        let (mut s, p, t) = world(&project_input("A"));
        entry(&mut s, &t, "2026-03-02T09:00:00Z", "2026-03-02T09:20:00Z");
        s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        rule(&mut s, 0, Nearest);
        entry(&mut s, &t, "2026-03-02T14:00:00Z", "2026-03-02T14:05:00Z");
        let preview = s.summary_preview(&req(&p)).unwrap();
        assert_eq!((preview.exact_secs, preview.billable_secs), (300, 0));
        assert_eq!(preview.totals["EUR"], 0);
        assert_eq!(preview.partly_billed_rows, 1);
        s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        assert_eq!(billed_and_report(&s, "UTC"), (1800, 1500));
    }

    #[test]
    fn r8_summaries_from_an_older_build_stay_as_issued_and_later_rows_bill_what_is_left() {
        let (mut s, p, t) = world(&project_input("A"));
        entry(&mut s, &t, "2026-03-02T09:00:00Z", "2026-03-02T09:20:00Z");
        let first = s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        entry(&mut s, &t, "2026-03-02T14:00:00Z", "2026-03-02T14:20:00Z");
        let second = s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        // What an older build froze: each part rounded on its own, 30 + 30 where the report has 45.
        let older = s.summaries.get_mut(&second).unwrap();
        older.rows[0].billable_secs = 1800;
        older.rows[0].cents = BTreeMap::from([("EUR".to_owned(), 3000)]);
        older.totals = older.rows[0].cents.clone();
        let issued = [s.summaries[&first].clone(), s.summaries[&second].clone()];
        // Five minutes more: the day rounds to 45, and 60 are issued.
        entry(&mut s, &t, "2026-03-02T16:00:00Z", "2026-03-02T16:05:00Z");
        let third = s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        let b = &s.summaries[&third];
        assert_eq!((b.rows[0].exact_secs, b.rows[0].billable_secs), (300, 0));
        assert_eq!(b.totals["EUR"], 0);
        // The day grows past what is issued: 75 less 60.
        entry(&mut s, &t, "2026-03-02T17:00:00Z", "2026-03-02T17:30:00Z");
        s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        assert_eq!(billed_and_report(&s, "UTC"), (4500, 4500));
        assert_eq!(s.summaries[&first], issued[0]);
        assert_eq!(s.summaries[&second], issued[1]);
    }

    #[test]
    fn r8_a_summary_in_another_zone_is_shared_out_over_the_days_of_this_one() {
        let (mut s, p, t) = world(&project_input("A"));
        // 04:50 to 05:10 UTC: 20 minutes on one day in UTC, 10 on each of two days in New York.
        entry(&mut s, &t, "2026-03-02T04:50:00Z", "2026-03-02T05:10:00Z");
        let first = s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        assert_eq!(s.summaries[&first].rows[0].billable_secs, 1800);
        entry(&mut s, &t, "2026-03-02T14:00:00Z", "2026-03-02T14:20:00Z");
        let new_york = SummaryRequest {
            zone: "America/New_York".into(),
            ..req(&p)
        };
        // New York's 2 March holds 10 + 20 minutes, which is 30. The first summary's 30 lie half on it.
        let preview = s.summary_preview(&new_york).unwrap();
        assert_eq!(preview.rows.len(), 1);
        assert_eq!(preview.rows[0].day, "2026-03-02");
        assert_eq!(preview.billable_secs, 900);
        s.create_summary(&ctx(NOW), &new_york).unwrap();
        assert_eq!(billed_and_report(&s, "America/New_York"), (2700, 2700));
    }

    #[test]
    fn r8_an_empty_segment_on_a_summary_in_another_zone_holds_no_share() {
        let (mut s, p, t) = world(&project_input("A"));
        // 2 March in UTC. In New York the 20 minutes are on 1 March, and the empty segment is on 2 March.
        entry(&mut s, &t, "2026-03-02T04:30:00Z", "2026-03-02T04:50:00Z");
        crate::billing::tests::seg(
            &mut s,
            &t,
            "empty",
            "2026-03-02T06:00:00Z",
            "2026-03-02T06:00:00Z",
        );
        s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        entry(&mut s, &t, "2026-03-02T03:00:00Z", "2026-03-02T03:20:00Z");
        entry(&mut s, &t, "2026-03-02T07:00:00Z", "2026-03-02T07:20:00Z");
        let new_york = SummaryRequest {
            zone: "America/New_York".into(),
            ..req(&p)
        };
        // 1 March: 40 minutes round to 45, and 30 are held. 2 March: 20 minutes round to 30, and nothing is held.
        let preview = s.summary_preview(&new_york).unwrap();
        let billed: Vec<i64> = preview.rows.iter().map(|r| r.billable_secs).collect();
        assert_eq!(billed, [900, 1800]);
    }

    /// What a correction made where the summary hadn't arrived comes to, once the files have crossed.
    fn corrected(s: &mut Shared, id: &str, a: &str, b: &str) {
        s.segments.get_mut(id).unwrap().edit = Some(crate::model::SegmentEdit {
            start: ts(a),
            end: ts(b),
            voided: false,
            reason: "corrected on another device".into(),
            settled: false,
        });
    }

    #[test]
    fn r8_billed_time_a_merge_moved_to_another_day_is_held_on_that_day() {
        let (mut base, p, t) = world(&project_input("A"));
        let id = entry(
            &mut base,
            &t,
            "2026-03-02T09:00:00Z",
            "2026-03-02T09:20:00Z",
        );
        let mut on_a = base.clone();
        let mut on_b = base;
        on_a.create_summary(&ctx(NOW), &req(&p)).unwrap();
        on_b.correct_segment(
            &Ctx {
                device: "device-b",
                ..ctx(NOW)
            },
            &crate::adjust::CorrectionInput {
                segment_id: id,
                start: ts("2026-03-03T09:00:00Z"),
                end: ts("2026-03-03T09:20:00Z"),
                reason: "wrong day".into(),
                allow_overlap: false,
            },
        )
        .unwrap();
        let mut s = crate::merge::merge(&on_a, &on_b);
        entry(&mut s, &t, "2026-03-03T14:00:00Z", "2026-03-03T14:20:00Z");
        let preview = s.summary_preview(&req(&p)).unwrap();
        assert_eq!((preview.exact_secs, preview.billable_secs), (1200, 900));
        s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        assert_eq!(billed_and_report(&s, "UTC"), (2700, 2700));
        crate::validate::check_shared(&s, ts(NOW)).unwrap();
    }

    #[test]
    fn r8_the_day_billed_time_was_moved_from_rounds_afresh() {
        let (mut s, p, t) = world(&project_input("A"));
        let id = entry(&mut s, &t, "2026-03-02T09:00:00Z", "2026-03-02T09:20:00Z");
        s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        corrected(&mut s, &id, "2026-03-03T09:00:00Z", "2026-03-03T09:20:00Z");
        entry(&mut s, &t, "2026-03-02T14:00:00Z", "2026-03-02T14:20:00Z");
        assert_eq!(s.summary_preview(&req(&p)).unwrap().billable_secs, 1800);
        s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        assert_eq!(billed_and_report(&s, "UTC"), (3600, 3600));
    }

    #[test]
    fn r8_part_of_a_row_moved_takes_its_share_with_it() {
        // 10 + 10 minutes bill 30 on one row. One 10 moves on, so half of the 30 goes with it.
        let start = || {
            let (mut s, p, t) = world(&project_input("A"));
            entry(&mut s, &t, "2026-03-02T09:00:00Z", "2026-03-02T09:10:00Z");
            let id = entry(&mut s, &t, "2026-03-02T10:00:00Z", "2026-03-02T10:10:00Z");
            s.create_summary(&ctx(NOW), &req(&p)).unwrap();
            corrected(&mut s, &id, "2026-03-03T10:00:00Z", "2026-03-03T10:10:00Z");
            (s, p, t)
        };
        for day in ["2026-03-02", "2026-03-03"] {
            let (mut s, p, t) = start();
            entry(
                &mut s,
                &t,
                &format!("{day}T14:00:00Z"),
                &format!("{day}T14:20:00Z"),
            );
            // 10 + 20 minutes are 30 on that day, and 15 are held on it.
            assert_eq!(
                s.summary_preview(&req(&p)).unwrap().billable_secs,
                900,
                "{day}"
            );
            s.create_summary(&ctx(NOW), &req(&p)).unwrap();
            assert_eq!(billed_and_report(&s, "UTC"), (2700, 2700), "{day}");
        }
    }

    #[test]
    fn r8_only_time_that_moved_takes_its_share_and_time_cut_stays_held() {
        // 23:00 to 00:10 bills 60 + 15. It then becomes 23:50 to 00:20: 50 minutes left the first day and only 10
        // came to the second, so 40 were cut, and the 10 that moved take 10 of the 60 with them.
        let start = || {
            let (mut s, p, t) = world(&project_input("A"));
            let id = entry(&mut s, &t, "2026-03-02T23:00:00Z", "2026-03-03T00:10:00Z");
            s.create_summary(&ctx(NOW), &req(&p)).unwrap();
            corrected(&mut s, &id, "2026-03-02T23:50:00Z", "2026-03-03T00:20:00Z");
            (s, p, t)
        };
        // On the first day, 10 + 60 minutes are 75, and 50 stay held.
        let (mut s, p, t) = start();
        entry(&mut s, &t, "2026-03-02T09:00:00Z", "2026-03-02T10:00:00Z");
        assert_eq!(s.summary_preview(&req(&p)).unwrap().billable_secs, 1500);
        // On the second, 20 + 60 minutes are 90, and 15 + 10 are held. What is held for the time cut isn't taken
        // off another day's work, so here the summaries come to more than the report.
        let (mut s, p, t) = start();
        entry(&mut s, &t, "2026-03-03T09:00:00Z", "2026-03-03T10:00:00Z");
        assert_eq!(s.summary_preview(&req(&p)).unwrap().billable_secs, 3900);
    }

    #[test]
    fn r8_a_little_time_brought_does_not_take_a_whole_row_along() {
        // Eight hours and then 5 minutes bill 8:00 + 15. The eight hours are cut to 10 minutes and the 5 become 10,
        // so at most 5 minutes moved, and only their share goes with them.
        let (mut s, p, t) = world(&project_input("A"));
        let long = entry(&mut s, &t, "2026-03-02T09:00:00Z", "2026-03-02T17:00:00Z");
        let short = entry(&mut s, &t, "2026-03-03T09:00:00Z", "2026-03-03T09:05:00Z");
        s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        corrected(
            &mut s,
            &long,
            "2026-03-02T09:00:00Z",
            "2026-03-02T09:10:00Z",
        );
        corrected(
            &mut s,
            &short,
            "2026-03-03T09:00:00Z",
            "2026-03-03T09:10:00Z",
        );
        entry(&mut s, &t, "2026-03-02T10:00:00Z", "2026-03-02T18:00:00Z");
        // 10 minutes + 8 hours are 8:15 on the first day, and 7:55 stay held on it.
        assert_eq!(s.summary_preview(&req(&p)).unwrap().billable_secs, 1200);
    }

    #[test]
    fn r8_time_moved_between_days_a_summary_names_takes_its_share_too() {
        let (mut s, p, t) = world(&project_input("A"));
        // 10 minutes on each side of midnight, 15 + 15. Then all 20 are on the first day.
        let id = entry(&mut s, &t, "2026-03-02T23:50:00Z", "2026-03-03T00:10:00Z");
        s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        corrected(&mut s, &id, "2026-03-02T23:30:00Z", "2026-03-02T23:50:00Z");
        entry(&mut s, &t, "2026-03-03T09:00:00Z", "2026-03-03T09:20:00Z");
        assert_eq!(s.summary_preview(&req(&p)).unwrap().billable_secs, 1800);
        s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        assert_eq!(billed_and_report(&s, "UTC"), (3600, 3600));
    }

    #[test]
    fn r8_time_corrected_on_its_own_day_stays_held_there() {
        // Cut short: the 30 issued stay, and cover the 10 + 20 minutes the day now holds.
        let (mut s, p, t) = world(&project_input("A"));
        let id = entry(&mut s, &t, "2026-03-02T09:00:00Z", "2026-03-02T09:20:00Z");
        s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        corrected(&mut s, &id, "2026-03-02T09:00:00Z", "2026-03-02T09:10:00Z");
        entry(&mut s, &t, "2026-03-02T14:00:00Z", "2026-03-02T14:20:00Z");
        assert_eq!(s.summary_preview(&req(&p)).unwrap().billable_secs, 0);
        // Made longer: 40 + 20 minutes are an hour, and 30 are held.
        let (mut s, p, t) = world(&project_input("A"));
        let id = entry(&mut s, &t, "2026-03-02T09:00:00Z", "2026-03-02T09:20:00Z");
        s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        corrected(&mut s, &id, "2026-03-02T09:00:00Z", "2026-03-02T09:40:00Z");
        entry(&mut s, &t, "2026-03-02T14:00:00Z", "2026-03-02T14:20:00Z");
        assert_eq!(s.summary_preview(&req(&p)).unwrap().billable_secs, 1800);
        // Gone altogether: the 30 stay on the date the row names.
        let (mut s, p, t) = world(&project_input("A"));
        let id = entry(&mut s, &t, "2026-03-02T09:00:00Z", "2026-03-02T09:20:00Z");
        s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        s.segments.get_mut(&id).unwrap().edit = Some(crate::model::SegmentEdit {
            start: ts("2026-03-02T09:00:00Z"),
            end: ts("2026-03-02T09:20:00Z"),
            voided: true,
            reason: "voided on another device".into(),
            settled: false,
        });
        entry(&mut s, &t, "2026-03-02T14:00:00Z", "2026-03-02T14:20:00Z");
        assert_eq!(s.summary_preview(&req(&p)).unwrap().billable_secs, 0);
    }

    #[test]
    fn r8_billed_time_moved_on_a_summary_in_another_zone_is_held_where_it_lies() {
        let (mut s, p, t) = world(&project_input("A"));
        let id = entry(&mut s, &t, "2026-03-02T14:00:00Z", "2026-03-02T14:20:00Z");
        s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        corrected(&mut s, &id, "2026-03-03T14:00:00Z", "2026-03-03T14:20:00Z");
        entry(&mut s, &t, "2026-03-03T15:00:00Z", "2026-03-03T15:20:00Z");
        let new_york = SummaryRequest {
            zone: "America/New_York".into(),
            ..req(&p)
        };
        assert_eq!(s.summary_preview(&new_york).unwrap().billable_secs, 900);
    }

    #[test]
    fn r8_a_billed_pause_and_later_work_round_as_one_day() {
        let (mut s, p, t) = world(&project_input("A"));
        s.mark_billable_pause(
            &ctx(NOW),
            &PauseInput {
                task_id: t.clone(),
                start: ts("2026-03-02T09:00:00Z"),
                end: ts("2026-03-02T09:20:00Z"),
                note: String::new(),
            },
        )
        .unwrap();
        s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        entry(&mut s, &t, "2026-03-02T14:00:00Z", "2026-03-02T14:20:00Z");
        let preview = s.summary_preview(&req(&p)).unwrap();
        assert_eq!(
            (
                preview.exact_secs,
                preview.pause_secs,
                preview.billable_secs
            ),
            (1200, 0, 900)
        );
        s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        assert_eq!(billed_and_report(&s, "UTC"), (2700, 2700));
    }

    #[test]
    fn r8_time_across_midnight_counts_on_both_of_its_days() {
        let (mut s, p, t) = world(&project_input("A"));
        entry(&mut s, &t, "2026-03-02T23:50:00Z", "2026-03-03T00:10:00Z");
        s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        entry(&mut s, &t, "2026-03-03T09:00:00Z", "2026-03-03T09:20:00Z");
        let preview = s.summary_preview(&req(&p)).unwrap();
        assert_eq!(
            preview.billable_secs, 900,
            "30 in all on 3 March, 15 billed"
        );
        s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        assert_eq!(billed_and_report(&s, "UTC"), (2700, 2700));
    }

    #[test]
    fn r8_a_rate_change_on_the_day_takes_the_difference_off_the_last_rate() {
        let (mut s, p, t) = world(&project_input("A"));
        s.set_rate(
            &ctx(NOW),
            &RateInput {
                project_id: p.clone(),
                rate_cents: Some(12000),
                currency: "EUR".into(),
                effective_from: Some("2026-03-02T10:00:00Z".to_owned()),
                confirm_retroactive: true,
            },
        )
        .unwrap();
        entry(&mut s, &t, "2026-03-02T09:00:00Z", "2026-03-02T09:20:00Z");
        s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        // 10 minutes at 60 and 10 at 120, of which the day leaves 15: 10 at 60 and 5 at 120.
        entry(&mut s, &t, "2026-03-02T09:50:00Z", "2026-03-02T10:10:00Z");
        let preview = s.summary_preview(&req(&p)).unwrap();
        assert_eq!(preview.billable_secs, 900);
        assert_eq!(preview.totals["EUR"], 1000 + 1000);
    }

    #[test]
    fn r8_a_row_with_no_time_in_it_bills_nothing() {
        use crate::model::RoundMethod::{Nearest, Up};
        let (mut s, p, t) = world(&project_input("A"));
        rule(&mut s, 0, Nearest);
        entry(&mut s, &t, "2026-03-02T09:00:00Z", "2026-03-02T09:20:00Z");
        s.create_summary(&ctx(NOW), &req(&p)).unwrap();
        rule(&mut s, 15, Up);
        // The day now rounds to 30 and 20 are issued, but this row has no time for that to be billed on.
        crate::billing::tests::seg(
            &mut s,
            &t,
            "empty",
            "2026-03-02T14:00:00Z",
            "2026-03-02T14:00:00Z",
        );
        let preview = s.summary_preview(&req(&p)).unwrap();
        assert_eq!(preview.rows.len(), 1);
        assert_eq!(preview.billable_secs, 0);
    }

    proptest::proptest! {
        /// Whatever the rule and however a day is cut into summaries, they come to what the report shows.
        #[test]
        fn r8_a_day_billed_in_any_parts_comes_to_what_the_report_shows(
            minutes in proptest::sample::select(vec![0u32, 1, 5, 6, 7, 10, 15, 30, 60]),
            method in proptest::sample::select(vec![
                crate::model::RoundMethod::Up,
                crate::model::RoundMethod::Down,
                crate::model::RoundMethod::Nearest,
            ]),
            parts in proptest::collection::vec(1i64..2000, 1..8),
            cuts in proptest::collection::vec(proptest::bool::ANY, 8),
        ) {
            let (mut s, p, t) = crate::billing::tests::fixture(Some((minutes, method)));
            rate_for(&mut s, &p);
            let mut at = ts("2026-03-02T22:00:00Z").secs();
            let mut waiting = false;
            for (i, secs) in parts.iter().enumerate() {
                let id = format!("s{i}");
                s.segments.insert(id.clone(), Segment {
                    id,
                    task_id: t.clone(),
                    device_id: crate::machine::tests::DEV.into(),
                    start: Ts(at),
                    end: Some(Ts(at + secs)),
                    ..Segment::default()
                });
                at += secs + 60;
                waiting = true;
                if cuts[i] {
                    s.create_summary(&at_device(), &req(&p)).unwrap();
                    waiting = false;
                }
            }
            if waiting {
                s.create_summary(&at_device(), &req(&p)).unwrap();
            }
            let (billed, report) = billed_and_report(&s, "UTC");
            proptest::prop_assert_eq!(billed, report);
        }
    }

    proptest::proptest! {
        /// A summary from another zone is shared out whole, whatever days its time falls on.
        #[test]
        fn r8_the_shares_of_a_summary_in_another_zone_add_up_to_its_rows(
            theirs in proptest::sample::select(vec!["UTC", "America/New_York", "Asia/Tokyo", "Australia/Lord_Howe"]),
            here in proptest::sample::select(vec!["UTC", "America/New_York", "Asia/Tokyo", "Australia/Lord_Howe"]),
            parts in proptest::collection::vec(
                (0i64..60_000, proptest::sample::select(vec![0i64, 0, 60, 1200, 3600, 40_000])),
                1..8,
            ),
            moved in proptest::collection::vec((proptest::sample::select(vec![0i64, 0, 30]), -2i64..=2), 8),
        ) {
            let (mut s, p, t) = crate::billing::tests::fixture(None);
            rate_for(&mut s, &p);
            let mut at_secs = ts("2026-03-02T00:00:00Z").secs();
            for (i, (gap, secs)) in parts.iter().enumerate() {
                at_secs += gap;
                let id = format!("s{i}");
                s.segments.insert(id.clone(), Segment {
                    id,
                    task_id: t.clone(),
                    device_id: crate::machine::tests::DEV.into(),
                    start: Ts(at_secs),
                    end: Some(Ts(at_secs + secs)),
                    ..Segment::default()
                });
                at_secs += secs;
            }
            let request = SummaryRequest { zone: theirs.to_owned(), ..req(&p) };
            let id = s.create_summary(&at_device(), &request).unwrap();
            let rows: i64 = s.summaries[&id].rows.iter().map(|r| r.billable_secs).sum();
            // Some of it arrives corrected from another device, moved days away or cut short.
            for (i, (cut, days)) in moved.iter().enumerate().take(parts.len()) {
                let seg = s.segments.get_mut(&format!("s{i}")).unwrap();
                let start = seg.start.secs() + days * 86_400;
                let end = (seg.end.unwrap().secs() + days * 86_400 - cut).max(start);
                if *cut != 0 || *days != 0 {
                    seg.edit = Some(crate::model::SegmentEdit {
                        start: Ts(start),
                        end: Ts(end),
                        voided: false,
                        reason: String::new(),
                        settled: false,
                    });
                }
            }
            let end = |x: &Segment| x.end.unwrap_or(x.start);
            let all = pieces(&s, &end);
            let held = s.held_by_task_day(&s.tasks, parse_zone(here).unwrap(), &all);
            proptest::prop_assert!(held.values().all(|v| *v >= 0));
            proptest::prop_assert_eq!(held.values().sum::<i64>(), rows);
        }
    }

    fn at_device() -> Ctx<'static> {
        at("pc-a")
    }
}
