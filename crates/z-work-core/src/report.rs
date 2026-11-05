//! Rows for the spreadsheet and CSV exports, sorted for reading. All user text stays plain text here.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use chrono::NaiveDate;
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::billing::{TaskDay, day_of, split_by_day, task_days_of};
use crate::error::CoreError;
use crate::model::{GapDecision, RemoteDevice, Segment, Shared, TaskState};
use crate::pieces::{Piece, PieceKind, pieces};
use crate::subtasks;
use crate::time::Ts;

/// What exports read: a consistent copy taken under the lock, so writing can happen outside it.
#[derive(Clone, Debug)]
pub struct ExportSource {
    pub data: Shared,
    pub device_id: String,
    pub remote_devices: BTreeMap<String, RemoteDevice>,
    pub now: Ts,
    /// When set, only these task-days exist as far as this source can tell.
    pub only: Option<Arc<Only>>,
}

/// A selection: the task-days kept, and the zone their days are in.
#[derive(Clone, Debug)]
pub struct Only {
    pub tz: Tz,
    pub keys: BTreeSet<(String, NaiveDate)>,
}

/// One Timeline entry: one task on one day, the same unit as an Excel row.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SelectedEntry {
    pub task_id: String,
    /// `YYYY-MM-DD`, in the request's zone.
    pub day: String,
}

const MAX_SELECTED: usize = 200_000;
const ANALYSIS_SETTINGS: [&str; 5] = [
    "clientZone",
    "clientWorkStart",
    "clientWorkEnd",
    "dailyTargetHours",
    "weeklyTargetHours",
];

impl ExportSource {
    /// A copy holding the selected task-days and nothing else, not even the other records' names. A
    /// selected task keeps its description whole, so Excel's Subtasks column shows its full list.
    /// Ids are checked against the store, and totals are worked out from there as usual.
    pub fn narrowed(&self, tz: Tz, entries: &[SelectedEntry]) -> Result<Self, CoreError> {
        if entries.is_empty() {
            return Err(CoreError::Validation(
                "select at least one entry to export".into(),
            ));
        }
        if entries.len() > MAX_SELECTED {
            return Err(CoreError::Validation("too many selected entries".into()));
        }
        let end = |s: &Segment| self.end_of(s);
        let exists: BTreeSet<(String, NaiveDate)> =
            task_days_of(&self.data, tz, pieces(&self.data, &end))
                .into_iter()
                .map(|td| (td.task_id, td.day))
                .collect();
        let mut keys = BTreeSet::new();
        for e in entries {
            let day = NaiveDate::parse_from_str(&e.day, "%Y-%m-%d")
                .map_err(|_| CoreError::Validation("a selected day is not a date".into()))?;
            let key = (e.task_id.clone(), day);
            if !exists.contains(&key) {
                return Err(CoreError::Validation(
                    "a selected entry has no time on that day any more; select again".into(),
                ));
            }
            keys.insert(key);
        }
        let tasks: BTreeSet<&str> = keys.iter().map(|k| k.0.as_str()).collect();
        let mut data = Shared {
            tasks: self
                .data
                .tasks
                .iter()
                .filter(|(id, _)| tasks.contains(id.as_str()))
                .map(|(k, v)| {
                    let mut task = v.clone();
                    // The day a task stopped is a day like any other: kept only if it was selected.
                    task.stopped_at = v
                        .stopped_at
                        .filter(|at| keys.contains(&(k.clone(), day_of(*at, tz))));
                    // Whether it's still running or paused is known only from its latest day. If that
                    // was left out, the selection can't say, so it isn't counted as in progress.
                    let latest = exists
                        .range((k.clone(), NaiveDate::MIN)..=(k.clone(), NaiveDate::MAX))
                        .next_back();
                    if matches!(v.state, TaskState::Running | TaskState::Paused)
                        && !latest.is_some_and(|key| keys.contains(key))
                    {
                        task.state = TaskState::Stopped;
                    }
                    (k.clone(), task)
                })
                .collect(),
            // What the analysis sheets read. Shifts are the user's schedule, not logged time, and the
            // sheets name only those on a selected day. Tags are only those the selected tasks carry.
            shifts: self.data.shifts.clone(),
            settings: self
                .data
                .settings
                .iter()
                .filter(|(k, _)| ANALYSIS_SETTINGS.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            ..Shared::default()
        };
        data.tags = self
            .data
            .tags
            .iter()
            .filter(|(id, _)| data.tasks.values().any(|t| t.tags.contains(id)))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let projects: BTreeSet<String> =
            data.tasks.values().map(|t| t.project_id.clone()).collect();
        data.projects = self
            .data
            .projects
            .iter()
            .filter(|(id, _)| projects.contains(*id))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        data.segments = self
            .data
            .segments
            .iter()
            .filter(|(_, s)| tasks.contains(s.task_id.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        data.gaps = self
            .data
            .gaps
            .iter()
            .filter(|(_, g)| tasks.contains(g.task_id.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        data.pauses = self
            .data
            .pauses
            .iter()
            .filter(|(_, p)| tasks.contains(p.task_id.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        data.pause_requests = self
            .data
            .pause_requests
            .iter()
            .filter(|(_, q)| tasks.contains(q.task_id.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        data.rates = self
            .data
            .rates
            .iter()
            .filter(|(_, r)| projects.contains(&r.project_id))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        data.rounding = self
            .data
            .rounding
            .iter()
            .filter(|(_, r)| r.project_id.as_ref().is_none_or(|p| projects.contains(p)))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        Ok(Self {
            data,
            device_id: self.device_id.clone(),
            remote_devices: self.remote_devices.clone(),
            now: self.now,
            only: Some(Arc::new(Only { tz, keys })),
        })
    }

    /// How many task-days a selection holds, or `None` when this is everything.
    #[must_use]
    pub fn selected_count(&self) -> Option<usize> {
        self.only.as_ref().map(|o| o.keys.len())
    }

    /// The parts of a stretch of a task's time inside the selection, one for each unbroken run of
    /// selected days, so nothing spans a day the selection left out.
    fn within_selection(&self, task_id: &str, start: Ts, end: Ts) -> Vec<(Ts, Ts)> {
        let Some(only) = &self.only else {
            return vec![(start, end)];
        };
        let mut out: Vec<(Ts, Ts)> = Vec::new();
        for (day, s, e) in split_by_day(start, end.max(start), only.tz) {
            if only.keys.contains(&(task_id.to_owned(), day)) {
                match out.last_mut() {
                    Some(last) if last.1 == s => last.1 = e,
                    _ => out.push((s, e)),
                }
            }
        }
        if out.is_empty()
            && end <= start
            && only
                .keys
                .contains(&(task_id.to_owned(), day_of(start, only.tz)))
        {
            out.push((start, start));
        }
        out
    }

    /// What counts as time, cut down to the selection when there is one, so a segment crossing
    /// midnight gives only its selected day.
    pub(crate) fn counted(&self) -> Vec<Piece> {
        let end = |s: &Segment| self.end_of(s);
        let all = pieces(&self.data, &end);
        let Some(only) = &self.only else {
            return all;
        };
        let mut out = Vec::with_capacity(all.len());
        for p in all {
            let parts: Vec<(Ts, Ts)> = split_by_day(p.start, p.end, only.tz)
                .into_iter()
                .filter(|(day, _, _)| only.keys.contains(&(p.task_id.clone(), *day)))
                .map(|(_, s, e)| (s, e))
                .collect();
            let mut merged: Vec<(Ts, Ts)> = Vec::new();
            for (s, e) in parts {
                match merged.last_mut() {
                    Some(last) if last.1 == s => last.1 = e,
                    _ => merged.push((s, e)),
                }
            }
            for (i, (s, e)) in merged.into_iter().enumerate() {
                let whole = s == p.start && e == p.end;
                out.push(Piece {
                    id: if whole {
                        p.id.clone()
                    } else {
                        format!("{}:d{i}", p.id)
                    },
                    start: s,
                    end: e,
                    open: p.open && whole,
                    ..p.clone()
                });
            }
        }
        out
    }

    /// Own open segments run to now. Another device's open segment counts only up to its last
    /// heartbeat, since nothing newer is known about it.
    #[must_use]
    pub fn end_of(&self, seg: &Segment) -> Ts {
        match seg.end {
            Some(e) => e.max(seg.start),
            None if seg.device_id == self.device_id => self.now.max(seg.start),
            None => {
                let beat = self
                    .remote_devices
                    .get(&seg.device_id)
                    .and_then(|d| d.last_heartbeat)
                    .map_or(seg.start, |h| h.max(seg.start));
                // Asked to pause: nothing after that request counts, whatever that device says.
                self.data
                    .pause_requests
                    .values()
                    .filter(|q| q.segment_id == seg.id)
                    .map(|q| q.at)
                    .min()
                    .map_or(beat, |at| beat.min(at).max(seg.start))
            }
        }
    }

    #[must_use]
    pub fn rows(&self, tz: Tz) -> Vec<DayRow> {
        let mut rows: Vec<DayRow> = task_days_of(&self.data, tz, self.counted())
            .into_iter()
            .map(|td| {
                let project = self.data.projects.get(&td.project_id);
                let task = self.data.tasks.get(&td.task_id);
                DayRow {
                    project_name: project
                        .map_or_else(|| "(unknown project)".to_owned(), |p| p.name.clone()),
                    project_color: project
                        .map_or_else(|| "#808080".to_owned(), |p| p.color.clone()),
                    task_name: task.map_or_else(String::new, |t| t.name.clone()),
                    subtasks: task.map_or_else(String::new, |t| subtasks::extract(&t.description)),
                    row: td,
                }
            })
            .collect();
        rows.sort_by(|a, b| {
            a.project_name
                .to_lowercase()
                .cmp(&b.project_name.to_lowercase())
                .then_with(|| a.project_name.cmp(&b.project_name))
                .then_with(|| a.row.day.cmp(&b.row.day))
                .then_with(|| a.row.first_start.cmp(&b.row.first_start))
                .then_with(|| a.row.task_id.cmp(&b.row.task_id))
        });
        rows
    }

    /// What counts as time, in start order, with names resolved. Raw times are alongside.
    #[must_use]
    pub fn segments(&self) -> Vec<SegmentRow> {
        let mut rows: Vec<SegmentRow> = self
            .counted()
            .into_iter()
            .filter_map(|p| {
                let task = self.data.tasks.get(&p.task_id).filter(|t| t.is_live())?;
                let project = self.data.projects.get(&task.project_id);
                let raw = self.data.segments.get(&p.source_id);
                // A selection shows nothing of a day it left out, raw times included: they're cut to
                // the selected piece, or left out when a correction moved the time elsewhere.
                let (raw_start, raw_end) = match raw {
                    Some(r) if self.only.is_some() => {
                        if r.start < p.end && r.end.is_none_or(|e| e > p.start) {
                            (Some(r.start.max(p.start)), r.end.map(|e| e.min(p.end)))
                        } else {
                            (None, None)
                        }
                    }
                    Some(r) => (Some(r.start), r.end),
                    None => (None, None),
                };
                Some(SegmentRow {
                    segment_id: p.id.clone(),
                    task_name: task.name.clone(),
                    project_id: task.project_id.clone(),
                    project_name: project
                        .map_or_else(|| "(unknown project)".to_owned(), |x| x.name.clone()),
                    start: p.start,
                    end: (!p.open).then_some(p.end),
                    effective_end: p.end,
                    kind: p.kind,
                    raw_start,
                    raw_end,
                })
            })
            .collect();
        rows.sort_by(|a, b| {
            a.start
                .cmp(&b.start)
                .then_with(|| a.segment_id.cmp(&b.segment_id))
        });
        rows
    }

    /// Everything laid over the raw record, for the exports' Adjustments sheet.
    #[must_use]
    pub fn adjustments(&self) -> Vec<AdjustmentRow> {
        let task_of = |id: &str| {
            self.data
                .tasks
                .get(id)
                .filter(|t| t.is_live())
                .map(|t| t.name.clone())
        };
        let mut rows = Vec::new();
        for g in self.data.gaps.values() {
            let Some(task_name) = task_of(&g.task_id) else {
                continue;
            };
            let kind = match g.decision {
                GapDecision::Pending => "Away, not confirmed (not counted)",
                GapDecision::Keep => "Away, kept",
                GapDecision::Drop => "Away, dropped (not counted)",
                GapDecision::Thinking => "Away, thinking",
            };
            for (start, end) in self.within_selection(&g.task_id, g.start, g.end) {
                rows.push(AdjustmentRow {
                    kind,
                    task_name: task_name.clone(),
                    start,
                    end,
                    note: String::new(),
                });
            }
        }
        for p in self.data.pauses.values().filter(|p| !p.deleted) {
            let Some(task_name) = task_of(&p.task_id) else {
                continue;
            };
            for (start, end) in self.within_selection(&p.task_id, p.start, p.end) {
                rows.push(AdjustmentRow {
                    kind: "Billable pause",
                    task_name: task_name.clone(),
                    start,
                    end,
                    note: p.note.clone(),
                });
            }
        }
        for s in self.data.segments.values() {
            let Some(task_name) = task_of(&s.task_id) else {
                continue;
            };
            let end = s.end.unwrap_or(s.start);
            if let Some(e) = &s.edit {
                // The recorded times could name a day the selection left out.
                let note = if self.only.is_some() {
                    e.reason.trim().to_owned()
                } else {
                    format!("{} Recorded {} to {}.", e.reason, s.start.iso(), end.iso())
                        .trim()
                        .to_owned()
                };
                for (from, to) in self.within_selection(&s.task_id, e.start, e.end) {
                    rows.push(AdjustmentRow {
                        kind: if e.voided { "Voided" } else { "Corrected" },
                        task_name: task_name.clone(),
                        start: from,
                        end: to,
                        note: note.clone(),
                    });
                }
            }
            if s.manual {
                for (from, to) in self.within_selection(&s.task_id, s.start, end) {
                    rows.push(AdjustmentRow {
                        kind: "Entered by hand",
                        task_name: task_name.clone(),
                        start: from,
                        end: to,
                        note: String::new(),
                    });
                }
            }
        }
        rows.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| a.kind.cmp(b.kind)));
        rows
    }
}

#[derive(Clone, Debug)]
pub struct AdjustmentRow {
    pub kind: &'static str,
    pub task_name: String,
    pub start: Ts,
    pub end: Ts,
    pub note: String,
}

#[derive(Clone, Debug)]
pub struct DayRow {
    pub project_name: String,
    pub project_color: String,
    pub task_name: String,
    pub subtasks: String,
    pub row: TaskDay,
}

#[derive(Clone, Debug)]
pub struct SegmentRow {
    pub segment_id: String,
    pub task_name: String,
    pub project_id: String,
    pub project_name: String,
    pub start: Ts,
    /// `None` while the segment is still open.
    pub end: Option<Ts>,
    pub effective_end: Ts,
    pub kind: PieceKind,
    pub raw_start: Option<Ts>,
    pub raw_end: Option<Ts>,
}

/// Chosen in the export dialog. Zones are validated before use.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRequest {
    pub day_zone: String,
    pub my_zone: String,
    #[serde(default)]
    pub client_zone: Option<String>,
    #[serde(default)]
    pub include_earnings: bool,
    /// Segment id to shift attribution text, computed by the UI's shift module.
    #[serde(default)]
    pub segment_shifts: BTreeMap<String, String>,
    /// Summary sheets computed by the UI's analytics modules, so they match the screen exactly. A
    /// selection ignores these: its sheets are worked out here, from the selected entries.
    #[serde(default)]
    pub analytics: Vec<AnalyticsSheet>,
    /// Export only these Timeline entries. Their days are in `day_zone`.
    #[serde(default)]
    pub selection: Option<Vec<SelectedEntry>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CellKind {
    Text,
    Int,
    Number,
    Percent,
    /// Seconds, written as an Excel duration.
    Duration,
    /// Exact amount in cents, written with a currency format.
    Eur,
    Usd,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsColumn {
    pub header: String,
    pub kind: CellKind,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsSheet {
    pub name: String,
    pub title: String,
    pub columns: Vec<AnalyticsColumn>,
    pub rows: Vec<Vec<serde_json::Value>>,
    /// Sheets holding prices are left out of a client-facing export.
    #[serde(default)]
    pub contains_prices: bool,
}

/// `2026-03-08 09:00:00 -04:00`. The offset keeps repeated fall-back hours unambiguous.
#[must_use]
pub fn zoned(t: Ts, tz: Tz) -> String {
    t.datetime()
        .with_timezone(&tz)
        .format("%Y-%m-%d %H:%M:%S %:z")
        .to_string()
}

#[must_use]
pub fn hms(secs: i64) -> String {
    format!("{}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
}

/// Whole hours and minutes. Seconds are dropped, not rounded.
#[must_use]
pub fn hm(secs: i64) -> String {
    format!("{}:{:02}", secs / 3600, (secs % 3600) / 60)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::billing::tests::{fixture, seg};
    use crate::machine::tests::{DEV, ctx, project_input, task_input};
    use crate::model::RoundMethod;
    use crate::validate::parse_zone;

    pub fn source(data: Shared, now: &str) -> ExportSource {
        ExportSource {
            only: None,
            data,
            device_id: DEV.into(),
            remote_devices: BTreeMap::new(),
            now: Ts::parse(now).unwrap(),
        }
    }

    #[test]
    fn sorted_by_project_day_then_start_with_a_row_per_task_per_day() {
        let (mut s, p, t) = fixture(Some((0, RoundMethod::Nearest)));
        let c = ctx("2026-01-01T00:00:00Z");
        let other = s.create_project(&c, &project_input("aardvark")).unwrap();
        let t2 = s.create_task(&c, &task_input("Other", &other)).unwrap();
        let t3 = s.create_task(&c, &task_input("Second", &p)).unwrap();
        seg(
            &mut s,
            &t,
            "a",
            "2026-02-03T09:00:00Z",
            "2026-02-03T10:00:00Z",
        );
        seg(
            &mut s,
            &t3,
            "b",
            "2026-02-02T15:00:00Z",
            "2026-02-02T15:30:00Z",
        );
        seg(
            &mut s,
            &t,
            "c",
            "2026-02-02T09:00:00Z",
            "2026-02-02T09:15:00Z",
        );
        seg(
            &mut s,
            &t,
            "d",
            "2026-02-02T11:00:00Z",
            "2026-02-02T11:15:00Z",
        );
        seg(
            &mut s,
            &t2,
            "e",
            "2026-02-09T09:00:00Z",
            "2026-02-09T09:15:00Z",
        );
        let rows = source(s, "2026-03-01T00:00:00Z").rows(parse_zone("UTC").unwrap());
        let got: Vec<_> = rows
            .iter()
            .map(|r| {
                (
                    r.project_name.as_str(),
                    r.row.day.to_string(),
                    r.task_name.as_str(),
                    r.row.exact_secs,
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("aardvark", "2026-02-09".to_string(), "Other", 900),
                ("Alpha", "2026-02-02".to_string(), "Write", 1800),
                ("Alpha", "2026-02-02".to_string(), "Second", 1800),
                ("Alpha", "2026-02-03".to_string(), "Write", 3600),
            ]
        );
    }

    #[test]
    fn day_follows_the_zone() {
        let (mut s, _, t) = fixture(Some((0, RoundMethod::Nearest)));
        seg(
            &mut s,
            &t,
            "a",
            "2026-02-03T02:00:00Z",
            "2026-02-03T03:00:00Z",
        );
        let src = source(s, "2026-03-01T00:00:00Z");
        assert_eq!(
            src.rows(parse_zone("UTC").unwrap())[0].row.day.to_string(),
            "2026-02-03"
        );
        assert_eq!(
            src.rows(parse_zone("America/Los_Angeles").unwrap())[0]
                .row
                .day
                .to_string(),
            "2026-02-02"
        );
        assert_eq!(
            src.rows(parse_zone("Asia/Kathmandu").unwrap())[0]
                .row
                .day
                .to_string(),
            "2026-02-03"
        );
    }

    #[test]
    fn open_segments_end_now_when_own_and_at_heartbeat_when_remote() {
        let (mut s, _, t) = fixture(Some((0, RoundMethod::Nearest)));
        s.segments.insert(
            "o".into(),
            Segment {
                id: "o".into(),
                task_id: t.clone(),
                device_id: DEV.into(),
                start: Ts::parse("2026-02-02T09:00:00Z").unwrap(),
                end: None,
                ..Segment::default()
            },
        );
        s.segments.insert(
            "r".into(),
            Segment {
                id: "r".into(),
                task_id: t,
                device_id: "other".into(),
                start: Ts::parse("2026-02-02T09:00:00Z").unwrap(),
                end: None,
                ..Segment::default()
            },
        );
        let mut src = source(s, "2026-02-02T09:10:00Z");
        src.remote_devices.insert(
            "other".into(),
            RemoteDevice {
                name: "PC2".into(),
                last_heartbeat: Ts::parse("2026-02-02T09:05:00Z"),
                written_at: Ts(0),
            },
        );
        let segs = src.segments();
        let by = |id: &str| {
            segs.iter()
                .find(|x| x.segment_id == id)
                .unwrap()
                .effective_end
                .iso()
        };
        assert_eq!(by("o"), "2026-02-02T09:10:00Z");
        assert_eq!(by("r"), "2026-02-02T09:05:00Z");
    }

    mod selection {
        use super::*;
        use crate::billing::tests::{fixture, seg};
        use crate::machine::tests::{ctx, task_input};
        use crate::machine::{RateInput, RoundingInput};
        use crate::model::RoundMethod;

        const UTC: Tz = chrono_tz::UTC;

        fn entry(task: &str, day: &str) -> SelectedEntry {
            SelectedEntry {
                task_id: task.to_owned(),
                day: day.to_owned(),
            }
        }

        /// Alpha has `Write` on 1 and 2 March and one stretch across midnight into 3 March, and
        /// `Secret` on 1 March.
        fn world() -> (ExportSource, String, String) {
            let (mut s, p, t) = fixture(None);
            let c = ctx("2026-01-01T00:00:00Z");
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
            s.set_rounding(
                &c,
                &RoundingInput {
                    project_id: None,
                    effective_from: "2000-01-01".into(),
                    increment_minutes: 15,
                    method: RoundMethod::Up,
                    confirm_retroactive: true,
                },
                "2026-06-01",
            )
            .unwrap();
            let other = s.create_task(&c, &task_input("Secret", &p)).unwrap();
            seg(
                &mut s,
                &t,
                "a",
                "2026-03-01T09:00:00Z",
                "2026-03-01T09:10:00Z",
            );
            seg(
                &mut s,
                &t,
                "b",
                "2026-03-02T09:00:00Z",
                "2026-03-02T10:00:00Z",
            );
            seg(
                &mut s,
                &t,
                "m",
                "2026-03-02T23:50:00Z",
                "2026-03-03T00:10:00Z",
            );
            seg(
                &mut s,
                &other,
                "z",
                "2026-03-01T11:00:00Z",
                "2026-03-01T12:00:00Z",
            );
            (source(s, "2026-04-01T00:00:00Z"), t, other)
        }

        #[test]
        fn empty_unknown_and_timeless_selections_are_refused() {
            let (src, t, _) = world();
            assert!(src.narrowed(UTC, &[]).is_err());
            assert!(src.narrowed(UTC, &[entry("nope", "2026-03-01")]).is_err());
            assert!(src.narrowed(UTC, &[entry(&t, "2026-03-09")]).is_err());
            assert!(src.narrowed(UTC, &[entry(&t, "not a day")]).is_err());
        }

        #[test]
        fn figures_match_the_same_rows_of_a_full_export() {
            let (src, t, other) = world();
            let full = src.rows(UTC);
            let only = src
                .narrowed(UTC, &[entry(&t, "2026-03-01"), entry(&other, "2026-03-01")])
                .unwrap();
            let rows = only.rows(UTC);
            assert_eq!(rows.len(), 2);
            for r in &rows {
                let same = full
                    .iter()
                    .find(|x| x.row.task_id == r.row.task_id && x.row.day == r.row.day)
                    .unwrap();
                assert_eq!(r.row.exact_secs, same.row.exact_secs);
                assert_eq!(r.row.billable_secs, same.row.billable_secs);
                assert_eq!(r.row.numerators(), same.row.numerators());
            }
        }

        #[test]
        fn a_multi_day_task_can_be_partly_selected_and_a_segment_across_midnight_is_cut() {
            let (src, t, _) = world();
            let only = src.narrowed(UTC, &[entry(&t, "2026-03-03")]).unwrap();
            let rows = only.rows(UTC);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].row.exact_secs, 600);
            let segs = only.segments();
            assert_eq!(segs.len(), 1);
            assert_eq!(segs[0].start.iso(), "2026-03-03T00:00:00Z");
            assert_eq!(
                segs[0].raw_start.unwrap().iso(),
                "2026-03-03T00:00:00Z",
                "no earlier day"
            );
            assert_eq!(segs[0].raw_end.unwrap().iso(), "2026-03-03T00:10:00Z");
        }

        #[test]
        fn nothing_unselected_reaches_the_csv_and_it_says_it_is_a_selection() {
            let (src, t, _) = world();
            let request = ExportRequest {
                day_zone: "UTC".into(),
                my_zone: "UTC".into(),
                client_zone: None,
                include_earnings: true,
                segment_shifts: BTreeMap::new(),
                analytics: Vec::new(),
                selection: Some(vec![entry(&t, "2026-03-01")]),
            };
            let csv = crate::csv::build(&src, &request).unwrap();
            assert!(!csv.contains("Secret"));
            assert!(!csv.contains("2026-03-02"));
            assert!(!csv.contains("2026-03-03"));
            assert!(csv.contains("selection of 1 task-day"));
            let none = ExportRequest {
                selection: Some(Vec::new()),
                ..request
            };
            assert!(crate::csv::build(&src, &none).is_err());
        }

        /// Gives `Write` a ticked subtask worked on 2 March alone, so never on a day selected below.
        fn add_subtask_of_another_day(src: &mut ExportSource, t: &str) {
            src.data.tasks.get_mut(t).unwrap().description = serde_json::json!({"type":"doc","content":[
                {"type":"taskList","content":[
                    {"type":"taskItem","attrs":{"checked":true,"subtaskId":"sub-other-day"},"content":[
                        {"type":"paragraph","content":[{"type":"text","text":"Subtask of another day"}]}
                    ]}
                ]}
            ]});
            src.data.segments.get_mut("b").unwrap().subtask_id = Some("sub-other-day".into());
        }

        #[test]
        fn a_selected_excel_row_lists_every_subtask_and_the_about_sheet_says_so() {
            let (mut src, t, _) = world();
            add_subtask_of_another_day(&mut src, &t);
            let request = ExportRequest {
                day_zone: "UTC".into(),
                my_zone: "UTC".into(),
                client_zone: None,
                include_earnings: true,
                segment_shifts: BTreeMap::new(),
                analytics: Vec::new(),
                selection: Some(vec![entry(&t, "2026-03-01")]),
            };
            let only = src.narrowed(UTC, &[entry(&t, "2026-03-01")]).unwrap();
            assert_eq!(only.rows(UTC)[0].subtasks, "[x] Subtask of another day");

            let dir = tempfile::tempdir().unwrap();
            let zip = dir.path().join("x.xlsx");
            std::fs::write(&zip, crate::xlsx::build(&src, &request).unwrap()).unwrap();
            let out = std::process::Command::new("unzip")
                .arg("-p")
                .arg(&zip)
                .arg("xl/sharedStrings.xml")
                .output()
                .unwrap();
            let strings = String::from_utf8_lossy(&out.stdout);
            assert!(strings.contains("[x] Subtask of another day"));
            assert!(strings.contains("Time and money on every sheet cover those entries only."));
            assert!(strings.contains("full subtask list and ticks as of the export"));
            assert!(strings.contains("subtasks worked on days outside the selection"));
            assert!(!strings.contains("Every sheet"));
        }

        /// The dialog and README say only Excel has this column. If CSV gains it, they must change.
        #[test]
        fn a_selected_csv_names_no_subtask() {
            let (mut src, t, _) = world();
            add_subtask_of_another_day(&mut src, &t);
            let request = ExportRequest {
                day_zone: "UTC".into(),
                my_zone: "UTC".into(),
                client_zone: None,
                include_earnings: true,
                segment_shifts: BTreeMap::new(),
                analytics: Vec::new(),
                selection: Some(vec![entry(&t, "2026-03-01")]),
            };
            let csv = crate::csv::build(&src, &request).unwrap();
            assert!(!csv.contains("Subtask of another day"));
        }

        #[test]
        fn the_workbook_builds_from_a_selection_with_its_own_analytics() {
            let (mut src, t, other) = world();
            add_subtask_of_another_day(&mut src, &t);
            // What `scripts/verify_selection.py` looks for: names that must and must not get in.
            let stamp = crate::model::Stamp::default();
            for (id, name) in [("tag-shown", "Shown tag"), ("tag-hidden", "Hidden tag")] {
                src.data.tags.insert(
                    id.into(),
                    crate::model::Tag {
                        id: id.into(),
                        name: name.into(),
                        color: "#abcdef".into(),
                        deleted: false,
                        stamp: stamp.clone(),
                    },
                );
            }
            src.data.tasks.get_mut(&t).unwrap().tags = vec!["tag-shown".into()];
            src.data.tasks.get_mut(&other).unwrap().tags = vec!["tag-hidden".into()];
            let c = ctx("2026-01-01T00:00:00Z");
            let mut project = project_input("LeakProject");
            project.color = "#123456".into();
            let hidden_project = src.data.create_project(&c, &project).unwrap();
            let hidden = src
                .data
                .create_task(&c, &task_input("LeakTask", &hidden_project))
                .unwrap();
            src.data
                .set_rate(
                    &c,
                    &RateInput {
                        project_id: hidden_project,
                        rate_cents: Some(9999),
                        currency: "USD".into(),
                        effective_from: Some("2026-01-01T00:00:00Z".to_owned()),
                        confirm_retroactive: true,
                    },
                )
                .unwrap();
            seg(
                &mut src.data,
                &hidden,
                "h",
                "2026-03-01T13:00:00Z",
                "2026-03-01T14:00:00Z",
            );
            // 1 March 2026 is a Sunday. One shift runs that day, and one only on weekdays.
            for (id, name, days) in [
                ("sh-sun", "Sunday shift", vec![7]),
                ("sh-wk", "Weekday shift", vec![1, 2, 3, 4, 5]),
            ] {
                src.data.shifts.insert(
                    id.into(),
                    crate::model::ShiftVersion {
                        id: id.into(),
                        shift_id: id.into(),
                        effective_from: "2020-01-01".into(),
                        name: name.into(),
                        color: "#0ea5e9".into(),
                        start: "08:50".into(),
                        end: "12:00".into(),
                        weekdays: days,
                        zone: "UTC".into(),
                        break_minutes: 0,
                        retired: false,
                        deleted: false,
                        stamp: stamp.clone(),
                    },
                );
            }
            let request = ExportRequest {
                day_zone: "UTC".into(),
                my_zone: "UTC".into(),
                client_zone: None,
                include_earnings: true,
                segment_shifts: BTreeMap::new(),
                analytics: vec![AnalyticsSheet {
                    name: "Leak".into(),
                    title: "Secret".into(),
                    columns: Vec::new(),
                    rows: Vec::new(),
                    contains_prices: false,
                }],
                selection: Some(vec![entry(&t, "2026-03-01")]),
            };
            let bytes = crate::xlsx::build(&src, &request).unwrap();
            assert!(bytes.starts_with(b"PK"));
            if let Ok(path) = std::env::var("ZWT_SAMPLE_SELECTION_XLSX") {
                std::fs::write(path, &bytes).unwrap();
            }
        }

        #[test]
        fn a_remote_open_segment_counts_the_same_when_selected() {
            let (mut src, t, _) = world();
            src.data.segments.insert(
                "open".into(),
                Segment {
                    id: "open".into(),
                    task_id: t.clone(),
                    device_id: "pc-b".into(),
                    start: Ts::parse("2026-03-05T09:00:00Z").unwrap(),
                    end: None,
                    ..Segment::default()
                },
            );
            src.remote_devices.insert(
                "pc-b".into(),
                RemoteDevice {
                    name: "B".into(),
                    last_heartbeat: Ts::parse("2026-03-05T10:00:00Z"),
                    written_at: Ts::parse("2026-03-05T10:00:00Z").unwrap(),
                },
            );
            let full = src.rows(UTC);
            let same = full
                .iter()
                .find(|r| r.row.day.to_string() == "2026-03-05")
                .unwrap();
            let only = src.narrowed(UTC, &[entry(&t, "2026-03-05")]).unwrap();
            let rows = only.rows(UTC);
            assert_eq!(rows[0].row.exact_secs, 3600);
            assert_eq!(rows[0].row.exact_secs, same.row.exact_secs);
        }

        #[test]
        fn a_correction_shows_no_recorded_time_from_a_day_left_out() {
            let (mut src, t, _) = world();
            let seg = src.data.segments.get_mut("b").unwrap();
            seg.edit = Some(crate::model::SegmentEdit {
                start: Ts::parse("2026-03-01T14:00:00Z").unwrap(),
                end: Ts::parse("2026-03-01T15:00:00Z").unwrap(),
                voided: false,
                reason: String::new(),
                settled: false,
            });
            let only = src.narrowed(UTC, &[entry(&t, "2026-03-01")]).unwrap();
            for row in only.segments() {
                for raw in [row.raw_start, row.raw_end].into_iter().flatten() {
                    assert!(raw.iso().starts_with("2026-03-01"), "{}", raw.iso());
                }
            }
        }

        #[test]
        fn an_adjustment_across_a_day_left_out_becomes_one_row_per_run() {
            let (mut src, t, _) = world();
            src.data.pauses.insert(
                "long".into(),
                crate::model::BillablePause {
                    id: "long".into(),
                    task_id: t.clone(),
                    start: Ts::parse("2026-03-01T12:00:00Z").unwrap(),
                    end: Ts::parse("2026-03-03T01:00:00Z").unwrap(),
                    note: String::new(),
                    deleted: false,
                    stamp: crate::model::Stamp::default(),
                },
            );
            let only = src
                .narrowed(UTC, &[entry(&t, "2026-03-01"), entry(&t, "2026-03-03")])
                .unwrap();
            let rows: Vec<_> = only
                .adjustments()
                .into_iter()
                .filter(|r| r.kind == "Billable pause")
                .collect();
            assert_eq!(rows.len(), 2);
            assert!(rows[0].end.iso().starts_with("2026-03-02T00:00"));
            assert!(rows[1].start.iso().starts_with("2026-03-03T00:00"));
        }

        #[test]
        fn a_pause_request_caps_what_a_remote_open_segment_counts() {
            let (mut src, t, _) = world();
            src.data.segments.insert(
                "open".into(),
                Segment {
                    id: "open".into(),
                    task_id: t.clone(),
                    device_id: "pc-b".into(),
                    start: Ts::parse("2026-03-05T09:00:00Z").unwrap(),
                    end: None,
                    ..Segment::default()
                },
            );
            src.remote_devices.insert(
                "pc-b".into(),
                RemoteDevice {
                    name: "B".into(),
                    last_heartbeat: Ts::parse("2026-03-05T12:00:00Z"),
                    written_at: Ts::parse("2026-03-05T12:00:00Z").unwrap(),
                },
            );
            let seg = src.data.segments["open"].clone();
            assert_eq!(src.end_of(&seg).iso(), "2026-03-05T12:00:00Z");
            src.data.pause_requests.insert(
                "q".into(),
                crate::model::PauseRequest {
                    id: "q".into(),
                    task_id: t.clone(),
                    segment_id: "open".into(),
                    requested_by: "pc-a".into(),
                    at: Ts::parse("2026-03-05T10:00:00Z").unwrap(),
                    stamp: crate::model::Stamp::default(),
                },
            );
            assert_eq!(src.end_of(&seg).iso(), "2026-03-05T10:00:00Z");
        }
    }
}
