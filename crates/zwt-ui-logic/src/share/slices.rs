//! The slices of the Share of time donut and the rows of its legend at each level: which rows there are, their
//! names, colours, minutes and percentages. Port of `features/analysis/shareSlices.ts`.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::levels::ShareLevel;
use super::shades::task_shades;
use crate::collate::locale_compare;
use crate::format::{hours_minutes, percent_label};
use crate::js;

/// Up to this many rows all get a slice, at the levels that group (tasks, subtasks, tag combinations).
pub const SHOW_ALL_UP_TO: usize = 9;
/// Past that, only this many rows (the ones with the most time) get a slice and the rest are grouped as "Other".
pub const TOP_ROWS: usize = 8;
/// The colour of the grouped rest: 1.x's strong border token, a mid grey in all four themes. The UI draws it with
/// its theme's `border-strong` colour; [`RowKind::Other`] marks the row.
pub const OTHER_COLOR: &str = "var(--border-strong)";
/// Written after a task's name for its time on no subtask: "Logo concepts (no subtask)".
pub const NO_SUBTASK_LABEL: &str = "(no subtask)";
/// The row of the tasks that carry no tag.
pub const NO_TAG_LABEL: &str = "No tag";
/// Between the tags of a combination: "Design + Client".
pub const TAG_SEPARATOR: &str = " + ";
/// Between the parts of a row's path in its title and its slice's label: "Brand refresh > Logo concepts > Moodboard".
pub const PATH_SEPARATOR: &str = " > ";
/// Between a row's name and the names after it in the legend, for the eye only: "Logo · Alpha".
pub const CONTEXT_SEPARATOR: &str = "·";
/// Under the legend of the Tags level, when a task has more than one tag.
pub const COMBINATION_NOTE: &str =
    "Tasks with several tags are counted once, under their combination.";
/// The grey of "No tag" (`UNTAGGED_COLOR` of `fam-p2.ts`), and of a project or task colour 1.x cannot find.
pub const UNTAGGED_COLOR: &str = "#6b7280";

/// A project as the chart needs it: its time in the basis in use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareProject {
    pub id: String,
    pub name: String,
    pub color: String,
    pub secs: i64,
}

/// A task with the project it belongs to and its time in the basis in use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareTask {
    pub id: String,
    pub name: String,
    pub project_id: String,
    pub project_name: String,
    pub secs: i64,
}

/// The time of a task on one of its subtasks (`id` is the subtask's), or on none (`id` is `None`; `name` unused).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareSubtask {
    pub task_id: String,
    pub task_name: String,
    pub project_id: String,
    pub project_name: String,
    pub id: Option<String>,
    pub name: String,
    pub secs: i64,
}

/// A tag that exists. Tags come in the order of the Settings list, which is the order of a combination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareTag {
    pub id: String,
    pub name: String,
    pub color: String,
}

/// A task with time, and the ids of the tags it carries, for the Tags level.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareTagTask {
    pub id: String,
    /// For the basis in use.
    pub secs: i64,
    pub exact: i64,
    pub billable_secs: i64,
    pub tag_ids: Vec<String>,
}

/// What the grouped rest of a level is made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OtherUnit {
    Tasks,
    Subtasks,
    Combinations,
}

impl OtherUnit {
    /// "tasks", "subtasks" or "combinations".
    #[must_use]
    pub fn plural(self) -> &'static str {
        match self {
            Self::Tasks => "tasks",
            Self::Subtasks => "subtasks",
            Self::Combinations => "combinations",
        }
    }

    /// "task", "subtask" or "combination".
    #[must_use]
    pub fn one(self) -> &'static str {
        match self {
            Self::Tasks => "task",
            Self::Subtasks => "subtask",
            Self::Combinations => "combination",
        }
    }
}

/// `otherLabel`: "Other (4 tasks)", "Other (3 subtasks)" or "Other (5 combinations)". A group has two rows at least.
#[must_use]
pub fn other_label(grouped: usize, unit: OtherUnit) -> String {
    format!("Other ({grouped} {})", unit.plural())
}

/// `otherTip`: what the tooltip of the Other slice says it is made of.
#[must_use]
pub fn other_tip(grouped: usize, unit: OtherUnit) -> String {
    format!(
        "The {grouped} {} with the least time. View data lists every {}.",
        unit.plural(),
        unit.one()
    )
}

/// `otherNote`: the note under the donut when rows are grouped.
#[must_use]
pub fn other_note(grouped: usize, unit: OtherUnit) -> String {
    format!(
        "Other adds up the {grouped} {} with the least time. View data lists every {}.",
        unit.plural(),
        unit.one()
    )
}

/// What a row stands for: a project, a task, a subtask (or a task's time on none), a combination of tags, or the
/// grouped rest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowKind {
    Project {
        id: String,
    },
    Task {
        id: String,
        project_id: String,
    },
    Subtask {
        /// The subtask's id; `None` for the task's time on none.
        id: Option<String>,
        task_id: String,
        project_id: String,
    },
    Tag {
        /// The tags, in the order the label writes them; none for the row "No tag".
        tag_ids: Vec<String>,
        /// The tasks counted in this row.
        task_ids: Vec<String>,
    },
    Other {
        /// How many rows the row adds up.
        grouped: usize,
        unit: OtherUnit,
    },
}

/// One slice and one legend row. Every row has time: a slice with none is not drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareRow {
    /// Unique within a level: `project:<id>`, `task:<id>`, `subtask:<task id>\0<subtask id>`, `tags:<ids>`, `other`.
    pub key: String,
    /// The row's own name: a project, a task, a subtask, `<Task> (no subtask)`, a combination of tags, or
    /// "Other (N tasks)".
    pub label: String,
    /// The whole path, outermost first, as one text: "Brand refresh > Logo concepts". The title, the slice's label and
    /// the accessible name carry it (PARITY 214).
    pub full_path: String,
    /// Seconds in the basis in use.
    pub secs: i64,
    /// Whole percent as written: "47%", or "<1%" for a row with time whose share rounds to 0.
    pub percent: String,
    /// The legend row's figure, also the tooltip's and the slice's: "16h 50m (47%)".
    pub text: String,
    /// A hex colour, or [`OTHER_COLOR`] for the grouped rest. For a combination of tags, the first tag's.
    pub color: String,
    /// What the row stands for.
    pub kind: RowKind,
    /// Names written after the label in muted text: only what the data leaves open (a task's project when there are
    /// several projects, a subtask's task and project).
    pub context: Vec<String>,
    /// The whole path, outermost first: project, task, subtask.
    pub path: Vec<String>,
    /// Whole minutes; all rows together add up to the total the middle of the donut shows.
    pub minutes: i64,
    /// Whole percent; all rows together add up to 100.
    pub whole_percent: i64,
    /// "16h 50m": hours always shown.
    pub hm: String,
    /// A combination of two tags or more: every tag's colour, drawn as stripes. Empty for a plain colour.
    pub stripes: Vec<String>,
}

impl ShareRow {
    /// The percentage in brackets, as the legend writes it: "(47%)", "(<1%)".
    #[must_use]
    pub fn pct(&self) -> String {
        format!("({})", self.percent)
    }

    /// The legend's line as written: the label, then each name of `context` after a middle dot: "Logo · Alpha".
    #[must_use]
    pub fn written(&self) -> String {
        let mut out = self.label.clone();
        for c in &self.context {
            out.push(' ');
            out.push_str(CONTEXT_SEPARATOR);
            out.push(' ');
            out.push_str(c);
        }
        out
    }

    /// `spokenName`: what a screen reader says for the button that opens the row: its name and the rest of its path,
    /// never the figures. `None` for a project, a combination and Other, which are their own name.
    #[must_use]
    pub fn spoken_name(&self) -> Option<String> {
        let project = self.path.first().map_or("", String::as_str);
        match &self.kind {
            RowKind::Task { .. } | RowKind::Subtask { id: None, .. } => {
                Some(format!("{}, project {project}", self.label))
            }
            RowKind::Subtask { id: Some(_), .. } => {
                let task = self.path.get(1).map_or("", String::as_str);
                Some(format!("{}, task {task}, project {project}", self.label))
            }
            RowKind::Project { .. } | RowKind::Tag { .. } | RowKind::Other { .. } => None,
        }
    }

    /// The row's own id: the project's, the task's or the subtask's; `None` for a task's time on no subtask, a
    /// combination and Other.
    #[must_use]
    pub fn id(&self) -> Option<&str> {
        match &self.kind {
            RowKind::Project { id } | RowKind::Task { id, .. } => Some(id),
            RowKind::Subtask { id, .. } => id.as_deref(),
            RowKind::Tag { .. } | RowKind::Other { .. } => None,
        }
    }

    /// The project the row belongs to; `None` for a combination and Other.
    #[must_use]
    pub fn project_id(&self) -> Option<&str> {
        match &self.kind {
            RowKind::Project { id } => Some(id),
            RowKind::Task { project_id, .. } | RowKind::Subtask { project_id, .. } => {
                Some(project_id)
            }
            RowKind::Tag { .. } | RowKind::Other { .. } => None,
        }
    }
}

/// `TagLine`: a combination of tags with the tasks that carry exactly it, for the View data table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagLine {
    pub key: String,
    pub label: String,
    pub tag_ids: Vec<String>,
    pub task_ids: Vec<String>,
    pub secs: i64,
    pub exact: i64,
    pub billable_secs: i64,
}

/// `ShareModel`: the rows of one level and their total.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareModel {
    pub level: ShareLevel,
    pub rows: Vec<ShareRow>,
    pub total_secs: i64,
    /// `Math.round(totalSecs / 60)`: the minutes the middle of the donut shows, and the sum of the rows' minutes.
    pub total_minutes: i64,
    /// Tags level: how many rows (grouped ones included) combine two tags or more. The footnote shows above 0.
    pub combinations: usize,
    /// Tags level: every combination with time, most first, grouped or not, for the View data table.
    pub lines: Vec<TagLine>,
}

// ------------------------------------------------------------------ rounding

/// Remainders closer than this are the same remainder: floating point noise, not a difference.
const EPS: f64 = 1e-9;

/// `apportion`: the largest-remainder (Hamilton) method. Splits `units` whole units among rows in proportion to
/// `weights`: each row gets the whole part of its exact share, then the units still left go one each to the rows
/// with the largest remainders, so the result adds up to `units` exactly. A tie goes to the larger weight, then to
/// the earlier row. A row with no weight (or a weight that is not a finite positive number) stays at 0.
#[must_use]
pub fn apportion(weights: &[f64], units: i64) -> Vec<i64> {
    let w: Vec<f64> = weights
        .iter()
        .map(|&x| if x.is_finite() && x > 0.0 { x } else { 0.0 })
        .collect();
    let total = w.iter().fold(0.0, |a, b| a + b);
    if total <= 0.0 || units <= 0 {
        return vec![0; w.len()];
    }
    let units_f = units as f64;
    struct Share {
        i: usize,
        weight: f64,
        floor: f64,
        rem: f64,
    }
    let shares: Vec<Share> = w
        .iter()
        .enumerate()
        .map(|(i, &weight)| {
            let exact = (weight * units_f) / total;
            let floor = (exact + EPS).floor();
            Share {
                i,
                weight,
                floor,
                rem: js::round((exact - floor) * 1e9),
            }
        })
        .collect();
    // Each floor is at most its exact share, so they fit in `units` and the conversion is exact.
    let mut out: Vec<i64> = shares.iter().map(|s| s.floor as i64).collect();
    let mut ranked: Vec<&Share> = shares.iter().filter(|s| s.weight > 0.0).collect();
    let by = |a: f64, b: f64| b.partial_cmp(&a).unwrap_or(Ordering::Equal);
    ranked.sort_by(|a, b| {
        by(a.rem, b.rem)
            .then(by(a.weight, b.weight))
            .then(a.i.cmp(&b.i))
    });
    let mut left = units - out.iter().sum::<i64>();
    let mut k = 0;
    while left > 0 && !ranked.is_empty() {
        if let Some(slot) = ranked.get(k % ranked.len()).and_then(|r| out.get_mut(r.i)) {
            *slot += 1;
        }
        k += 1;
        left -= 1;
    }
    out
}

// ------------------------------------------------------------------ rows

/// A row before its minutes and percentage are known.
struct Draft {
    key: String,
    label: String,
    context: Vec<String>,
    path: Vec<String>,
    secs: i64,
    color: String,
    stripes: Vec<String>,
    kind: RowKind,
}

/// `byTime`: most time first; the same time by name, then by id, so the order never depends on the input's order.
fn by_time(a: (&str, &str, i64), b: (&str, &str, i64)) -> Ordering {
    b.2.cmp(&a.2)
        .then_with(|| locale_compare(a.1, b.1))
        .then_with(|| a.0.cmp(b.0))
}

/// `finish`: works out minutes and percent for the rows that have time and writes their texts.
fn finish(
    level: ShareLevel,
    drafts: Vec<Draft>,
    combinations: usize,
    lines: Vec<TagLine>,
) -> ShareModel {
    let live: Vec<Draft> = drafts.into_iter().filter(|d| d.secs > 0).collect();
    let total_secs: i64 = live.iter().map(|d| d.secs).sum();
    let total_minutes = js::round_minutes(total_secs);
    let weights: Vec<f64> = live.iter().map(|d| d.secs as f64).collect();
    let minutes = apportion(&weights, total_minutes);
    let percents = apportion(&weights, 100);
    let rows = live
        .into_iter()
        .enumerate()
        .map(|(i, d)| {
            let m = minutes.get(i).copied().unwrap_or(0);
            let p = percents.get(i).copied().unwrap_or(0);
            let hm = hours_minutes(m);
            let percent = percent_label(p, d.secs);
            ShareRow {
                text: format!("{hm} ({percent})"),
                full_path: d.path.join(PATH_SEPARATOR),
                key: d.key,
                label: d.label,
                secs: d.secs,
                percent,
                color: d.color,
                kind: d.kind,
                context: d.context,
                path: d.path,
                minutes: m,
                whole_percent: p,
                hm,
                stripes: d.stripes,
            }
        })
        .collect();
    ShareModel {
        level,
        rows,
        total_secs,
        total_minutes,
        combinations,
        lines,
    }
}

/// `cut`: the rows that get a slice (up to nine, or the eight with the most time) and the rest, which are grouped.
/// `ranked` is most time first.
fn cut<T>(ranked: &[T]) -> (&[T], &[T]) {
    if ranked.len() > SHOW_ALL_UP_TO {
        ranked.split_at_checked(TOP_ROWS).unwrap_or((ranked, &[]))
    } else {
        (ranked, &[])
    }
}

/// `otherDraft`: the one row that adds up the `grouped` rows of `secs` in all.
fn other_draft(grouped: usize, secs: i64, unit: OtherUnit) -> Draft {
    let label = other_label(grouped, unit);
    Draft {
        key: "other".to_owned(),
        label: label.clone(),
        context: Vec::new(),
        path: vec![label],
        secs,
        color: OTHER_COLOR.to_owned(),
        stripes: Vec::new(),
        kind: RowKind::Other { grouped, unit },
    }
}

/// `projectOrder`: the projects of `ranked` (project id, seconds; most time first) in the order of their time, the
/// grouped rows' time counted; a tie goes to the project that ranks first.
fn project_order<'a>(ranked: impl Iterator<Item = (&'a str, i64)>) -> Vec<String> {
    let mut order: Vec<(&str, i64, usize)> = Vec::new();
    for (i, (project_id, secs)) in ranked.enumerate() {
        match order.iter_mut().find(|e| e.0 == project_id) {
            Some(e) => e.1 += secs,
            None => order.push((project_id, secs, i)),
        }
    }
    order.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));
    order.into_iter().map(|e| e.0.to_owned()).collect()
}

fn distinct<'a>(ids: impl Iterator<Item = &'a str>) -> usize {
    ids.collect::<BTreeSet<_>>().len()
}

/// `projectShares`: every project with time, most time first, in the colour it has everywhere else.
#[must_use]
pub fn project_shares(projects: &[ShareProject]) -> ShareModel {
    let mut with_time: Vec<&ShareProject> = projects.iter().filter(|p| p.secs > 0).collect();
    with_time.sort_by(|a, b| by_time((&a.id, &a.name, a.secs), (&b.id, &b.name, b.secs)));
    let drafts = with_time
        .into_iter()
        .map(|p| Draft {
            key: format!("project:{}", p.id),
            label: p.name.clone(),
            context: Vec::new(),
            path: vec![p.name.clone()],
            secs: p.secs,
            color: p.color.clone(),
            stripes: Vec::new(),
            kind: RowKind::Project { id: p.id.clone() },
        })
        .collect();
    finish(ShareLevel::Projects, drafts, 0, Vec::new())
}

/// `taskShares`: the tasks with time. Up to nine all show; with more, the eight with the most time show and the rest
/// become one "Other (N tasks)" row. Shown tasks are grouped by project, the projects in the order of their time (the
/// grouped tasks' time counted), each project's tasks by time, Other last. Each task has a shade of its project's
/// colour. A row names its project after it only when the tasks belong to several projects.
#[must_use]
pub fn task_shares(tasks: &[ShareTask], project_color: impl Fn(&str) -> String) -> ShareModel {
    let mut ranked: Vec<&ShareTask> = tasks.iter().filter(|t| t.secs > 0).collect();
    ranked.sort_by(|a, b| by_time((&a.id, &a.name, a.secs), (&b.id, &b.name, b.secs)));
    let (shown, rest) = cut(&ranked);
    let several_projects = distinct(ranked.iter().map(|t| t.project_id.as_str())) > 1;
    let mut drafts = Vec::with_capacity(shown.len() + 1);
    for project_id in project_order(ranked.iter().map(|t| (t.project_id.as_str(), t.secs))) {
        let mine: Vec<&ShareTask> = shown
            .iter()
            .copied()
            .filter(|t| t.project_id == project_id)
            .collect();
        let base = project_color(&project_id);
        let shades = task_shades(&base, mine.len());
        for (i, t) in mine.into_iter().enumerate() {
            drafts.push(Draft {
                key: format!("task:{}", t.id),
                label: t.name.clone(),
                context: if several_projects {
                    vec![t.project_name.clone()]
                } else {
                    Vec::new()
                },
                path: vec![t.project_name.clone(), t.name.clone()],
                secs: t.secs,
                color: shades.get(i).cloned().unwrap_or_else(|| base.clone()),
                stripes: Vec::new(),
                kind: RowKind::Task {
                    id: t.id.clone(),
                    project_id: project_id.clone(),
                },
            });
        }
    }
    if !rest.is_empty() {
        drafts.push(other_draft(
            rest.len(),
            rest.iter().map(|t| t.secs).sum(),
            OtherUnit::Tasks,
        ));
    }
    finish(ShareLevel::Tasks, drafts, 0, Vec::new())
}

/// The row's own name: the subtask's text, or the task's name and "(no subtask)" for time on none.
fn subtask_label(s: &ShareSubtask) -> String {
    match s.id {
        None => format!("{} {NO_SUBTASK_LABEL}", s.task_name),
        Some(_) => s.name.clone(),
    }
}

struct RankedSubtask<'a> {
    s: &'a ShareSubtask,
    label: String,
    /// The task's id and the subtask's, joined by a NUL as 1.x's key does.
    rank_id: String,
    index: usize,
}

/// `subtaskShares`: the time per subtask, and a task's time on no subtask as the row `<Task> (no subtask)`. Grouped
/// like the tasks: past nine rows the eight with the most time show and the rest are "Other (N subtasks)". Shown
/// rows are grouped by project (the projects by their time), then by task (the tasks by theirs), then by time, and
/// take shades of their project's colour. A row names its task when the data holds several tasks (a
/// "(no subtask)" row names it already), and its project when the data holds several projects.
#[must_use]
pub fn subtask_shares(
    subtasks: &[ShareSubtask],
    project_color: impl Fn(&str) -> String,
) -> ShareModel {
    let mut ranked: Vec<RankedSubtask> = subtasks
        .iter()
        .filter(|s| s.secs > 0)
        .map(|s| RankedSubtask {
            s,
            label: subtask_label(s),
            rank_id: format!("{}\u{0}{}", s.task_id, s.id.as_deref().unwrap_or_default()),
            index: 0,
        })
        .collect();
    // Of two rows with the same time the named subtask comes first, then the one for time on none: 1.x puts
    // U+10FFFF, which sorts after every letter, before the name of the second.
    ranked.sort_by(|a, b| {
        b.s.secs
            .cmp(&a.s.secs)
            .then_with(|| a.s.id.is_none().cmp(&b.s.id.is_none()))
            .then_with(|| locale_compare(&a.label, &b.label))
            .then_with(|| a.rank_id.cmp(&b.rank_id))
    });
    for (i, r) in ranked.iter_mut().enumerate() {
        r.index = i;
    }
    let (shown, rest) = cut(&ranked);
    let several_tasks = distinct(ranked.iter().map(|r| r.s.task_id.as_str())) > 1;
    let several_projects = distinct(ranked.iter().map(|r| r.s.project_id.as_str())) > 1;
    let mut task_secs: BTreeMap<&str, i64> = BTreeMap::new();
    for r in &ranked {
        *task_secs.entry(r.s.task_id.as_str()).or_default() += r.s.secs;
    }
    let secs_of = |task_id: &str| task_secs.get(task_id).copied().unwrap_or(0);
    let mut drafts = Vec::with_capacity(shown.len() + 1);
    for project_id in project_order(ranked.iter().map(|r| (r.s.project_id.as_str(), r.s.secs))) {
        // Within a project: the task with the most time first, and in a task the subtask with the most time first.
        let mut mine: Vec<&RankedSubtask> = shown
            .iter()
            .filter(|r| r.s.project_id == project_id)
            .collect();
        mine.sort_by(|a, b| {
            secs_of(&b.s.task_id)
                .cmp(&secs_of(&a.s.task_id))
                .then_with(|| locale_compare(&a.s.task_name, &b.s.task_name))
                .then_with(|| a.s.task_id.cmp(&b.s.task_id))
                .then_with(|| a.index.cmp(&b.index))
        });
        let base = project_color(&project_id);
        let shades = task_shades(&base, mine.len());
        for (i, r) in mine.into_iter().enumerate() {
            let s = r.s;
            let mut context = Vec::new();
            if s.id.is_some() && several_tasks {
                context.push(s.task_name.clone());
            }
            if several_projects {
                context.push(s.project_name.clone());
            }
            let last = match s.id {
                None => NO_SUBTASK_LABEL.to_owned(),
                Some(_) => s.name.clone(),
            };
            drafts.push(Draft {
                key: format!("subtask:{}", r.rank_id),
                label: r.label.clone(),
                context,
                path: vec![s.project_name.clone(), s.task_name.clone(), last],
                secs: s.secs,
                color: shades.get(i).cloned().unwrap_or_else(|| base.clone()),
                stripes: Vec::new(),
                kind: RowKind::Subtask {
                    id: s.id.clone(),
                    task_id: s.task_id.clone(),
                    project_id: project_id.clone(),
                },
            });
        }
    }
    if !rest.is_empty() {
        drafts.push(other_draft(
            rest.len(),
            rest.iter().map(|r| r.s.secs).sum(),
            OtherUnit::Subtasks,
        ));
    }
    finish(ShareLevel::Subtasks, drafts, 0, Vec::new())
}

// ------------------------------------------------------------------ tag combinations

/// `TagRowKey`: the row a task's tags put its time in: one row per distinct set of tags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagRowKey {
    pub key: String,
    /// The tags' names joined by " + ", or "No tag".
    pub label: String,
    /// The task's tags that exist, in the order of the Settings list.
    pub tag_ids: Vec<String>,
    /// One colour per tag, or the grey of "No tag".
    pub colors: Vec<String>,
}

/// `tagKeyOf`: the row of a task: the exact set of tags it carries, named in the order of `tags` (the Settings
/// list), so "Design + Client" is that row whichever order the task was tagged in. One tag is that tag's row; a task
/// with none is "No tag". A tag that no longer exists is not counted.
#[must_use]
pub fn tag_key_of<S: AsRef<str>>(task_tag_ids: &[S], tags: &[ShareTag]) -> TagRowKey {
    let mine: Vec<&ShareTag> = tags
        .iter()
        .filter(|t| task_tag_ids.iter().any(|id| id.as_ref() == t.id))
        .collect();
    if mine.is_empty() {
        return TagRowKey {
            key: "tags:none".to_owned(),
            label: NO_TAG_LABEL.to_owned(),
            tag_ids: Vec::new(),
            colors: vec![UNTAGGED_COLOR.to_owned()],
        };
    }
    let tag_ids: Vec<String> = mine.iter().map(|t| t.id.clone()).collect();
    TagRowKey {
        key: format!("tags:{}", tag_ids.join(",")),
        label: mine
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>()
            .join(TAG_SEPARATOR),
        tag_ids,
        colors: mine.iter().map(|t| t.color.clone()).collect(),
    }
}

/// `TagWeight`: a share of a task's time that goes to one row.
#[derive(Clone, Debug, PartialEq)]
pub struct TagWeight {
    pub row: TagRowKey,
    /// The fraction of the task's time this row gets; a task's weights add up to 1.
    pub weight: f64,
}

/// `tagWeightsOf`: THE counting rule of the Tags level, in one place (owner decision "B"): a task's time counts
/// once, under the combination of tags it carries, so the rows add up to the total in the middle and to 100%.
#[must_use]
pub fn tag_weights_of<S: AsRef<str>>(task_tag_ids: &[S], tags: &[ShareTag]) -> Vec<TagWeight> {
    vec![TagWeight {
        row: tag_key_of(task_tag_ids, tags),
        weight: 1.0,
    }]
}

/// A task's seconds times a weight, in whole seconds (exact for the weight 1 of the current rule).
fn weighted(secs: i64, weight: f64) -> i64 {
    js::round(secs as f64 * weight) as i64
}

struct Line {
    row: TagRowKey,
    task_ids: Vec<String>,
    secs: i64,
    exact: i64,
    billable_secs: i64,
}

/// `tagShares`: the time per combination of tags. Rows are by time, most first; past nine rows the eight with the
/// most time show and the rest are "Other (N combinations)". A row with one tag has that tag's colour, "No tag" the
/// grey, and a combination the stripes of its tags' colours, its first tag's colour standing for it elsewhere.
#[must_use]
pub fn tag_shares(tasks: &[ShareTagTask], tags: &[ShareTag]) -> ShareModel {
    let mut lines: Vec<Line> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for t in tasks.iter().filter(|t| t.secs > 0) {
        for TagWeight { row, weight } in tag_weights_of(&t.tag_ids, tags) {
            let i = *index.entry(row.key.clone()).or_insert_with(|| {
                lines.push(Line {
                    row,
                    task_ids: Vec::new(),
                    secs: 0,
                    exact: 0,
                    billable_secs: 0,
                });
                lines.len() - 1
            });
            if let Some(line) = lines.get_mut(i) {
                line.task_ids.push(t.id.clone());
                line.secs += weighted(t.secs, weight);
                line.exact += weighted(t.exact, weight);
                line.billable_secs += weighted(t.billable_secs, weight);
            }
        }
    }
    let mut ranked = lines;
    ranked.sort_by(|a, b| {
        by_time(
            (&a.row.key, &a.row.label, a.secs),
            (&b.row.key, &b.row.label, b.secs),
        )
    });
    let (shown, rest) = cut(&ranked);
    let mut drafts: Vec<Draft> = shown
        .iter()
        .map(|l| Draft {
            key: l.row.key.clone(),
            label: l.row.label.clone(),
            context: Vec::new(),
            path: vec![l.row.label.clone()],
            secs: l.secs,
            color: l
                .row
                .colors
                .first()
                .cloned()
                .unwrap_or_else(|| UNTAGGED_COLOR.to_owned()),
            stripes: if l.row.colors.len() > 1 {
                l.row.colors.clone()
            } else {
                Vec::new()
            },
            kind: RowKind::Tag {
                tag_ids: l.row.tag_ids.clone(),
                task_ids: l.task_ids.clone(),
            },
        })
        .collect();
    if !rest.is_empty() {
        drafts.push(other_draft(
            rest.len(),
            rest.iter().map(|l| l.secs).sum(),
            OtherUnit::Combinations,
        ));
    }
    let combinations = ranked.iter().filter(|l| l.row.tag_ids.len() > 1).count();
    let every_line = ranked
        .into_iter()
        .map(|l| TagLine {
            key: l.row.key,
            label: l.row.label,
            tag_ids: l.row.tag_ids,
            task_ids: l.task_ids,
            secs: l.secs,
            exact: l.exact,
            billable_secs: l.billable_secs,
        })
        .collect();
    finish(ShareLevel::Tags, drafts, combinations, every_line)
}
