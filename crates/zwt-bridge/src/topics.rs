//! Topics: which slices of the data changed, as a small bitset, and the diff of two `LocalFile`s that yields it.
//!
//! PLAN.md section 5: after each commit the worker compares the sections of the file it cares about and sets one
//! bit per topic whose data moved. View topics (`TASKS`, `RUNNING`, `DAY`, `CHIPS`, `SHARE`) are recomputed and
//! shown; the others (`PROJECTS`, `TAGS`, `SETTINGS`, `PREFS`, `LEASE`) name the section that moved, for screens of
//! later milestones. The diff errs towards setting a bit: a bit set for nothing costs one recompute, whose output
//! the hub then finds unchanged; a bit missed leaves a stale screen.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::{BitAnd, BitOr, BitOrAssign, Sub};

use z_work_core::model::{LocalFile, LocalPrefs, Project, Segment, Task};

/// A set of topics. `Topics::TASKS | Topics::DAY` and so on.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Topics(u16);

impl Topics {
    pub const NONE: Self = Self(0);
    /// A project's name, colour, archive state or any other field.
    pub const PROJECTS: Self = Self(1);
    /// The task list: its rows ([`crate::views::TaskRow`]) and the Stopped group's paging
    /// ([`crate::views::StoppedPage`]).
    pub const TASKS: Self = Self(1 << 1);
    /// Which task runs on this device and since when ([`crate::views::Running`]).
    pub const RUNNING: Self = Self(1 << 2);
    /// The pieces of the selected day ([`crate::views::DayView`]).
    pub const DAY: Self = Self(1 << 3);
    /// The day picker: its chips ([`zwt_ui_logic::day::DayChip`]) and its page ([`crate::views::DayPage`]).
    pub const CHIPS: Self = Self(1 << 4);
    /// The Share of time card ([`crate::views::ShareData`]).
    pub const SHARE: Self = Self(1 << 5);
    /// A tag's name, colour or deletion.
    pub const TAGS: Self = Self(1 << 6);
    /// Replicated settings (zones, work hours and the like).
    pub const SETTINGS: Self = Self(1 << 7);
    /// This device's preferences, apart from the lease memo.
    pub const PREFS: Self = Self(1 << 8);
    /// The lease memo: which computer holds the data.
    pub const LEASE: Self = Self(1 << 9);

    /// The topics an [`crate::worker::Update`] carries data for.
    pub const VIEWS: Self =
        Self(Self::TASKS.0 | Self::RUNNING.0 | Self::DAY.0 | Self::CHIPS.0 | Self::SHARE.0);
    pub const ALL: Self = Self((1 << 10) - 1);

    const NAMES: [(Self, &'static str); 10] = [
        (Self::PROJECTS, "PROJECTS"),
        (Self::TASKS, "TASKS"),
        (Self::RUNNING, "RUNNING"),
        (Self::DAY, "DAY"),
        (Self::CHIPS, "CHIPS"),
        (Self::SHARE, "SHARE"),
        (Self::TAGS, "TAGS"),
        (Self::SETTINGS, "SETTINGS"),
        (Self::PREFS, "PREFS"),
        (Self::LEASE, "LEASE"),
    ];

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// True when every topic of `other` is in `self`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// True when `self` and `other` share a topic.
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// The names of the topics in the set, in bit order.
    pub fn names(self) -> impl Iterator<Item = &'static str> {
        Self::NAMES
            .into_iter()
            .filter(move |(t, _)| self.contains(*t))
            .map(|(_, n)| n)
    }
}

impl BitOr for Topics {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for Topics {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for Topics {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl Sub for Topics {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self(self.0 & !rhs.0)
    }
}

impl fmt::Debug for Topics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return f.write_str("Topics(NONE)");
        }
        f.write_str("Topics(")?;
        for (i, name) in self.names().enumerate() {
            if i > 0 {
                f.write_str(" | ")?;
            }
            f.write_str(name)?;
        }
        f.write_str(")")
    }
}

/// A half-open span of unix seconds, `[start, end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: i64,
    pub end: i64,
}

impl Span {
    #[must_use]
    pub const fn new(start: i64, end: i64) -> Self {
        Self { start, end }
    }

    /// True when `[start, end)` (an open end is `i64::MAX`) overlaps this span. A zero-length interval counts at its
    /// start, so a segment opened this second is inside the day it opened in.
    #[must_use]
    pub const fn touches(self, start: i64, end: i64) -> bool {
        start < self.end && (end > self.start || (end == start && start >= self.start))
    }
}

/// The time ranges the views show, in unix seconds: what a changed segment has to touch to matter to them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Scope {
    /// The selected day of the day axis.
    pub day: Span,
    /// The days of the day picker's page.
    pub chips: Span,
    /// The period of the Share of time card.
    pub share: Span,
}

/// The topics whose data differs between `before` and `after`, for views over `scope`.
///
/// Rules, by section:
/// - projects: any change sets `PROJECTS`; a new or gone project, or a new colour, also `TASKS | DAY | CHIPS |
///   SHARE`; a new name `TASKS | SHARE` (a rename never touches the day axis); archive or deletion `TASKS`.
/// - tasks: any change sets `TASKS`; name, tags or description (subtasks) `SHARE`; a name `DAY` too (the day axis'
///   blocks are labelled with it); project or liveness `DAY | CHIPS | SHARE`; a new or gone task
///   `DAY | CHIPS | SHARE` too.
/// - segments: any change sets `TASKS`, plus `DAY`, `CHIPS`, `SHARE` for each span either version touches; chips
///   count closed time only, so a segment that is open here in every version leaves `CHIPS` alone, unless it starts
///   before the picker's page: any version that does sets `CHIPS`, as it can move the first day with time, which
///   decides whether the picker can show older days ([`crate::views::DayPage::older`]).
/// - idle gaps, billable pauses, pause requests: `TASKS | DAY | CHIPS | SHARE` (they cut or add pieces).
/// - another device's heartbeat: the same, when that device has an open segment (its end follows the beat).
/// - tags `TAGS | TASKS | SHARE`; settings `SETTINGS`; the lease memo `LEASE`; other preferences `PREFS`.
/// - rounding rules `DAY`: the day axis' sub-grid splits the hours by the increment in force on its day
///   ([`crate::views::DayHead::increment_minutes`]).
/// - `RUNNING` when [`crate::views::running`] differs: the task timed here, its name, or the start of the piece
///   being timed (a gap cut into a running segment moves it).
///
/// Sections no milestone-1 view reads (shifts, rates, templates, summaries, prefixes, the audit log) set nothing yet.
#[must_use]
pub fn diff(before: &LocalFile, after: &LocalFile, scope: &Scope) -> Topics {
    let (b, a) = (&before.data, &after.data);
    let own = after.device_id.as_str();
    let mut topics = Topics::NONE;

    for (old, new) in changes(&b.projects, &a.projects) {
        topics |= project_topics(old, new);
    }
    for (old, new) in changes(&b.tasks, &a.tasks) {
        topics |= task_topics(old, new);
    }
    for (old, new) in changes(&b.segments, &a.segments) {
        topics |= segment_topics(old, new, own, scope);
    }
    let pieces_moved = changes(&b.gaps, &a.gaps).next().is_some()
        || changes(&b.pauses, &a.pauses).next().is_some()
        || changes(&b.pause_requests, &a.pause_requests)
            .next()
            .is_some();
    let beat_moved = changes(&before.remote_devices, &after.remote_devices)
        .next()
        .is_some()
        && remote_open(before, after);
    if pieces_moved || beat_moved {
        topics |= Topics::TASKS | Topics::DAY | Topics::CHIPS | Topics::SHARE;
    }
    if changes(&b.tags, &a.tags).next().is_some() {
        topics |= Topics::TAGS | Topics::TASKS | Topics::SHARE;
    }
    if changes(&b.rounding, &a.rounding).next().is_some() {
        topics |= Topics::DAY;
    }
    if b.settings != a.settings {
        topics |= Topics::SETTINGS;
    }
    if before.prefs.lease_memo != after.prefs.lease_memo {
        topics |= Topics::LEASE;
    }
    if !prefs_equal_but_lease(&before.prefs, &after.prefs) {
        topics |= Topics::PREFS;
    }
    // Compared as shown: the task, its name and the start of the piece being timed, which a sleep gap or a
    // rename moves without touching the segment's own times.
    if crate::views::running(before) != crate::views::running(after) {
        topics |= Topics::RUNNING;
    }
    topics
}

/// The pairs `(before, after)` of every key whose record differs, including records added or removed.
fn changes<'a, V: PartialEq>(
    before: &'a BTreeMap<String, V>,
    after: &'a BTreeMap<String, V>,
) -> impl Iterator<Item = (Option<&'a V>, Option<&'a V>)> {
    let gone_or_changed = before.iter().filter_map(|(k, old)| {
        let new = after.get(k);
        (new != Some(old)).then_some((Some(old), new))
    });
    let added = after
        .iter()
        .filter(|(k, _)| !before.contains_key(*k))
        .map(|(_, new)| (None, Some(new)));
    gone_or_changed.chain(added)
}

fn project_topics(old: Option<&Project>, new: Option<&Project>) -> Topics {
    let (Some(o), Some(n)) = (old, new) else {
        return Topics::PROJECTS | Topics::TASKS | Topics::DAY | Topics::CHIPS | Topics::SHARE;
    };
    let mut t = Topics::PROJECTS;
    if o.name != n.name {
        t |= Topics::TASKS | Topics::SHARE;
    }
    if o.archived != n.archived || o.deleted != n.deleted {
        t |= Topics::TASKS;
    }
    if o.color != n.color {
        t |= Topics::TASKS | Topics::DAY | Topics::CHIPS | Topics::SHARE;
    }
    t
}

fn task_topics(old: Option<&Task>, new: Option<&Task>) -> Topics {
    let (Some(o), Some(n)) = (old, new) else {
        return Topics::TASKS | Topics::DAY | Topics::CHIPS | Topics::SHARE;
    };
    let mut t = Topics::TASKS;
    if o.name != n.name || o.tags != n.tags || o.description != n.description {
        t |= Topics::SHARE;
    }
    // The blocks of the day axis carry their task's name (review R3); a task with no block that day recomputes to
    // the same rows, which the worker then drops.
    if o.name != n.name {
        t |= Topics::DAY;
    }
    if o.project_id != n.project_id || o.is_live() != n.is_live() {
        t |= Topics::DAY | Topics::CHIPS | Topics::SHARE;
    }
    t
}

/// The widest interval a version of a segment can count in: raw times and any correction, an open end as `MAX`.
fn reach(s: &Segment) -> (i64, i64) {
    let raw_end = s.end.map_or(i64::MAX, |e| e.secs());
    match &s.edit {
        Some(e) => (
            s.start.secs().min(e.start.secs()),
            raw_end.max(e.end.secs()),
        ),
        None => (s.start.secs(), raw_end),
    }
}

/// Open on this device and uncorrected: still being timed here, so it adds no closed time to a chip.
fn timed_here(s: &Segment, own: &str) -> bool {
    s.end.is_none() && s.edit.is_none() && s.device_id == own
}

fn segment_topics(
    old: Option<&Segment>,
    new: Option<&Segment>,
    own: &str,
    scope: &Scope,
) -> Topics {
    let mut t = Topics::TASKS;
    for s in [old, new].into_iter().flatten() {
        let (start, end) = reach(s);
        if scope.day.touches(start, end) {
            t |= Topics::DAY;
        }
        if scope.share.touches(start, end) {
            t |= Topics::SHARE;
        }
        // Before the page counts whether open here or not: a piece running here counts from its start for the first day
        // with time (review F2). A running segment starts now, inside today's page, so a timer never sets it.
        if (!timed_here(s, own) && scope.chips.touches(start, end)) || start < scope.chips.start {
            t |= Topics::CHIPS;
        }
    }
    t
}

/// Some device other than this one has an open segment in either version, so its heartbeat ends a piece.
fn remote_open(before: &LocalFile, after: &LocalFile) -> bool {
    [before, after].into_iter().any(|f| {
        f.data
            .segments
            .values()
            .any(|s| s.end.is_none() && s.device_id != f.device_id)
    })
}

fn prefs_equal_but_lease(a: &LocalPrefs, b: &LocalPrefs) -> bool {
    if a.lease_memo == b.lease_memo {
        return a == b;
    }
    let mut a = a.clone();
    a.lease_memo.clone_from(&b.lease_memo);
    a == *b
}

/// This device's open, uncorrected segment of a live task: `(task id, start)`. One at most while the core keeps
/// one task running per device; the earliest wins if there were more.
pub(crate) fn own_open(file: &LocalFile) -> Option<(&str, i64)> {
    let own = file.device_id.as_str();
    file.data
        .segments
        .values()
        .filter(|s| timed_here(s, own))
        .filter(|s| file.data.tasks.get(&s.task_id).is_some_and(Task::is_live))
        .map(|s| (s.task_id.as_str(), s.start.secs()))
        .min_by_key(|&(id, start)| (start, id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_operations_and_names() {
        let t = Topics::TASKS | Topics::DAY;
        assert!(t.contains(Topics::TASKS));
        assert!(!t.contains(Topics::TASKS | Topics::SHARE));
        assert!(t.intersects(Topics::DAY | Topics::SHARE));
        assert_eq!(t - Topics::DAY, Topics::TASKS);
        assert_eq!(t & Topics::VIEWS, t);
        assert_eq!(format!("{t:?}"), "Topics(TASKS | DAY)");
        assert_eq!(format!("{:?}", Topics::NONE), "Topics(NONE)");
        assert_eq!(Topics::ALL.names().count(), 10);
        assert!(Topics::ALL.contains(Topics::VIEWS | Topics::LEASE));
    }

    #[test]
    fn a_span_takes_overlaps_open_ends_and_a_segment_opened_this_second() {
        let day = Span::new(100, 200);
        assert!(day.touches(150, i64::MAX), "open since inside the day");
        assert!(day.touches(50, i64::MAX), "open since before the day");
        assert!(!day.touches(200, i64::MAX), "opened when the day ended");
        assert!(day.touches(150, 150), "zero length, inside");
        assert!(day.touches(100, 100), "zero length at the start");
        assert!(!day.touches(50, 100), "ended when the day began");
        assert!(day.touches(50, 101));
    }
}
