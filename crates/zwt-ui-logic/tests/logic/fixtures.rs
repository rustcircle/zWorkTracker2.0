//! In-memory stores for the tests, in the spirit of 1.x's `mkSnapshot`: every task is "Task <id>" unless named.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use z_work_core::Ts;
use z_work_core::model::{
    BillablePause, GapDecision, GapKind, IdleGap, LocalFile, Project, RemoteDevice, RoundMethod,
    RoundingVersion, Segment, Shared, Stamp, Tag, Task, TaskState,
};
use zwt_ui_logic::share::{ShareLevel, ShareModel, ShareRow, ShareTask, ShareView, share_of_time};

/// This device.
pub const HERE: &str = "dev";

pub fn ts(iso: &str) -> Ts {
    Ts::parse(iso).unwrap_or_else(|| panic!("not an ISO time: {iso}"))
}

pub fn at(iso: &str) -> i64 {
    ts(iso).secs()
}

/// A TipTap document with one task list of `items` (subtask id, text).
pub fn task_list(items: &[(&str, &str)]) -> Value {
    let items: Vec<Value> = items
        .iter()
        .map(|(id, text)| {
            json!({
                "type": "taskItem",
                "attrs": { "checked": false, "subtaskId": id },
                "content": [{ "type": "paragraph", "content": [{ "type": "text", "text": text }] }]
            })
        })
        .collect();
    json!({ "type": "doc", "content": [{ "type": "taskList", "content": items }] })
}

#[derive(Clone, Debug)]
pub struct Store {
    pub file: LocalFile,
    next: u32,
}

impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}

impl Store {
    pub fn new() -> Self {
        Self {
            file: LocalFile {
                version: z_work_core::model::SCHEMA_VERSION,
                device_id: HERE.to_owned(),
                device_name: "Test PC".to_owned(),
                last_heartbeat: None,
                prefs: Default::default(),
                remote_devices: BTreeMap::new(),
                newest_stamp: None,
                latest_change_here: None,
                data: Shared::default(),
            },
            next: 0,
        }
    }

    fn next_id(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}{:03}", self.next)
    }

    pub fn project(mut self, id: &str, name: &str, color: &str) -> Self {
        self.file.data.projects.insert(
            id.to_owned(),
            Project {
                id: id.to_owned(),
                name: name.to_owned(),
                color: color.to_owned(),
                archived: false,
                deleted: false,
                billable: true,
                budget_hours: None,
                client_zone: None,
                billing_type: Default::default(),
                fixed_fee_cents: None,
                fee_currency: None,
                created_at: ts("2026-01-01T00:00:00Z"),
                stamp: Stamp::default(),
                identity_stamp: None,
                billing_stamp: None,
                budget_stamp: None,
                archive_stamp: None,
            },
        );
        self
    }

    /// Alpha, Beta and Gamma of 1.x's Share of time tests.
    pub fn abc(self) -> Self {
        self.project("p-a", "Alpha", "#2563eb")
            .project("p-b", "Beta", "#dc2626")
            .project("p-c", "Gamma", "#059669")
    }

    pub fn tag(mut self, id: &str, name: &str, color: &str) -> Self {
        self.file.data.tags.insert(
            id.to_owned(),
            Tag {
                id: id.to_owned(),
                name: name.to_owned(),
                color: color.to_owned(),
                deleted: false,
                stamp: Stamp::default(),
            },
        );
        self
    }

    /// A stopped task called "Task <id>".
    pub fn task(self, id: &str, project_id: &str) -> Self {
        let name = format!("Task {id}");
        self.named(id, project_id, &name)
    }

    pub fn named(mut self, id: &str, project_id: &str, name: &str) -> Self {
        self.file.data.tasks.insert(
            id.to_owned(),
            Task {
                id: id.to_owned(),
                name: name.to_owned(),
                name_stamp: Stamp::default(),
                project_id: project_id.to_owned(),
                project_stamp: Stamp::default(),
                description: Value::Null,
                description_stamp: Stamp::default(),
                deleted: false,
                deleted_stamp: Stamp::default(),
                discarded: None,
                continued_from: None,
                created_at: ts("2026-01-01T00:00:00Z"),
                state: TaskState::Stopped,
                pause_reason: None,
                state_stamp: Stamp::default(),
                stopped_at: None,
                tags: Vec::new(),
                tags_stamp: Stamp::default(),
                active_subtask: None,
                active_subtask_stamp: Stamp::default(),
            },
        );
        self
    }

    fn edit_task(mut self, id: &str, f: impl FnOnce(&mut Task)) -> Self {
        let task = self
            .file
            .data
            .tasks
            .get_mut(id)
            .unwrap_or_else(|| panic!("no task {id}"));
        f(task);
        self
    }

    pub fn tags(self, id: &str, tag_ids: &[&str]) -> Self {
        self.edit_task(id, |t| {
            t.tags = tag_ids.iter().map(|s| (*s).to_owned()).collect()
        })
    }

    pub fn subtasks(self, id: &str, items: &[(&str, &str)]) -> Self {
        let doc = task_list(items);
        self.edit_task(id, |t| t.description = doc)
    }

    pub fn state(self, id: &str, state: TaskState) -> Self {
        self.edit_task(id, |t| t.state = state)
    }

    pub fn discard(self, id: &str) -> Self {
        self.edit_task(id, |t| t.discarded = Some(Stamp::default()))
    }

    fn segment(
        mut self,
        task: &str,
        start: &str,
        end: Option<&str>,
        subtask: Option<&str>,
        device: &str,
    ) -> Self {
        let id = self.next_id("s");
        self.file.data.segments.insert(
            id.clone(),
            Segment {
                id,
                task_id: task.to_owned(),
                device_id: device.to_owned(),
                start: ts(start),
                end: end.map(ts),
                subtask_id: subtask.map(str::to_owned),
                ..Segment::default()
            },
        );
        self
    }

    /// A closed segment of this device.
    pub fn seg(self, task: &str, start: &str, end: &str) -> Self {
        self.segment(task, start, Some(end), None, HERE)
    }

    /// A closed segment of this device, logged on a subtask.
    pub fn seg_on(self, task: &str, start: &str, end: &str, subtask: &str) -> Self {
        self.segment(task, start, Some(end), Some(subtask), HERE)
    }

    /// A segment still running on this device.
    pub fn open(self, task: &str, start: &str) -> Self {
        self.segment(task, start, None, None, HERE)
            .state(task, TaskState::Running)
    }

    /// A segment still running on another device, last heard of at `heartbeat`.
    pub fn open_elsewhere(
        mut self,
        task: &str,
        start: &str,
        device: &str,
        heartbeat: &str,
    ) -> Self {
        self.file.remote_devices.insert(
            device.to_owned(),
            RemoteDevice {
                name: "Other PC".to_owned(),
                last_heartbeat: Some(ts(heartbeat)),
                written_at: ts(heartbeat),
            },
        );
        self.segment(task, start, None, None, device)
            .state(task, TaskState::Running)
    }

    /// A billable pause: counts in Billable only.
    pub fn pause(mut self, task: &str, start: &str, end: &str) -> Self {
        let id = self.next_id("bp");
        self.file.data.pauses.insert(
            id.clone(),
            BillablePause {
                id,
                task_id: task.to_owned(),
                start: ts(start),
                end: ts(end),
                note: String::new(),
                deleted: false,
                stamp: Stamp::default(),
            },
        );
        self
    }

    /// Time away inside the segment `segment_id` (see [`Self::last_segment`]), with the user's decision.
    pub fn away(mut self, segment_id: &str, start: &str, end: &str, decision: GapDecision) -> Self {
        let task_id = self.file.data.segments[segment_id].task_id.clone();
        let id = self.next_id("g");
        self.file.data.gaps.insert(
            id.clone(),
            IdleGap {
                id,
                segment_id: segment_id.to_owned(),
                task_id,
                device_id: HERE.to_owned(),
                start: ts(start),
                end: ts(end),
                kind: GapKind::Idle,
                decision,
                decision_stamp: Stamp::default(),
            },
        );
        self
    }

    /// The id of the segment added last.
    pub fn last_segment(&self) -> String {
        self.file
            .data
            .segments
            .keys()
            .max()
            .cloned()
            .expect("a segment")
    }

    /// A rounding rule `id` of `minutes` (0: off) from `from` (`yyyy-mm-dd`), for every project or for `project`.
    pub fn rounding(mut self, id: &str, project: Option<&str>, from: &str, minutes: u32) -> Self {
        self.file.data.rounding.insert(
            id.to_owned(),
            RoundingVersion {
                id: id.to_owned(),
                project_id: project.map(str::to_owned),
                effective_from: from.to_owned(),
                increment_minutes: minutes,
                method: RoundMethod::Up,
                deleted: false,
                stamp: Stamp::default(),
            },
        );
        self
    }

    /// Only the time of `tasks`: what a filter on those tasks leaves of the store.
    pub fn only(mut self, tasks: &[&str]) -> Self {
        self.file
            .data
            .segments
            .retain(|_, s| tasks.contains(&s.task_id.as_str()));
        self.file
            .data
            .pauses
            .retain(|_, p| tasks.contains(&p.task_id.as_str()));
        self
    }
}

/// 1.x's `NOW` of the Share of time tests.
pub const NOW: &str = "2026-03-12T12:00:00Z";

/// The week of 10 March 2026 (UTC), Monday to Monday.
pub fn week() -> (i64, i64) {
    (at("2026-03-09T00:00:00Z"), at("2026-03-16T00:00:00Z"))
}

/// March 2026 (UTC).
pub fn month() -> (i64, i64) {
    (at("2026-03-01T00:00:00Z"), at("2026-04-01T00:00:00Z"))
}

/// The card for `store` over `range` at 1.x's `NOW`, with `picked`.
pub fn card(store: &Store, range: (i64, i64), picked: Option<ShareLevel>) -> ShareView {
    share_of_time(&store.file, range.0, range.1, at(NOW), picked)
}

/// What 1.x's tests read off a legend.
pub fn figures(rows: &[ShareRow]) -> Vec<String> {
    rows.iter().map(|r| r.text.clone()).collect()
}

pub fn labels(rows: &[ShareRow]) -> Vec<String> {
    rows.iter().map(|r| r.label.clone()).collect()
}

pub fn written(rows: &[ShareRow]) -> Vec<String> {
    rows.iter().map(ShareRow::written).collect()
}

pub fn titles(rows: &[ShareRow]) -> Vec<String> {
    rows.iter().map(|r| r.full_path.clone()).collect()
}

/// The label and the names after it, as `dotted` in 1.x's tests.
pub fn dotted(rows: &[ShareRow]) -> Vec<Vec<String>> {
    rows.iter()
        .map(|r| {
            std::iter::once(r.label.clone())
                .chain(r.context.iter().cloned())
                .collect()
        })
        .collect()
}

pub fn sum(xs: impl IntoIterator<Item = i64>) -> i64 {
    xs.into_iter().sum()
}

/// `expectConsistent` of `shareSlices.test.ts` and `shareRows.test.ts`: what every model must satisfy.
pub fn expect_consistent(model: &ShareModel) {
    assert_eq!(model.total_minutes, (model.total_secs + 30).div_euclid(60));
    assert_eq!(
        sum(model.rows.iter().map(|r| r.minutes)),
        model.total_minutes
    );
    if !model.rows.is_empty() {
        assert_eq!(sum(model.rows.iter().map(|r| r.whole_percent)), 100);
    }
    for r in &model.rows {
        assert!(r.secs > 0);
        assert_eq!(r.text, format!("{} {}", r.hm, r.pct()));
        assert_eq!(r.hm, zwt_ui_logic::format::hours_minutes(r.minutes));
        assert_eq!(r.full_path, r.path.join(" > "));
        assert!(!r.path.is_empty());
        // A row is never a whole unit away from its exact share of the total it is rounded to.
        let share = r.secs as f64 / model.total_secs as f64;
        assert!((r.minutes as f64 - share * model.total_minutes as f64).abs() < 1.0);
        assert!((r.whole_percent as f64 - share * 100.0).abs() < 1.0);
    }
    let keys: std::collections::BTreeSet<&str> =
        model.rows.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(keys.len(), model.rows.len(), "keys are distinct");
    // The middle of the donut writes the same total the rows add up to.
    if model.total_minutes >= 60 {
        assert_eq!(
            zwt_ui_logic::format::fmt_hm(model.total_secs),
            zwt_ui_logic::format::hours_minutes(model.total_minutes)
        );
    }
}

/// `lcg` of the TypeScript tests: a repeatable stream of numbers in [0, 1).
pub fn lcg(seed: u64) -> impl FnMut() -> f64 {
    let mut s = seed;
    move || {
        s = (s * 1_664_525 + 1_013_904_223) % 4_294_967_296;
        s as f64 / 4_294_967_296.0
    }
}

/// `Math.floor(rnd() * n)`.
pub fn below(rnd: &mut impl FnMut() -> f64, n: i64) -> i64 {
    (rnd() * n as f64).floor() as i64
}

/// The colours of the projects A, B and C of the share tests, and the unknown grey for any other.
pub fn color_of(project_id: &str) -> String {
    match project_id {
        "A" => "#2563eb",
        "B" => "#dc2626",
        "C" => "#059669",
        _ => "#6b7280",
    }
    .to_owned()
}

/// Task `id` of project `project_id` with `secs`, named "Task <id>" of "Project <project_id>".
pub fn t(id: &str, project_id: &str, secs: i64) -> ShareTask {
    ShareTask {
        id: id.to_owned(),
        name: format!("Task {id}"),
        project_id: project_id.to_owned(),
        project_name: format!("Project {project_id}"),
        secs,
    }
}
