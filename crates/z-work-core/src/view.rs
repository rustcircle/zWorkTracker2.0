//! The read model the UI receives. Tokens of internal bookkeeping (stamps) stay out of it.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::detect::Detected;
use crate::lease::{HolderInfo, LeaseStateName};
use crate::machine::RequiredField;
use crate::model::{
    BillablePause, BillingSummary, BillingType, IdleGap, PauseReason, PendingCopy, RateVersion,
    RoundingVersion, ShiftVersion, SyncProvider, Tag, TaskState, Template, Theme,
};
use crate::pieces::PieceKind;
use crate::reminder::Reminder;
use crate::summary::ProjectFigures;
use crate::sync::TargetStatus;
use crate::time::Ts;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectView {
    pub id: String,
    pub name: String,
    pub color: String,
    pub archived: bool,
    pub billable: bool,
    pub budget_hours: Option<f64>,
    pub client_zone: Option<String>,
    pub created_at: Ts,
    /// It has tasks or billing summaries, so it can only be archived.
    pub has_tasks: bool,
    pub billing_type: BillingType,
    pub fixed_fee_cents: Option<i64>,
    pub fee_currency: Option<String>,
    pub figures: ProjectFigures,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskView {
    pub id: String,
    pub name: String,
    pub project_id: String,
    pub description: Value,
    pub state: TaskState,
    pub pause_reason: Option<PauseReason>,
    pub created_at: Ts,
    pub stopped_at: Option<Ts>,
    /// Name of the other device this task is running on, if any.
    pub running_elsewhere: Option<String>,
    pub continued_from: Option<String>,
    pub tags: Vec<String>,
    pub active_subtask: Option<String>,
    pub subtasks: Vec<SubtaskView>,
    /// Seconds on a live billing summary, and seconds not yet on one.
    pub billed_secs: i64,
    pub unbilled_secs: i64,
    pub thinking_secs: i64,
    pub pause_secs: i64,
    /// Away with no answer yet: counted nowhere until confirmed.
    pub unconfirmed_secs: i64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubtaskView {
    pub id: String,
    pub text: String,
    pub checked: bool,
    pub secs: i64,
}

/// One recorded segment with its correction, for the corrections screen.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawSegmentView {
    pub id: String,
    pub task_id: String,
    pub device_id: String,
    pub raw_start: Ts,
    pub raw_end: Option<Ts>,
    pub start: Ts,
    pub end: Option<Ts>,
    pub voided: bool,
    pub corrected: bool,
    pub manual: bool,
    pub reason: String,
    pub billed: bool,
}

/// This device's reference prefix, and any other device using the same one.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BillingView {
    pub prefix: String,
    pub clashes: Vec<String>,
    pub summaries: Vec<BillingSummary>,
}

/// What a warm start copies into a new task, a snapshot of the most recently worked task.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopySource {
    pub task_id: String,
    pub name: String,
    pub project_id: String,
    pub project_archived: bool,
    pub description: Value,
    pub last_worked_at: Option<Ts>,
    pub tags: Vec<String>,
}

/// Cold until the first task of this launch has started.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchView {
    pub warm: bool,
    pub cold_required: Vec<RequiredField>,
    pub copy_source: Option<CopySource>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentView {
    pub id: String,
    pub task_id: String,
    pub device_id: String,
    pub start: Ts,
    pub end: Option<Ts>,
    /// For a segment still open on another device: the last moment known about it.
    pub provisional_end: Option<Ts>,
    /// The recorded segment this piece came from, or the pause record.
    pub segment_id: String,
    pub kind: PieceKind,
    pub subtask_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Notice {
    pub id: String,
    pub level: &'static str,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetView {
    pub provider: SyncProvider,
    pub root: String,
    pub folder: String,
    pub status: TargetStatus,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncView {
    pub targets: Vec<TargetView>,
    pub detected: Vec<Detected>,
    pub prompt_pending: bool,
    pub pending_copy: Option<PendingCopy>,
    /// A folder waiting for the user to choose between its restored data and this device's.
    pub join_pending: Option<JoinPendingView>,
}

/// The restore a folder has in its past. What the user chooses over.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinPendingView {
    pub root: String,
    /// The version file's name.
    pub taken_from: String,
    /// When that version was taken.
    pub at: Ts,
    /// The device that restored it.
    pub device_name: String,
}

/// Another device holding the lease, or the one last known to.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseHolderView {
    pub device_id: String,
    pub device_name: String,
    pub generation: u64,
    pub beat_seq: u64,
    pub last_sign_at: Option<Ts>,
    pub age_secs: Option<i64>,
    pub clock_suspect: bool,
}

impl LeaseHolderView {
    /// How long ago it was last heard of, on this computer's clock.
    #[must_use]
    pub fn of(holder: &HolderInfo, now: Ts) -> Self {
        Self {
            device_id: holder.device_id.clone(),
            device_name: holder.device_name.clone(),
            generation: holder.generation,
            beat_seq: holder.beat_seq,
            last_sign_at: Some(holder.last_sign_at),
            age_secs: Some((now.secs() - holder.last_sign_at.secs()).max(0)),
            clock_suspect: holder.clock_suspect,
        }
    }
}

/// The single-device lease, as far as the page goes.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseView {
    pub state: LeaseStateName,
    pub read_only: bool,
    pub holder: Option<LeaseHolderView>,
    pub generation: Option<u64>,
    pub note: Option<String>,
    pub reachable_roots: Vec<String>,
    pub unreachable_roots: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformView {
    /// Running from an MSIX package, whose files Windows removes with the app.
    pub packaged: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrefsView {
    pub theme: Theme,
    pub hide_earnings: bool,
    pub last_currency: Option<String>,
    pub client_zone_prompt_done: bool,
}

/// The first-run setup.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupView {
    pub pending: bool,
    pub step: u32,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// The service's count of committed changes when this was built. Replies can arrive out of order,
    /// and one with a lower count than another is older.
    pub revision: u64,
    pub now: Ts,
    pub device_id: String,
    pub device_name: String,
    pub system_zone: String,
    pub tzdb_version: &'static str,
    pub projects: Vec<ProjectView>,
    pub tasks: Vec<TaskView>,
    pub segments: Vec<SegmentView>,
    pub shifts: Vec<ShiftVersion>,
    pub rates: Vec<RateVersion>,
    pub rounding: Vec<RoundingVersion>,
    pub settings: BTreeMap<String, Value>,
    pub prefs: PrefsView,
    pub notices: Vec<Notice>,
    /// Always shown while they apply, such as a task running on two devices at once.
    pub warnings: Vec<String>,
    /// Shown while this computer's clock is behind the newest change it has seen. See `ClockBehindView`.
    pub clock_behind: Option<ClockBehindView>,
    pub sync: SyncView,
    pub launch: LaunchView,
    pub raw_segments: Vec<RawSegmentView>,
    pub gaps: Vec<IdleGap>,
    pub pauses: Vec<BillablePause>,
    pub tags: Vec<Tag>,
    pub templates: Vec<Template>,
    pub billing: BillingView,
    pub setup: SetupView,
    pub pause_requests: Vec<crate::model::PauseRequest>,
    pub reminder: Option<Reminder>,
    pub reminder_settings: ReminderSettings,
    pub backup_keep: u32,
    pub has_pdf_logo: bool,
    pub lease: LeaseView,
    pub platform: PlatformView,
}

/// This computer's clock is behind another computer's, or behind a change saved here, by `hlc::CLOCK_BEHIND_WARN_SECS`
/// or more (`Service::clock_behind`). `src/lib/types.ts` has the same shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClockBehindView {
    /// Seconds from `Snapshot.now` to `newest_at`: at least 120.
    pub behind_secs: i64,
    /// The other computer's clock when it last wrote its file, or this one's when it last saved a change.
    pub newest_at: Ts,
    /// The other computer whose clock is ahead of this one, by name. None when it was a change saved here, so this clock
    /// seems to have been set back. Always sent, as null when there is none.
    pub cause_device: Option<String>,
    /// Changes made here are dated a day or more past this clock (`hlc::HLC_MAX_LEAD_SECS`), the furthest they may go,
    /// to come after what this computer has seen, so they can't be dated after it until the clock is fixed, and may
    /// lose to it.
    pub capped: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReminderSettings {
    pub idle_minutes: i64,
    pub long_run_minutes: i64,
    pub remind_past_shift: bool,
}
