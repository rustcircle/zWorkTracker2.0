//! The data each view topic shows, computed from a `LocalFile`: pure functions, no clock and no I/O.
//!
//! Rows keep only what the screen shows, so equal rows mean an unchanged screen. Nothing here changes from one
//! second to the next: the running clock is `now-sec` in the UI, and a [`TaskRow`] or [`Running`] carries the
//! seconds it needs to derive its elapsed text from that one property.

use std::collections::BTreeMap;

use chrono::{Datelike, Days, NaiveDate};
use chrono_tz::Tz;
use z_work_core::Ts;
use z_work_core::billing::start_of_day;
use z_work_core::machine::{RequiredField, cold_required, doc_has_text};
use z_work_core::model::{LocalFile, PauseReason, Shared, Task, TaskState};
use z_work_core::pieces::PieceKind;
use zwt_ui_logic::counted::{is_live, pieces_at};
use zwt_ui_logic::day::{DayChip, DayPiece, day_chips, day_increment, day_pieces, first_day};
use zwt_ui_logic::format;
use zwt_ui_logic::share::{ShareLevel, ShareRow, share_of_time};

use crate::topics::{Scope, Span};

/// Days on one page of the day picker (PLAN.md M1: two weeks of sample data). The page shown first ends today; the
/// picker's arrows move one whole page back or forward, as 1.x's "Show older" adds a page of 14 days
/// (TimelineTab.tsx `PAGE`).
pub const CHIP_DAYS: u32 = 14;

/// One timer button: whether it works, and why not (1.x `Control`; empty `why` when enabled).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Control {
    pub enabled: bool,
    pub why: String,
}

impl Control {
    fn on() -> Self {
        Self {
            enabled: true,
            why: String::new(),
        }
    }

    fn off(why: impl Into<String>) -> Self {
        Self {
            enabled: false,
            why: why.into(),
        }
    }
}

/// The four timer buttons of a task (1.x `controlsFor`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Controls {
    pub start: Control,
    pub pause: Control,
    pub resume: Control,
    pub stop: Control,
}

/// A live tag on a task, in the task's order (1.x `TagChips`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagChip {
    pub id: String,
    pub name: String,
    pub color: String,
}

/// Stopped tasks listed before the rest wait behind "Show all N stopped tasks" (1.x TaskRail.tsx `STOPPED_PAGE`).
pub const STOPPED_PAGE: usize = 8;

/// The group a task is listed under. 1.x's groups, in 1.x's order (TaskRail.tsx: Active, Drafts, Stopped), which the
/// task list keeps (owner, 2026-11-05: "activities/tasks is just the same, the running on top").
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TaskGroup {
    /// Running and paused tasks: "Active".
    Active,
    /// Tasks not started yet: "Drafts".
    Drafts,
    /// Finished tasks: "Stopped", [`STOPPED_PAGE`] of them until every one is asked for ([`StoppedPage`]).
    Stopped,
}

impl TaskGroup {
    /// The groups, top first.
    pub const ALL: [Self; 3] = [Self::Active, Self::Drafts, Self::Stopped];

    /// The group of a task in `state` (TaskRail.tsx: `running` or `paused`; `draft`; `stopped`).
    #[must_use]
    pub const fn of(state: TaskState) -> Self {
        match state {
            TaskState::Running | TaskState::Paused => Self::Active,
            TaskState::Draft => Self::Drafts,
            TaskState::Stopped => Self::Stopped,
        }
    }

    /// The group's place from the top: 0 Active, 1 Drafts, 2 Stopped (its index in [`Self::ALL`]).
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Active => 0,
            Self::Drafts => 1,
            Self::Stopped => 2,
        }
    }
}

/// The paging of the Stopped group (1.x TaskRail.tsx `showAllStopped`): its first [`STOPPED_PAGE`] tasks, or every
/// one of them after "Show all N stopped tasks", until "Show fewer".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct StoppedPage {
    /// Every stopped task, listed or not: the N of "Show all N stopped tasks".
    pub total: usize,
    /// Every stopped task is listed, and the link reads "Show fewer"; else the first [`STOPPED_PAGE`] are.
    pub all: bool,
}

impl StoppedPage {
    /// More stopped tasks than one page: the link under the group is there (1.x `stopped.length > STOPPED_PAGE`).
    #[must_use]
    pub const fn has_link(self) -> bool {
        self.total > STOPPED_PAGE
    }
}

/// The task list as the rail lists it: [`task_rows`] with the Stopped group cut to its page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskList {
    pub rows: Vec<TaskRow>,
    pub stopped: StoppedPage,
}

/// One row of the task list.
///
/// Elapsed time in the UI: `closed_secs + (now_sec - open_since)` while `open_since` is set (the task runs on this
/// device), else `closed_secs`. 1.x's session clock ("this session") is `now_sec - open_since`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskRow {
    pub id: String,
    pub name: String,
    pub project_id: String,
    /// Empty when the project is not found (1.x shows a "No project" badge; see `has_project`).
    pub project_name: String,
    pub project_color: String,
    pub has_project: bool,
    /// Archived or deleted project (1.x draws the chip as archived).
    pub project_archived: bool,
    pub state: TaskState,
    /// The group the task is listed under ([`TaskGroup::of`] its state).
    pub group: TaskGroup,
    /// Closed time logged on the task (1.x `loggedSecs`): counted pieces, without billable pauses and without the
    /// piece still being timed here.
    pub closed_secs: i64,
    /// "2h 05m logged" as 1.x shows it under a paused or stopped task; empty when 1.x shows nothing.
    pub logged_text: String,
    /// Start (unix s) of the piece being timed on this device; `None` unless the task runs here.
    pub open_since: Option<i64>,
    pub tags: Vec<TagChip>,
    /// The pause reason, "Paused. Time is not being counted.", or "Stopped 2026-03-11 16:30 UTC."; empty otherwise.
    pub note: String,
    /// `note` is why a paused task was paused, which 1.x draws boxed (`.task-note-pause`); false for "Paused. Time is
    /// not being counted." and in every other state.
    pub note_is_reason: bool,
    /// The cold-launch note under a paused task; empty once a task has started in this launch.
    pub cold_note: String,
    pub controls: Controls,
}

/// The task running on this device, if any.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Running {
    pub task_id: Option<String>,
    /// Empty when nothing runs.
    pub task_name: String,
    /// Start (unix s) of the piece being timed: 1.x's session clock is `now_sec - started_at`.
    pub started_at: Option<i64>,
}

impl Running {
    #[must_use]
    pub const fn is_running(&self) -> bool {
        self.task_id.is_some()
    }
}

/// The scalar part of the day axis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DayHead {
    pub date: NaiveDate,
    /// Today in the viewing zone, so the UI can mark the picker without a clock of its own.
    pub today: NaiveDate,
    /// "Today, Wed 4 Nov 2026", "Yesterday, ...", "Mon 2 Nov 2026" (1.x `relativeDay`).
    pub label: String,
    /// The day's bounds in unix seconds, `[start, end)`; 23 or 25 hours long across a DST change.
    pub start: i64,
    pub end: i64,
    /// The minutes the axis' sub-grid splits each hour by: the rounding increment in force on the day
    /// ([`zwt_ui_logic::day::day_increment`]; 15 when none is saved or rounding is off). Never 0.
    pub increment_minutes: u32,
}

/// The day axis: its head and the day's pieces (an open piece has `end: None`; the UI extends it to `now-sec`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DayView {
    pub head: DayHead,
    pub pieces: Vec<DayPiece>,
}

/// The period of the Share of time card: whole days in the viewing zone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Period {
    pub from_day: NaiveDate,
    /// Inclusive.
    pub to_day: NaiveDate,
    /// `[start, end)` in unix seconds.
    pub start: i64,
    pub end: i64,
    /// "2 Nov – 8 Nov 2026" (1.x `labelOf('week', ...)`).
    pub label: String,
}

/// The scalar part of the Share of time card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareHead {
    /// The level the user picked; it stays until they pick another (PARITY 212).
    pub picked: Option<ShareLevel>,
    /// The level drawn: `picked`, or the default when the picked one has nothing to show.
    pub shown: ShareLevel,
    pub subtasks_available: bool,
    /// Donut middle: "16h 50m".
    pub total_text: String,
    pub note: Option<String>,
    pub footnote: Option<String>,
    pub period: Period,
}

/// The Share of time card: head and legend rows (keyed by `ShareRow::key`; render `ShareRow::text`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareData {
    pub head: ShareHead,
    pub rows: Vec<ShareRow>,
}

/// The day `at` falls on in `zone`.
#[must_use]
pub fn day_of(at: Ts, zone: Tz) -> NaiveDate {
    at.datetime().with_timezone(&zone).date_naive()
}

fn next_day(day: NaiveDate) -> Option<NaiveDate> {
    day.checked_add_days(Days::new(1))
}

/// `[start of day, start of next day)` in unix seconds.
#[must_use]
pub fn day_span(day: NaiveDate, zone: Tz) -> Span {
    let end = next_day(day).map_or(Ts::MAX, |d| start_of_day(d, zone));
    Span::new(start_of_day(day, zone).secs(), end.secs())
}

/// The first day of the page of `days` days ending with `last` (the calendar's first day if it starts earlier).
fn page_first(last: NaiveDate, days: u32) -> NaiveDate {
    let back = u64::from(days.saturating_sub(1));
    last.checked_sub_days(Days::new(back))
        .unwrap_or(NaiveDate::MIN)
}

/// The span of a page of the day picker: `days` days ending with `last`.
#[must_use]
pub fn chips_span(last: NaiveDate, days: u32, zone: Tz) -> Span {
    Span::new(
        day_span(page_first(last, days), zone).start,
        day_span(last, zone).end,
    )
}

/// The page of the day picker: the days its chips show, and which way its arrows can move it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DayPage {
    /// The oldest day shown, on the left.
    pub first: NaiveDate,
    /// The newest day shown, on the right: today on the page shown first.
    pub last: NaiveDate,
    /// A day with time lies before `first` (1.x has no limit: its "Show older" reaches the first day with time), so
    /// the left arrow (older days) works.
    pub older: bool,
    /// `last` is before today, so the right arrow (newer days) works; no page goes past today.
    pub newer: bool,
}

/// The last day of the page `back` pages before the one ending `today` (a page is [`CHIP_DAYS`] days).
#[must_use]
pub fn page_last(today: NaiveDate, back: u32) -> NaiveDate {
    let days = u64::from(back).saturating_mul(u64::from(CHIP_DAYS));
    today
        .checked_sub_days(Days::new(days))
        .unwrap_or(NaiveDate::MIN)
}

/// The page of [`CHIP_DAYS`] days ending with `last`, and whether the picker can show older days (one of them has
/// time, [`zwt_ui_logic::day::first_day`]) or newer ones (`last` is before `today`).
#[must_use]
pub fn day_page(file: &LocalFile, last: NaiveDate, today: NaiveDate, zone: Tz) -> DayPage {
    let first = page_first(last, CHIP_DAYS);
    DayPage {
        first,
        last,
        older: first_day(file, zone).is_some_and(|day| day < first),
        newer: last < today,
    }
}

/// The week (Monday to Sunday) containing `today`: 1.x's Analysis opens on it (`defaultRange`).
#[must_use]
pub fn week_of(today: NaiveDate, zone: Tz) -> Period {
    let back = u64::from(today.weekday().num_days_from_monday());
    let from_day = today.checked_sub_days(Days::new(back)).unwrap_or(today);
    let to_day = from_day.checked_add_days(Days::new(6)).unwrap_or(from_day);
    let (from_year, to_year) = (from_day.year(), to_day.year());
    let label = if from_year == to_year {
        format!(
            "{} – {} {to_year}",
            format::short_day(from_day),
            format::short_day(to_day)
        )
    } else {
        format!(
            "{} {from_year} – {} {to_year}",
            format::short_day(from_day),
            format::short_day(to_day)
        )
    };
    Period {
        from_day,
        to_day,
        start: day_span(from_day, zone).start,
        end: day_span(to_day, zone).end,
        label,
    }
}

/// The scope of the views: the spans a changed segment must touch to matter to them. `chips_last` is the last day of
/// the day picker's page (today on the page shown first, [`page_last`]).
#[must_use]
pub fn scope_of(day: NaiveDate, chips_last: NaiveDate, period: &Period, zone: Tz) -> Scope {
    Scope {
        day: day_span(day, zone),
        chips: chips_span(chips_last, CHIP_DAYS, zone),
        share: Span::new(period.start, period.end),
    }
}

/// Per task: closed seconds, and the start of the piece being timed here.
#[derive(Default)]
struct Figures {
    closed: i64,
    open_since: Option<i64>,
}

fn figures(file: &LocalFile) -> BTreeMap<&str, Figures> {
    let mut out: BTreeMap<&str, Figures> = BTreeMap::new();
    // The end of the piece being timed does not matter: it counts as no closed time.
    for p in pieces_at(file, Ts::MAX) {
        if p.kind == PieceKind::BillablePause {
            continue;
        }
        let Some((id, _)) = file.data.tasks.get_key_value(&p.task_id) else {
            continue;
        };
        let f = out.entry(id.as_str()).or_default();
        let (start, end) = (p.start.secs(), p.end.secs());
        if is_live(&p, file) {
            f.open_since = Some(f.open_since.map_or(start, |s| s.min(start)));
        } else {
            f.closed += (end - start).max(0);
        }
    }
    out
}

/// The three parts of each group, top first ([`task_rows`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Tier {
    /// A segment of it is open on this device: the task running here.
    RunningHere,
    /// It has a segment: it ran, or runs on another device.
    Ran,
    /// No segment counts for it.
    NeverRan,
}

/// When a task last ran: whether a segment of it is open here or elsewhere, and the latest end of the others.
#[derive(Clone, Copy, Debug, Default)]
struct LastRun {
    here: bool,
    elsewhere: bool,
    end: Option<i64>,
}

/// Per task, from its segments as they count (a correction's times, a voided segment left out); billable pauses
/// are not running the task, so they are not looked at.
fn last_runs(file: &LocalFile) -> BTreeMap<&str, LastRun> {
    let mut out: BTreeMap<&str, LastRun> = BTreeMap::new();
    for seg in file.data.segments.values() {
        let Some((_, end)) = seg.effective() else {
            continue;
        };
        let run = out.entry(seg.task_id.as_str()).or_default();
        match end {
            Some(end) => run.end = Some(run.end.map_or(end.secs(), |e| e.max(end.secs()))),
            None if seg.device_id == file.device_id => run.here = true,
            None => run.elsewhere = true,
        }
    }
    out
}

/// The task's place in its group, as `(tier, last run, created)`: see [`task_rows`]. A task running elsewhere runs
/// now, later than any end on record, so it is `i64::MAX`; the list then needs no clock and no tick moves it.
fn place(run: Option<&LastRun>, created: i64) -> (Tier, i64, i64) {
    match run {
        Some(r) if r.here => (Tier::RunningHere, i64::MAX, created),
        Some(r) if r.elsewhere => (Tier::Ran, i64::MAX, created),
        Some(LastRun { end: Some(end), .. }) => (Tier::Ran, *end, created),
        _ => (Tier::NeverRan, 0, created),
    }
}

/// 1.x `pauseReasonText`.
fn pause_reason_text(reason: Option<&PauseReason>, shared: &Shared) -> Option<String> {
    Some(match reason? {
        PauseReason::User => "Paused by you.".to_owned(),
        PauseReason::AutoPaused { by_task_id } => {
            let by = shared
                .tasks
                .get(by_task_id)
                .map_or("another task", |t| t.name.as_str());
            format!("Auto-paused because \"{by}\" was started.")
        }
        PauseReason::AppQuit => {
            "Paused when the app was closed. Time stopped at that moment.".to_owned()
        }
        PauseReason::CrashRecovery => "Paused after the app closed unexpectedly. Time counts up to the last moment the app was seen running.".to_owned(),
        PauseReason::SyncReconciled => "Paused while merging with another device.".to_owned(),
        PauseReason::BackupRestored => "Paused: the restored backup held this running. Time stopped at the last moment the backup knows about.".to_owned(),
        PauseReason::RemoteRequest => "Paused because another device asked for it. Time stopped at the moment of the request.".to_owned(),
    })
}

const COLD_NOTE: &str = "After a restart, Resume asks for every field again and starts a new task that continues this one. This task stays as it is.";

fn live_tag_count(task: &Task, shared: &Shared) -> usize {
    task.tags
        .iter()
        .filter(|id| shared.tags.get(*id).is_some_and(|t| !t.deleted))
        .count()
}

/// 1.x `coldMissing`: the required fields a draft still lacks for the first start of a launch.
fn cold_missing(task: &Task, shared: &Shared, warm: bool) -> Vec<&'static str> {
    if warm {
        return Vec::new();
    }
    let mut required = vec![RequiredField::Name, RequiredField::Project];
    for f in cold_required(shared) {
        if !required.contains(&f) {
            required.push(f);
        }
    }
    required
        .into_iter()
        .filter_map(|f| {
            let (missing, word) = match f {
                RequiredField::Name => (task.name.trim().is_empty(), "name"),
                RequiredField::Project => (task.project_id.is_empty(), "project"),
                RequiredField::Description => (!doc_has_text(&task.description), "description"),
                RequiredField::Tags => (live_tag_count(task, shared) == 0, "tags"),
            };
            missing.then_some(word)
        })
        .collect()
}

/// 1.x `controlsFor`, for a task timed on this device (milestone 1 has no other devices).
fn controls_for(task: &Task, shared: &Shared, warm: bool) -> Controls {
    let project = shared.projects.get(&task.project_id);
    let project_block = match project {
        None => Some("This task has no project.".to_owned()),
        Some(p) if p.archived || p.deleted => Some(format!(
            "Project \"{}\" is archived. Restore it first.",
            p.name
        )),
        Some(_) => None,
    };
    let wrong_state =
        |needs: &str| Control::off(format!("Only available when the task is {needs}."));
    match task.state {
        TaskState::Draft => {
            let missing = cold_missing(task, shared, warm);
            let cold_block = (!missing.is_empty()).then(|| {
                let them = missing.len() > 1 || missing.first() == Some(&"tags");
                format!(
                    "The first start after launch needs {}. Edit the task to add {}.",
                    missing.join(", "),
                    if them { "them" } else { "it" }
                )
            });
            Controls {
                start: project_block
                    .or(cold_block)
                    .map_or_else(Control::on, Control::off),
                pause: wrong_state("running"),
                resume: wrong_state("paused"),
                stop: Control::off("Start the task first."),
            }
        }
        TaskState::Running => Controls {
            start: wrong_state("a draft"),
            pause: Control::on(),
            resume: wrong_state("paused"),
            stop: Control::on(),
        },
        TaskState::Paused => Controls {
            start: wrong_state("a draft"),
            pause: wrong_state("running"),
            // Cold: Resume opens a form for a new linked task in 1.x, so the old project does not matter.
            resume: if warm {
                project_block.map_or_else(Control::on, Control::off)
            } else {
                Control::on()
            },
            stop: Control::on(),
        },
        TaskState::Stopped => {
            let done = || Control::off("This task is finished.");
            Controls {
                start: done(),
                pause: done(),
                resume: done(),
                stop: done(),
            }
        }
    }
}

/// The task list: 1.x's groups in 1.x's order, Active (running and paused tasks), Drafts, then Stopped (TaskRail.tsx;
/// owner, 2026-11-05), and inside each group the owner's order (2026-11-04), which replaces 1.x's order of each group
/// where they differ:
/// 1. the task running here, on top;
/// 2. then the tasks that ran, by when they last ran, the most recent first: the end of their latest segment, or now
///    while one runs on another device;
/// 3. then the tasks that never ran (no segment counts for them), the newest created first;
/// 4. ties by creation time, the newest first, then by id.
///
/// So the running task heads the list, a draft with time added by hand heads the Drafts, and a stopped task whose
/// time was all voided ends the Stopped group. Every stopped task is here; [`task_list`] cuts the group to its page.
/// `warm`: a task has started in this launch.
#[must_use]
pub fn task_rows(file: &LocalFile, warm: bool) -> Vec<TaskRow> {
    let shared = &file.data;
    let figures = figures(file);
    let runs = last_runs(file);
    let mut rows: Vec<((TaskGroup, Tier, i64, i64), TaskRow)> = Vec::new();
    for task in shared.tasks.values().filter(|t| t.is_live()) {
        let f = figures.get(task.id.as_str());
        let closed_secs = f.map_or(0, |f| f.closed);
        let project = shared.projects.get(&task.project_id);
        let note = match task.state {
            TaskState::Paused => pause_reason_text(task.pause_reason.as_ref(), shared)
                .unwrap_or_else(|| "Paused. Time is not being counted.".to_owned()),
            TaskState::Stopped => task.stopped_at.map_or_else(String::new, |at| {
                let iso = at.iso();
                let minute: String = iso.chars().take(16).collect();
                format!("Stopped {} UTC.", minute.replace('T', " "))
            }),
            TaskState::Draft | TaskState::Running => String::new(),
        };
        let logged_text = match task.state {
            TaskState::Paused | TaskState::Stopped if closed_secs > 0 => {
                format!("{} logged", format::duration(closed_secs))
            }
            _ => String::new(),
        };
        let row = TaskRow {
            id: task.id.clone(),
            name: task.name.clone(),
            project_id: task.project_id.clone(),
            project_name: project.map_or_else(String::new, |p| p.name.clone()),
            project_color: project.map_or_else(
                || zwt_ui_logic::day::UNKNOWN_PROJECT_COLOR.to_owned(),
                |p| p.color.clone(),
            ),
            has_project: project.is_some(),
            project_archived: project.is_some_and(|p| p.archived || p.deleted),
            state: task.state,
            group: TaskGroup::of(task.state),
            closed_secs,
            logged_text,
            open_since: f.and_then(|f| f.open_since),
            tags: task
                .tags
                .iter()
                .filter_map(|id| shared.tags.get(id).filter(|t| !t.deleted))
                .map(|t| TagChip {
                    id: t.id.clone(),
                    name: t.name.clone(),
                    color: t.color.clone(),
                })
                .collect(),
            note,
            note_is_reason: task.state == TaskState::Paused && task.pause_reason.is_some(),
            cold_note: if task.state == TaskState::Paused && !warm {
                COLD_NOTE.to_owned()
            } else {
                String::new()
            },
            controls: controls_for(task, shared, warm),
        };
        let (tier, last, created) = place(runs.get(task.id.as_str()), task.created_at.secs());
        rows.push(((row.group, tier, last, created), row));
    }
    // The group first; within it the tier, then the latest run first, then the newest task, then the id: a total
    // order, so the list does not depend on the order the store keeps.
    rows.sort_by(|((ga, ta, la, ca), a), ((gb, tb, lb, cb), b)| {
        ga.cmp(gb)
            .then_with(|| ta.cmp(tb))
            .then_with(|| lb.cmp(la))
            .then_with(|| cb.cmp(ca))
            .then_with(|| a.id.cmp(&b.id))
    });
    rows.into_iter().map(|(_, r)| r).collect()
}

/// [`task_rows`] as the rail lists them (1.x `stoppedShown`): every Active and Draft task, and the first
/// [`STOPPED_PAGE`] stopped ones, the most recently run, unless `all_stopped`.
#[must_use]
pub fn task_list(file: &LocalFile, warm: bool, all_stopped: bool) -> TaskList {
    let mut rows = task_rows(file, warm);
    let total = rows
        .iter()
        .filter(|r| r.group == TaskGroup::Stopped)
        .count();
    if !all_stopped {
        let mut stopped = 0usize;
        rows.retain(|r| {
            if r.group != TaskGroup::Stopped {
                return true;
            }
            stopped += 1;
            stopped <= STOPPED_PAGE
        });
    }
    TaskList {
        rows,
        stopped: StoppedPage {
            total,
            all: all_stopped,
        },
    }
}

/// The task running on this device and the start of the piece being timed.
#[must_use]
pub fn running(file: &LocalFile) -> Running {
    let open = pieces_at(file, Ts::MAX)
        .into_iter()
        .filter(|p| is_live(p, file) && p.kind != PieceKind::BillablePause)
        .min_by(|a, b| a.start.cmp(&b.start).then_with(|| a.id.cmp(&b.id)));
    let Some(piece) = open else {
        return Running::default();
    };
    Running {
        task_name: file
            .data
            .tasks
            .get(&piece.task_id)
            .map_or_else(String::new, |t| t.name.clone()),
        task_id: Some(piece.task_id),
        started_at: Some(piece.start.secs()),
    }
}

/// The day axis of `date`, with the increment its sub-grid splits the hours by.
#[must_use]
pub fn day_view(file: &LocalFile, date: NaiveDate, today: NaiveDate, zone: Tz) -> DayView {
    let span = day_span(date, zone);
    DayView {
        head: DayHead {
            date,
            today,
            label: format::relative_day(date, today),
            start: span.start,
            end: span.end,
            increment_minutes: day_increment(file, date),
        },
        pieces: day_pieces(file, date, zone),
    }
}

/// The day picker: the [`CHIP_DAYS`] chips of the page ending with `last` (today on the page shown first), the oldest
/// first, so the picker runs from the past to the future like the day axis; closed time only.
#[must_use]
pub fn chips(file: &LocalFile, last: NaiveDate, zone: Tz) -> Vec<DayChip> {
    day_chips(file, last, CHIP_DAYS, zone)
}

/// The Share of time card over `period`, with this device's running piece counted up to `now`.
#[must_use]
pub fn share(file: &LocalFile, period: &Period, now: Ts, picked: Option<ShareLevel>) -> ShareData {
    let view = share_of_time(file, period.start, period.end, now.secs(), picked);
    ShareData {
        head: ShareHead {
            picked: view.picked,
            shown: view.shown,
            subtasks_available: view.subtasks_available,
            total_text: view.total_text,
            note: view.note,
            footnote: view.footnote,
            period: period.clone(),
        },
        rows: view.rows,
    }
}
