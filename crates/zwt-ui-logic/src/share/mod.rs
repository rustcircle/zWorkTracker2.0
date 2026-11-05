//! The Share of time donut: which level it shows, and its rows with their names, colours, minutes and percentages.
//!
//! Ports of `features/analysis/shareLevels.ts` ([`ShareLevel`], [`reduce_level`], [`shown_level`]),
//! `features/analysis/shareSlices.ts` (rows and shades) and the card in `features/analysis/SectionsA.tsx`
//! ([`share_of_time`]). PARITY rows 208-219.
//!
//! Basis: Exact (worked time). 1.x's card follows the Analysis tab's Time basis, which starts on Billable; Billable is
//! rounded per task and day in a zone, which the M1 call does not take, and the basis switch is milestone 5.

mod input;
mod levels;
mod shades;
mod slices;

pub use input::{ShareInput, UNKNOWN_PROJECT, UNTITLED_SUBTASK};
pub use levels::{
    Availability, LevelEvent, LevelState, ShareLevel, availability_of, default_level, reduce_level,
    shown_level, start_level,
};
pub use shades::task_shades;
pub use slices::{
    COMBINATION_NOTE, CONTEXT_SEPARATOR, NO_SUBTASK_LABEL, NO_TAG_LABEL, OTHER_COLOR, OtherUnit,
    PATH_SEPARATOR, RowKind, SHOW_ALL_UP_TO, ShareModel, ShareProject, ShareRow, ShareSubtask,
    ShareTag, ShareTagTask, ShareTask, TAG_SEPARATOR, TOP_ROWS, TagLine, TagRowKey, TagWeight,
    UNTAGGED_COLOR, apportion, other_label, other_note, other_tip, project_shares, subtask_shares,
    tag_key_of, tag_shares, tag_weights_of, task_shares,
};

use z_work_core::model::LocalFile;

use crate::format::hours_minutes;

/// What the Share of time card draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareView {
    /// The level the user picked, as given: it stays picked whatever is shown (PARITY 212, 213).
    pub picked: Option<ShareLevel>,
    /// The level drawn: the pick, or while it has nothing to show the default, or the nearest level above it.
    pub shown: ShareLevel,
    /// False while no time in the period is on a subtask: the Subtasks segment is then off, with a tooltip that says
    /// so.
    pub subtasks_available: bool,
    /// Seconds of all the rows.
    pub total_secs: i64,
    /// The middle of the donut: "16h 50m".
    pub total_text: String,
    /// The rows, in the order of the legend and of the slices. Empty when nothing is logged in the period.
    pub rows: Vec<ShareRow>,
    /// The minutes of `total_text`, which the rows' minutes add up to.
    pub total_minutes: i64,
    /// Tags level: how many combinations of two tags or more have time.
    pub combinations: usize,
    /// The note under the donut when rows are grouped: "Other adds up the 4 tasks with the least time. ...".
    pub note: Option<String>,
    /// The footnote under the legend of the Tags level when a task has several tags ([`COMBINATION_NOTE`]).
    pub footnote: Option<String>,
}

impl ShareView {
    /// The card for one level's rows.
    #[must_use]
    pub fn of_model(
        picked: Option<ShareLevel>,
        subtasks_available: bool,
        model: ShareModel,
    ) -> Self {
        let note = model.rows.iter().find_map(|r| match r.kind {
            RowKind::Other { grouped, unit } => Some(other_note(grouped, unit)),
            _ => None,
        });
        Self {
            picked,
            shown: model.level,
            subtasks_available,
            total_secs: model.total_secs,
            total_text: hours_minutes(model.total_minutes),
            total_minutes: model.total_minutes,
            combinations: model.combinations,
            footnote: (model.combinations > 0).then(|| COMBINATION_NOTE.to_owned()),
            note,
            rows: model.rows,
        }
    }
}

/// The Share of time card for the half-open period `[from, to)` (unix seconds) of `file`, at `now`.
///
/// An open segment of this device counts up to `now`. `picked` is the level the user chose, `None` until they choose
/// one: then the card shows the default level of the filter, Projects in M1, which has no filter. A picked level is
/// shown while it has rows; Subtasks without time on a subtask shows the default (or the nearest level above it) and
/// `picked` stays Subtasks, so it comes back when subtask time does.
#[must_use]
pub fn share_of_time(
    file: &LocalFile,
    from: i64,
    to: i64,
    now: i64,
    picked: Option<ShareLevel>,
) -> ShareView {
    let input = ShareInput::from_file(file, from, to, now);
    let auto = default_level(0, 0);
    let available = input.availability();
    let shown = shown_level(picked.unwrap_or(auto), auto, available);
    ShareView::of_model(picked, available.subtasks, input.model(shown))
}
