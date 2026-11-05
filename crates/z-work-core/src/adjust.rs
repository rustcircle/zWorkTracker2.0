//! Manual entries, corrections, away-time decisions and billable pauses. Each one writes to the audit
//! trail, and none of them changes a segment's raw times.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::error::CoreError;
use crate::machine::Ctx;
use crate::model::{
    AuditEntry, BillablePause, GapDecision, GapKind, IdleGap, PauseReason, Segment, SegmentEdit,
    Shared, TaskState,
};
use crate::time::Ts;
use crate::validate::{MAX_SEGMENT_SECS, check_chosen_date};

const MAX_REASON: usize = 500;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualEntryInput {
    pub task_id: String,
    pub start: Ts,
    pub end: Ts,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub subtask_id: Option<String>,
    /// The user saw the overlap warning and wants the entry anyway.
    #[serde(default)]
    pub allow_overlap: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CorrectionInput {
    pub segment_id: String,
    pub start: Ts,
    pub end: Ts,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub allow_overlap: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PauseInput {
    pub task_id: String,
    pub start: Ts,
    pub end: Ts,
    #[serde(default)]
    pub note: String,
}

fn clean_reason(raw: &str) -> Result<String, CoreError> {
    let reason = raw.trim();
    if reason.chars().count() > MAX_REASON {
        return Err(CoreError::Validation("the reason is too long".into()));
    }
    if reason.chars().any(char::is_control) {
        return Err(CoreError::Validation(
            "the reason has characters that can't be stored".into(),
        ));
    }
    Ok(reason.to_owned())
}

fn check_range(ctx: &Ctx<'_>, start: Ts, end: Ts) -> Result<(), CoreError> {
    if end <= start {
        return Err(CoreError::Validation(
            "the end must be after the start".into(),
        ));
    }
    if end > ctx.now {
        return Err(CoreError::Validation(
            "the end can't be in the future".into(),
        ));
    }
    check_chosen_date(start, "the start")?;
    check_chosen_date(end, "the end")?;
    if end.secs() - start.secs() > MAX_SEGMENT_SECS {
        return Err(CoreError::Validation("that span is too long".into()));
    }
    Ok(())
}

pub(crate) fn seg_json(s: &Segment) -> Value {
    json!({
        "rawStart": s.start,
        "rawEnd": s.end,
        "edit": s.edit,
        "subtaskId": s.subtask_id,
    })
}

/// What the owner of a segment has to hear about a settlement, see `Shared::reclaim_settled`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reclaimed {
    /// Its own record replaced the settlement, and holds this much more time.
    Returned {
        task_name: String,
        settled_by: String,
        secs: i64,
    },
    /// The settled time is on a summary, so it stays, this far from what the owner recorded.
    OnSummary {
        task_name: String,
        reference: String,
        secs: i64,
    },
}

/// Where a settlement ends. `known` is what the reports count, `ExportSource::end_of`. A lease beat is fresher, as
/// device files only travel with a sync: pass it only when it shows this segment's timer was running. Nothing after
/// a pause request counts, as in `end_of`.
#[must_use]
pub fn settled_end(seg: &Segment, known: Ts, beat: Option<Ts>, asked: Option<Ts>, now: Ts) -> Ts {
    let shown = beat.map_or(known, |b| known.max(asked.map_or(b, |a| b.min(a))));
    shown.min(now).max(seg.start)
}

impl Shared {
    #[expect(
        clippy::too_many_arguments,
        reason = "one flat audit record; a builder adds nothing"
    )]
    pub fn audit(
        &mut self,
        ctx: &Ctx<'_>,
        entity: &str,
        entity_id: &str,
        action: &str,
        reason: &str,
        before: Value,
        after: Value,
    ) {
        let id = Uuid::new_v4().to_string();
        self.audit_log.insert(
            id.clone(),
            AuditEntry {
                id,
                at: ctx.now,
                by: ctx.device.to_owned(),
                entity: entity.to_owned(),
                entity_id: entity_id.to_owned(),
                action: action.to_owned(),
                reason: reason.to_owned(),
                before,
                after,
            },
        );
    }

    fn is_billed(&self, segment_id: &str) -> bool {
        self.summaries
            .values()
            .any(|b| !b.voided && b.segment_ids.iter().any(|s| s == segment_id))
    }

    /// Other time logged on this device that the span overlaps. Segments on other PCs may overlap.
    fn overlapping(&self, device: &str, except: Option<&str>, start: Ts, end: Ts) -> Vec<String> {
        self.segments
            .values()
            .filter(|s| s.device_id == device && Some(s.id.as_str()) != except)
            .filter(|s| {
                s.effective()
                    .is_some_and(|(a, b)| a < end && b.is_none_or(|b| b > start))
            })
            .filter_map(|s| self.tasks.get(&s.task_id))
            .filter(|t| t.is_live())
            .map(|t| t.name.clone())
            .collect()
    }

    fn refuse_overlap(&self, names: &[String]) -> Result<(), CoreError> {
        if names.is_empty() {
            return Ok(());
        }
        Err(CoreError::Overlap(names.join(", ")))
    }

    /// A pause over one on a live summary would be a second claim on time that summary holds. This is refused
    /// rather than cut to fit, so what is stored is what the user typed, and they hear why. A device that
    /// hasn't seen the summary yet can't refuse, and `pieces` leaves the billed pause its claim.
    fn refuse_over_billed_pause(&self, task_id: &str, start: Ts, end: Ts) -> Result<(), CoreError> {
        for b in self.summaries.values().filter(|b| !b.voided) {
            let over = b
                .pause_ids
                .iter()
                .filter_map(|id| self.pauses.get(id))
                .any(|p| !p.deleted && p.task_id == task_id && p.start < end && p.end > start);
            if over {
                return Err(CoreError::Conflict(format!(
                    "this time overlaps a billable pause on summary {}; void that summary first",
                    b.reference
                )));
            }
        }
        Ok(())
    }

    /// Time logged over a pause on a live summary would be billed a second time: the summary keeps what it
    /// holds, and the new record takes the pause's share. Refused for the same reasons as a second pause. Only
    /// what the summary holds is at stake, not the pause's whole span, as work may have covered part of it all
    /// along, so this tries the change on a copy. A device that hasn't seen the summary yet can't refuse, and
    /// `pause_time_short` tells the user once both have met.
    fn refuse_taking_from_billed_pause(&self, changed: &Segment) -> Result<(), CoreError> {
        let holds_one = self
            .summaries
            .values()
            .filter(|b| !b.voided)
            .flat_map(|b| &b.pause_ids)
            .filter_map(|id| self.pauses.get(id))
            .any(|p| p.task_id == changed.task_id);
        if !holds_one {
            return Ok(());
        }
        let mut after = self.clone();
        after.segments.insert(changed.id.clone(), changed.clone());
        let (was, will_be) = (self.pause_time_short(), after.pause_time_short());
        let short = |of: &BTreeMap<&str, i64>, id: &str| of.get(id).copied().unwrap_or(0);
        match self
            .summaries
            .values()
            .find(|b| short(&will_be, &b.id) > short(&was, &b.id))
        {
            Some(b) => Err(CoreError::Conflict(format!(
                "this time overlaps a billable pause on summary {}; void that summary first",
                b.reference
            ))),
            None => Ok(()),
        }
    }

    pub fn add_manual_entry(
        &mut self,
        ctx: &Ctx<'_>,
        input: &ManualEntryInput,
    ) -> Result<String, CoreError> {
        self.live_task(&input.task_id)?;
        check_range(ctx, input.start, input.end)?;
        let reason = clean_reason(&input.reason)?;
        let id = Uuid::new_v4().to_string();
        let seg = Segment {
            id: id.clone(),
            task_id: input.task_id.clone(),
            device_id: ctx.device.to_owned(),
            start: input.start,
            end: Some(input.end),
            manual: true,
            subtask_id: input.subtask_id.clone(),
            edit: None,
            edit_stamp: ctx.stamp(),
        };
        self.refuse_taking_from_billed_pause(&seg)?;
        if !input.allow_overlap {
            self.refuse_overlap(&self.overlapping(ctx.device, None, input.start, input.end))?;
        }
        let after = seg_json(&seg);
        self.segments.insert(id.clone(), seg);
        self.audit(
            ctx,
            "segment",
            &id,
            "manualEntry",
            &reason,
            Value::Null,
            after,
        );
        Ok(id)
    }

    pub fn correct_segment(
        &mut self,
        ctx: &Ctx<'_>,
        input: &CorrectionInput,
    ) -> Result<(), CoreError> {
        check_range(ctx, input.start, input.end)?;
        let reason = clean_reason(&input.reason)?;
        let edit = SegmentEdit {
            start: input.start,
            end: input.end,
            voided: false,
            reason: reason.clone(),
            settled: false,
        };
        let mut changed = self.correctable(&input.segment_id)?.clone();
        changed.edit = Some(edit.clone());
        self.refuse_taking_from_billed_pause(&changed)?;
        if !input.allow_overlap {
            let names = self.overlapping(
                &changed.device_id,
                Some(&input.segment_id),
                input.start,
                input.end,
            );
            self.refuse_overlap(&names)?;
        }
        self.apply_edit(ctx, &input.segment_id, Some(edit), "correct", &reason)
    }

    /// Removes the time from every report. The raw record stays, and this can be undone.
    pub fn void_segment(&mut self, ctx: &Ctx<'_>, id: &str, reason: &str) -> Result<(), CoreError> {
        let reason = clean_reason(reason)?;
        let seg = self.correctable(id)?;
        // What counts now. A segment closed from another device has no raw end to go back to.
        let (start, end) = seg
            .edit
            .as_ref()
            .map_or((seg.start, seg.end.unwrap_or(seg.start)), |e| {
                (e.start, e.end)
            });
        self.apply_edit(
            ctx,
            id,
            Some(SegmentEdit {
                start,
                end,
                voided: true,
                reason: reason.clone(),
                settled: false,
            }),
            "void",
            &reason,
        )
    }

    /// Takes off any correction or void, back to the raw times.
    pub fn restore_segment(
        &mut self,
        ctx: &Ctx<'_>,
        id: &str,
        reason: &str,
    ) -> Result<(), CoreError> {
        let reason = clean_reason(reason)?;
        let mut changed = self.correctable(id)?.clone();
        if changed.end.is_none() {
            return Err(CoreError::Conflict(
                "this segment's device never reported an end, so it can only be corrected".into(),
            ));
        }
        changed.edit = None;
        self.refuse_taking_from_billed_pause(&changed)?;
        self.apply_edit(ctx, id, None, "restore", &reason)
    }

    /// Writes a recorded edit closing another device's open segment, as that device may never close it. Raw
    /// times stay, and the owner's own record replaces this when it comes, see `reclaim_settled`. Audited.
    pub fn settle_foreign_open_segment(
        &mut self,
        ctx: &Ctx<'_>,
        segment_id: &str,
        end: Ts,
        reason: &str,
    ) -> Result<(), CoreError> {
        let reason = clean_reason(reason)?;
        let seg = self
            .segments
            .get(segment_id)
            .ok_or(CoreError::NotFound("work segment"))?;
        if seg.device_id == ctx.device || !seg.is_open() {
            return Err(CoreError::Conflict(
                "this segment isn't running on another computer".into(),
            ));
        }
        self.live_task(&seg.task_id)?;
        let (start, task_id) = (seg.start, seg.task_id.clone());
        let end = end
            .min(ctx.now)
            .min(start.later(MAX_SEGMENT_SECS))
            .max(start);
        let edit = SegmentEdit {
            start,
            end,
            voided: false,
            reason: reason.clone(),
            settled: true,
        };
        self.apply_edit(ctx, segment_id, Some(edit), "settleLost", &reason)?;
        let running = self
            .tasks
            .get(&task_id)
            .is_some_and(|t| t.state == TaskState::Running);
        let still_open = self
            .segments
            .values()
            .any(|s| s.task_id == task_id && s.is_open());
        if running && !still_open {
            self.set_state(
                ctx,
                &task_id,
                TaskState::Paused,
                Some(PauseReason::SyncReconciled),
            );
        }
        Ok(())
    }

    /// Closes all the time `device` left open. `end_of` gives each one's end. Returns the segments closed.
    pub fn settle_foreign_device(
        &mut self,
        ctx: &Ctx<'_>,
        device: &str,
        end_of: &dyn Fn(&Segment) -> Ts,
        reason: &str,
    ) -> Vec<String> {
        let open: Vec<(String, Ts)> = self
            .segments
            .values()
            .filter(|s| s.device_id == device && s.is_open())
            .map(|s| (s.id.clone(), end_of(s)))
            .collect();
        let mut settled = Vec::with_capacity(open.len());
        for (id, end) in open {
            if self
                .settle_foreign_open_segment(ctx, &id, end, reason)
                .is_ok()
            {
                settled.push(id);
            }
        }
        settled
    }

    /// The earliest moment another device asked for this segment's timer to pause, if any did.
    #[must_use]
    pub fn earliest_pause_request(&self, segment_id: &str) -> Option<Ts> {
        self.pause_requests
            .values()
            .filter(|q| q.segment_id == segment_id)
            .map(|q| q.at)
            .min()
    }

    fn summary_holding(&self, segment_id: &str) -> Option<&str> {
        self.summaries
            .values()
            .find(|b| !b.voided && b.segment_ids.iter().any(|s| s == segment_id))
            .map(|b| b.reference.as_str())
    }

    /// Settlements `reclaim_settled` has something to do about on `device`.
    fn settlements_due(&self, device: &str) -> Vec<String> {
        self.segments
            .values()
            .filter(|s| {
                let differs = matches!(
                    (s.end, &s.edit),
                    (Some(real), Some(e)) if e.settled && real != e.end
                );
                differs
                    && self.tasks.get(&s.task_id).is_some_and(|t| t.is_live())
                    && (s.device_id == device || !self.is_billed(&s.id))
            })
            .map(|s| s.id.clone())
            .collect()
    }

    /// True when `reclaim_settled` would change something now.
    #[must_use]
    pub fn settlement_due(&self, device: &str) -> bool {
        !self.settlements_due(device).is_empty()
    }

    /// Lifts a settlement once the owner's recorded end is known, which is better evidence in either direction. Any
    /// device does this, in case the owner wrote its file and then died. A settled end already on a summary stays,
    /// and its owner hears about it once. Returns what the owner of a segment has to be told.
    pub fn reclaim_settled(&mut self, ctx: &Ctx<'_>) -> Vec<Reclaimed> {
        let mut told = Vec::new();
        for id in self.settlements_due(ctx.device) {
            let Some(seg) = self.segments.get(&id) else {
                continue;
            };
            let (Some(real), Some(edit)) = (seg.end, seg.edit.clone()) else {
                continue;
            };
            let more = real.secs() - edit.end.secs();
            let task_name = self
                .tasks
                .get(&seg.task_id)
                .map_or_else(String::new, |t| t.name.clone());
            let owner = seg.device_id == ctx.device;
            let settled_by = seg.edit_stamp.by.clone();
            let reference = self.summary_holding(&id).map(str::to_owned);
            let kept = reference.is_some().then_some(SegmentEdit {
                settled: false,
                ..edit
            });
            let (action, reason) = if kept.is_some() {
                ("reclaimOnSummary", "this device's record differs")
            } else {
                ("reclaimAfterReturn", "this device's own record")
            };
            if self.apply_edit(ctx, &id, kept, action, reason).is_err() {
                continue;
            }
            match reference {
                Some(reference) => told.push(Reclaimed::OnSummary {
                    task_name,
                    reference,
                    secs: more.abs(),
                }),
                None if owner && more > 0 => told.push(Reclaimed::Returned {
                    task_name,
                    settled_by,
                    secs: more,
                }),
                None => {}
            }
        }
        told
    }

    fn correctable(&self, id: &str) -> Result<&Segment, CoreError> {
        let seg = self
            .segments
            .get(id)
            .ok_or(CoreError::NotFound("work segment"))?;
        if seg.is_open() {
            return Err(CoreError::Conflict(
                "this segment is still running; pause the task first".into(),
            ));
        }
        if self.is_billed(id) {
            return Err(CoreError::Conflict(
                "this time is on a billing summary; void that summary first".into(),
            ));
        }
        self.live_task(&seg.task_id)?;
        Ok(seg)
    }

    fn apply_edit(
        &mut self,
        ctx: &Ctx<'_>,
        id: &str,
        edit: Option<SegmentEdit>,
        action: &str,
        reason: &str,
    ) -> Result<(), CoreError> {
        let seg = self
            .segments
            .get_mut(id)
            .ok_or(CoreError::NotFound("work segment"))?;
        let before = seg_json(seg);
        seg.edit = edit;
        seg.edit_stamp = ctx.next(&seg.edit_stamp);
        let after = seg_json(seg);
        self.audit(ctx, "segment", id, action, reason, before, after);
        Ok(())
    }

    /// Records time away inside a running segment. It counts nowhere until the user decides.
    pub fn add_gap(
        &mut self,
        ctx: &Ctx<'_>,
        segment_id: &str,
        start: Ts,
        end: Ts,
        kind: GapKind,
    ) -> Option<String> {
        let seg = self.segments.get(segment_id)?;
        let start = start.max(seg.start);
        if end <= start {
            return None;
        }
        let id = Uuid::new_v4().to_string();
        let gap = IdleGap {
            id: id.clone(),
            segment_id: segment_id.to_owned(),
            task_id: seg.task_id.clone(),
            device_id: seg.device_id.clone(),
            start,
            end,
            kind,
            decision: GapDecision::Pending,
            decision_stamp: ctx.stamp(),
        };
        self.gaps.insert(id.clone(), gap);
        self.audit(
            ctx,
            "gap",
            &id,
            "detected",
            "",
            Value::Null,
            json!({"start": start, "end": end, "kind": kind}),
        );
        Some(id)
    }

    pub fn decide_gap(
        &mut self,
        ctx: &Ctx<'_>,
        id: &str,
        decision: GapDecision,
    ) -> Result<(), CoreError> {
        let gap = self.gaps.get(id).ok_or(CoreError::NotFound("time away"))?;
        let segment_id = gap.segment_id.clone();
        self.live_task(&gap.task_id)?;
        if self.is_billed(&segment_id) {
            return Err(CoreError::Conflict(
                "this time is on a billing summary; void that summary first".into(),
            ));
        }
        let gap = self
            .gaps
            .get_mut(id)
            .ok_or(CoreError::NotFound("time away"))?;
        let before = json!({"decision": gap.decision});
        gap.decision = decision;
        gap.decision_stamp = ctx.next(&gap.decision_stamp);
        self.audit(
            ctx,
            "gap",
            id,
            "decide",
            "",
            before,
            json!({"decision": decision}),
        );
        Ok(())
    }

    pub fn mark_billable_pause(
        &mut self,
        ctx: &Ctx<'_>,
        input: &PauseInput,
    ) -> Result<String, CoreError> {
        self.live_task(&input.task_id)?;
        check_range(ctx, input.start, input.end)?;
        let note = clean_reason(&input.note)?;
        self.refuse_over_billed_pause(&input.task_id, input.start, input.end)?;
        let id = Uuid::new_v4().to_string();
        self.pauses.insert(
            id.clone(),
            BillablePause {
                id: id.clone(),
                task_id: input.task_id.clone(),
                start: input.start,
                end: input.end,
                note: note.clone(),
                deleted: false,
                stamp: ctx.stamp(),
            },
        );
        self.audit(
            ctx,
            "pause",
            &id,
            "markBillable",
            &note,
            Value::Null,
            json!({"taskId": input.task_id, "start": input.start, "end": input.end}),
        );
        Ok(id)
    }

    pub fn unmark_billable_pause(&mut self, ctx: &Ctx<'_>, id: &str) -> Result<(), CoreError> {
        if self
            .summaries
            .values()
            .any(|b| !b.voided && b.pause_ids.iter().any(|p| p == id))
        {
            return Err(CoreError::Conflict(
                "this time is on a billing summary; void that summary first".into(),
            ));
        }
        let task_id = &self
            .pauses
            .get(id)
            .ok_or(CoreError::NotFound("billable pause"))?
            .task_id;
        self.live_task(task_id)?;
        let pause = self
            .pauses
            .get_mut(id)
            .ok_or(CoreError::NotFound("billable pause"))?;
        pause.deleted = true;
        pause.stamp = ctx.next(&pause.stamp);
        let after = json!({"taskId": pause.task_id, "start": pause.start, "end": pause.end});
        self.audit(ctx, "pause", id, "unmarkBillable", "", after, Value::Null);
        Ok(())
    }

    /// Makes one subtask active. If the task is running here, time up to now stays on the previous
    /// subtask and a new segment starts, so each segment belongs to exactly one subtask.
    pub fn set_active_subtask(
        &mut self,
        ctx: &Ctx<'_>,
        task_id: &str,
        subtask_id: Option<&str>,
    ) -> Result<(), CoreError> {
        let task = self.live_task(task_id)?;
        if subtask_id.is_some_and(str::is_empty) {
            return Err(CoreError::NotFound("subtask"));
        }
        if let Some(id) = subtask_id
            && !crate::subtasks::items(&task.description)
                .iter()
                .any(|i| i.id == id)
        {
            return Err(CoreError::NotFound("subtask"));
        }
        let t = self
            .tasks
            .get_mut(task_id)
            .ok_or(CoreError::NotFound("task"))?;
        let changed = t.active_subtask.as_deref() != subtask_id;
        t.active_subtask = subtask_id.map(str::to_owned);
        t.active_subtask_stamp = ctx.next(&t.active_subtask_stamp);
        if changed {
            let open: Vec<String> = self
                .segments
                .values()
                .filter(|s| s.task_id == task_id && s.device_id == ctx.device && s.end.is_none())
                .map(|s| s.id.clone())
                .collect();
            if !open.is_empty() {
                for id in &open {
                    self.close_segment(id, ctx.now);
                }
                self.open_segment(ctx, task_id);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::tests::{ctx, project_input, task_input};
    use crate::model::{Segment, TaskState};
    use crate::pieces::{PieceKind, pieces};
    use proptest::prelude::*;

    fn world() -> (Shared, String) {
        let mut s = Shared::default();
        let c = ctx("2026-01-01T08:00:00Z");
        let p = s.create_project(&c, &project_input("A")).unwrap();
        let t = s.create_task(&c, &task_input("T", &p)).unwrap();
        (s, t)
    }
    fn ts(x: &str) -> Ts {
        Ts::parse(x).unwrap()
    }
    fn secs_by(s: &Shared, kind: PieceKind) -> i64 {
        let end = |seg: &Segment| seg.end.unwrap_or(seg.start);
        pieces(s, &end)
            .iter()
            .filter(|p| p.kind == kind)
            .map(crate::pieces::Piece::secs)
            .sum()
    }
    fn manual(t: &str, a: &str, b: &str) -> ManualEntryInput {
        ManualEntryInput {
            task_id: t.into(),
            start: ts(a),
            end: ts(b),
            reason: "forgot".into(),
            subtask_id: None,
            allow_overlap: false,
        }
    }

    #[test]
    fn a_manual_entry_counts_and_is_audited() {
        let (mut s, t) = world();
        let c = ctx("2026-01-02T12:00:00Z");
        let id = s
            .add_manual_entry(
                &c,
                &manual(&t, "2026-01-02T09:00:00Z", "2026-01-02T10:00:00Z"),
            )
            .unwrap();
        assert!(s.segments[&id].manual);
        assert_eq!(secs_by(&s, PieceKind::Work), 3600);
        let log: Vec<_> = s.audit_log.values().collect();
        assert_eq!(
            (log[0].action.as_str(), log[0].reason.as_str()),
            ("manualEntry", "forgot")
        );
    }

    #[test]
    fn manual_entries_refuse_nonsense_and_warn_about_overlap() {
        let (mut s, t) = world();
        let c = ctx("2026-01-02T12:00:00Z");
        assert!(
            s.add_manual_entry(
                &c,
                &manual(&t, "2026-01-02T10:00:00Z", "2026-01-02T09:00:00Z")
            )
            .is_err()
        );
        assert!(
            s.add_manual_entry(
                &c,
                &manual(&t, "2026-01-02T11:00:00Z", "2026-01-02T13:00:00Z")
            )
            .is_err()
        );
        s.add_manual_entry(
            &c,
            &manual(&t, "2026-01-02T09:00:00Z", "2026-01-02T10:00:00Z"),
        )
        .unwrap();
        let over = manual(&t, "2026-01-02T09:30:00Z", "2026-01-02T10:30:00Z");
        assert!(matches!(
            s.add_manual_entry(&c, &over),
            Err(CoreError::Overlap(_))
        ));
        let ok = ManualEntryInput {
            allow_overlap: true,
            ..over
        };
        assert!(s.add_manual_entry(&c, &ok).is_ok());
    }

    #[test]
    fn a_correction_keeps_raw_times_and_can_be_undone() {
        let (mut s, t) = world();
        let c = ctx("2026-01-02T12:00:00Z");
        let id = s
            .add_manual_entry(
                &c,
                &manual(&t, "2026-01-02T09:00:00Z", "2026-01-02T10:00:00Z"),
            )
            .unwrap();
        s.correct_segment(
            &c,
            &CorrectionInput {
                segment_id: id.clone(),
                start: ts("2026-01-02T09:00:00Z"),
                end: ts("2026-01-02T09:30:00Z"),
                reason: "left early".into(),
                allow_overlap: false,
            },
        )
        .unwrap();
        assert_eq!(
            s.segments[&id].end,
            Some(ts("2026-01-02T10:00:00Z")),
            "raw stays"
        );
        assert_eq!(secs_by(&s, PieceKind::Work), 1800);
        s.void_segment(&c, &id, "duplicate").unwrap();
        assert_eq!(secs_by(&s, PieceKind::Work), 0);
        s.restore_segment(&c, &id, "my mistake").unwrap();
        assert_eq!(secs_by(&s, PieceKind::Work), 3600);
        let actions: Vec<_> = s.audit_log.values().map(|a| a.action.clone()).collect();
        for want in ["manualEntry", "correct", "void", "restore"] {
            assert!(actions.iter().any(|a| a == want), "{want}");
        }
    }

    #[test]
    fn a_running_segment_cannot_be_corrected() {
        let (mut s, t) = world();
        let c = ctx("2026-01-02T09:00:00Z");
        s.start(&c, &t).unwrap();
        let id = s.segments.keys().next().unwrap().clone();
        assert!(
            s.void_segment(&ctx("2026-01-02T10:00:00Z"), &id, "x")
                .is_err()
        );
    }

    const LAPTOP: &str = "device-laptop";

    fn on_laptop(iso: &str) -> Ctx<'static> {
        Ctx::at(ts(iso), LAPTOP)
    }

    /// A store holding one timer another device started at 09:00, and the segment's id.
    fn left_running() -> (Shared, String, String) {
        let (mut s, t) = world();
        s.start(&on_laptop("2026-01-02T09:00:00Z"), &t).unwrap();
        let id = s.segments.keys().next().unwrap().clone();
        (s, t, id)
    }

    fn settle(s: &mut Shared, id: &str, end: &str, now: &str) -> Result<(), CoreError> {
        s.settle_foreign_open_segment(&ctx(now), id, ts(end), "the laptop is gone")
    }

    fn correction(id: &str, a: &str, b: &str) -> CorrectionInput {
        CorrectionInput {
            segment_id: id.into(),
            start: ts(a),
            end: ts(b),
            reason: "as it was".into(),
            allow_overlap: false,
        }
    }

    fn effective_end(s: &Shared, id: &str) -> Option<Ts> {
        s.segments[id].effective().and_then(|(_, end)| end)
    }

    fn this_month(p: &str) -> crate::summary::SummaryRequest {
        crate::summary::SummaryRequest {
            project_id: p.to_owned(),
            from: "2026-01-01".into(),
            to: "2026-01-31".into(),
            zone: "UTC".into(),
        }
    }

    #[test]
    fn settled_segment_can_be_corrected_voided_billed_not_restored() {
        let (mut s, t, id) = left_running();
        let p = s.tasks[&t].project_id.clone();
        let later = ctx("2026-02-01T12:00:00Z");
        // Before settling, it can't be fixed or billed, nor paused from here.
        assert!(
            s.correct_segment(
                &later,
                &correction(&id, "2026-01-02T09:00:00Z", "2026-01-02T10:00:00Z")
            )
            .is_err()
        );
        assert!(s.void_segment(&later, &id, "gone").is_err());
        assert!(matches!(
            s.pause(&later, &t),
            Err(CoreError::RunningElsewhere(_))
        ));
        assert!(s.summary_preview(&this_month(&p)).unwrap().running_excluded);

        settle(&mut s, &id, "2026-01-02T10:00:00Z", "2026-02-01T12:00:00Z").unwrap();
        let seg = &s.segments[&id];
        assert_eq!(seg.end, None, "raw stays");
        assert!(!seg.is_open());
        assert!(seg.edit.as_ref().unwrap().settled);
        assert_eq!(seg.edit.as_ref().unwrap().reason, "the laptop is gone");
        assert_eq!(s.tasks[&t].state, TaskState::Paused, "nothing times it now");
        assert_eq!(
            s.tasks[&t].pause_reason,
            Some(PauseReason::SyncReconciled),
            "the words the page already has for this"
        );
        let audited: Vec<_> = s
            .audit_log
            .values()
            .filter(|a| a.entity_id == id)
            .map(|a| (a.action.as_str(), a.reason.as_str()))
            .collect();
        assert_eq!(audited, vec![("settleLost", "the laptop is gone")]);

        // Restoring would leave nothing to go back to.
        let refused = s.restore_segment(&later, &id, "undo").unwrap_err();
        assert_eq!(
            refused.to_string(),
            "this segment's device never reported an end, so it can only be corrected"
        );

        // Voiding keeps the end it had, and undoing that needs a correction too.
        s.void_segment(&later, &id, "not work").unwrap();
        assert_eq!(
            s.segments[&id].edit.as_ref().unwrap().end,
            ts("2026-01-02T10:00:00Z")
        );
        assert_eq!(secs_by(&s, PieceKind::Work), 0);
        assert!(s.restore_segment(&later, &id, "undo").is_err());
        s.correct_segment(
            &later,
            &correction(&id, "2026-01-02T09:00:00Z", "2026-01-02T09:45:00Z"),
        )
        .unwrap();
        let edit = s.segments[&id].edit.as_ref().unwrap();
        assert!(
            !edit.settled,
            "the owner's record won't lift what a person chose"
        );
        assert_eq!(secs_by(&s, PieceKind::Work), 45 * 60);

        // And it reaches a summary, after which it is like any billed time.
        let preview = s.summary_preview(&this_month(&p)).unwrap();
        assert!(!preview.running_excluded);
        assert_eq!(preview.segment_ids, vec![id.clone()]);
        assert_eq!(preview.billable_secs, 45 * 60);
        let summary = s.create_summary(&later, &this_month(&p)).unwrap();
        assert_eq!(s.summaries[&summary].segment_ids, vec![id.clone()]);
        assert!(s.void_segment(&later, &id, "gone").is_err());
        assert!(
            s.correct_segment(
                &later,
                &correction(&id, "2026-01-02T09:00:00Z", "2026-01-02T09:30:00Z")
            )
            .is_err()
        );
    }

    #[test]
    fn pause_request_caps_the_settled_end() {
        let start = ts("2026-01-02T09:00:00Z");
        let seg = Segment {
            start,
            ..Segment::default()
        };
        let at = |clock: &str| ts(&format!("2026-01-02T{clock}:00Z"));
        let now = at("18:00");
        // (known, lease beat, pause request) and what it comes to.
        for (known, beat, asked, want) in [
            // No beat to go by: what the reports count, which a request has already capped.
            ("10:00", None, None, "10:00"),
            ("10:00", None, Some("09:30"), "10:00"),
            // The beat is fresher than the last device file.
            ("10:00", Some("11:00"), None, "11:00"),
            ("10:00", Some("09:30"), None, "10:00"),
            // Nothing after a request counts, but what was known before it stays.
            ("10:00", Some("12:00"), Some("11:00"), "11:00"),
            ("10:00", Some("12:00"), Some("09:30"), "10:00"),
            ("10:00", Some("10:30"), Some("11:00"), "10:30"),
            // Never ahead of this clock, nor before the start.
            ("10:00", Some("20:00"), None, "18:00"),
            ("08:00", None, None, "09:00"),
        ] {
            assert_eq!(
                settled_end(&seg, at(known), beat.map(at), asked.map(at), now),
                at(want),
                "{known} {beat:?} {asked:?}"
            );
        }

        // Through the store: the laptop was heard of later than it was asked to pause.
        let (mut s, t, id) = left_running();
        let c = ctx("2026-01-02T10:00:00Z");
        s.request_remote_pause(&c, &t).unwrap();
        assert_eq!(
            s.earliest_pause_request(&id),
            Some(ts("2026-01-02T10:00:00Z"))
        );
        let source = crate::report::ExportSource {
            only: None,
            data: s.clone(),
            device_id: crate::machine::tests::DEV.to_owned(),
            remote_devices: BTreeMap::from([(
                LAPTOP.to_owned(),
                crate::model::RemoteDevice {
                    name: "LAPTOP".to_owned(),
                    last_heartbeat: Some(ts("2026-01-02T11:00:00Z")),
                    written_at: ts("2026-01-02T11:00:00Z"),
                },
            )]),
            now: ts("2026-01-03T12:00:00Z"),
        };
        let end = source.end_of(&s.segments[&id]);
        assert_eq!(end, ts("2026-01-02T10:00:00Z"));
        s.settle_foreign_open_segment(&ctx("2026-01-03T12:00:00Z"), &id, end, "gone")
            .unwrap();
        assert_eq!(effective_end(&s, &id), Some(ts("2026-01-02T10:00:00Z")));
    }

    /// A store with the laptop's timer, told through a sync that it last wrote at 09:30.
    #[test]
    fn of_two_requests_to_pause_the_earlier_one_counts() {
        let (mut s, t, id) = left_running();
        s.request_remote_pause(&ctx("2026-01-02T10:00:00Z"), &t)
            .unwrap();
        let first = s.pause_requests.values().next().unwrap().clone();
        // Each device asked before it had read the other's request, so a merge brings both.
        let asked = |key: &str, at: &str| crate::model::PauseRequest {
            id: key.to_owned(),
            at: ts(at),
            ..first.clone()
        };
        let later = asked("0", "2026-01-02T11:00:00Z");
        s.pause_requests.insert(later.id.clone(), later);
        assert_eq!(
            s.earliest_pause_request(&id),
            Some(ts("2026-01-02T10:00:00Z"))
        );
        let earlier = asked("z", "2026-01-02T09:45:00Z");
        s.pause_requests.insert(earlier.id.clone(), earlier);
        assert_eq!(
            s.earliest_pause_request(&id),
            Some(ts("2026-01-02T09:45:00Z"))
        );
        assert_eq!(s.earliest_pause_request("another segment"), None);
    }

    fn service_with_laptop_timer(
        dir: &std::path::Path,
        clock: &crate::time::ManualClock,
    ) -> (crate::service::Service, String, String) {
        let mut svc =
            crate::service::Service::open(dir, std::sync::Arc::new(clock.clone()), "pc").unwrap();
        let (s, t, id) = left_running();
        let seen = BTreeMap::from([(
            LAPTOP.to_owned(),
            crate::model::RemoteDevice {
                name: "LAPTOP".to_owned(),
                last_heartbeat: Some(ts("2026-01-02T09:30:00Z")),
                written_at: ts("2026-01-02T09:30:00Z"),
            },
        )]);
        svc.absorb_remote(&s, seen, Vec::new());
        (svc, t, id)
    }

    #[test]
    fn fallback_settle_needs_silence_or_request() {
        let dir = tempfile::tempdir().unwrap();
        let clock = crate::time::ManualClock::at("2026-01-02T10:00:00Z");
        let (mut svc, _, id) = service_with_laptop_timer(dir.path(), &clock);
        let refused = svc.settle_lost_segment(&id, "gone").unwrap_err();
        assert_eq!(
            refused.to_string(),
            "LAPTOP was seen 30 min ago, so it may still be running this"
        );
        clock.set("2026-01-03T09:29:59Z");
        assert!(svc.settle_lost_segment(&id, "gone").is_err());
        assert!(
            svc.file().data.segments[&id].is_open(),
            "and nothing changed"
        );

        // A day of silence is enough, and it ends where the figures always said it did.
        clock.set("2026-01-03T09:30:00Z");
        svc.settle_lost_segment(&id, "gone").unwrap();
        assert_eq!(
            effective_end(&svc.file().data, &id),
            Some(ts("2026-01-02T09:30:00Z"))
        );
        // Nothing is left to settle, and neither is a timer of this computer's own: no waiting would change that.
        let not_elsewhere = "this segment isn't running on another computer";
        assert_eq!(
            svc.settle_lost_segment(&id, "gone")
                .unwrap_err()
                .to_string(),
            not_elsewhere
        );
        let project = svc.create_project(&project_input("Own")).unwrap();
        let own = svc.create_task(&task_input("Own", &project)).unwrap();
        svc.mutate(|f, c| f.data.start(c, &own)).unwrap();
        let own_segment = svc.file().data.own_open_segments(&svc.file().device_id);
        assert_eq!(
            svc.settle_lost_segment(&own_segment[0], "gone")
                .unwrap_err()
                .to_string(),
            not_elsewhere
        );

        // Asking is enough, at any time. A blank reason says what happened.
        let dir = tempfile::tempdir().unwrap();
        let clock = crate::time::ManualClock::at("2026-01-02T10:00:00Z");
        let (mut svc, t, id) = service_with_laptop_timer(dir.path(), &clock);
        svc.request_remote_pause(&t).unwrap();
        // It changes shared data, so a read-only device has to take over first.
        svc.add_sync_target(crate::model::SyncTarget {
            provider: crate::model::SyncProvider::Custom,
            root: "/not/there".to_owned(),
        })
        .unwrap();
        svc.lease.state = crate::lease::LeaseStateName::HeldByOtherStale;
        assert_eq!(
            svc.settle_lost_segment(&id, "  ").unwrap_err().code(),
            "read_only"
        );
        svc.lease.state = crate::lease::LeaseStateName::HeldByMe;
        svc.settle_lost_segment(&id, "  ").unwrap();
        assert_eq!(
            svc.file().data.segments[&id].edit.as_ref().unwrap().reason,
            "Closed from pc: LAPTOP stopped responding. Ended at its last known moment."
        );

        // Not this device's own, which it can simply pause.
        let dir = tempfile::tempdir().unwrap();
        let clock = crate::time::ManualClock::at("2026-01-02T10:00:00Z");
        let mut svc =
            crate::service::Service::open(dir.path(), std::sync::Arc::new(clock.clone()), "pc")
                .unwrap();
        let p = svc.create_project(&project_input("A")).unwrap();
        let own = svc.create_task(&task_input("T", &p)).unwrap();
        svc.mutate(|f, c| f.data.start(c, &own).map(|_| ()))
            .unwrap();
        let seg = svc.file().data.segments.keys().next().unwrap().clone();
        clock.advance(2 * 86_400);
        assert!(svc.settle_lost_segment(&seg, "gone").is_err());
        assert!(svc.settle_lost_segment("no-such", "gone").is_err());
    }

    #[test]
    fn settling_refuses_what_is_not_left_running_elsewhere() {
        let (mut s, _, id) = left_running();
        let now = "2026-01-03T12:00:00Z";
        // Its own device closes it itself.
        let own = s
            .settle_foreign_open_segment(&on_laptop(now), &id, ts(now), "gone")
            .unwrap_err();
        assert_eq!(own.code(), "conflict");
        // Once is enough.
        settle(&mut s, &id, "2026-01-02T10:00:00Z", now).unwrap();
        assert_eq!(
            settle(&mut s, &id, "2026-01-02T11:00:00Z", now)
                .unwrap_err()
                .code(),
            "conflict"
        );
        assert_eq!(effective_end(&s, &id), Some(ts("2026-01-02T10:00:00Z")));
        // Already closed by its owner.
        let (mut s, t, id) = left_running();
        s.pause(&on_laptop("2026-01-02T10:00:00Z"), &t).unwrap();
        assert_eq!(
            settle(&mut s, &id, "2026-01-02T10:00:00Z", now)
                .unwrap_err()
                .code(),
            "conflict"
        );
        // A task deleted with its time is gone, and that time with it.
        let (mut s, t, id) = left_running();
        s.delete_task(&ctx(now), &t).unwrap();
        assert_eq!(
            settle(&mut s, &id, "2026-01-02T10:00:00Z", now)
                .unwrap_err()
                .code(),
            "not_found"
        );
        // Unknown id, and a reason that can't be stored.
        let (mut s, _, id) = left_running();
        assert_eq!(
            settle(&mut s, "no-such", now, now).unwrap_err().code(),
            "not_found"
        );
        assert_eq!(
            s.settle_foreign_open_segment(&ctx(now), &id, ts(now), "a\nb")
                .unwrap_err()
                .code(),
            "validation"
        );
    }

    #[test]
    fn a_settled_end_stays_between_the_start_and_this_clock_and_may_have_no_length() {
        let start = "2026-01-02T09:00:00Z";
        let now = "2026-01-03T12:00:00Z";
        for (end, want) in [
            ("2026-01-02T10:00:00Z", "2026-01-02T10:00:00Z"),
            // Ahead of this clock.
            ("2026-01-09T10:00:00Z", now),
            // Before it began: a device last heard of then has nothing to its name.
            ("2026-01-01T10:00:00Z", start),
            (start, start),
        ] {
            let (mut s, _, id) = left_running();
            settle(&mut s, &id, end, now).unwrap();
            assert_eq!(effective_end(&s, &id), Some(ts(want)), "{end}");
            assert_eq!(s.segments[&id].edit.as_ref().unwrap().start, ts(start));
            assert!(crate::validate::check_shared(&s, ts(now)).is_ok());
        }
        // This clock behind the start: it can't be negative.
        let (mut s, _, id) = left_running();
        settle(&mut s, &id, now, "2026-01-02T08:00:00Z").unwrap();
        assert_eq!(effective_end(&s, &id), Some(ts(start)));
        // Never longer than any segment may be, or no reader would take it.
        let (mut s, _, id) = left_running();
        let much_later = "2028-01-02T09:00:00Z";
        settle(&mut s, &id, much_later, much_later).unwrap();
        let (a, b) = s.segments[&id].effective().unwrap();
        assert_eq!(b.unwrap().secs() - a.secs(), MAX_SEGMENT_SECS);
        assert!(crate::validate::check_shared(&s, ts(much_later)).is_ok());
    }

    #[test]
    fn a_task_still_running_on_a_third_device_stays_running() {
        let (mut s, t, id) = left_running();
        let third = Ctx::at(ts("2026-01-02T09:30:00Z"), "device-third");
        s.open_segment(&third, &t);
        settle(&mut s, &id, "2026-01-02T10:00:00Z", "2026-01-03T12:00:00Z").unwrap();
        assert_eq!(s.tasks[&t].state, TaskState::Running);
        assert!(matches!(
            s.pause(&ctx("2026-01-03T12:00:00Z"), &t),
            Err(CoreError::RunningElsewhere(d)) if d == "device-third"
        ));
    }

    #[test]
    fn settling_a_device_closes_all_it_left_and_nothing_of_anyone_else() {
        let (mut s, t, laptop_seg) = left_running();
        let other_task = s
            .create_task(
                &ctx("2026-01-02T08:00:00Z"),
                &task_input("U", &s.tasks[&t].project_id.clone()),
            )
            .unwrap();
        s.open_segment(&on_laptop("2026-01-02T09:10:00Z"), &other_task);
        let third = Ctx::at(ts("2026-01-02T09:30:00Z"), "device-third");
        s.open_segment(&third, &t);
        let c = ctx("2026-01-03T12:00:00Z");
        let ends = |seg: &Segment| seg.start.later(600);
        let mut settled = s.settle_foreign_device(&c, LAPTOP, &ends, "gone");
        settled.sort();
        let mut expected: Vec<String> = s
            .segments
            .values()
            .filter(|x| x.device_id == LAPTOP)
            .map(|x| x.id.clone())
            .collect();
        expected.sort();
        assert_eq!(settled, expected);
        assert!(settled.contains(&laptop_seg));
        assert_eq!(
            effective_end(&s, &laptop_seg),
            Some(ts("2026-01-02T09:10:00Z"))
        );
        assert!(
            s.segments
                .values()
                .filter(|x| x.device_id == "device-third")
                .all(Segment::is_open),
            "only that device's"
        );
        assert!(
            s.settle_foreign_device(&c, LAPTOP, &ends, "gone")
                .is_empty(),
            "nothing left"
        );
        assert!(
            s.settle_foreign_device(&c, crate::machine::tests::DEV, &ends, "gone")
                .is_empty(),
            "nor this device's own"
        );
    }

    /// The state a task can be merged into whichever way its clocks fall, with nothing left timing it.
    fn running_again(s: &mut Shared, t: &str) {
        s.set_state(&ctx("2026-01-03T12:30:00Z"), t, TaskState::Running, None);
    }

    #[test]
    fn a_settled_segment_is_not_running_elsewhere_for_a_pause() {
        let (mut s, t, id) = left_running();
        settle(&mut s, &id, "2026-01-02T10:00:00Z", "2026-01-03T12:00:00Z").unwrap();
        running_again(&mut s, &t);
        s.pause(&ctx("2026-01-03T13:00:00Z"), &t).unwrap();
        assert_eq!(s.tasks[&t].state, TaskState::Paused);
        assert_eq!(s.tasks[&t].pause_reason, Some(PauseReason::User));
    }

    #[test]
    fn a_settled_segment_is_not_keeping_a_task_running_after_a_merge() {
        let (mut s, t, id) = left_running();
        settle(&mut s, &id, "2026-01-02T10:00:00Z", "2026-01-03T12:00:00Z").unwrap();
        running_again(&mut s, &t);
        assert!(s.reconcile(&ctx("2026-01-03T13:00:00Z")));
        assert_eq!(s.tasks[&t].state, TaskState::Paused);
        assert_eq!(s.tasks[&t].pause_reason, Some(PauseReason::SyncReconciled));
    }

    #[test]
    fn a_settled_segment_does_not_keep_a_task_running_when_its_own_timer_closes() {
        let (mut s, t, id) = left_running();
        s.open_segment(&ctx("2026-01-02T09:30:00Z"), &t);
        settle(&mut s, &id, "2026-01-02T10:00:00Z", "2026-01-03T12:00:00Z").unwrap();
        assert_eq!(s.tasks[&t].state, TaskState::Running, "still timed here");
        let c = ctx("2026-01-03T13:00:00Z");
        s.close_own_open(&c, c.now, &PauseReason::AppQuit);
        assert_eq!(s.tasks[&t].state, TaskState::Paused);
        assert_eq!(s.tasks[&t].pause_reason, Some(PauseReason::AppQuit));
    }

    #[test]
    fn a_settled_segment_cannot_be_asked_to_pause() {
        let (mut s, t, id) = left_running();
        settle(&mut s, &id, "2026-01-02T10:00:00Z", "2026-01-03T12:00:00Z").unwrap();
        let c = ctx("2026-01-03T13:00:00Z");
        assert_eq!(
            s.request_remote_pause(&c, &t).unwrap_err().code(),
            "conflict"
        );
        assert!(s.pause_requests.is_empty());
    }

    #[test]
    fn its_owner_still_closes_a_segment_settled_while_it_ran() {
        let (mut s, t, id) = left_running();
        settle(&mut s, &id, "2026-01-02T10:00:00Z", "2026-01-03T12:00:00Z").unwrap();
        // Own bookkeeping goes by the raw end.
        assert_eq!(s.own_open_segments(LAPTOP), vec![id.clone()]);
        assert_eq!(s.own_segments_to_close(LAPTOP), vec![id.clone()]);
        let back = on_laptop("2026-01-03T14:00:00Z");
        assert!(s.reconcile(&back));
        assert_eq!(s.segments[&id].end, Some(back.now));
        assert_eq!(s.reclaim_settled(&back).len(), 1);
        assert_eq!(s.segments[&id].edit, None);
        assert_eq!(s.tasks[&t].state, TaskState::Paused);
    }

    /// The laptop's own record arrives: it closed its timer at `real`.
    fn owner_closed(s: &mut Shared, id: &str, real: &str) {
        s.segments.get_mut(id).unwrap().end = Some(ts(real));
    }

    #[test]
    fn the_owners_record_lifts_a_settlement_on_any_device_in_either_direction() {
        for real in ["2026-01-02T11:00:00Z", "2026-01-02T09:30:00Z"] {
            let (mut s, _, id) = left_running();
            settle(&mut s, &id, "2026-01-02T10:00:00Z", "2026-01-03T12:00:00Z").unwrap();
            owner_closed(&mut s, &id, real);
            assert!(s.settlement_due(crate::machine::tests::DEV));
            let told = s.reclaim_settled(&ctx("2026-01-03T13:00:00Z"));
            assert!(told.is_empty(), "only its owner hears of it");
            assert_eq!(s.segments[&id].edit, None);
            assert_eq!(effective_end(&s, &id), Some(ts(real)));
            let log = s
                .audit_log
                .values()
                .find(|a| a.action == "reclaimAfterReturn")
                .unwrap();
            assert_eq!(log.reason, "this device's own record");
            assert!(
                s.restore_segment(&ctx("2026-01-03T13:00:00Z"), &id, "x")
                    .is_ok()
            );
        }
    }

    #[test]
    fn its_owner_hears_only_of_time_that_came_back() {
        let (mut s, _, id) = left_running();
        settle(&mut s, &id, "2026-01-02T10:00:00Z", "2026-01-03T12:00:00Z").unwrap();
        owner_closed(&mut s, &id, "2026-01-02T11:30:00Z");
        assert_eq!(
            s.reclaim_settled(&on_laptop("2026-01-03T13:00:00Z")),
            vec![Reclaimed::Returned {
                task_name: "T".to_owned(),
                settled_by: crate::machine::tests::DEV.to_owned(),
                secs: 5400,
            }]
        );
        assert_eq!(effective_end(&s, &id), Some(ts("2026-01-02T11:30:00Z")));

        let (mut s, _, id) = left_running();
        settle(&mut s, &id, "2026-01-02T10:00:00Z", "2026-01-03T12:00:00Z").unwrap();
        owner_closed(&mut s, &id, "2026-01-02T09:45:00Z");
        assert!(
            s.reclaim_settled(&on_laptop("2026-01-03T13:00:00Z"))
                .is_empty()
        );
        assert_eq!(s.segments[&id].edit, None, "lifted just the same");
    }

    #[test]
    fn a_settlement_that_agrees_or_a_correction_on_top_stays() {
        let later = "2026-01-03T13:00:00Z";
        let (mut s, _, id) = left_running();
        settle(&mut s, &id, "2026-01-02T10:00:00Z", "2026-01-03T12:00:00Z").unwrap();
        owner_closed(&mut s, &id, "2026-01-02T10:00:00Z");
        assert!(!s.settlement_due(crate::machine::tests::DEV));
        s.reclaim_settled(&ctx(later));
        assert!(s.segments[&id].edit.is_some());

        let (mut s, _, id) = left_running();
        settle(&mut s, &id, "2026-01-02T10:00:00Z", "2026-01-03T12:00:00Z").unwrap();
        s.correct_segment(
            &ctx("2026-01-03T12:30:00Z"),
            &correction(&id, "2026-01-02T09:00:00Z", "2026-01-02T09:40:00Z"),
        )
        .unwrap();
        owner_closed(&mut s, &id, "2026-01-02T11:00:00Z");
        assert!(s.reclaim_settled(&on_laptop(later)).is_empty());
        assert_eq!(effective_end(&s, &id), Some(ts("2026-01-02T09:40:00Z")));

        let (mut s, _, id) = left_running();
        settle(&mut s, &id, "2026-01-02T10:00:00Z", "2026-01-03T12:00:00Z").unwrap();
        s.void_segment(&ctx("2026-01-03T12:30:00Z"), &id, "not work")
            .unwrap();
        owner_closed(&mut s, &id, "2026-01-02T11:00:00Z");
        s.reclaim_settled(&on_laptop(later));
        assert_eq!(s.segments[&id].effective(), None, "still voided");
    }

    #[test]
    fn on_a_summary_a_settlement_stays_and_only_its_owner_hears_once() {
        let (mut s, t, id) = left_running();
        let p = s.tasks[&t].project_id.clone();
        let billing = ctx("2026-01-03T12:00:00Z");
        settle(&mut s, &id, "2026-01-02T10:00:00Z", "2026-01-03T12:00:00Z").unwrap();
        let summary = s.create_summary(&billing, &this_month(&p)).unwrap();
        let reference = s.summaries[&summary].reference.clone();
        owner_closed(&mut s, &id, "2026-01-02T11:30:00Z");

        // Another device leaves it alone, and has nothing to do.
        let elsewhere = ctx("2026-01-03T13:00:00Z");
        let before = s.clone();
        assert!(!s.settlement_due(elsewhere.device));
        assert!(s.reclaim_settled(&elsewhere).is_empty());
        assert_eq!(s, before);

        // Its owner hears, once.
        let owner = on_laptop("2026-01-03T14:00:00Z");
        assert!(s.settlement_due(LAPTOP));
        assert_eq!(
            s.reclaim_settled(&owner),
            vec![Reclaimed::OnSummary {
                task_name: "T".to_owned(),
                reference,
                secs: 5400,
            }]
        );
        let edit = s.segments[&id].edit.as_ref().unwrap();
        assert_eq!(
            edit.end,
            ts("2026-01-02T10:00:00Z"),
            "what was billed stands"
        );
        assert!(!edit.settled);
        assert!(!s.settlement_due(LAPTOP));
        assert!(s.reclaim_settled(&owner).is_empty());
        // What it tells the user to do can be done.
        s.void_summary(&owner, &summary).unwrap();
        s.restore_segment(&owner, &id, "this is what ran").unwrap();
        assert_eq!(effective_end(&s, &id), Some(ts("2026-01-02T11:30:00Z")));
    }

    #[test]
    fn a_summary_that_billed_more_than_its_owner_recorded_says_by_how_much() {
        let (mut s, t, id) = left_running();
        let p = s.tasks[&t].project_id.clone();
        settle(&mut s, &id, "2026-01-02T11:30:00Z", "2026-01-03T12:00:00Z").unwrap();
        let summary = s
            .create_summary(&ctx("2026-01-03T12:00:00Z"), &this_month(&p))
            .unwrap();
        let reference = s.summaries[&summary].reference.clone();
        owner_closed(&mut s, &id, "2026-01-02T10:00:00Z");
        assert_eq!(
            s.reclaim_settled(&on_laptop("2026-01-03T14:00:00Z")),
            vec![Reclaimed::OnSummary {
                task_name: "T".to_owned(),
                reference,
                secs: 5400,
            }]
        );
    }

    #[test]
    fn a_settlement_waiting_on_a_summary_lifts_when_that_is_voided_elsewhere() {
        let (mut s, t, id) = left_running();
        let p = s.tasks[&t].project_id.clone();
        let billing = ctx("2026-01-03T12:00:00Z");
        settle(&mut s, &id, "2026-01-02T10:00:00Z", "2026-01-03T12:00:00Z").unwrap();
        let summary = s.create_summary(&billing, &this_month(&p)).unwrap();
        owner_closed(&mut s, &id, "2026-01-02T11:30:00Z");
        assert!(!s.settlement_due(billing.device));
        s.void_summary(&billing, &summary).unwrap();
        assert!(s.settlement_due(billing.device));
        s.reclaim_settled(&billing);
        assert_eq!(effective_end(&s, &id), Some(ts("2026-01-02T11:30:00Z")));
    }

    #[test]
    fn a_deleted_tasks_settlement_is_left_alone() {
        let (mut s, t, id) = left_running();
        settle(&mut s, &id, "2026-01-02T10:00:00Z", "2026-01-03T12:00:00Z").unwrap();
        s.delete_task(&ctx("2026-01-03T12:30:00Z"), &t).unwrap();
        owner_closed(&mut s, &id, "2026-01-02T11:30:00Z");
        assert!(!s.settlement_due(LAPTOP));
        assert!(
            s.reclaim_settled(&on_laptop("2026-01-03T13:00:00Z"))
                .is_empty()
        );
    }

    #[test]
    fn devices_that_lift_a_settlement_in_any_order_end_up_the_same() {
        let (mut base, _, id) = left_running();
        settle(
            &mut base,
            &id,
            "2026-01-02T10:00:00Z",
            "2026-01-03T12:00:00Z",
        )
        .unwrap();
        let mut owner = base.clone();
        owner_closed(&mut owner, &id, "2026-01-02T11:30:00Z");
        // The owner has reconciled and lifted. Another read its file and lifted too, an hour off by its clock.
        let mut laptop = owner.clone();
        laptop.reclaim_settled(&on_laptop("2026-01-03T13:00:00Z"));
        let mut other = owner.clone();
        other.reclaim_settled(&ctx("2026-01-03T14:00:00Z"));
        // And a third merged only the settlement. None of them can bring it back.
        let (ab, ba) = (
            crate::merge::merge(&laptop, &other),
            crate::merge::merge(&other, &laptop),
        );
        let abc = crate::merge::merge(&ab, &base);
        let cba = crate::merge::merge(&base, &ba);
        assert_eq!(abc.segments[&id], cba.segments[&id]);
        assert_eq!(abc.segments[&id].edit, None);
        assert_eq!(effective_end(&abc, &id), Some(ts("2026-01-02T11:30:00Z")));
    }

    proptest! {
        /// However the clocks fall, and with both devices acting before they have met.
        #[test]
        fn a_settlement_and_the_owners_record_settle_on_one_answer(
            settled_at in 0i64..120,
            real in 0i64..120,
            laptop_skew in -7200i64..7200,
            desktop_skew in -7200i64..7200,
        ) {
            let start = ts("2026-01-02T09:00:00Z");
            let at = |skew: i64, step: i64| ts("2026-02-01T12:00:00Z").later(skew + step * 60);
            let on = |skew: i64, step: i64, device: &'static str| Ctx::at(at(skew, step), device);
            let desktop = crate::machine::tests::DEV;
            let (base, _, id) = left_running();
            let mut here = base.clone();
            here.settle_foreign_open_segment(
                &on(desktop_skew, 0, desktop),
                &id,
                start.later(settled_at * 60),
                "gone",
            )
            .unwrap();
            let mut there = base;
            there.segments.get_mut(&id).unwrap().end = Some(start.later(real * 60));
            let (first_here, first_there) = (
                crate::merge::merge(&here, &there),
                crate::merge::merge(&there, &here),
            );
            let mut here = first_here;
            here.reclaim_settled(&on(desktop_skew, 1, desktop));
            let mut there = first_there;
            there.reclaim_settled(&on(laptop_skew, 1, LAPTOP));
            let met = crate::merge::merge(&here, &there);
            prop_assert_eq!(&met, &crate::merge::merge(&there, &here));
            prop_assert!(!met.settlement_due(desktop) && !met.settlement_due(LAPTOP));
            let seg = &met.segments[&id];
            prop_assert_eq!(seg.end, Some(start.later(real * 60)));
            if settled_at == real {
                prop_assert_eq!(seg.effective(), Some((start, seg.end)));
                prop_assert!(seg.edit.as_ref().is_some_and(|e| e.settled));
            } else {
                prop_assert_eq!(&seg.edit, &None);
            }
        }
    }

    #[test]
    fn a_settlement_survives_whichever_copy_closed_the_segment() {
        let (mut settled, _, id) = left_running();
        let owner = settled.clone();
        settle(
            &mut settled,
            &id,
            "2026-01-02T10:00:00Z",
            "2026-01-03T12:00:00Z",
        )
        .unwrap();
        let (ab, ba) = (
            crate::merge::merge(&settled, &owner),
            crate::merge::merge(&owner, &settled),
        );
        assert_eq!(ab.segments[&id], ba.segments[&id]);
        assert!(ab.segments[&id].edit.as_ref().unwrap().settled);
        assert_eq!(ab.segments[&id].end, None);
    }

    fn with_gap(decision: GapDecision) -> Shared {
        let (mut s, t) = world();
        let c = ctx("2026-01-02T09:00:00Z");
        s.start(&c, &t).unwrap();
        let later = ctx("2026-01-02T12:00:00Z");
        s.pause(&later, &t).unwrap();
        let id = s.segments.keys().next().unwrap().clone();
        let g = s
            .add_gap(
                &later,
                &id,
                ts("2026-01-02T10:00:00Z"),
                ts("2026-01-02T11:00:00Z"),
                GapKind::Sleep,
            )
            .unwrap();
        if decision != GapDecision::Pending {
            s.decide_gap(&later, &g, decision).unwrap();
        }
        s
    }

    #[test]
    fn time_away_counts_only_after_it_is_confirmed() {
        let s = with_gap(GapDecision::Pending);
        assert_eq!(
            secs_by(&s, PieceKind::Work),
            2 * 3600,
            "unanswered counts nowhere"
        );
        let seg = s.segments.values().next().unwrap();
        assert_eq!(seg.end, Some(ts("2026-01-02T12:00:00Z")), "raw stays");
        assert_eq!(
            secs_by(&with_gap(GapDecision::Drop), PieceKind::Work),
            2 * 3600
        );
        assert_eq!(
            secs_by(&with_gap(GapDecision::Keep), PieceKind::Work),
            3 * 3600
        );
        let thinking = with_gap(GapDecision::Thinking);
        assert_eq!(secs_by(&thinking, PieceKind::Work), 2 * 3600);
        assert_eq!(secs_by(&thinking, PieceKind::Thinking), 3600);
    }

    #[test]
    fn a_billable_pause_never_counts_time_already_counted() {
        let (mut s, t) = world();
        let c = ctx("2026-01-02T18:00:00Z");
        s.add_manual_entry(
            &c,
            &manual(&t, "2026-01-02T09:00:00Z", "2026-01-02T10:00:00Z"),
        )
        .unwrap();
        s.add_manual_entry(
            &c,
            &manual(&t, "2026-01-02T11:00:00Z", "2026-01-02T12:00:00Z"),
        )
        .unwrap();
        let id = s
            .mark_billable_pause(
                &c,
                &PauseInput {
                    task_id: t.clone(),
                    start: ts("2026-01-02T09:30:00Z"),
                    end: ts("2026-01-02T11:30:00Z"),
                    note: "sketching".into(),
                },
            )
            .unwrap();
        assert_eq!(secs_by(&s, PieceKind::Work), 2 * 3600);
        assert_eq!(secs_by(&s, PieceKind::BillablePause), 3600);
        s.unmark_billable_pause(&c, &id).unwrap();
        assert_eq!(secs_by(&s, PieceKind::BillablePause), 0);
    }

    #[test]
    fn switching_subtask_while_running_splits_the_segment_and_keeps_the_total() {
        let (mut s, t) = world();
        let c = ctx("2026-01-02T09:00:00Z");
        let doc = serde_json::json!({"type":"doc","content":[{"type":"taskList","content":[
            {"type":"taskItem","attrs":{"checked":false,"subtaskId":"a"},"content":[{"type":"paragraph","content":[{"type":"text","text":"One"}]}]},
            {"type":"taskItem","attrs":{"checked":false,"subtaskId":"b"},"content":[{"type":"paragraph","content":[{"type":"text","text":"Two"}]}]}]}]});
        s.tasks.get_mut(&t).unwrap().description = doc;
        s.set_active_subtask(&c, &t, Some("a")).unwrap();
        s.start(&c, &t).unwrap();
        s.set_active_subtask(&ctx("2026-01-02T09:10:00Z"), &t, Some("b"))
            .unwrap();
        s.pause(&ctx("2026-01-02T09:30:00Z"), &t).unwrap();
        let end = |seg: &Segment| seg.end.unwrap_or(seg.start);
        let all = pieces(&s, &end);
        let on = |id: &str| -> i64 {
            all.iter()
                .filter(|p| p.subtask_id.as_deref() == Some(id))
                .map(crate::pieces::Piece::secs)
                .sum()
        };
        assert_eq!((on("a"), on("b")), (600, 1200));
        assert_eq!(
            all.iter().map(crate::pieces::Piece::secs).sum::<i64>(),
            1800
        );
        assert_eq!(s.tasks[&t].state, TaskState::Paused);
        assert!(s.set_active_subtask(&c, &t, Some("nope")).is_err());
    }

    #[test]
    fn r1_a_date_no_reader_would_take_is_refused_when_it_is_entered() {
        let (mut s, t) = world();
        let c = ctx("2026-01-02T12:00:00Z");
        let ok = s
            .add_manual_entry(
                &c,
                &manual(&t, "2026-01-02T09:00:00Z", "2026-01-02T10:00:00Z"),
            )
            .unwrap();
        for (start, end) in [
            ("0026-01-02T09:00:00Z", "0026-01-02T10:00:00Z"),
            ("1999-12-31T23:00:00Z", "1999-12-31T23:30:00Z"),
        ] {
            let mistyped = [
                s.add_manual_entry(&c, &manual(&t, start, end)).map(|_| ()),
                s.correct_segment(
                    &c,
                    &CorrectionInput {
                        segment_id: ok.clone(),
                        start: ts(start),
                        end: ts(end),
                        reason: "x".into(),
                        allow_overlap: true,
                    },
                ),
                s.mark_billable_pause(
                    &c,
                    &PauseInput {
                        task_id: t.clone(),
                        start: ts(start),
                        end: ts(end),
                        note: String::new(),
                    },
                )
                .map(|_| ()),
            ];
            for (i, got) in mistyped.into_iter().enumerate() {
                let e = got.unwrap_err();
                assert_eq!(e.code(), "validation", "{start}, case {i}");
                assert!(
                    e.to_string().contains("2000-01-01"),
                    "{start}, case {i}: {e}"
                );
            }
        }
        // The first moment allowed is one the reader takes, so the two can't drift apart.
        let first = s
            .add_manual_entry(
                &c,
                &manual(&t, "2000-01-01T00:00:00Z", "2000-01-01T01:00:00Z"),
            )
            .unwrap();
        s.mark_billable_pause(
            &c,
            &PauseInput {
                task_id: t.clone(),
                start: ts("2000-01-01T00:00:00Z"),
                end: ts("2000-01-01T00:30:00Z"),
                note: String::new(),
            },
        )
        .unwrap();
        assert!(s.segments.contains_key(&first));
        crate::validate::check_shared(&s, c.now).unwrap();
    }

    fn pause_input(t: &str, a: &str, b: &str) -> PauseInput {
        PauseInput {
            task_id: t.into(),
            start: ts(a),
            end: ts(b),
            note: String::new(),
        }
    }
    fn billed_day(s: &mut Shared, c: &Ctx<'_>, t: &str) -> String {
        let project_id = s.tasks[t].project_id.clone();
        s.create_summary(
            c,
            &crate::summary::SummaryRequest {
                project_id,
                from: "2026-01-02".into(),
                to: "2026-01-02".into(),
                zone: "UTC".into(),
            },
        )
        .unwrap()
    }
    /// A pause on summary A-001, from 10:00 to 11:00.
    fn with_billed_pause() -> (Shared, String, String, Ctx<'static>) {
        let (mut s, t) = world();
        let c = ctx("2026-01-02T18:00:00Z");
        s.mark_billable_pause(
            &c,
            &pause_input(&t, "2026-01-02T10:00:00Z", "2026-01-02T11:00:00Z"),
        )
        .unwrap();
        let summary = billed_day(&mut s, &c, &t);
        assert_eq!(s.summaries[&summary].reference, "A-001");
        (s, t, summary, c)
    }
    fn says_its_on_the_summary(e: &CoreError) {
        assert_eq!(e.code(), "conflict", "{e}");
        assert!(e.to_string().contains("A-001"), "{e}");
    }

    #[test]
    fn r6_a_pause_overlapping_a_billed_one_is_refused_when_it_is_marked() {
        let (mut s, t, summary, c) = with_billed_pause();
        let before = s.pauses.len();
        let over = pause_input(&t, "2026-01-02T09:30:00Z", "2026-01-02T10:30:00Z");
        says_its_on_the_summary(&s.mark_billable_pause(&c, &over).unwrap_err());
        assert_eq!(s.pauses.len(), before, "nothing was stored");
        // Touching it is fine: the two don't share a second.
        s.mark_billable_pause(
            &c,
            &pause_input(&t, "2026-01-02T09:30:00Z", "2026-01-02T10:00:00Z"),
        )
        .unwrap();
        // So is the same time on another task, which bills on its own.
        let other = s
            .create_task(&c, &task_input("Other", &s.tasks[&t].project_id.clone()))
            .unwrap();
        s.mark_billable_pause(
            &c,
            &pause_input(&other, "2026-01-02T10:00:00Z", "2026-01-02T11:00:00Z"),
        )
        .unwrap();
        // Voiding the summary puts the time back to unbilled, and then it can be marked.
        s.void_summary(&c, &summary).unwrap();
        s.mark_billable_pause(&c, &over).unwrap();
    }

    #[test]
    fn r6_a_manual_entry_or_correction_overlapping_a_billed_pause_is_refused() {
        let (mut s, t, summary, c) = with_billed_pause();
        let over = |a: &str, b: &str| ManualEntryInput {
            allow_overlap: true,
            ..manual(&t, a, b)
        };
        says_its_on_the_summary(
            &s.add_manual_entry(&c, &over("2026-01-02T10:30:00Z", "2026-01-02T11:30:00Z"))
                .unwrap_err(),
        );
        let entry = s
            .add_manual_entry(&c, &over("2026-01-02T11:00:00Z", "2026-01-02T12:00:00Z"))
            .unwrap();
        says_its_on_the_summary(
            &s.correct_segment(
                &c,
                &CorrectionInput {
                    segment_id: entry.clone(),
                    start: ts("2026-01-02T10:45:00Z"),
                    end: ts("2026-01-02T12:00:00Z"),
                    reason: "started earlier".into(),
                    allow_overlap: true,
                },
            )
            .unwrap_err(),
        );
        assert_eq!(secs_by(&s, PieceKind::BillablePause), 3600, "untouched");
        s.void_summary(&c, &summary).unwrap();
        s.correct_segment(
            &c,
            &CorrectionInput {
                segment_id: entry,
                start: ts("2026-01-02T10:45:00Z"),
                end: ts("2026-01-02T12:00:00Z"),
                reason: "started earlier".into(),
                allow_overlap: true,
            },
        )
        .unwrap();
    }

    #[test]
    fn r6_taking_a_correction_or_void_off_an_entry_cannot_put_it_over_a_billed_pause() {
        let (mut s, t) = world();
        let c = ctx("2026-01-02T18:00:00Z");
        let entry = s
            .add_manual_entry(
                &c,
                &manual(&t, "2026-01-02T10:30:00Z", "2026-01-02T11:30:00Z"),
            )
            .unwrap();
        s.void_segment(&c, &entry, "wrong day").unwrap();
        s.mark_billable_pause(
            &c,
            &pause_input(&t, "2026-01-02T10:00:00Z", "2026-01-02T11:00:00Z"),
        )
        .unwrap();
        let summary = billed_day(&mut s, &c, &t);
        says_its_on_the_summary(&s.restore_segment(&c, &entry, "my mistake").unwrap_err());
        s.void_summary(&c, &summary).unwrap();
        s.restore_segment(&c, &entry, "my mistake").unwrap();
    }

    fn rows_of(s: &Shared, id: &str) -> Vec<(i64, i64, i64)> {
        s.summaries[id]
            .rows
            .iter()
            .map(|r| (r.exact_secs, r.pause_secs, r.billable_secs))
            .collect()
    }
    fn bill_day(s: &mut Shared, c: &Ctx<'_>, t: &str, day: &str) -> Result<String, CoreError> {
        let project_id = s.tasks[t].project_id.clone();
        s.create_summary(
            c,
            &crate::summary::SummaryRequest {
                project_id,
                from: day.into(),
                to: day.into(),
                zone: "UTC".into(),
            },
        )
    }

    /// The entry runs past midnight and the pause is billed alone for the next day, holding only what the
    /// entry leaves it. Neither undoing a void nor shortening the entry takes anything from what it holds.
    #[test]
    fn r6_a_change_that_takes_nothing_from_what_a_summary_holds_is_allowed() {
        let (mut s, t) = world();
        let c = ctx("2026-01-03T18:00:00Z");
        let entry = s
            .add_manual_entry(
                &c,
                &ManualEntryInput {
                    allow_overlap: true,
                    ..manual(&t, "2026-01-01T23:00:00Z", "2026-01-02T01:00:00Z")
                },
            )
            .unwrap();
        s.mark_billable_pause(
            &c,
            &pause_input(&t, "2026-01-02T00:30:00Z", "2026-01-02T02:00:00Z"),
        )
        .unwrap();
        let summary = bill_day(&mut s, &c, &t, "2026-01-02").unwrap();
        assert!(s.summaries[&summary].segment_ids.is_empty());
        assert_eq!(rows_of(&s, &summary)[0].1, 3600, "01:00 to 02:00 only");
        s.correct_segment(
            &c,
            &CorrectionInput {
                segment_id: entry.clone(),
                start: ts("2026-01-01T23:00:00Z"),
                end: ts("2026-01-02T00:45:00Z"),
                reason: "left early".into(),
                allow_overlap: true,
            },
        )
        .unwrap();
        s.void_segment(&c, &entry, "wrong").unwrap();
        s.restore_segment(&c, &entry, "my mistake").unwrap();
        assert!(s.pause_time_short().is_empty());
        // Going over what the summary does hold is still refused, even by a little.
        says_its_on_the_summary(
            &s.correct_segment(
                &c,
                &CorrectionInput {
                    segment_id: entry,
                    start: ts("2026-01-01T23:00:00Z"),
                    end: ts("2026-01-02T01:01:00Z"),
                    reason: "left late".into(),
                    allow_overlap: true,
                },
            )
            .unwrap_err(),
        );
    }

    #[test]
    fn r6_billing_the_later_of_two_overlapping_pauses_first_holds_every_second_once() {
        let (mut s, t) = world();
        let c = ctx("2026-01-03T18:00:00Z");
        s.mark_billable_pause(
            &c,
            &pause_input(&t, "2026-01-01T23:30:00Z", "2026-01-02T00:30:00Z"),
        )
        .unwrap();
        s.mark_billable_pause(
            &c,
            &pause_input(&t, "2026-01-02T00:00:00Z", "2026-01-02T01:00:00Z"),
        )
        .unwrap();
        assert_eq!(secs_by(&s, PieceKind::BillablePause), 5400);
        let later = bill_day(&mut s, &c, &t, "2026-01-02").unwrap();
        let earlier = bill_day(&mut s, &ctx("2026-01-03T18:05:00Z"), &t, "2026-01-01").unwrap();
        let held: i64 = [&later, &earlier]
            .iter()
            .flat_map(|id| rows_of(&s, id))
            .map(|r| r.1)
            .sum();
        assert_eq!(held, 5400);
        assert_eq!(secs_by(&s, PieceKind::BillablePause), 5400);
        assert!(s.pause_time_short().is_empty());
    }

    /// Possible in the core though the page doesn't offer it: a summary treats a running timer as empty.
    #[test]
    fn r6_a_pause_a_timer_then_closes_over_shows_as_short_on_its_summary() {
        let (mut s, t) = world();
        s.start(&ctx("2026-01-02T09:30:00Z"), &t).unwrap();
        let c = ctx("2026-01-02T10:30:00Z");
        s.mark_billable_pause(
            &c,
            &pause_input(&t, "2026-01-02T09:00:00Z", "2026-01-02T10:00:00Z"),
        )
        .unwrap();
        let summary = billed_day(&mut s, &c, &t);
        assert!(s.pause_time_short().is_empty(), "the timer is empty so far");
        s.stop(&ctx("2026-01-02T11:00:00Z"), &t).unwrap();
        assert_eq!(s.pause_time_short().get(summary.as_str()), Some(&1800));
    }

    /// The later summary is dated earlier, on a clock that is behind, so live the pauses share the time the
    /// other way round from how they were frozen. It has only moved between summaries, and each second is on one.
    #[test]
    fn r6_a_summary_dated_before_one_it_saw_is_not_taken_for_billing_twice() {
        let (mut s, t, first, _) = with_billed_pause();
        pause_from_elsewhere(
            &mut s,
            &t,
            "elsewhere",
            "2026-01-02T09:30:00Z",
            "2026-01-02T10:30:00Z",
        );
        let behind = ctx("2026-01-02T17:00:00Z");
        let second = bill_day(&mut s, &behind, &t, "2026-01-02").unwrap();
        assert_eq!(rows_of(&s, &first)[0].1, 3600);
        assert_eq!(rows_of(&s, &second)[0].1, 1800);
        assert_eq!(secs_by(&s, PieceKind::BillablePause), 5400);
        assert!(s.pause_time_short().is_empty());
    }

    #[test]
    fn r6_a_billed_pause_removed_where_the_summary_had_not_arrived_shows_as_short_on_it() {
        let (mut s, _, summary, _) = with_billed_pause();
        let pause = s.summaries[&summary].pause_ids[0].clone();
        s.pauses.get_mut(&pause).unwrap().deleted = true;
        assert_eq!(s.pause_time_short().get(summary.as_str()), Some(&3600));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        /// However pauses overlap and whichever day is billed first, the summaries hold the time there was,
        /// each second once, and go on holding what the live figures say.
        #[test]
        fn r6_billing_overlapping_pauses_in_any_order_holds_each_second_once(
            spans in prop::collection::vec((0i64..5760, 1i64..1500), 1..7),
            order in Just(vec![0, 1, 2, 3]).prop_shuffle(),
        ) {
            let (mut s, t) = world();
            let base = ts("2026-01-05T00:00:00Z").secs();
            let at = |minute: i64| Ts(base + minute * 60).iso();
            let mut covered = std::collections::BTreeSet::new();
            for (i, (start, len)) in spans.iter().enumerate() {
                pause_from_elsewhere(&mut s, &t, &format!("p{i}"), &at(*start), &at(start + len));
                covered.extend(*start..start + len);
            }
            let there_was = 60 * i64::try_from(covered.len()).unwrap();
            let c = ctx("2026-01-20T12:00:00Z");
            let mut held = 0;
            for day in order {
                if let Ok(id) = bill_day(&mut s, &c, &t, &format!("2026-01-{:02}", 5 + day)) {
                    held += rows_of(&s, &id).iter().map(|r| r.1).sum::<i64>();
                }
            }
            prop_assert_eq!(held, there_was);
            prop_assert_eq!(secs_by(&s, PieceKind::BillablePause), there_was);
            prop_assert!(s.pause_time_short().is_empty());
        }
    }

    /// As when the second pause was marked on a device that hadn't yet seen the summary.
    fn pause_from_elsewhere(s: &mut Shared, t: &str, id: &str, a: &str, b: &str) {
        s.pauses.insert(
            id.into(),
            BillablePause {
                id: id.into(),
                task_id: t.into(),
                start: ts(a),
                end: ts(b),
                note: String::new(),
                deleted: false,
                stamp: ctx("2026-01-02T17:00:00Z").stamp(),
            },
        );
    }

    #[test]
    fn r6_a_billed_pause_keeps_its_claim_over_one_that_starts_earlier() {
        let (mut s, t, summary, c) = with_billed_pause();
        let billed = s.summaries[&summary].pause_ids[0].clone();
        pause_from_elsewhere(
            &mut s,
            &t,
            "elsewhere",
            "2026-01-02T09:30:00Z",
            "2026-01-02T10:30:00Z",
        );
        let end = |seg: &Segment| seg.end.unwrap_or(seg.start);
        let spans_of = |s: &Shared, id: &str| -> Vec<(String, String)> {
            pieces(s, &end)
                .iter()
                .filter(|p| p.source_id == id)
                .map(|p| (p.start.iso(), p.end.iso()))
                .collect()
        };
        assert_eq!(
            spans_of(&s, &billed),
            vec![(
                "2026-01-02T10:00:00Z".to_owned(),
                "2026-01-02T11:00:00Z".to_owned()
            )],
            "what the summary lists stays what it lists"
        );
        assert_eq!(
            spans_of(&s, "elsewhere"),
            vec![(
                "2026-01-02T09:30:00Z".to_owned(),
                "2026-01-02T10:00:00Z".to_owned()
            )]
        );
        // The next summary bills what is left, so the two hold the 1.5 h there was.
        let next = billed_day(&mut s, &ctx("2026-01-02T18:05:00Z"), &t);
        let billable = |s: &Shared, id: &str| -> i64 {
            s.summaries[id].rows.iter().map(|r| r.billable_secs).sum()
        };
        assert_eq!(billable(&s, &summary), 3600);
        assert_eq!(billable(&s, &next), 1800);
        // Billed too, it still has only what its summary lists: the earlier summary keeps the first claim.
        assert_eq!(
            spans_of(&s, &billed),
            vec![(
                "2026-01-02T10:00:00Z".to_owned(),
                "2026-01-02T11:00:00Z".to_owned()
            )]
        );
        assert_eq!(
            spans_of(&s, "elsewhere"),
            vec![(
                "2026-01-02T09:30:00Z".to_owned(),
                "2026-01-02T10:00:00Z".to_owned()
            )]
        );
        // With nothing billed, the earlier start claims the span again, as it always did.
        s.void_summary(&c, &next).unwrap();
        s.void_summary(&c, &summary).unwrap();
        assert_eq!(
            spans_of(&s, "elsewhere"),
            vec![(
                "2026-01-02T09:30:00Z".to_owned(),
                "2026-01-02T10:30:00Z".to_owned()
            )]
        );
        assert_eq!(secs_by(&s, PieceKind::BillablePause), 5400);
    }

    #[test]
    fn r6_billed_pauses_that_overlap_each_other_share_the_time_the_same_way_on_every_device() {
        let (mut s, t, _, c) = with_billed_pause();
        pause_from_elsewhere(
            &mut s,
            &t,
            "elsewhere",
            "2026-01-02T09:30:00Z",
            "2026-01-02T10:30:00Z",
        );
        billed_day(&mut s, &c, &t);
        pause_from_elsewhere(
            &mut s,
            &t,
            "later",
            "2026-01-02T08:00:00Z",
            "2026-01-02T12:00:00Z",
        );
        // The billed ones first, then the rest by start: 09:30 to 11:00, and what the last one has left.
        assert_eq!(secs_by(&s, PieceKind::BillablePause), 4 * 3600);
        let end = |seg: &Segment| seg.end.unwrap_or(seg.start);
        let all = pieces(&s, &end);
        let last: i64 = all
            .iter()
            .filter(|p| p.source_id == "later")
            .map(crate::pieces::Piece::secs)
            .sum();
        assert_eq!(last, 5400 + 3600);
        assert_eq!(all, pieces(&s.clone(), &end));
    }
}
