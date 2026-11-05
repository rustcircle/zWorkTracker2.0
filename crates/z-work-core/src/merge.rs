//! Deterministic state merge between devices. Each replicated record is merged by taking the
//! maximum under a total order, which makes `merge` idempotent, commutative and associative.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::Serialize;

use crate::model::{
    AuditEntry, BillablePause, BillingSummary, DevicePrefix, IdleGap, PauseRequest, Project,
    RateVersion, RoundingVersion, Segment, SettingEntry, Shared, ShiftVersion, Stamp, Tag, Task,
    Template,
};

fn canonical<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

fn stamp_cmp(a: &Stamp, b: &Stamp) -> Ordering {
    a.at.cmp(&b.at).then_with(|| a.by.cmp(&b.by))
}

/// Last writer wins by stamp, with the content as a final tie-break so the order stays total.
fn lww<'a, T: Serialize>(a: &'a T, sa: &Stamp, b: &'a T, sb: &Stamp) -> &'a T {
    match stamp_cmp(sa, sb).then_with(|| canonical(a).cmp(&canonical(b))) {
        Ordering::Less => b,
        _ => a,
    }
}

/// Like `lww`, but a tombstone outranks any edit, so a deleted record never comes back.
fn lww_tombstone<'a, T: Serialize>(
    a: &'a T,
    da: bool,
    sa: &Stamp,
    b: &'a T,
    db: bool,
    sb: &Stamp,
) -> &'a T {
    match da
        .cmp(&db)
        .then_with(|| stamp_cmp(sa, sb))
        .then_with(|| canonical(a).cmp(&canonical(b)))
    {
        Ordering::Less => b,
        _ => a,
    }
}

/// Each group of fields merges by its own clock, so a rename on one device and a fee on another both survive.
/// A tombstone outranks everything, so a deleted project never comes back.
///
/// A copy from a build without these clocks says that all its fields were written at its whole stamp, which is
/// what that build means by it. Nothing can tell a stale field on such a copy from an edit, so each one wins over
/// a group clock older than that stamp: a rename made there can carry a stale fee along and wipe the fee set
/// elsewhere before it, and take the rename along too, where a whole-record merge would lose one or the other
/// whole. A copy it merely passes on holds the same values, so only clocks rise, once. Every copy ends up the same.
fn merge_project(a: &Project, b: &Project) -> Project {
    let identity = |p: &Project| (p.name.clone(), p.color.clone(), p.client_zone.clone());
    let billing = |p: &Project| {
        (
            p.billable,
            p.billing_type,
            p.fixed_fee_cents,
            p.fee_currency.clone(),
        )
    };
    let ((name, color, client_zone), identity_stamp) = field(
        &identity(a),
        a.identity_clock(),
        &identity(b),
        b.identity_clock(),
    );
    let ((billable, billing_type, fixed_fee_cents, fee_currency), billing_stamp) = field(
        &billing(a),
        a.billing_clock(),
        &billing(b),
        b.billing_clock(),
    );
    let (budget_hours, budget_stamp) = field(
        &a.budget_hours,
        a.budget_clock(),
        &b.budget_hours,
        b.budget_clock(),
    );
    let (archived, archive_stamp) = field(
        &a.archived,
        a.archive_clock(),
        &b.archived,
        b.archive_clock(),
    );
    Project {
        id: a.id.clone(),
        name,
        color,
        archived,
        deleted: a.deleted || b.deleted,
        billable,
        budget_hours,
        client_zone,
        billing_type,
        fixed_fee_cents,
        fee_currency,
        created_at: a.created_at.min(b.created_at),
        stamp: if stamp_cmp(&a.stamp, &b.stamp) == Ordering::Less {
            b.stamp.clone()
        } else {
            a.stamp.clone()
        },
        identity_stamp: Some(identity_stamp),
        billing_stamp: Some(billing_stamp),
        budget_stamp: Some(budget_stamp),
        archive_stamp: Some(archive_stamp),
    }
}

fn merge_shift(a: &ShiftVersion, b: &ShiftVersion) -> ShiftVersion {
    lww_tombstone(a, a.deleted, &a.stamp, b, b.deleted, &b.stamp).clone()
}

fn merge_rate(a: &RateVersion, b: &RateVersion) -> RateVersion {
    lww_tombstone(a, a.deleted, &a.stamp, b, b.deleted, &b.stamp).clone()
}

fn merge_rounding(a: &RoundingVersion, b: &RoundingVersion) -> RoundingVersion {
    lww_tombstone(a, a.deleted, &a.stamp, b, b.deleted, &b.stamp).clone()
}

fn merge_setting(a: &SettingEntry, b: &SettingEntry) -> SettingEntry {
    lww(a, &a.stamp, b, &b.stamp).clone()
}

/// Either copy's marker stands for good. Between two, the later, so any order of merging gives the same.
fn sticky(a: &Option<Stamp>, b: &Option<Stamp>) -> Option<Stamp> {
    match (a, b) {
        (Some(x), Some(y)) if stamp_cmp(x, y) == Ordering::Less => Some(y.clone()),
        (Some(x), _) => Some(x.clone()),
        (None, y) => y.clone(),
    }
}

/// The fields that travel with the state clock.
#[derive(Serialize)]
struct StateKey<'a>(
    &'a crate::model::TaskState,
    &'a Option<crate::model::PauseReason>,
);

/// Per-field last writer wins. A name edit on one device and a description edit on another both survive.
fn merge_task(a: &Task, b: &Task) -> Task {
    let (name, name_stamp) = field(&a.name, &a.name_stamp, &b.name, &b.name_stamp);
    let (project_id, project_stamp) = field(
        &a.project_id,
        &a.project_stamp,
        &b.project_id,
        &b.project_stamp,
    );
    let (description, description_stamp) = field(
        &a.description,
        &a.description_stamp,
        &b.description,
        &b.description_stamp,
    );
    let (deleted, deleted_stamp) =
        field(&a.deleted, &a.deleted_stamp, &b.deleted, &b.deleted_stamp);
    let (tags, tags_stamp) = field(&a.tags, &a.tags_stamp, &b.tags, &b.tags_stamp);
    let (active_subtask, active_subtask_stamp) = field(
        &a.active_subtask,
        &a.active_subtask_stamp,
        &b.active_subtask,
        &b.active_subtask_stamp,
    );
    // Stopped is final: it outranks any other state whatever the clocks say.
    let state_key = |t: &'_ Task| canonical(&StateKey(&t.state, &t.pause_reason));
    let state_from_a = a
        .state
        .rank()
        .cmp(&b.state.rank())
        .then_with(|| stamp_cmp(&a.state_stamp, &b.state_stamp))
        .then_with(|| state_key(a).cmp(&state_key(b)))
        != Ordering::Less;
    let state = if state_from_a { a } else { b };
    Task {
        id: a.id.clone(),
        name,
        name_stamp,
        project_id,
        project_stamp,
        description,
        description_stamp,
        deleted,
        deleted_stamp,
        discarded: sticky(&a.discarded, &b.discarded),
        continued_from: a.continued_from.clone().max(b.continued_from.clone()),
        created_at: a.created_at.min(b.created_at),
        state: state.state,
        pause_reason: state.pause_reason.clone(),
        state_stamp: state.state_stamp.clone(),
        stopped_at: a.stopped_at.max(b.stopped_at),
        tags,
        tags_stamp,
        active_subtask,
        active_subtask_stamp,
    }
}

fn field<T: Serialize + Clone>(a: &T, sa: &Stamp, b: &T, sb: &Stamp) -> (T, Stamp) {
    let from_a = stamp_cmp(sa, sb).then_with(|| canonical(a).cmp(&canonical(b))) != Ordering::Less;
    if from_a {
        (a.clone(), sa.clone())
    } else {
        (b.clone(), sb.clone())
    }
}

/// The recorded times: a closed copy beats an open copy, and between closed copies the later end
/// wins. A correction merges separately, so it survives whichever copy closed the segment.
fn merge_segment(a: &Segment, b: &Segment) -> Segment {
    let rank = |s: &Segment| (s.end.is_some(), s.end);
    let raw = |s: &Segment| {
        let mut r = s.clone();
        r.edit = None;
        r.edit_stamp = Stamp::default();
        canonical(&r)
    };
    let base = match rank(a).cmp(&rank(b)).then_with(|| raw(a).cmp(&raw(b))) {
        Ordering::Less => b,
        _ => a,
    };
    let (edit, edit_stamp) = field(&a.edit, &a.edit_stamp, &b.edit, &b.edit_stamp);
    Segment {
        edit,
        edit_stamp,
        ..base.clone()
    }
}

/// Only the decision changes after a gap is recorded.
fn merge_gap(a: &IdleGap, b: &IdleGap) -> IdleGap {
    let (decision, decision_stamp) = field(
        &a.decision,
        &a.decision_stamp,
        &b.decision,
        &b.decision_stamp,
    );
    let fixed = |g: &IdleGap| {
        let mut f = g.clone();
        f.decision = crate::model::GapDecision::Pending;
        f.decision_stamp = Stamp::default();
        canonical(&f)
    };
    let base = if fixed(a) >= fixed(b) { a } else { b };
    IdleGap {
        decision,
        decision_stamp,
        ..base.clone()
    }
}

fn merge_pause(a: &BillablePause, b: &BillablePause) -> BillablePause {
    lww_tombstone(a, a.deleted, &a.stamp, b, b.deleted, &b.stamp).clone()
}

fn merge_tag(a: &Tag, b: &Tag) -> Tag {
    lww_tombstone(a, a.deleted, &a.stamp, b, b.deleted, &b.stamp).clone()
}

fn merge_template(a: &Template, b: &Template) -> Template {
    lww_tombstone(a, a.deleted, &a.stamp, b, b.deleted, &b.stamp).clone()
}

/// A voided summary stays voided, so its time can't silently return to billed.
fn merge_summary(a: &BillingSummary, b: &BillingSummary) -> BillingSummary {
    lww_tombstone(a, a.voided, &a.stamp, b, b.voided, &b.stamp).clone()
}

fn merge_pause_request(a: &PauseRequest, b: &PauseRequest) -> PauseRequest {
    lww(a, &a.stamp, b, &b.stamp).clone()
}

fn merge_prefix(a: &DevicePrefix, b: &DevicePrefix) -> DevicePrefix {
    lww(a, &a.stamp, b, &b.stamp).clone()
}

/// The audit trail only grows. The same id always holds the same entry, barring a damaged file.
fn merge_audit(a: &AuditEntry, b: &AuditEntry) -> AuditEntry {
    if canonical(a) >= canonical(b) {
        a.clone()
    } else {
        b.clone()
    }
}

fn merge_map<T: Clone + PartialEq>(
    a: &BTreeMap<String, T>,
    b: &BTreeMap<String, T>,
    f: impl Fn(&T, &T) -> T,
) -> BTreeMap<String, T> {
    let mut out = a.clone();
    for (k, vb) in b {
        match out.get_mut(k) {
            // Devices mostly hold the same records, and a merge is idempotent: nothing to work out.
            Some(va) if *va == *vb => {}
            Some(va) => *va = f(va, vb),
            None => {
                out.insert(k.clone(), vb.clone());
            }
        }
    }
    out
}

#[must_use]
pub fn merge(a: &Shared, b: &Shared) -> Shared {
    Shared {
        projects: merge_map(&a.projects, &b.projects, merge_project),
        tasks: merge_map(&a.tasks, &b.tasks, merge_task),
        segments: merge_map(&a.segments, &b.segments, merge_segment),
        settings: merge_map(&a.settings, &b.settings, merge_setting),
        shifts: merge_map(&a.shifts, &b.shifts, merge_shift),
        rates: merge_map(&a.rates, &b.rates, merge_rate),
        rounding: merge_map(&a.rounding, &b.rounding, merge_rounding),
        gaps: merge_map(&a.gaps, &b.gaps, merge_gap),
        pauses: merge_map(&a.pauses, &b.pauses, merge_pause),
        tags: merge_map(&a.tags, &b.tags, merge_tag),
        templates: merge_map(&a.templates, &b.templates, merge_template),
        summaries: merge_map(&a.summaries, &b.summaries, merge_summary),
        prefixes: merge_map(&a.prefixes, &b.prefixes, merge_prefix),
        pause_requests: merge_map(&a.pause_requests, &b.pause_requests, merge_pause_request),
        audit_log: merge_map(&a.audit_log, &b.audit_log, merge_audit),
    }
}

/// Strategies for replicated data, for the merge laws here and the hybrid logical clock's properties (`hlc`).
#[cfg(test)]
pub(crate) mod strategies {
    use super::*;
    use crate::model::{PauseReason, TaskState};
    use crate::time::Ts;
    use proptest::prelude::*;
    use serde_json::{Value, json};

    pub(crate) fn stamp() -> impl Strategy<Value = Stamp> {
        (0i64..4, prop::sample::select(vec!["a", "b", "c"])).prop_map(|(t, d)| Stamp {
            at: Ts(t),
            by: d.to_owned(),
        })
    }

    /// With any mix of group clocks, as copies from older and newer builds meet in one folder.
    pub(crate) fn project() -> impl Strategy<Value = Project> {
        (
            (0u8..3, any::<bool>(), any::<bool>(), 0u8..3, stamp()),
            (0u8..2, prop::option::of(0u8..3), 0i64..2),
            (any::<bool>(), 0u8..3, prop::option::of(0u8..3)),
            (
                prop::option::of(stamp()),
                prop::option::of(stamp()),
                prop::option::of(stamp()),
                prop::option::of(stamp()),
            ),
        )
            .prop_map(
                |(
                    (id, archived, deleted, n, stamp),
                    (color, zone, created),
                    (fixed, fee, budget),
                    (identity_stamp, billing_stamp, budget_stamp, archive_stamp),
                )| Project {
                    id: format!("p{id}"),
                    name: format!("name{n}"),
                    color: format!("#11223{color}"),
                    archived,
                    deleted,
                    billable: n == 1,
                    budget_hours: budget.map(f64::from),
                    client_zone: zone.map(|z| format!("zone{z}")),
                    billing_type: if fixed {
                        crate::model::BillingType::FixedFee
                    } else {
                        crate::model::BillingType::Hourly
                    },
                    fixed_fee_cents: fixed.then_some(i64::from(fee) * 100),
                    fee_currency: fixed.then(|| "EUR".to_owned()),
                    created_at: Ts(created),
                    stamp,
                    identity_stamp,
                    billing_stamp,
                    budget_stamp,
                    archive_stamp,
                },
            )
    }

    fn task() -> impl Strategy<Value = Task> {
        (
            (0u8..3, 0u8..3, any::<bool>(), stamp(), stamp(), stamp()),
            (stamp(), 0u8..4, stamp(), prop::option::of(0i64..4), 0u8..3),
            (stamp(), stamp(), 0u8..3, 0u8..3, prop::option::of(stamp())),
        )
            .prop_map(
                |(
                    (id, n, deleted, name_stamp, project_stamp, description_stamp),
                    (deleted_stamp, st, state_stamp, stopped, m),
                    (tags_stamp, active_subtask_stamp, tag, active, discarded),
                )| Task {
                    id: format!("t{id}"),
                    name: format!("name{n}"),
                    name_stamp,
                    project_id: format!("p{m}"),
                    project_stamp,
                    description: if m == 1 {
                        json!({"type":"doc"})
                    } else {
                        Value::Null
                    },
                    description_stamp,
                    deleted,
                    deleted_stamp,
                    discarded,
                    continued_from: (m == 2).then(|| format!("t{n}")),
                    created_at: Ts(i64::from(n)),
                    state: [
                        TaskState::Draft,
                        TaskState::Running,
                        TaskState::Paused,
                        TaskState::Stopped,
                    ][st as usize],
                    pause_reason: (st == 2).then_some(PauseReason::User),
                    state_stamp,
                    stopped_at: stopped.map(Ts),
                    tags: (0..tag).map(|i| format!("g{i}")).collect(),
                    tags_stamp,
                    active_subtask: (active > 0).then(|| format!("a{active}")),
                    active_subtask_stamp,
                },
            )
    }

    fn segment() -> impl Strategy<Value = Segment> {
        (
            0u8..4,
            prop::option::of(1i64..5),
            prop::option::of((1i64..5, any::<bool>())),
            stamp(),
        )
            .prop_map(|(id, end, edit, edit_stamp)| Segment {
                id: format!("s{id}"),
                task_id: "t0".into(),
                device_id: "a".into(),
                start: Ts(0),
                end: end.map(Ts),
                edit: edit.map(|(e, settled)| crate::model::SegmentEdit {
                    start: Ts(0),
                    end: Ts(e),
                    voided: e == 2,
                    reason: String::new(),
                    settled,
                }),
                edit_stamp,
                ..Segment::default()
            })
    }

    fn gap() -> impl Strategy<Value = IdleGap> {
        (0u8..3, 0u8..4, stamp()).prop_map(|(id, d, decision_stamp)| IdleGap {
            id: format!("g{id}"),
            segment_id: "s0".into(),
            task_id: "t0".into(),
            device_id: "a".into(),
            start: Ts(0),
            end: Ts(1),
            kind: crate::model::GapKind::Sleep,
            decision: [
                crate::model::GapDecision::Pending,
                crate::model::GapDecision::Keep,
                crate::model::GapDecision::Drop,
                crate::model::GapDecision::Thinking,
            ][d as usize],
            decision_stamp,
        })
    }

    fn pause() -> impl Strategy<Value = BillablePause> {
        (0u8..3, any::<bool>(), 0u8..3, stamp()).prop_map(|(id, deleted, n, stamp)| BillablePause {
            id: format!("b{id}"),
            task_id: "t0".into(),
            start: Ts(0),
            end: Ts(i64::from(n) + 1),
            note: format!("{n}"),
            deleted,
            stamp,
        })
    }

    fn tag() -> impl Strategy<Value = Tag> {
        (0u8..3, any::<bool>(), 0u8..3, stamp()).prop_map(|(id, deleted, n, stamp)| Tag {
            id: format!("g{id}"),
            name: format!("n{n}"),
            color: "#000000".into(),
            deleted,
            stamp,
        })
    }

    fn template() -> impl Strategy<Value = Template> {
        (0u8..3, any::<bool>(), 0u8..3, stamp()).prop_map(|(id, deleted, n, stamp)| Template {
            id: format!("m{id}"),
            name: format!("n{n}"),
            project_id: None,
            description: Value::Null,
            tags: Vec::new(),
            deleted,
            stamp,
        })
    }

    fn summary() -> impl Strategy<Value = BillingSummary> {
        (0u8..3, any::<bool>(), 0u32..3, stamp()).prop_map(|(id, voided, number, stamp)| {
            BillingSummary {
                id: format!("y{id}"),
                reference: format!("A-{number}"),
                prefix: "A".into(),
                number,
                device_id: "a".into(),
                project_id: "p0".into(),
                from: "2026-01-01".into(),
                to: "2026-01-02".into(),
                zone: "UTC".into(),
                created_at: Ts(0),
                segment_ids: vec![format!("s{number}")],
                pause_ids: Vec::new(),
                rows: Vec::new(),
                totals: BTreeMap::new(),
                voided,
                stamp,
            }
        })
    }

    fn audit() -> impl Strategy<Value = AuditEntry> {
        (0u8..3, 0u8..2).prop_map(|(id, n)| AuditEntry {
            id: format!("z{id}"),
            at: Ts(0),
            by: "a".into(),
            entity: "segment".into(),
            entity_id: "s0".into(),
            action: format!("a{n}"),
            reason: String::new(),
            before: Value::Null,
            after: Value::Null,
        })
    }

    fn prefix() -> impl Strategy<Value = (String, DevicePrefix)> {
        (0u8..2, 0u8..3, stamp()).prop_map(|(d, n, stamp)| {
            (
                format!("d{d}"),
                DevicePrefix {
                    prefix: format!("P{n}"),
                    stamp,
                },
            )
        })
    }

    fn pause_request() -> impl Strategy<Value = PauseRequest> {
        (0u8..3, 0u8..3, 0i64..4, stamp()).prop_map(|(id, n, at, stamp)| PauseRequest {
            id: format!("q{id}"),
            task_id: format!("t{n}"),
            segment_id: format!("s{n}"),
            requested_by: "b".into(),
            at: Ts(at),
            stamp,
        })
    }

    #[allow(clippy::type_complexity)]
    fn phase2() -> impl Strategy<
        Value = (
            Vec<IdleGap>,
            Vec<BillablePause>,
            Vec<Tag>,
            Vec<Template>,
            Vec<BillingSummary>,
            Vec<AuditEntry>,
            Vec<(String, DevicePrefix)>,
            Vec<PauseRequest>,
        ),
    > {
        (
            prop::collection::vec(gap(), 0..3),
            prop::collection::vec(pause(), 0..3),
            prop::collection::vec(tag(), 0..3),
            prop::collection::vec(template(), 0..3),
            prop::collection::vec(summary(), 0..3),
            prop::collection::vec(audit(), 0..3),
            prop::collection::vec(prefix(), 0..2),
            prop::collection::vec(pause_request(), 0..3),
        )
    }

    fn setting() -> impl Strategy<Value = (String, SettingEntry)> {
        (0u8..2, 0u8..3, stamp()).prop_map(|(k, v, stamp)| {
            (
                format!("k{k}"),
                SettingEntry {
                    value: json!(v),
                    stamp,
                },
            )
        })
    }

    fn shift() -> impl Strategy<Value = ShiftVersion> {
        (0u8..2, 0u8..3, any::<bool>(), stamp()).prop_map(|(id, n, deleted, stamp)| ShiftVersion {
            id: format!("v{id}"),
            shift_id: "x".into(),
            effective_from: "2026-01-01".into(),
            name: format!("n{n}"),
            color: "#000000".into(),
            start: "09:00".into(),
            end: "17:00".into(),
            weekdays: vec![1],
            zone: "UTC".into(),
            break_minutes: 0,
            retired: false,
            deleted,
            stamp,
        })
    }

    fn rate() -> impl Strategy<Value = RateVersion> {
        (0u8..2, prop::option::of(0i64..3), any::<bool>(), stamp()).prop_map(
            |(id, rate_cents, deleted, stamp)| RateVersion {
                id: format!("r{id}"),
                project_id: "p0".into(),
                effective_from: Ts(0),
                rate_cents,
                currency: if deleted { "EUR".into() } else { "USD".into() },
                deleted,
                stamp,
            },
        )
    }

    fn rounding() -> impl Strategy<Value = RoundingVersion> {
        (0u8..2, 0u32..3, any::<bool>(), stamp()).prop_map(|(id, m, deleted, stamp)| {
            RoundingVersion {
                id: format!("o{id}"),
                project_id: None,
                effective_from: "2026-01-01".into(),
                increment_minutes: m * 15,
                method: if m == 1 {
                    crate::model::RoundMethod::Up
                } else {
                    crate::model::RoundMethod::Nearest
                },
                deleted,
                stamp,
            }
        })
    }

    pub(crate) fn shared() -> impl Strategy<Value = Shared> {
        (
            prop::collection::vec(project(), 0..4),
            prop::collection::vec(task(), 0..4),
            prop::collection::vec(segment(), 0..5),
            prop::collection::vec(setting(), 0..3),
            prop::collection::vec(shift(), 0..3),
            prop::collection::vec(rate(), 0..3),
            prop::collection::vec(rounding(), 0..3),
            phase2(),
        )
            .prop_map(
                |(ps, ts, ss, st, sh, ra, ro, (gs, bs, ta, te, su, au, pr, pq))| Shared {
                    projects: ps.into_iter().map(|p| (p.id.clone(), p)).collect(),
                    tasks: ts.into_iter().map(|t| (t.id.clone(), t)).collect(),
                    segments: ss.into_iter().map(|s| (s.id.clone(), s)).collect(),
                    settings: st.into_iter().collect(),
                    shifts: sh.into_iter().map(|s| (s.id.clone(), s)).collect(),
                    rates: ra.into_iter().map(|r| (r.id.clone(), r)).collect(),
                    rounding: ro.into_iter().map(|r| (r.id.clone(), r)).collect(),
                    gaps: gs.into_iter().map(|x| (x.id.clone(), x)).collect(),
                    pauses: bs.into_iter().map(|x| (x.id.clone(), x)).collect(),
                    tags: ta.into_iter().map(|x| (x.id.clone(), x)).collect(),
                    templates: te.into_iter().map(|x| (x.id.clone(), x)).collect(),
                    summaries: su.into_iter().map(|x| (x.id.clone(), x)).collect(),
                    audit_log: au.into_iter().map(|x| (x.id.clone(), x)).collect(),
                    prefixes: pr.into_iter().collect(),
                    pause_requests: pq.into_iter().map(|x| (x.id.clone(), x)).collect(),
                },
            )
    }
}

#[cfg(test)]
mod tests {
    use super::strategies::{project, shared};
    use super::*;
    use crate::model::{BillingType, TaskState};
    use crate::time::Ts;
    use proptest::prelude::*;
    use serde::Deserialize;
    use serde_json::{Value, json};

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(600))]

        #[test]
        fn idempotent(a in shared()) {
            prop_assert_eq!(merge(&a, &a), a);
        }

        #[test]
        fn commutative(a in shared(), b in shared()) {
            prop_assert_eq!(merge(&a, &b), merge(&b, &a));
        }

        #[test]
        fn associative(a in shared(), b in shared(), c in shared()) {
            prop_assert_eq!(merge(&merge(&a, &b), &c), merge(&a, &merge(&b, &c)));
        }

        #[test]
        fn absorbs_what_it_has_seen(a in shared(), b in shared()) {
            let m = merge(&a, &b);
            prop_assert_eq!(merge(&m, &a), m.clone());
            prop_assert_eq!(merge(&m, &b), m);
        }

        #[test]
        fn the_laws_hold_for_copies_of_one_project(a in project(), b in project(), c in project()) {
            let one = |p: Project| Shared {
                projects: BTreeMap::from([(
                    "p".to_owned(),
                    Project { id: "p".to_owned(), ..p },
                )]),
                ..Shared::default()
            };
            let (a, b, c) = (one(a), one(b), one(c));
            prop_assert_eq!(merge(&a, &a), a.clone());
            prop_assert_eq!(merge(&a, &b), merge(&b, &a));
            prop_assert_eq!(merge(&merge(&a, &b), &c), merge(&a, &merge(&b, &c)));
            let m = merge(&a, &b);
            prop_assert_eq!(merge(&m, &a), m.clone());
            prop_assert_eq!(merge(&m, &b), m);
        }

        #[test]
        fn each_group_of_a_project_comes_whole_from_the_copy_with_its_later_clock(
            a in project(),
            b in project(),
        ) {
            let b = Project { id: a.id.clone(), ..b };
            let m = merge_project(&a, &b);
            let later = |x: &Stamp, y: &Stamp| stamp_cmp(x, y) != Ordering::Less;
            let identity = |p: &Project| (p.name.clone(), p.color.clone(), p.client_zone.clone());
            let billing = |p: &Project| (p.billable, p.billing_type, p.fixed_fee_cents, p.fee_currency.clone());
            let (ia, ib) = (a.identity_clock(), b.identity_clock());
            prop_assert_eq!(m.identity_clock(), if later(ia, ib) { ia } else { ib });
            prop_assert!(identity(&m) == identity(&a) || identity(&m) == identity(&b));
            if stamp_cmp(ia, ib) != Ordering::Equal {
                let winner = if later(ia, ib) { &a } else { &b };
                prop_assert_eq!(identity(&m), identity(winner));
            }
            let (ba, bb) = (a.billing_clock(), b.billing_clock());
            prop_assert_eq!(m.billing_clock(), if later(ba, bb) { ba } else { bb });
            prop_assert!(billing(&m) == billing(&a) || billing(&m) == billing(&b));
            if stamp_cmp(ba, bb) != Ordering::Equal {
                let winner = if later(ba, bb) { &a } else { &b };
                prop_assert_eq!(billing(&m), billing(winner));
            }
            let (ga, gb) = (a.budget_clock(), b.budget_clock());
            prop_assert_eq!(m.budget_clock(), if later(ga, gb) { ga } else { gb });
            prop_assert!(m.budget_hours == a.budget_hours || m.budget_hours == b.budget_hours);
            if stamp_cmp(ga, gb) != Ordering::Equal {
                let winner = if later(ga, gb) { &a } else { &b };
                prop_assert_eq!(m.budget_hours, winner.budget_hours);
            }
            let (ra, rb) = (a.archive_clock(), b.archive_clock());
            prop_assert_eq!(m.archive_clock(), if later(ra, rb) { ra } else { rb });
            if stamp_cmp(ra, rb) != Ordering::Equal {
                let winner = if later(ra, rb) { &a } else { &b };
                prop_assert_eq!(m.archived, winner.archived);
            }
            prop_assert_eq!(m.deleted, a.deleted || b.deleted);
            prop_assert_eq!(&m.stamp, if later(&a.stamp, &b.stamp) { &a.stamp } else { &b.stamp });
            prop_assert_eq!(m.created_at, a.created_at.min(b.created_at));
        }

        #[test]
        fn a_task_deleted_with_its_time_stays_gone(a in shared(), b in shared()) {
            let m = merge(&a, &b);
            for (id, t) in &m.tasks {
                let marked = [&a, &b]
                    .iter()
                    .any(|s| s.tasks.get(id).is_some_and(|x| x.discarded.is_some()));
                prop_assert_eq!(t.discarded.is_some(), marked);
                prop_assert!(!marked || !t.is_live());
            }
        }
    }

    #[test]
    fn closed_segment_beats_open_copy() {
        let open = Segment {
            id: "s".into(),
            task_id: "t".into(),
            device_id: "a".into(),
            start: Ts(5),
            end: None,
            ..Segment::default()
        };
        let closed = Segment {
            end: Some(Ts(9)),
            ..open.clone()
        };
        assert_eq!(merge_segment(&open, &closed).end, Some(Ts(9)));
        assert_eq!(merge_segment(&closed, &open).end, Some(Ts(9)));
    }

    #[test]
    fn stopped_outranks_a_later_state_and_metadata_merges_separately() {
        let base = Task {
            id: "t".into(),
            name: "old".into(),
            name_stamp: Stamp {
                at: Ts(1),
                by: "a".into(),
            },
            project_id: "p".into(),
            project_stamp: Stamp {
                at: Ts(1),
                by: "a".into(),
            },
            description: Value::Null,
            description_stamp: Stamp {
                at: Ts(1),
                by: "a".into(),
            },
            deleted: false,
            deleted_stamp: Stamp {
                at: Ts(1),
                by: "a".into(),
            },
            discarded: None,
            continued_from: None,
            created_at: Ts(0),
            state: TaskState::Stopped,
            pause_reason: None,
            state_stamp: Stamp {
                at: Ts(1),
                by: "a".into(),
            },
            stopped_at: Some(Ts(1)),
            tags: Vec::new(),
            tags_stamp: Default::default(),
            active_subtask: None,
            active_subtask_stamp: Default::default(),
        };
        let later = Task {
            name: "new".into(),
            name_stamp: Stamp {
                at: Ts(5),
                by: "b".into(),
            },
            state: TaskState::Running,
            state_stamp: Stamp {
                at: Ts(5),
                by: "b".into(),
            },
            stopped_at: None,
            ..base.clone()
        };
        let m = merge_task(&base, &later);
        assert_eq!(m.state, TaskState::Stopped);
        assert_eq!(m.name, "new");
        assert_eq!(m.stopped_at, Some(Ts(1)));
    }

    #[test]
    fn name_and_description_edits_on_two_devices_both_survive() {
        let stamp = |at, by: &str| Stamp {
            at: Ts(at),
            by: by.into(),
        };
        let base = Task {
            id: "t".into(),
            name: "old".into(),
            name_stamp: stamp(1, "a"),
            project_id: "p".into(),
            project_stamp: stamp(1, "a"),
            description: Value::Null,
            description_stamp: stamp(1, "a"),
            deleted: false,
            deleted_stamp: stamp(1, "a"),
            discarded: None,
            continued_from: None,
            created_at: Ts(0),
            state: TaskState::Paused,
            pause_reason: None,
            state_stamp: stamp(1, "a"),
            stopped_at: None,
            tags: Vec::new(),
            tags_stamp: Default::default(),
            active_subtask: None,
            active_subtask_stamp: Default::default(),
        };
        let mut on_a = base.clone();
        on_a.name = "renamed on A".into();
        on_a.name_stamp = stamp(5, "a");
        let mut on_b = base;
        on_b.description = json!({"type":"doc","content":[]});
        on_b.description_stamp = stamp(4, "b");
        for m in [merge_task(&on_a, &on_b), merge_task(&on_b, &on_a)] {
            assert_eq!(m.name, "renamed on A");
            assert_eq!(m.description, json!({"type":"doc","content":[]}));
        }
    }

    #[test]
    fn nothing_later_undoes_the_delete_of_a_task_with_its_time() {
        let stamp = |at, by: &str| Stamp {
            at: Ts(at),
            by: by.into(),
        };
        let gone = Task {
            id: "t".into(),
            name: "old".into(),
            name_stamp: stamp(1, "a"),
            project_id: "p".into(),
            project_stamp: stamp(1, "a"),
            description: Value::Null,
            description_stamp: stamp(1, "a"),
            deleted: true,
            deleted_stamp: stamp(2, "a"),
            discarded: Some(stamp(2, "a")),
            continued_from: None,
            created_at: Ts(0),
            state: TaskState::Paused,
            pause_reason: None,
            state_stamp: stamp(1, "a"),
            stopped_at: None,
            tags: Vec::new(),
            tags_stamp: Default::default(),
            active_subtask: None,
            active_subtask_stamp: Default::default(),
        };
        // Every clock of the other copy is later, and its build may not even know of the marker.
        let later = Task {
            name: "renamed".into(),
            name_stamp: stamp(9, "b"),
            description: json!({"type":"doc","content":[]}),
            description_stamp: stamp(9, "b"),
            deleted: false,
            deleted_stamp: stamp(9, "b"),
            discarded: None,
            state: TaskState::Running,
            state_stamp: stamp(9, "b"),
            ..gone.clone()
        };
        for m in [merge_task(&gone, &later), merge_task(&later, &gone)] {
            assert_eq!(m.discarded, gone.discarded);
            assert!(!m.is_live());
            // The edits themselves still merge as usual, so every device ends up with the same record.
            assert_eq!(m.name, "renamed");
        }
        // Deleted on two devices: the same marker on both, whichever way the merge goes.
        let also = Task {
            discarded: Some(stamp(5, "b")),
            ..gone.clone()
        };
        assert_eq!(
            merge_task(&gone, &also).discarded,
            merge_task(&also, &gone).discarded
        );
        assert_eq!(merge_task(&gone, &also).discarded, Some(stamp(5, "b")));
    }

    #[test]
    fn a_copy_written_by_a_build_without_the_marker_reads_back_without_it_and_loses() {
        let task = json!({
            "id": "t", "name": "T", "nameStamp": {"at": "2026-01-01T00:00:00Z", "by": "old"},
            "projectId": "p", "projectStamp": {"at": "2026-01-01T00:00:00Z", "by": "old"},
            "descriptionStamp": {"at": "2026-01-01T00:00:00Z", "by": "old"},
            "deleted": true, "deletedStamp": {"at": "2026-01-02T00:00:00Z", "by": "new"},
            "createdAt": "2026-01-01T00:00:00Z", "state": "paused",
            "stateStamp": {"at": "2026-01-03T00:00:00Z", "by": "old"},
        });
        let old: Task = serde_json::from_value(task).unwrap();
        assert_eq!(old.discarded, None, "a missing field defaults");
        // What that build itself makes of it: a paused task with `deleted` set is still shown.
        assert!(old.is_live());
        let new = Task {
            discarded: Some(old.deleted_stamp.clone()),
            ..old.clone()
        };
        assert!(!merge_task(&old, &new).is_live());
        assert!(!merge_task(&new, &old).is_live());
    }

    #[test]
    fn a_later_edit_cannot_undo_a_tombstone() {
        let a = Project {
            id: "p".into(),
            name: "n".into(),
            color: "#000000".into(),
            archived: false,
            deleted: true,
            billable: false,
            budget_hours: None,
            client_zone: None,
            created_at: Ts(0),
            stamp: Stamp {
                at: Ts(1),
                by: "a".into(),
            },
            billing_type: Default::default(),
            fixed_fee_cents: None,
            fee_currency: None,
            identity_stamp: None,
            billing_stamp: None,
            budget_stamp: None,
            archive_stamp: None,
        };
        let renamed = Project {
            deleted: false,
            name: "later".into(),
            stamp: Stamp {
                at: Ts(9),
                by: "b".into(),
            },
            ..a.clone()
        };
        assert!(merge_project(&a, &renamed).deleted);
        assert!(merge_project(&renamed, &a).deleted);
    }

    #[test]
    fn tombstone_does_not_resurrect() {
        let a = Project {
            id: "p".into(),
            name: "n".into(),
            color: "#000000".into(),
            archived: false,
            deleted: false,
            billable: false,
            budget_hours: None,
            client_zone: None,
            created_at: Ts(0),
            stamp: Stamp {
                at: Ts(1),
                by: "a".into(),
            },
            billing_type: Default::default(),
            fixed_fee_cents: None,
            fee_currency: None,
            identity_stamp: None,
            billing_stamp: None,
            budget_stamp: None,
            archive_stamp: None,
        };
        let gone = Project {
            deleted: true,
            stamp: Stamp {
                at: Ts(2),
                by: "b".into(),
            },
            ..a.clone()
        };
        assert!(merge_project(&a, &gone).deleted);
        assert!(merge_project(&gone, &a).deleted);
    }

    fn clock(at: i64, by: &str) -> Stamp {
        Stamp {
            at: Ts(at),
            by: by.into(),
        }
    }
    fn plain_project() -> Project {
        Project {
            id: "p".into(),
            name: "P".into(),
            color: "#112233".into(),
            archived: false,
            deleted: false,
            billable: true,
            budget_hours: None,
            client_zone: None,
            billing_type: BillingType::Hourly,
            fixed_fee_cents: None,
            fee_currency: None,
            created_at: Ts(0),
            stamp: clock(1, "a"),
            identity_stamp: None,
            billing_stamp: None,
            budget_stamp: None,
            archive_stamp: None,
        }
    }
    /// The fee and budget from the finding, set at `at` on device a.
    fn with_fee_and_budget(p: &Project, at: i64) -> Project {
        let mut p = p.clone();
        p.pin_clocks();
        p.billing_type = BillingType::FixedFee;
        p.fixed_fee_cents = Some(500_000);
        p.fee_currency = Some("EUR".into());
        p.billing_stamp = Some(clock(at, "a"));
        p.budget_hours = Some(40.0);
        p.budget_stamp = Some(clock(at, "a"));
        p.stamp = clock(at, "a");
        p
    }
    fn renamed(p: &Project, at: i64) -> Project {
        let mut p = p.clone();
        p.pin_clocks();
        p.name = "renamed".into();
        p.identity_stamp = Some(clock(at, "b"));
        p.stamp = clock(at, "b");
        p
    }

    #[test]
    fn r10_a_rename_on_one_device_and_a_fee_and_budget_on_another_both_survive() {
        let base = plain_project();
        let (fee, rename) = (with_fee_and_budget(&base, 5), renamed(&base, 6));
        for m in [merge_project(&fee, &rename), merge_project(&rename, &fee)] {
            assert_eq!(m.name, "renamed");
            assert_eq!(m.billing_type, BillingType::FixedFee);
            assert_eq!(m.fixed_fee_cents, Some(500_000));
            assert_eq!(m.fee_currency.as_deref(), Some("EUR"));
            assert_eq!(m.budget_hours, Some(40.0));
            assert_eq!(m.stamp, clock(6, "b"), "a build that compares only this");
            assert_eq!(m.identity_stamp, Some(clock(6, "b")));
            assert_eq!(m.billing_stamp, Some(clock(5, "a")));
            assert_eq!(m.budget_stamp, Some(clock(5, "a")));
            assert_eq!(m.archive_stamp, Some(clock(1, "a")));
        }
    }

    #[test]
    fn r10_archiving_and_editing_on_two_devices_both_survive_and_a_tombstone_still_outranks_them() {
        let base = plain_project();
        let mut archived = base.clone();
        archived.pin_clocks();
        archived.archived = true;
        archived.archive_stamp = Some(clock(5, "a"));
        archived.stamp = clock(5, "a");
        let rename = renamed(&base, 6);
        let m = merge_project(&archived, &rename);
        assert!(m.archived, "a later edit of the name doesn't bring it back");
        assert_eq!(m.name, "renamed");
        assert_eq!(m, merge_project(&rename, &archived));
        let mut gone = base.clone();
        gone.pin_clocks();
        gone.deleted = true;
        gone.stamp = clock(2, "a");
        assert!(merge_project(&gone, &rename).deleted);
        assert_eq!(merge_project(&gone, &rename).name, "renamed");
    }

    /// A copy as a build without group clocks writes one, read the way this build reads it.
    fn from_an_older_build(p: &Project) -> Project {
        #[derive(Deserialize, Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Older {
            id: String,
            name: String,
            color: String,
            archived: bool,
            deleted: bool,
            billable: bool,
            budget_hours: Option<f64>,
            client_zone: Option<String>,
            billing_type: BillingType,
            fixed_fee_cents: Option<i64>,
            fee_currency: Option<String>,
            created_at: Ts,
            stamp: Stamp,
        }
        let read: Older = serde_json::from_value(serde_json::to_value(p).unwrap()).unwrap();
        let written = serde_json::to_string(&read).unwrap();
        assert!(!written.contains("identityStamp"));
        serde_json::from_str(&written).unwrap()
    }

    #[test]
    fn r10_a_copy_from_an_older_build_counts_at_its_whole_stamp_and_never_stops_a_merge() {
        // A file from before the clocks reads with none, and needs no other field.
        let old: Project = serde_json::from_value(json!({
            "id": "p", "name": "P", "color": "#112233", "createdAt": "2026-01-01T00:00:00Z",
            "stamp": {"at": "2026-01-02T00:00:00Z", "by": "old"},
        }))
        .unwrap();
        assert_eq!(old.identity_stamp, None);
        assert_eq!(old.billing_clock(), &old.stamp);
        let written = serde_json::to_string(&old).unwrap();
        assert!(!written.contains("Stamp"), "it stays as it was: {written}");
        // Its edit of any group is as new as its whole stamp: earlier than this clock, it gives way here...
        let base = plain_project();
        let fee = with_fee_and_budget(&base, 5);
        let mut older_edit = from_an_older_build(&base);
        older_edit.name = "older name".into();
        older_edit.stamp = clock(3, "o");
        for m in [
            merge_project(&fee, &older_edit),
            merge_project(&older_edit, &fee),
        ] {
            assert_eq!(m.fixed_fee_cents, Some(500_000));
            assert_eq!(m.budget_hours, Some(40.0));
            // ...and later than the name's clock, it takes the name, as a whole-record merge never could.
            assert_eq!(m.name, "older name");
            assert_eq!(m.stamp, clock(5, "a"));
        }
    }

    /// The defined behaviour for what a build without clocks can do to a field. It rewrites every field at
    /// its whole stamp, so nothing tells a stale field from an edit: its later edit carries its stale fee along.
    /// That is what such a build does by itself, and every copy still ends up the same.
    #[test]
    fn r10_a_later_edit_on_an_older_build_can_still_carry_an_older_fee_and_every_copy_converges() {
        let base = plain_project();
        let fee = with_fee_and_budget(&base, 5);
        let mut older = from_an_older_build(&base);
        older.name = "renamed on the older build".into();
        older.stamp = clock(6, "o");
        let m = merge_project(&fee, &older);
        assert_eq!(m, merge_project(&older, &fee));
        assert_eq!(m.name, "renamed on the older build");
        assert_eq!(m.fixed_fee_cents, None, "as that build itself now holds");
        assert_eq!(m.budget_hours, None);
        assert_eq!(m.stamp, clock(6, "o"));
        // The older build keeps its copy, and reading it again changes nothing here.
        assert_eq!(merge_project(&m, &older), m);
    }

    /// The same, where a whole-record merge would have gone the other way: a later edit here of something else
    /// would have kept the fee and lost the rename. One of the two goes either way. This keeps the rename, and
    /// the edit here, and the older build's copy then follows along.
    #[test]
    fn r10_a_stale_field_from_an_older_build_beats_an_older_group_clock_even_after_a_later_edit() {
        let base = plain_project();
        let mut later = with_fee_and_budget(&base, 5);
        later.archived = true;
        later.archive_stamp = Some(clock(9, "a"));
        later.stamp = clock(9, "a");
        let mut older = from_an_older_build(&base);
        older.name = "renamed on the older build".into();
        older.stamp = clock(6, "o");
        let m = merge_project(&later, &older);
        assert_eq!(m, merge_project(&older, &later));
        assert_eq!(m.name, "renamed on the older build");
        assert_eq!(m.fixed_fee_cents, None);
        assert_eq!(m.budget_hours, None);
        assert!(m.archived, "its clock is later than that copy");
        assert_eq!(m.stamp, clock(9, "a"));
        // That build takes this whole, as its stamp is later, and passing it on changes no value.
        let followed = from_an_older_build(&m);
        let settled = merge_project(&m, &followed);
        assert_eq!(settled, merge_project(&followed, &m));
        assert_eq!(
            (
                settled.name.as_str(),
                settled.fixed_fee_cents,
                settled.archived
            ),
            (m.name.as_str(), m.fixed_fee_cents, m.archived)
        );
        assert_eq!(merge_project(&settled, &followed), settled);
    }

    #[test]
    fn r10_an_older_build_passing_a_copy_on_changes_no_value_and_settles() {
        let base = plain_project();
        let m = merge_project(&with_fee_and_budget(&base, 5), &renamed(&base, 6));
        let passed_on = from_an_older_build(&m);
        let again = merge_project(&m, &passed_on);
        assert_eq!(again, merge_project(&passed_on, &m));
        for (got, want) in [
            (again.name.as_str(), "renamed"),
            (again.fee_currency.as_deref().unwrap_or(""), "EUR"),
        ] {
            assert_eq!(got, want);
        }
        assert_eq!(again.fixed_fee_cents, Some(500_000));
        assert_eq!(again.budget_hours, Some(40.0));
        // All its clocks rise to its stamp, as that build vouches for all of it. That is all, and it happens once.
        assert_eq!(again.billing_stamp, Some(clock(6, "b")));
        assert_eq!(merge_project(&again, &passed_on), again);
    }

    /// The one case that loses an edit: the older build read a copy from before the merge, at the same stamp as
    /// the merge, and passes it on. The name set earlier gives way. A whole-record merge lost it the same way.
    #[test]
    fn r10_an_older_build_holding_a_copy_from_before_a_merge_can_undo_an_earlier_edit() {
        let base = plain_project();
        let rename = renamed(&base, 5);
        let fee = with_fee_and_budget(&base, 6);
        let merged = merge_project(&rename, &fee);
        assert_eq!(merged.name, "renamed");
        let kept = from_an_older_build(&fee);
        let m = merge_project(&merged, &kept);
        assert_eq!(m, merge_project(&kept, &merged));
        assert_eq!(
            m.name, "P",
            "the older copy vouches for its name at its stamp"
        );
        assert_eq!(m.fixed_fee_cents, Some(500_000), "the later edit stays");
        assert_eq!(merge_project(&m, &kept), m, "and it settles");
    }

    #[test]
    fn a_phase_1_list_and_its_upgraded_copy_converge_after_normalizing() {
        let doc = json!({"type":"doc","content":[{"type":"taskList","content":[
            {"type":"taskItem","attrs":{"checked":false},"content":[{"type":"paragraph","content":[{"type":"text","text":"One"}]}]}]}]});
        let stamp = Stamp {
            at: Ts(1),
            by: "a".to_owned(),
        };
        let task = |description: Value| Task {
            id: "t".into(),
            name: "T".into(),
            name_stamp: stamp.clone(),
            project_id: "p".into(),
            project_stamp: stamp.clone(),
            description,
            description_stamp: stamp.clone(),
            deleted: false,
            deleted_stamp: stamp.clone(),
            discarded: None,
            continued_from: None,
            created_at: Ts(1),
            state: TaskState::Draft,
            pause_reason: None,
            state_stamp: stamp.clone(),
            stopped_at: None,
            tags: Vec::new(),
            tags_stamp: stamp.clone(),
            active_subtask: None,
            active_subtask_stamp: stamp.clone(),
        };
        let mut old = Shared::default();
        old.tasks.insert("t".into(), task(doc));
        let mut upgraded = old.clone();
        upgraded.normalize();
        let mut one = merge(&old, &upgraded);
        one.normalize();
        let mut two = merge(&upgraded, &old);
        two.normalize();
        assert_eq!(one, two);
        assert_eq!(one, upgraded);
    }
}
