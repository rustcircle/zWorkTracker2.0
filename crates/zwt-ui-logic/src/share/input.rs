//! What the Share of time levels read from a store for one period: the worked time per project, task and subtask.
//!
//! Follows how 1.x's Analysis tab feeds the card (`ShareOfTime` and `shareModelOf` in `SectionsA.tsx`, `projectsOf`
//! and `tasksOf` in `fam-projects.ts`, `subtasksOf` in `fam-subtasks.ts`, `stretchesOf` and `buildSpans` in
//! `engine.ts`) with no filter and the Exact basis.

use std::collections::BTreeMap;

use z_work_core::Ts;
use z_work_core::model::LocalFile;
use z_work_core::pieces::PieceKind;
use z_work_core::subtasks::{self, Item};

use super::levels::{Availability, ShareLevel, availability_of};
use super::slices::{
    ShareModel, ShareProject, ShareSubtask, ShareTag, ShareTagTask, ShareTask, UNTAGGED_COLOR,
    project_shares, subtask_shares, tag_shares, task_shares,
};
use crate::counted::pieces_at;

/// What the legend writes for a subtask with no text (`UNTITLED_SUBTASK` of `fam-subtasks.ts`).
pub const UNTITLED_SUBTASK: &str = "Untitled subtask";
/// The name 1.x writes for a project it cannot find.
pub const UNKNOWN_PROJECT: &str = "Unknown project";

/// The inputs of the four levels for one period, in the Exact basis: worked time (work and thinking, not billable
/// pauses) of the live tasks, clipped to the period.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareInput {
    /// Projects with time. A task whose project is missing from the store counts at the other levels only, as in 1.x.
    pub projects: Vec<ShareProject>,
    /// Tasks with time.
    pub tasks: Vec<ShareTask>,
    /// Per task with time: one row per subtask with time, and one (`id: None`) for its time on no subtask.
    pub subtasks: Vec<ShareSubtask>,
    /// Tasks with time and their tags.
    pub tag_tasks: Vec<ShareTagTask>,
    /// The tags that exist, in the order of the Settings list (by id, as the core lists them).
    pub tags: Vec<ShareTag>,
    /// Every project's colour, by id.
    pub project_colors: BTreeMap<String, String>,
}

impl ShareInput {
    /// Reads `file` for the half-open period `[from, to)` (unix seconds). An open segment of this device counts up to
    /// `now`, another device's up to its last heartbeat.
    ///
    /// A stretch logged on a subtask counts for that subtask only while the subtask is still in its task's list;
    /// otherwise it is time on no subtask, as on the task card.
    #[must_use]
    pub fn from_file(file: &LocalFile, from: i64, to: i64, now: i64) -> Self {
        let data = &file.data;
        let mut exact: BTreeMap<&str, i64> = BTreeMap::new();
        let mut on_item: BTreeMap<&str, BTreeMap<String, i64>> = BTreeMap::new();
        let mut items: BTreeMap<&str, Vec<Item>> = BTreeMap::new();
        for p in pieces_at(file, Ts(now)) {
            if p.kind == PieceKind::BillablePause {
                continue;
            }
            let (a, b) = (p.start.secs().max(from), p.end.secs().min(to));
            if b <= a {
                continue;
            }
            let Some(task) = data.tasks.get(&p.task_id) else {
                continue;
            };
            let len = b - a;
            *exact.entry(task.id.as_str()).or_default() += len;
            if let Some(sub) = p.subtask_id.as_deref().filter(|s| !s.is_empty()) {
                let list = items
                    .entry(task.id.as_str())
                    .or_insert_with(|| subtasks::items(&task.description));
                if list.iter().any(|i| i.id == sub) {
                    *on_item
                        .entry(task.id.as_str())
                        .or_default()
                        .entry(sub.to_owned())
                        .or_default() += len;
                }
            }
        }

        let project_colors: BTreeMap<String, String> = data
            .projects
            .values()
            .map(|p| (p.id.clone(), p.color.clone()))
            .collect();
        let project_name = |id: &str| {
            data.projects
                .get(id)
                .map_or_else(|| UNKNOWN_PROJECT.to_owned(), |p| p.name.clone())
        };

        let mut tasks = Vec::with_capacity(exact.len());
        let mut subtask_rows = Vec::new();
        let mut tag_tasks = Vec::with_capacity(exact.len());
        let mut by_project: BTreeMap<&str, i64> = BTreeMap::new();
        for (&task_id, &secs) in &exact {
            let Some(task) = data.tasks.get(task_id) else {
                continue;
            };
            let project_id = task.project_id.as_str();
            *by_project.entry(project_id).or_default() += secs;
            let project = project_name(project_id);
            let mut used = 0;
            let texts = items.get(task_id);
            for (id, &part) in on_item.get(task_id).into_iter().flatten() {
                used += part;
                let text = texts
                    .and_then(|list| list.iter().find(|i| &i.id == id))
                    .map_or("", |i| i.text.trim());
                subtask_rows.push(ShareSubtask {
                    task_id: task.id.clone(),
                    task_name: task.name.clone(),
                    project_id: project_id.to_owned(),
                    project_name: project.clone(),
                    id: Some(id.clone()),
                    name: if text.is_empty() {
                        UNTITLED_SUBTASK.to_owned()
                    } else {
                        text.to_owned()
                    },
                    secs: part,
                });
            }
            if secs - used > 0 {
                subtask_rows.push(ShareSubtask {
                    task_id: task.id.clone(),
                    task_name: task.name.clone(),
                    project_id: project_id.to_owned(),
                    project_name: project.clone(),
                    id: None,
                    name: String::new(),
                    secs: secs - used,
                });
            }
            tag_tasks.push(ShareTagTask {
                id: task.id.clone(),
                secs,
                exact: secs,
                // The Exact basis: Billable (rounded per task and day) is the Analysis tab's, milestone 5.
                billable_secs: secs,
                tag_ids: task.tags.clone(),
            });
            tasks.push(ShareTask {
                id: task.id.clone(),
                name: task.name.clone(),
                project_id: project_id.to_owned(),
                project_name: project,
                secs,
            });
        }
        let projects = by_project
            .into_iter()
            .filter_map(|(id, secs)| {
                data.projects.get(id).map(|p| ShareProject {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    color: p.color.clone(),
                    secs,
                })
            })
            .collect();
        let tags = data
            .tags
            .values()
            .filter(|t| !t.deleted)
            .map(|t| ShareTag {
                id: t.id.clone(),
                name: t.name.clone(),
                color: t.color.clone(),
            })
            .collect();
        Self {
            projects,
            tasks,
            subtasks: subtask_rows,
            tag_tasks,
            tags,
            project_colors,
        }
    }

    /// A project's colour, or 1.x's grey for one it cannot find (`pcolor`).
    #[must_use]
    pub fn project_color(&self, project_id: &str) -> String {
        self.project_colors
            .get(project_id)
            .cloned()
            .unwrap_or_else(|| UNTAGGED_COLOR.to_owned())
    }

    /// Which levels have something to show (`availabilityOf` of the subtask rows).
    #[must_use]
    pub fn availability(&self) -> Availability {
        availability_of(self.subtasks.iter().map(|s| (s.id.is_some(), s.secs)))
    }

    /// The rows of one level (`shareModelOf`).
    #[must_use]
    pub fn model(&self, level: ShareLevel) -> ShareModel {
        match level {
            ShareLevel::Projects => project_shares(&self.projects),
            ShareLevel::Tasks => task_shares(&self.tasks, |id| self.project_color(id)),
            ShareLevel::Subtasks => subtask_shares(&self.subtasks, |id| self.project_color(id)),
            ShareLevel::Tags => tag_shares(&self.tag_tasks, &self.tags),
        }
    }
}
