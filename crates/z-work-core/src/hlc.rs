//! Every stamp a commit writes is later than every stamp this device has seen, so the most recent save wins even from a
//! computer whose clock is behind: a hybrid logical clock. Unit: one second, the resolution of `Ts` and of the cloud
//! files, whose bytes stay as 1.3.1 wrote them.
//!
//! The newest stamp seen, its own or another device's (the floor), is kept by `Service` and saved with the device row
//! as `LocalFile::newest_stamp`. A commit stamps at `commit_stamp_at`: the clock, or one second past the floor when that
//! is later, never more than `HLC_MAX_LEAD_SECS` past the clock. All stamps of one commit share that moment
//! (`Ctx::stamp_at`), and `Ctx::next` still goes one second past the version an edit replaces. Only `Stamp.at`, which
//! orders edits, follows the floor: logged times, creation and audit times, heartbeats and file times stay on the wall
//! clock (`Ctx::now`).

use std::cmp::Ordering;
use std::collections::BTreeMap;

use crate::model::{
    AuditEntry, BillablePause, BillingSummary, DevicePrefix, IdleGap, PauseRequest, Project,
    RateVersion, RoundingVersion, Segment, SettingEntry, Shared, ShiftVersion, Stamp, Tag, Task,
    Template,
};
use crate::time::Ts;

/// How far past this computer's clock a commit's stamp may go to beat what it has seen. Readers refuse records more than
/// two days ahead of their own clock (`validate`), so a stamp further ahead would never reach them. One day leaves a day
/// for a reader whose clock is behind this one.
pub const HLC_MAX_LEAD_SECS: i64 = 86_400;

/// A clock this far behind the newest change seen is wrong, not drifting: the figure the lease already uses for a holder
/// whose clock seems to differ (`lease::CLOCK_AHEAD_SECS`).
pub const CLOCK_BEHIND_WARN_SECS: i64 = crate::lease::CLOCK_AHEAD_SECS;

/// The `at` of every stamp one commit writes: `now`, or one second past `newest` when that is later, but never more than
/// `HLC_MAX_LEAD_SECS` past `now`.
#[must_use]
pub fn commit_stamp_at(now: Ts, newest: Option<&Stamp>) -> Ts {
    newest.map_or(now, |n| {
        now.max(n.at.later(1).min(now.later(HLC_MAX_LEAD_SECS)))
    })
}

/// Merge's order of stamps: `at`, then `by`.
fn order(a: &Stamp, b: &Stamp) -> Ordering {
    a.at.cmp(&b.at).then_with(|| a.by.cmp(&b.by))
}

/// The later of two stamps, in merge's order: `at`, then `by`.
#[must_use]
pub fn later(a: Option<Stamp>, b: Option<Stamp>) -> Option<Stamp> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if order(&x, &y) == Ordering::Less {
            y
        } else {
            x
        }),
        (x, None) => x,
        (None, y) => y,
    }
}

/// As `later`, without copying either.
#[must_use]
pub fn later_ref<'a>(a: Option<&'a Stamp>, b: Option<&'a Stamp>) -> Option<&'a Stamp> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if order(x, y) == Ordering::Less { y } else { x }),
        (x, None) => x,
        (None, y) => y,
    }
}

/// A replicated record and the stamps it carries.
///
/// Each implementation takes its record apart field by field, with no `..`, so a field added to the model doesn't
/// compile until it is either visited here or left out on purpose.
pub(crate) trait Stamped {
    /// Calls `f` with every stamp of the record, the optional ones included.
    fn each_stamp<'a>(&'a self, f: &mut impl FnMut(&'a Stamp));
}

/// Raises `newest` to the newest stamp of `record`.
pub(crate) fn raise<'a>(newest: &mut Option<&'a Stamp>, record: &'a impl Stamped) {
    record.each_stamp(&mut |s| *newest = later_ref(*newest, Some(s)));
}

impl Stamped for Project {
    fn each_stamp<'a>(&'a self, f: &mut impl FnMut(&'a Stamp)) {
        let Self {
            id: _,
            name: _,
            color: _,
            archived: _,
            deleted: _,
            billable: _,
            budget_hours: _,
            client_zone: _,
            billing_type: _,
            fixed_fee_cents: _,
            fee_currency: _,
            created_at: _,
            stamp,
            identity_stamp,
            billing_stamp,
            budget_stamp,
            archive_stamp,
        } = self;
        f(stamp);
        for group in [identity_stamp, billing_stamp, budget_stamp, archive_stamp]
            .into_iter()
            .flatten()
        {
            f(group);
        }
    }
}

impl Stamped for Task {
    fn each_stamp<'a>(&'a self, f: &mut impl FnMut(&'a Stamp)) {
        let Self {
            id: _,
            name: _,
            name_stamp,
            project_id: _,
            project_stamp,
            description: _,
            description_stamp,
            deleted: _,
            deleted_stamp,
            discarded,
            continued_from: _,
            created_at: _,
            state: _,
            pause_reason: _,
            state_stamp,
            stopped_at: _,
            tags: _,
            tags_stamp,
            active_subtask: _,
            active_subtask_stamp,
        } = self;
        for s in [
            name_stamp,
            project_stamp,
            description_stamp,
            deleted_stamp,
            state_stamp,
            tags_stamp,
            active_subtask_stamp,
        ] {
            f(s);
        }
        if let Some(s) = discarded {
            f(s);
        }
    }
}

impl Stamped for Segment {
    fn each_stamp<'a>(&'a self, f: &mut impl FnMut(&'a Stamp)) {
        // `start`, `end` and the edit's times are logged time, on the wall clock.
        let Self {
            id: _,
            task_id: _,
            device_id: _,
            start: _,
            end: _,
            manual: _,
            subtask_id: _,
            edit: _,
            edit_stamp,
        } = self;
        f(edit_stamp);
    }
}

impl Stamped for IdleGap {
    fn each_stamp<'a>(&'a self, f: &mut impl FnMut(&'a Stamp)) {
        let Self {
            id: _,
            segment_id: _,
            task_id: _,
            device_id: _,
            start: _,
            end: _,
            kind: _,
            decision: _,
            decision_stamp,
        } = self;
        f(decision_stamp);
    }
}

impl Stamped for BillablePause {
    fn each_stamp<'a>(&'a self, f: &mut impl FnMut(&'a Stamp)) {
        let Self {
            id: _,
            task_id: _,
            start: _,
            end: _,
            note: _,
            deleted: _,
            stamp,
        } = self;
        f(stamp);
    }
}

impl Stamped for PauseRequest {
    fn each_stamp<'a>(&'a self, f: &mut impl FnMut(&'a Stamp)) {
        // `at` is when the time stopped, on the asking device's wall clock.
        let Self {
            id: _,
            task_id: _,
            segment_id: _,
            requested_by: _,
            at: _,
            stamp,
        } = self;
        f(stamp);
    }
}

impl Stamped for Tag {
    fn each_stamp<'a>(&'a self, f: &mut impl FnMut(&'a Stamp)) {
        let Self {
            id: _,
            name: _,
            color: _,
            deleted: _,
            stamp,
        } = self;
        f(stamp);
    }
}

impl Stamped for Template {
    fn each_stamp<'a>(&'a self, f: &mut impl FnMut(&'a Stamp)) {
        let Self {
            id: _,
            name: _,
            project_id: _,
            description: _,
            tags: _,
            deleted: _,
            stamp,
        } = self;
        f(stamp);
    }
}

impl Stamped for AuditEntry {
    /// None: `at` and `by` say when and where a change was made, on the wall clock, and order nothing.
    fn each_stamp<'a>(&'a self, _: &mut impl FnMut(&'a Stamp)) {
        let Self {
            id: _,
            at: _,
            by: _,
            entity: _,
            entity_id: _,
            action: _,
            reason: _,
            before: _,
            after: _,
        } = self;
    }
}

impl Stamped for BillingSummary {
    fn each_stamp<'a>(&'a self, f: &mut impl FnMut(&'a Stamp)) {
        let Self {
            id: _,
            reference: _,
            prefix: _,
            number: _,
            device_id: _,
            project_id: _,
            from: _,
            to: _,
            zone: _,
            created_at: _,
            segment_ids: _,
            pause_ids: _,
            rows: _,
            totals: _,
            voided: _,
            stamp,
        } = self;
        f(stamp);
    }
}

impl Stamped for DevicePrefix {
    fn each_stamp<'a>(&'a self, f: &mut impl FnMut(&'a Stamp)) {
        let Self { prefix: _, stamp } = self;
        f(stamp);
    }
}

impl Stamped for ShiftVersion {
    fn each_stamp<'a>(&'a self, f: &mut impl FnMut(&'a Stamp)) {
        let Self {
            id: _,
            shift_id: _,
            effective_from: _,
            name: _,
            color: _,
            start: _,
            end: _,
            weekdays: _,
            zone: _,
            break_minutes: _,
            retired: _,
            deleted: _,
            stamp,
        } = self;
        f(stamp);
    }
}

impl Stamped for RateVersion {
    fn each_stamp<'a>(&'a self, f: &mut impl FnMut(&'a Stamp)) {
        let Self {
            id: _,
            project_id: _,
            effective_from: _,
            rate_cents: _,
            currency: _,
            deleted: _,
            stamp,
        } = self;
        f(stamp);
    }
}

impl Stamped for RoundingVersion {
    fn each_stamp<'a>(&'a self, f: &mut impl FnMut(&'a Stamp)) {
        let Self {
            id: _,
            project_id: _,
            effective_from: _,
            increment_minutes: _,
            method: _,
            deleted: _,
            stamp,
        } = self;
        f(stamp);
    }
}

impl Stamped for SettingEntry {
    fn each_stamp<'a>(&'a self, f: &mut impl FnMut(&'a Stamp)) {
        let Self { value: _, stamp } = self;
        f(stamp);
    }
}

fn each_in<'a, T: Stamped>(map: &'a BTreeMap<String, T>, f: &mut impl FnMut(&'a Stamp)) {
    for record in map.values() {
        record.each_stamp(f);
    }
}

impl Shared {
    /// Calls `f` with every `Stamp` in the replicated data, the optional ones included.
    pub fn for_each_stamp<'a>(&'a self, mut f: impl FnMut(&'a Stamp)) {
        let Self {
            projects,
            tasks,
            segments,
            settings,
            shifts,
            rates,
            rounding,
            gaps,
            pauses,
            tags,
            templates,
            summaries,
            prefixes,
            audit_log,
            pause_requests,
        } = self;
        each_in(projects, &mut f);
        each_in(tasks, &mut f);
        each_in(segments, &mut f);
        each_in(settings, &mut f);
        each_in(shifts, &mut f);
        each_in(rates, &mut f);
        each_in(rounding, &mut f);
        each_in(gaps, &mut f);
        each_in(pauses, &mut f);
        each_in(tags, &mut f);
        each_in(templates, &mut f);
        each_in(summaries, &mut f);
        each_in(prefixes, &mut f);
        each_in(audit_log, &mut f);
        each_in(pause_requests, &mut f);
    }

    /// The newest stamp, and the newest one written by a device other than `device`, in one pass.
    #[must_use]
    pub fn newest_stamps(&self, device: &str) -> (Option<&Stamp>, Option<&Stamp>) {
        let (mut all, mut other) = (None, None);
        self.for_each_stamp(|s| {
            all = later_ref(all, Some(s));
            if s.by != device {
                other = later_ref(other, Some(s));
            }
        });
        (all, other)
    }

    /// The newest stamp dated no later than `until`.
    #[must_use]
    pub fn newest_stamp_until(&self, until: Ts) -> Option<&Stamp> {
        let mut newest = None;
        self.for_each_stamp(|s| {
            if s.at <= until {
                newest = later_ref(newest, Some(s));
            }
        });
        newest
    }

    #[must_use]
    pub fn newest_stamp(&self) -> Option<Stamp> {
        let mut newest = None;
        self.for_each_stamp(|s| newest = later_ref(newest, Some(s)));
        newest.cloned()
    }

    #[must_use]
    pub fn newest_stamp_not_by(&self, device: &str) -> Option<Stamp> {
        self.newest_stamps(device).1.cloned()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use proptest::prelude::*;
    use serde_json::Value;

    use super::*;
    use crate::machine::Ctx;
    use crate::merge::merge;
    use crate::merge::strategies::{shared, stamp};

    const CAP: i64 = HLC_MAX_LEAD_SECS;

    fn at(secs: i64, by: &str) -> Stamp {
        Stamp {
            at: Ts(secs),
            by: by.to_owned(),
        }
    }

    #[test]
    fn a_commit_stamps_at_the_clock_unless_it_has_seen_later_and_never_past_a_day_ahead() {
        let now = Ts(1_800_000_000);
        assert_eq!(commit_stamp_at(now, None), now);
        assert_eq!(commit_stamp_at(now, Some(&at(now.0 - 5, "x"))), now);
        assert_eq!(commit_stamp_at(now, Some(&at(now.0, "x"))), now.later(1));
        assert_eq!(
            commit_stamp_at(now, Some(&at(now.0 + 3600, "x"))),
            now.later(3601)
        );
        assert_eq!(
            commit_stamp_at(now, Some(&at(now.0 + CAP - 1, "x"))),
            now.later(CAP)
        );
        assert_eq!(
            commit_stamp_at(now, Some(&at(now.0 + 300 * 86_400, "x"))),
            now.later(CAP)
        );
    }

    #[test]
    fn a_context_off_the_floor_keeps_the_wall_clock_for_times_and_stamps_past_the_floor() {
        let now = Ts(1_800_000_000);
        let floor = at(now.0 + 600, "other");
        let c = Ctx::hlc(now, "me", Some(&floor));
        assert_eq!(c.now, now);
        assert_eq!(c.stamp_at, now.later(601));
        assert_eq!(c.stamp(), at(now.0 + 601, "me"));
        // An edit still beats the version it replaces, even one past the cap.
        assert_eq!(c.next(&at(now.0 + 5, "x")), at(now.0 + 601, "me"));
        let capped = Ctx::hlc(now, "me", Some(&at(now.0 + 2 * CAP, "other")));
        assert_eq!(capped.stamp_at, now.later(CAP));
        assert_eq!(
            capped.next(&at(now.0 + 2 * CAP, "other")),
            at(now.0 + 2 * CAP + 1, "me")
        );
        let plain = Ctx::at(now, "me");
        assert_eq!((plain.now, plain.stamp_at), (now, now));
    }

    #[test]
    fn the_starter_tags_never_move_the_clock() {
        let mut data = Shared::default();
        data.seed_tags();
        let newest = data.newest_stamp().unwrap();
        assert_eq!(newest.at, Ts::EARLIEST);
        assert_eq!(
            commit_stamp_at(Ts(1_800_000_000), Some(&newest)),
            Ts(1_800_000_000)
        );
    }

    /// Every `{"at", "by"}` object in `v`, which is how a `Stamp` serialises and nothing else in `Shared` does.
    fn stamps_in_json(v: &Value, out: &mut Vec<Stamp>) {
        match v {
            Value::Object(m) => {
                if let (2, Some(Value::String(a)), Some(Value::String(b))) =
                    (m.len(), m.get("at"), m.get("by"))
                {
                    out.push(Stamp {
                        at: Ts::parse(a).unwrap(),
                        by: b.clone(),
                    });
                    return;
                }
                m.values().for_each(|x| stamps_in_json(x, out));
            }
            Value::Array(a) => a.iter().for_each(|x| stamps_in_json(x, out)),
            _ => {}
        }
    }

    fn sorted(mut v: Vec<Stamp>) -> Vec<Stamp> {
        v.sort_by(order);
        v
    }

    /// One random edit per record: kept as it was, replaced by `other`'s record of the same kind and id (new, or
    /// changed with its stamps), or removed.
    fn mix<T: Clone>(
        a: &BTreeMap<String, T>,
        b: &BTreeMap<String, T>,
        picks: &mut impl Iterator<Item = u8>,
    ) -> BTreeMap<String, T> {
        let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
        let mut out = BTreeMap::new();
        for k in keys {
            let kept = match (picks.next().unwrap_or(0), a.get(k), b.get(k)) {
                (0, Some(x), _) | (1, Some(x), None) => Some(x),
                (1, _, Some(y)) => Some(y),
                (2, _, _) => None,
                (_, x, y) => x.or(y),
            };
            if let Some(r) = kept {
                out.insert(k.clone(), r.clone());
            }
        }
        out
    }

    fn edited(prev: &Shared, other: &Shared, picks: &[u8]) -> Shared {
        let p = &mut picks.iter().copied().cycle();
        Shared {
            projects: mix(&prev.projects, &other.projects, p),
            tasks: mix(&prev.tasks, &other.tasks, p),
            segments: mix(&prev.segments, &other.segments, p),
            settings: mix(&prev.settings, &other.settings, p),
            shifts: mix(&prev.shifts, &other.shifts, p),
            rates: mix(&prev.rates, &other.rates, p),
            rounding: mix(&prev.rounding, &other.rounding, p),
            gaps: mix(&prev.gaps, &other.gaps, p),
            pauses: mix(&prev.pauses, &other.pauses, p),
            tags: mix(&prev.tags, &other.tags, p),
            templates: mix(&prev.templates, &other.templates, p),
            summaries: mix(&prev.summaries, &other.summaries, p),
            prefixes: mix(&prev.prefixes, &other.prefixes, p),
            audit_log: mix(&prev.audit_log, &other.audit_log, p),
            pause_requests: mix(&prev.pause_requests, &other.pause_requests, p),
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(600))]

        /// 1.
        #[test]
        fn the_commit_stamp_is_never_before_now_nor_past_the_cap(
            now in Ts::MIN.0..Ts::MAX.0,
            f in prop::option::of((Ts::MIN.0..Ts::MAX.0, "[a-c]")),
        ) {
            let floor = f.map(|(t, by)| at(t, &by));
            let s = commit_stamp_at(Ts(now), floor.as_ref());
            prop_assert!(s.0 >= now);
            prop_assert!(s.0 <= now + CAP);
        }

        /// 2.
        #[test]
        fn the_commit_stamp_beats_the_floor_while_under_the_cap(
            now in Ts::MIN.0..Ts::MAX.0,
            lead in -3 * CAP..3 * CAP,
            by in "[a-c]",
        ) {
            let floor = at((now + lead).clamp(Ts::MIN.0, Ts::MAX.0 - 1), &by);
            let s = commit_stamp_at(Ts(now), Some(&floor));
            // That is, `floor.at + 1 <= now + CAP`.
            if floor.at.0 < now + CAP {
                prop_assert!(s > floor.at);
            } else {
                prop_assert_eq!(s.0, now + CAP);
            }
        }

        /// 3. Every stamp in the JSON of the data is visited, once, and the newest is what `newest_stamp` says.
        #[test]
        fn for_each_stamp_visits_every_stamp(data in shared(), device in "[a-c]") {
            let mut found = Vec::new();
            stamps_in_json(&serde_json::to_value(&data).unwrap(), &mut found);
            let mut visited = Vec::new();
            data.for_each_stamp(|s| visited.push(s.clone()));
            let found = sorted(found);
            prop_assert_eq!(sorted(visited), found.clone());
            prop_assert_eq!(data.newest_stamp(), found.last().cloned());
            let (all, other) = data.newest_stamps(&device);
            prop_assert_eq!(all.cloned(), found.last().cloned());
            prop_assert_eq!(other.cloned(), found.iter().rfind(|s| s.by != device).cloned());
            prop_assert_eq!(data.newest_stamp_not_by(&device), other.cloned());
        }

        /// 4. `later` is symmetric and keeps the stamp a merge keeps.
        #[test]
        fn later_is_merges_order(a in prop::option::of(stamp()), b in prop::option::of(stamp())) {
            prop_assert_eq!(later(a.clone(), b.clone()), later(b.clone(), a.clone()));
            prop_assert_eq!(
                later_ref(a.as_ref(), b.as_ref()).cloned(),
                later(a.clone(), b.clone())
            );
            let both = [a.clone(), b.clone()];
            let max = both.iter().flatten().max_by(|x, y| order(x, y)).cloned();
            prop_assert_eq!(later(a.clone(), b.clone()), max);
            // Two copies of one setting with the same value merge to the later clock.
            if let (Some(x), Some(y)) = (a, b) {
                let one = |s: Stamp| Shared {
                    settings: BTreeMap::from([(
                        "k".to_owned(),
                        SettingEntry { value: Value::from(1), stamp: s },
                    )]),
                    ..Shared::default()
                };
                let merged = merge(&one(x.clone()), &one(y.clone()));
                prop_assert_eq!(Some(merged.settings["k"].stamp.clone()), later(Some(x), Some(y)));
            }
        }

        /// Addendum P11: the floor after a save, taken from the records that save writes, equals the walk of every
        /// stamp, as long as the floor before covered what the store held.
        #[test]
        fn the_floor_after_a_save_equals_the_full_walk_on_random_edits(
            prev in shared(),
            other in shared(),
            picks in prop::collection::vec(0u8..4, 1..64),
            restored in prop::option::of(stamp()),
        ) {
            let next = edited(&prev, &other, &picks);
            // A restore can leave the floor above anything the store holds.
            let floor = later(prev.newest_stamp(), restored);
            let planned = crate::store::plan_rows(&prev, &next, false).unwrap();
            prop_assert_eq!(
                later(floor.clone(), planned.newest().cloned()),
                later(floor, next.newest_stamp())
            );
            // A full write, after a save that failed, writes every record: its newest is the whole walk.
            let full = crate::store::plan_rows(&prev, &next, true).unwrap();
            prop_assert_eq!(full.newest().cloned(), next.newest_stamp());
            // Nothing written, nothing newer.
            prop_assert_eq!(crate::store::plan_rows(&next, &next, false).unwrap().newest(), None);
        }
    }
}
