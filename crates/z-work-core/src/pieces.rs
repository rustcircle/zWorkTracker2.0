//! What counts as time. Raw segments are never altered: corrections, away-time decisions and billable
//! pauses are overlays, and this module turns segment + overlays into the pieces every report reads.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::model::{BillablePause, GapDecision, Segment, Shared};
use crate::time::Ts;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PieceKind {
    /// Tracked time. Counts in Exact and Billable.
    Work,
    /// Away time the user marked as thinking. Counts in Exact and Billable, reported separately.
    Thinking,
    /// A pause the user marked as still working. Counts in Billable only, reported separately.
    BillablePause,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Piece {
    pub id: String,
    /// The segment this came from, or the pause record for a billable pause.
    pub source_id: String,
    pub task_id: String,
    pub device_id: Option<String>,
    pub start: Ts,
    pub end: Ts,
    /// The source segment is still open, and this is its last piece.
    pub open: bool,
    pub kind: PieceKind,
    pub subtask_id: Option<String>,
}

impl Piece {
    #[must_use]
    pub fn secs(&self) -> i64 {
        self.end.secs() - self.start.secs()
    }
}

fn subtract(intervals: &mut Vec<(Ts, Ts)>, cut: (Ts, Ts)) {
    let mut out = Vec::with_capacity(intervals.len() + 1);
    for &(s, e) in intervals.iter() {
        if cut.1 <= s || cut.0 >= e {
            out.push((s, e));
            continue;
        }
        if cut.0 > s {
            out.push((s, cut.0));
        }
        if cut.1 < e {
            out.push((cut.1, e));
        }
    }
    *intervals = out;
}

/// The pieces of every live task, in no particular order. `end_of` decides where an open segment ends.
#[must_use]
pub fn pieces(shared: &Shared, end_of: &dyn Fn(&Segment) -> Ts) -> Vec<Piece> {
    pieces_for_summary(shared, end_of, &[])
}

/// As `pieces`, with the pauses a new summary will list already in the place they take once it is made.
/// What it freezes is then what it goes on to hold, whichever period is billed first.
#[must_use]
pub(crate) fn pieces_for_summary(
    shared: &Shared,
    end_of: &dyn Fn(&Segment) -> Ts,
    listed: &[String],
) -> Vec<Piece> {
    let mut gaps_by_segment: BTreeMap<&str, Vec<(Ts, Ts, GapDecision)>> = BTreeMap::new();
    for g in shared.gaps.values() {
        gaps_by_segment
            .entry(g.segment_id.as_str())
            .or_default()
            .push((g.start, g.end, g.decision));
    }
    let mut out = Vec::with_capacity(shared.segments.len());
    let mut spans: BTreeMap<&str, Vec<(Ts, Ts)>> = BTreeMap::new();
    for seg in shared.segments.values() {
        if !shared.tasks.get(&seg.task_id).is_some_and(|t| t.is_live()) {
            continue;
        }
        let Some((start, end)) = seg.effective() else {
            continue;
        };
        let open = end.is_none();
        let end = end.unwrap_or_else(|| end_of(seg)).max(start);
        spans
            .entry(seg.task_id.as_str())
            .or_default()
            .push((start, end));
        let push = |out: &mut Vec<Piece>, n: usize, s: Ts, e: Ts, kind: PieceKind, last: bool| {
            out.push(Piece {
                id: if n == 0 {
                    seg.id.clone()
                } else {
                    format!("{}:{n}", seg.id)
                },
                source_id: seg.id.clone(),
                task_id: seg.task_id.clone(),
                device_id: Some(seg.device_id.clone()),
                start: s,
                end: e,
                open: open && last,
                kind,
                subtask_id: seg.subtask_id.clone(),
            });
        };
        let mut gaps = gaps_by_segment
            .get(seg.id.as_str())
            .cloned()
            .unwrap_or_default();
        gaps.retain(|g| g.2 != GapDecision::Keep);
        if gaps.is_empty() {
            push(&mut out, 0, start, end, PieceKind::Work, true);
            continue;
        }
        gaps.sort_by_key(|g| (g.0, g.1));
        let mut cursor = start;
        let mut n = 0;
        for (gs, ge, decision) in gaps {
            let gs = gs.max(cursor).min(end);
            let ge = ge.min(end);
            if ge <= gs {
                continue;
            }
            if gs > cursor {
                push(&mut out, n, cursor, gs, PieceKind::Work, false);
                n += 1;
            }
            if decision == GapDecision::Thinking {
                push(&mut out, n, gs, ge, PieceKind::Thinking, false);
                n += 1;
            }
            cursor = ge;
        }
        // An open segment always keeps a last piece, even if empty, so its timer has somewhere to run.
        if cursor < end || open || n == 0 {
            push(&mut out, n, cursor, end, PieceKind::Work, true);
        }
    }
    // Sorted by a replicated key, so every device gets the same result, and a span marked twice bills once.
    // A pause on a live summary goes first, earliest summary first, as each summary was worked out with the
    // ones before it holding their time. Otherwise a pause marked where a summary hadn't arrived would take a
    // share of what that summary holds, and go on to bill it a second time.
    let mut on_summary: BTreeMap<&str, (Ts, u32, &str)> = BTreeMap::new();
    for b in shared.summaries.values().filter(|b| !b.voided) {
        let claim = (b.created_at, b.number, b.id.as_str());
        for id in &b.pause_ids {
            let first = on_summary.entry(id.as_str()).or_insert(claim);
            *first = (*first).min(claim);
        }
    }
    let listed: BTreeSet<&str> = listed.iter().map(String::as_str).collect();
    let order = |p: &BillablePause| {
        let claim = on_summary.get(p.id.as_str()).copied();
        let next = listed.contains(p.id.as_str());
        (claim.is_none(), claim, !next, p.start, p.id.clone())
    };
    let mut live_pauses: Vec<_> = shared.pauses.values().filter(|p| !p.deleted).collect();
    live_pauses.sort_by_cached_key(|p| order(p));
    let mut billed_pauses: BTreeMap<&str, Vec<(Ts, Ts)>> = BTreeMap::new();
    for pause in live_pauses {
        let Some(task) = shared.tasks.get(&pause.task_id).filter(|t| t.is_live()) else {
            continue;
        };
        // A pause never bills time that is already counted, or dropped, inside a segment.
        let mut free = vec![(pause.start, pause.end)];
        for span in spans.get(task.id.as_str()).into_iter().flatten() {
            subtract(&mut free, *span);
        }
        for span in billed_pauses.get(task.id.as_str()).into_iter().flatten() {
            subtract(&mut free, *span);
        }
        billed_pauses
            .entry(pause.task_id.as_str())
            .or_default()
            .extend(free.iter().copied());
        for (i, (s, e)) in free.into_iter().enumerate() {
            out.push(Piece {
                id: format!("{}:{i}", pause.id),
                source_id: pause.id.clone(),
                task_id: pause.task_id.clone(),
                device_id: None,
                start: s,
                end: e,
                open: false,
                kind: PieceKind::BillablePause,
                subtask_id: None,
            });
        }
    }
    out
}

/// Seconds away with no decision yet, per task. They count nowhere until confirmed.
#[must_use]
pub fn unconfirmed_secs(shared: &Shared) -> BTreeMap<String, i64> {
    let mut out = BTreeMap::new();
    for g in shared.gaps.values() {
        if g.decision == GapDecision::Pending {
            *out.entry(g.task_id.clone()).or_insert(0) += (g.end.secs() - g.start.secs()).max(0);
        }
    }
    out
}
