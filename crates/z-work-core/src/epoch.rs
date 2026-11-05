//! The fence that keeps a restored version from being undone by the next merge.
//!
//! Nothing a merge does can roll data back: a tombstone, a stopped task or a voided summary outranks any stamp. So a
//! restore raises a key that each device's file carries. A file from below this device's key never reaches the merge,
//! and one from above replaces its data whole, with copies kept. A folder this device doesn't follow yet is different,
//! as its restore may be news to data held only here: only a store with nothing in it follows that by itself.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};

use crate::backup;
use crate::error::CoreError;
use crate::lease::CLOCK_AHEAD_SECS;
use crate::machine::Ctx;
use crate::merge::merge;
use crate::model::{
    DeviceFile, EpochInfo, JoinPending, LocalFile, PauseReason, RemoteDevice, Segment, Shared,
    SyncTarget,
};
use crate::service::{RestoreNote, Service};
use crate::sync::{SyncIo, TargetRead, bounded, read_target};
use crate::time::Ts;
use crate::validate::MAX_EPOCH;

const WAITING: &str = "This folder holds data restored from a saved version, and this computer has data from before that. Its data is not read or written until you choose what to do in Settings, under Sync between devices.";
const ASK_BEFORE_MOVE: &str = "A folder in this move holds data restored from a saved version, and this computer has data from before that, so nothing was copied. Choose what to do in Settings, under Sync between devices, then try again.";
const SYNC_BEFORE_MOVE: &str = "A restored version has just reached this computer, so nothing was copied. Sync now, then try again.";
const NOT_WAITING: &str = "that folder is not waiting for a choice";
const FOLDER_CHANGED: &str =
    "That folder has changed since this was asked, so nothing was changed. Try again in a moment.";
const NO_HIGHER_RESTORE: &str = "A restored version in your sync folders has the highest number a restore can have, so no restore can go above it. Nothing was changed.";

fn key_of(file: &DeviceFile) -> (u64, &str) {
    EpochInfo::key(file.epoch_info.as_ref())
}

/// The number of a restore made above `top`, so long as a reader takes it. Above that, every other device would ignore
/// this one's file, and it would ignore theirs. Only a forged file gets there.
pub(crate) fn epoch_above(top: u64) -> Result<u64, CoreError> {
    top.checked_add(1)
        .filter(|epoch| *epoch <= MAX_EPOCH)
        .ok_or_else(|| CoreError::Conflict(NO_HIGHER_RESTORE.to_owned()))
}

fn files_of(read: &TargetRead) -> impl Iterator<Item = &DeviceFile> {
    read.own.iter().chain(&read.files)
}

/// The highest restore a folder's files follow.
fn highest(read: &TargetRead) -> Option<&EpochInfo> {
    files_of(read)
        .filter_map(|f| f.epoch_info.as_ref())
        .max_by(|a, b| EpochInfo::key(Some(a)).cmp(&EpochInfo::key(Some(b))))
}

/// What one set of reads means for this device's key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EpochPlan {
    /// A folder this device follows holds this restore, above its own: its data is replaced by it.
    pub adopt: Option<EpochInfo>,
    /// Folders not followed yet with a restore above even that, highest first. A store with nothing in it follows the
    /// first by itself. Any other leaves them all alone until the user has chosen.
    pub higher: Vec<(String, EpochInfo)>,
    /// Folders not followed yet that hold files and no such restore. What they hold at this device's key merges, and
    /// they are followed from now on. One that holds nothing stays as it is, as the files may just not be here yet, and
    /// what arrives could then replace this device's data with no choice given.
    pub joining: Vec<String>,
}

/// Decides from what was read, and from nothing else. The order of `reads` matters only among folders holding the same
/// restore, so which of them a question names is stable.
#[must_use]
pub fn plan(
    local: Option<&EpochInfo>,
    joined: &[String],
    reads: &[(String, &TargetRead)],
) -> EpochPlan {
    let mut top = local;
    for (_, read) in reads.iter().filter(|(root, _)| joined.contains(root)) {
        let found = highest(read);
        if EpochInfo::key(found) > EpochInfo::key(top) {
            top = found;
        }
    }
    let mut out = EpochPlan {
        adopt: top
            .filter(|_| EpochInfo::key(top) > EpochInfo::key(local))
            .cloned(),
        ..EpochPlan::default()
    };
    for (root, read) in reads.iter().filter(|(root, _)| !joined.contains(root)) {
        match highest(read) {
            Some(info) if EpochInfo::key(Some(info)) > EpochInfo::key(top) => {
                out.higher.push((root.clone(), info.clone()));
            }
            _ if files_of(read).next().is_some() => out.joining.push(root.clone()),
            _ => {}
        }
    }
    out.higher
        .sort_by(|(_, a), (_, b)| EpochInfo::key(Some(b)).cmp(&EpochInfo::key(Some(a))));
    out
}

/// Takes out what comes from before the restore this device follows, and says so of every other device's file. Returns
/// how many of those there were. This device's own file is left without a word, as the next write replaces it.
pub fn fence(read: &mut TargetRead, local: Option<&EpochInfo>) -> usize {
    let bar = EpochInfo::key(local);
    if read.own.as_ref().is_some_and(|f| key_of(f) < bar) {
        read.own = None;
    }
    let before = read.files.len();
    let warnings = &mut read.warnings;
    read.files.retain(|f| {
        let keep = key_of(f) >= bar;
        if !keep {
            warnings.push(format!(
                "Skipped {}: it holds data from before a restored version. Update and sync that device.",
                f.device_name
            ));
        }
        keep
    });
    before - read.files.len()
}

/// Whether a file follows the same restore as this device.
pub(crate) fn at_key(file: &DeviceFile, local: Option<&EpochInfo>) -> bool {
    key_of(file) == EpochInfo::key(local)
}

/// Leaves only what follows the same restore as this device.
pub(crate) fn only_at_key(read: &mut TargetRead, local: Option<&EpochInfo>) {
    read.own = read.own.take().filter(|f| at_key(f, local));
    read.files.retain(|f| at_key(f, local));
}

fn holds_above(read: &TargetRead, local: Option<&EpochInfo>) -> bool {
    files_of(read).any(|f| key_of(f) > EpochInfo::key(local))
}

/// All the files read at one restore, which together are what it replaces data with.
fn merged_at<'a>(key: (u64, &str), files: impl Iterator<Item = &'a DeviceFile>) -> Option<Shared> {
    files
        .filter(|f| key_of(f) == key)
        .fold(None, |out: Option<Shared>, f| {
            Some(out.map_or_else(|| f.data.clone(), |data| merge(&data, &f.data)))
        })
}

/// Nothing a person made, so nothing to lose or to bring past a restore. Tombstones count: they would.
fn holds_data(data: &Shared) -> bool {
    !(data.projects.is_empty() && data.tasks.is_empty() && data.segments.is_empty())
}

/// The part of a segment after the restore, going by the times that count. `None` when no time is left there.
fn cut_at(segment: &Segment, since: Ts) -> Option<Segment> {
    let mut cut = segment.clone();
    cut.start = cut.start.max(since);
    if let Some(edit) = &mut cut.edit {
        if edit.voided || edit.end <= since {
            return None;
        }
        edit.start = edit.start.max(since);
    }
    Some(cut)
}

/// What this device timed since the restore was made, which the restored data can't hold, goes into it. A timer that
/// was running across the restore keeps its whole run if the data knows of it, and only the time since if not. Its
/// time away comes with it, so sleep stays unconfirmed.
fn carry_forward(here: &Shared, base: &mut Shared, device: &str, since: Ts) {
    // By segment, with where its time away is cut, if it was cut itself.
    let mut carried: BTreeMap<&str, Option<Ts>> = BTreeMap::new();
    for segment in here.segments.values() {
        if segment.device_id != device || segment.end.is_none_or(|end| end <= since) {
            continue;
        }
        if let Some(kept) = base.segments.get_mut(&segment.id) {
            kept.end = kept.end.max(segment.end);
            carried.insert(&segment.id, None);
            continue;
        }
        let Some(cut) = cut_at(segment, since) else {
            continue;
        };
        if !base.tasks.contains_key(&segment.task_id) {
            let Some(task) = here.tasks.get(&segment.task_id) else {
                continue;
            };
            base.tasks.insert(task.id.clone(), task.clone());
            if let Some(project) = here.projects.get(&task.project_id) {
                base.projects
                    .entry(project.id.clone())
                    .or_insert_with(|| project.clone());
            }
        }
        base.segments.insert(segment.id.clone(), cut);
        carried.insert(&segment.id, Some(since));
    }
    for gap in here.gaps.values() {
        let Some(from) = carried.get(gap.segment_id.as_str()) else {
            continue;
        };
        let start = from.map_or(gap.start, |from| gap.start.max(from));
        if start < gap.end && !base.gaps.contains_key(&gap.id) {
            let mut gap = gap.clone();
            gap.start = start;
            base.gaps.insert(gap.id.clone(), gap);
        }
    }
}

/// The records here that following a restore leaves behind. The record of changes and the reference prefixes stay.
fn records_not_in(here: &Shared, there: &Shared) -> usize {
    fn absent<V>(a: &BTreeMap<String, V>, b: &BTreeMap<String, V>) -> usize {
        a.keys().filter(|k| !b.contains_key(*k)).count()
    }
    absent(&here.projects, &there.projects)
        + absent(&here.tasks, &there.tasks)
        + absent(&here.segments, &there.segments)
        + absent(&here.settings, &there.settings)
        + absent(&here.shifts, &there.shifts)
        + absent(&here.rates, &there.rates)
        + absent(&here.rounding, &there.rounding)
        + absent(&here.gaps, &there.gaps)
        + absent(&here.pauses, &there.pauses)
        + absent(&here.tags, &there.tags)
        + absent(&here.templates, &there.templates)
        + absent(&here.summaries, &there.summaries)
        + absent(&here.pause_requests, &there.pause_requests)
}

fn join_folder(file: &mut LocalFile, root: &str) {
    if file.prefs.sync_targets.iter().any(|t| t.root == root) {
        let joined = file.prefs.sync_joined.get_or_insert_with(Vec::new);
        if !joined.iter().any(|r| r == root) {
            joined.push(root.to_owned());
        }
    }
}

impl Service {
    fn restorer_name(&self, by: &str) -> String {
        if by == self.file.device_id {
            self.file.device_name.clone()
        } else {
            self.device_name_of(by)
        }
    }

    /// Who made a restore is said by name, even of a device nothing has been taken from yet.
    fn learn_restorer<'a>(
        &mut self,
        info: &EpochInfo,
        mut files: impl Iterator<Item = &'a DeviceFile>,
    ) {
        if let Some(f) = files.find(|f| f.device_id == info.by) {
            self.file
                .remote_devices
                .entry(f.device_id.clone())
                .or_insert_with(|| RemoteDevice {
                    name: f.device_name.clone(),
                    last_heartbeat: f.last_heartbeat,
                    written_at: f.written_at,
                });
        }
    }

    /// While a first start waits for its data (`awaiting_cloud`), before the restores are planned: this device's own
    /// files in the folders are the data its old store held, so they join the data here first, as that store held them.
    /// Planned against them, a restore the old store would have followed is followed as it would have been, keeping what
    /// this device timed since (`carry_forward`), where an empty store would have dropped them below its key. A newer own
    /// file shows this device already follows a later restore than its old store says, so the key goes up to the
    /// highest one they carry, before anything at it is merged. Own files from before that key stay out, as the fence
    /// keeps them out of any merge. Returns true when this device's data changed.
    pub(crate) fn take_own_files(&mut self, reads: &[(SyncTarget, TargetRead)]) -> bool {
        if !self.awaiting_cloud() {
            return false;
        }
        let own: Vec<&DeviceFile> = reads
            .iter()
            .filter(|(_, r)| r.error.is_none())
            .filter_map(|(_, r)| r.own.as_ref())
            .collect();
        let Some(top) = own
            .iter()
            .map(|f| f.epoch_info.as_ref())
            .max_by(|a, b| EpochInfo::key(*a).cmp(&EpochInfo::key(*b)))
        else {
            return false;
        };
        let raised = top
            .filter(|_| EpochInfo::key(top) > EpochInfo::key(self.file.prefs.epoch.as_ref()))
            .cloned();
        let key = EpochInfo::key(raised.as_ref().or(self.file.prefs.epoch.as_ref()));
        let at_key: Vec<DeviceFile> = own
            .into_iter()
            .filter(|f| key_of(f) == key)
            .cloned()
            .collect();
        let now = self.now();
        let beat = at_key
            .iter()
            .filter_map(|f| f.last_heartbeat)
            .max()
            .map(|b| b.min(now));
        if raised.is_some() || beat > self.file.last_heartbeat {
            // As `finish_awaiting` takes it: a run its own file knows of is not forgotten, even if a restore follows.
            let saved = self.mutate_system(|f, _| {
                if raised.is_some() {
                    f.prefs.epoch = raised;
                }
                f.last_heartbeat = f.last_heartbeat.max(beat);
                Ok(())
            });
            if let Err(e) = saved {
                self.local_error = Some(format!("Could not save data from the sync folder: {e}"));
                return false;
            }
        }
        self.absorb_remote(&Shared::default(), BTreeMap::new(), at_key)
    }

    /// What a cycle does about restores before it merges, inside the lock. Afterwards `reads` holds only what may be
    /// merged at this device's key, and a folder left alone reads as one with an error, so it gets no write and its
    /// last known figures stand. Returns true when this device's data was replaced.
    pub(crate) fn follow_restores(&mut self, reads: &mut [(SyncTarget, TargetRead)]) -> bool {
        let joined = self.file.prefs.sync_joined.clone().unwrap_or_default();
        let plan = {
            let read: Vec<(String, &TargetRead)> =
                reads.iter().map(|(t, r)| (t.root.clone(), r)).collect();
            plan(self.file.prefs.epoch.as_ref(), &joined, &read)
        };
        let has_data = plan.adopt.is_some() || holds_data(&self.file.data);
        let first = plan.higher.first();
        let raised = first.filter(|_| !has_data).map(|(_, info)| info.clone());
        let asked = first.filter(|_| has_data).map(|(root, info)| JoinPending {
            root: root.clone(),
            info: info.clone(),
        });
        for info in plan.adopt.iter().chain(asked.as_ref().map(|a| &a.info)) {
            self.learn_restorer(info, reads.iter().flat_map(|(_, r)| &r.files));
        }
        // A question whose folder no longer holds anything past this device's key has answered itself.
        let waiting = asked.or_else(|| {
            let read = |root: &str| {
                reads
                    .iter()
                    .any(|(t, r)| t.root == root && r.error.is_none())
            };
            let current = self.file.prefs.join_pending.as_ref();
            current.filter(|w| !read(&w.root)).cloned()
        });
        let mut followed = joined.clone();
        followed.extend(plan.joining.iter().cloned());
        if raised.is_some() {
            followed.extend(plan.higher.iter().map(|(root, _)| root.clone()));
        }
        // Why a restore found here couldn't be followed, when it couldn't.
        let mut not_followed = None;
        let raising = raised.is_some();
        if followed != joined || waiting != self.file.prefs.join_pending || raising {
            // The key goes up before anything at it is merged. A crash between the two then merges it at the next sync,
            // where the other way round would leave its data here, and out at the old key.
            let saved = self.mutate_system(|f, _| {
                f.prefs.sync_joined = Some(followed);
                f.prefs.join_pending = waiting;
                if raising {
                    f.prefs.epoch = raised;
                }
                Ok(())
            });
            if let Err(e) = saved {
                self.local_error = Some(format!("Could not save data from the sync folder: {e}"));
                not_followed = raising.then(|| e.to_string());
            }
        }
        let mut adopted = false;
        if let Some(info) = plan.adopt {
            let key = EpochInfo::key(Some(&info));
            let base = merged_at(key, reads.iter().flat_map(|(_, r)| files_of(r)));
            let since = self.followed_since(&info, reads);
            match self.adopt_epoch(info, base.unwrap_or_default(), since) {
                Ok(()) => adopted = true,
                Err(e) => {
                    self.local_error =
                        Some(format!("Could not save data from the sync folder: {e}"));
                    not_followed = Some(e.to_string());
                }
            }
        }
        let local = self.file.prefs.epoch.clone();
        for (_, read) in reads.iter_mut() {
            if holds_above(read, local.as_ref()) {
                let why = not_followed.as_ref().map_or_else(
                    || WAITING.to_owned(),
                    |e| {
                        format!(
                            "This folder holds a restored version that couldn't be saved on this computer, so nothing was read from it or written to it. Will retry: {e}"
                        )
                    },
                );
                *read = TargetRead {
                    error: Some(why),
                    ..TargetRead::default()
                };
            } else {
                fence(read, local.as_ref());
            }
        }
        adopted
    }

    /// From when what this device logged is kept as it follows `info`: from the restore, by the restoring computer's
    /// clock. Once that clock is seen to run ahead of this one, it can't place the restore on this computer's clock, so
    /// what was logged here since these folders were last read without the restore is kept instead.
    fn followed_since(&self, info: &EpochInfo, reads: &[(SyncTarget, TargetRead)]) -> Ts {
        let now = self.now();
        let ahead = |at: Ts| at.secs() - now.secs() > CLOCK_AHEAD_SECS;
        let restorer_ahead = ahead(info.at)
            || reads
                .iter()
                .flat_map(|(_, r)| files_of(r))
                .any(|f| f.device_id == info.by && ahead(f.written_at));
        if !restorer_ahead {
            return info.at;
        }
        let key = EpochInfo::key(Some(info));
        reads
            .iter()
            .filter(|(_, r)| files_of(r).any(|f| key_of(f) == key))
            .filter_map(|(t, _)| self.last_read_at(&t.root))
            .max()
            .map_or(info.at, |seen| seen.min(info.at))
    }

    /// Before a move merges anything. A move never replaces this device's data by itself: an empty store takes on a
    /// restore it finds, and otherwise the error says what stands in the way, with nothing copied yet.
    pub(crate) fn restores_before_move(
        &mut self,
        reads: &[(String, &TargetRead)],
    ) -> Result<(), String> {
        let joined = self.file.prefs.sync_joined.clone().unwrap_or_default();
        let plan = plan(self.file.prefs.epoch.as_ref(), &joined, reads);
        if plan.adopt.is_some() {
            return Err(SYNC_BEFORE_MOVE.to_owned());
        }
        let Some((root, info)) = plan.higher.into_iter().next() else {
            let answered = self
                .file
                .prefs
                .join_pending
                .as_ref()
                .is_some_and(|w| reads.iter().any(|(root, _)| *root == w.root));
            if answered {
                self.mutate_system(|f, _| {
                    f.prefs.join_pending = None;
                    Ok(())
                })
                .map_err(|e| e.to_string())?;
            }
            return Ok(());
        };
        if holds_data(&self.file.data) {
            self.learn_restorer(&info, reads.iter().flat_map(|(_, r)| &r.files));
            self.mutate_system(|f, _| {
                f.prefs.join_pending = Some(JoinPending { root, info });
                Ok(())
            })
            .map_err(|e| e.to_string())?;
            return Err(ASK_BEFORE_MOVE.to_owned());
        }
        self.mutate_system(|f, _| {
            f.prefs.epoch = Some(info);
            Ok(())
        })
        .map_err(|e| e.to_string())
    }

    /// Replaces this device's data with a restored version, in one save with the restore's key, so a crash can't leave
    /// half of it. A failure leaves everything as it was, its timer included. A local safety copy and a saved version
    /// keep what was here, and what was timed after `since`, when the restore was made, stays in the data.
    pub(crate) fn adopt_epoch(
        &mut self,
        info: EpochInfo,
        base: Shared,
        since: Ts,
    ) -> Result<(), CoreError> {
        self.settle_away()?;
        let now = self.now();
        let device = self.file.device_id.clone();
        let ctx = Ctx::hlc(now, &device, self.newest_seen());
        let mut here = self.file.clone();
        let stopped = here
            .data
            .close_own_open(&ctx, now, &PauseReason::SyncReconciled);
        if !stopped.is_empty() {
            here.last_heartbeat = Some(now);
        }
        let safety = backup::safety_copy(&self.paths, &here, now)?;
        let mut before = self.device_file();
        before.last_heartbeat = here.last_heartbeat;
        before.data.clone_from(&here.data);

        let mut data = base;
        carry_forward(&here.data, &mut data, &device, since);
        // A task the version held running here stopped being timed when the version was taken.
        data.close_own_open(&ctx, info.base_taken_at, &PauseReason::SyncReconciled);
        data.reconcile(&ctx);
        let left_behind = records_not_in(&here.data, &data);
        let name = self.restorer_name(&info.by);
        let taken = self.when_text(info.base_taken_at);
        self.install_restored(
            data,
            RestoreNote {
                entity: "version",
                name: info.restored_from.clone(),
                after: json!({"safetyCopy": safety, "epoch": info.epoch, "by": info.by}),
                closed_at: info.base_taken_at,
                reason: PauseReason::SyncReconciled,
                last_heartbeat: here.last_heartbeat,
                epoch: Some(info),
            },
        )?;
        self.keep_pre_adopt(before);

        let kept = match left_behind {
            0 => format!("What was here before is kept in {safety}."),
            1 => format!("1 record here that isn't in it is kept in {safety}."),
            n => format!("{n} records here that aren't in it are kept in {safety}."),
        };
        self.notify(
            "warning",
            format!(
                "{name} restored the version from {taken}. This computer now follows it. {kept} The data from before this is also being saved as a version in your sync folders."
            ),
        );
        for task in stopped {
            let message = format!(
                "\"{task}\" was running when {name} restored a saved version, so its timer here has stopped. The time since that restore is kept."
            );
            self.pause_alerts.push(message.clone());
            self.notify("warning", message);
        }
        Ok(())
    }

    /// The restore a folder is waiting on an answer about.
    fn join_question(&self, root: &str) -> Result<EpochInfo, CoreError> {
        self.file
            .prefs
            .join_pending
            .as_ref()
            .filter(|w| w.root == root)
            .map(|w| w.info.clone())
            .ok_or_else(|| CoreError::Conflict(NOT_WAITING.to_owned()))
    }

    /// The user's answer about a folder holding a restore, with `base` the merge of what it holds at that restore. The
    /// folder may be one a move stopped at, not connected yet. Following it replaces this device's data, which any
    /// computer may do. Keeping both merges them and makes a new restore, so the other devices follow the union, which
    /// can bring rolled-back records back. That changes every computer's data, so it is refused while this one is
    /// read-only, as what it has read of the holder's folder may be behind. Merged first: a crash before the key is
    /// raised leaves the data at the old key, where it passes no fence, and the question still open.
    pub fn resolve_join(
        &mut self,
        root: &str,
        follow: bool,
        base: Shared,
    ) -> Result<(), CoreError> {
        let info = self.join_question(root)?;
        if follow {
            let since = info.at;
            self.adopt_epoch(info, base, since)?;
            return self.mutate_system(|f, _| {
                f.prefs.join_pending = None;
                join_folder(f, root);
                Ok(())
            });
        }
        self.lease_gate()?;
        let number = epoch_above(info.epoch)?;
        self.local_error = None;
        self.absorb_remote(&base, BTreeMap::new(), Vec::new());
        if let Some(e) = &self.local_error {
            return Err(CoreError::Conflict(e.clone()));
        }
        let name = self.restorer_name(&info.by);
        self.mutate_system(|f, c| {
            let minted = EpochInfo {
                epoch: number,
                by: c.device.to_owned(),
                at: c.now,
                restored_from: info.restored_from.clone(),
                base_taken_at: info.base_taken_at,
            };
            f.data.audit(
                c,
                "version",
                &minted.restored_from,
                "keepWithRestore",
                "",
                Value::Null,
                json!({"epoch": minted.epoch}),
            );
            f.prefs.epoch = Some(minted);
            f.prefs.join_pending = None;
            join_folder(f, root);
            Ok(())
        })?;
        // A restore the others follow, made here: this computer claims the lease, as for any change.
        if self.lease.active() {
            let now = self.now();
            self.lease.note_change(now);
        }
        self.notify(
            "info",
            format!(
                "Kept this computer's data together with the version restored on {name}. Rolled-back items may return on every computer."
            ),
        );
        Ok(())
    }
}

/// Carries out the user's answer about a folder holding a restore: reads it, outside the lock, and answers with what it
/// holds at that restore. Run under the cycle lock, like a cycle, so a sync can't run past it.
pub fn resolve_join<I: SyncIo + Clone + 'static>(
    service: &Mutex<Service>,
    io_impl: &I,
    root: &str,
    follow: bool,
    limit: Duration,
) -> Result<(), CoreError> {
    let lock = || service.lock().unwrap_or_else(PoisonError::into_inner);
    let (own_id, now, hung) = {
        let s = lock();
        s.join_question(root)?;
        if !follow {
            s.lease_gate()?;
        }
        (s.file().device_id.clone(), s.now(), s.hung_targets())
    };
    let (io, folder) = (io_impl.clone(), root.to_owned());
    let read = bounded(&hung, root, limit, move || {
        read_target(&io, Path::new(&folder), &own_id, now)
    })
    .map_err(CoreError::Conflict)?;
    if let Some(e) = read.error.clone() {
        return Err(CoreError::Conflict(e));
    }
    let mut s = lock();
    let info = s.join_question(root)?;
    let base = merged_at(EpochInfo::key(Some(&info)), files_of(&read))
        .ok_or_else(|| CoreError::Conflict(FOLDER_CHANGED.to_owned()))?;
    s.resolve_join(root, follow, base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::{ProjectInput, TaskInput};
    use crate::model::{GapDecision, GapKind, IdleGap, SCHEMA_VERSION, SegmentEdit, Stamp};
    use crate::time::ManualClock;
    use proptest::prelude::*;
    use std::sync::Arc;

    const T0: &str = "2026-03-01T09:00:00Z";

    fn at(mins: i64) -> Ts {
        Ts::parse(T0).unwrap().later(mins * 60)
    }

    fn info(epoch: u64, by: &str) -> EpochInfo {
        EpochInfo {
            epoch,
            by: by.to_owned(),
            at: at(0),
            restored_from: "20260301T090000Z--a1b2c3d4--auto".to_owned(),
            base_taken_at: at(0),
        }
    }

    fn file(device: &str, key: Option<(u64, &str)>) -> DeviceFile {
        DeviceFile {
            schema_version: SCHEMA_VERSION,
            device_id: device.to_owned(),
            device_name: device.to_uppercase(),
            written_at: at(0),
            last_heartbeat: None,
            epoch_info: key.map(|(n, by)| info(n, by)),
            data: Shared::default(),
        }
    }

    fn folder(files: Vec<DeviceFile>, own: Option<DeviceFile>) -> TargetRead {
        TargetRead {
            files,
            own,
            ..TargetRead::default()
        }
    }

    fn roots(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn a_restore_in_a_followed_folder_is_adopted_and_one_in_another_waits_highest_first() {
        // This device's own file counts too: a store put back by hand is behind its own file.
        let followed = folder(
            vec![file("b", Some((2, "a")))],
            Some(file("me", Some((3, "a")))),
        );
        let older = folder(vec![file("c", None)], None);
        let past = folder(vec![file("d", Some((4, "a")))], None);
        let further = folder(vec![file("e", Some((4, "b")))], None);
        let same = folder(vec![file("f", Some((3, "a")))], None);
        let below = folder(vec![file("g", None)], None);
        let empty = folder(Vec::new(), None);
        let reads = [
            ("/a", &followed),
            ("/old", &older),
            ("/past", &past),
            ("/further", &further),
            ("/same", &same),
            ("/below", &below),
            ("/empty", &empty),
        ]
        .map(|(root, read)| (root.to_owned(), read));
        let p = plan(Some(&info(1, "z")), &roots(&["/a", "/old"]), &reads);
        assert_eq!(p.adopt, Some(info(3, "a")));
        assert_eq!(
            p.higher,
            vec![
                ("/further".to_owned(), info(4, "b")),
                ("/past".to_owned(), info(4, "a"))
            ]
        );
        assert_eq!(p.joining, roots(&["/same", "/below"]));
    }

    #[test]
    fn no_restore_ranks_as_epoch_zero_and_a_tie_on_the_number_goes_by_device() {
        let read = folder(vec![file("b", None)], Some(file("me", None)));
        let p = plan(None, &roots(&["/a"]), &[("/a".to_owned(), &read)]);
        assert_eq!(p, EpochPlan::default());
        let read = folder(vec![file("b", Some((2, "a")))], None);
        let p = plan(
            Some(&info(2, "b")),
            &roots(&["/a"]),
            &[("/a".to_owned(), &read)],
        );
        assert_eq!(p.adopt, None, "(2, a) is below (2, b)");
        let p = plan(
            Some(&info(2, "0")),
            &roots(&["/a"]),
            &[("/a".to_owned(), &read)],
        );
        assert_eq!(p.adopt, Some(info(2, "a")));
    }

    fn keyed() -> impl Strategy<Value = Option<(u64, &'static str)>> {
        prop_oneof![
            Just(None),
            (0_u64..4, prop::sample::select(vec!["a", "b"])).prop_map(Some)
        ]
    }

    proptest! {
        #[test]
        fn the_plan_goes_by_the_highest_restore_whatever_order_the_folders_come_in(
            local in keyed(),
            folders in prop::collection::vec((prop::collection::vec(keyed(), 0..3), any::<bool>()), 0..5),
            turn in any::<usize>(),
        ) {
            let reads: Vec<(String, TargetRead)> = folders
                .iter()
                .enumerate()
                .map(|(i, (keys, _))| {
                    let files = keys.iter().enumerate().map(|(j, k)| file(&format!("d{j}"), *k)).collect();
                    (format!("/{i}"), folder(files, None))
                })
                .collect();
            let joined: Vec<String> = folders
                .iter()
                .enumerate()
                .filter(|(_, (_, joined))| *joined)
                .map(|(i, _)| format!("/{i}"))
                .collect();
            let local = local.map(|(n, by)| info(n, by));
            let refs: Vec<(String, &TargetRead)> = reads.iter().map(|(r, read)| (r.clone(), read)).collect();
            let p = plan(local.as_ref(), &joined, &refs);

            let mine = EpochInfo::key(local.as_ref());
            let top = refs
                .iter()
                .filter(|(root, _)| joined.contains(root))
                .flat_map(|(_, read)| files_of(read))
                .map(key_of)
                .fold(mine, Ord::max);
            prop_assert_eq!(p.adopt.as_ref().map(|i| EpochInfo::key(Some(i))), (top > mine).then_some(top));
            for pair in p.higher.windows(2) {
                prop_assert!(EpochInfo::key(Some(&pair[0].1)) >= EpochInfo::key(Some(&pair[1].1)));
            }
            for (root, found) in &p.higher {
                prop_assert!(!joined.contains(root) && EpochInfo::key(Some(found)) > top);
            }
            for root in &p.joining {
                prop_assert!(!joined.contains(root) && !p.higher.iter().any(|(r, _)| r == root));
            }

            let mut turned = refs.clone();
            if !turned.is_empty() {
                let by = turn % turned.len();
                turned.rotate_left(by);
            }
            let q = plan(local.as_ref(), &joined, &turned);
            prop_assert_eq!(&q.adopt, &p.adopt);
            let set = |plan: &EpochPlan| {
                let mut higher: Vec<String> = plan.higher.iter().map(|(r, _)| r.clone()).collect();
                let mut joining = plan.joining.clone();
                higher.sort();
                joining.sort();
                (higher, joining)
            };
            prop_assert_eq!(set(&q), set(&p));
        }
    }

    #[test]
    fn the_fence_drops_what_is_older_and_says_so_of_other_devices_only() {
        let mut read = folder(
            vec![
                file("b", None),
                file("c", Some((2, "a"))),
                file("d", Some((1, "z"))),
                file("e", Some((3, "a"))),
            ],
            Some(file("me", Some((1, "a")))),
        );
        assert_eq!(fence(&mut read, Some(&info(2, "a"))), 2);
        assert!(
            read.own.is_none(),
            "this device's own file goes without a word"
        );
        let left: Vec<&str> = read.files.iter().map(|f| f.device_id.as_str()).collect();
        assert_eq!(
            left,
            ["c", "e"],
            "what is above is for the plan, not the fence"
        );
        assert_eq!(
            read.warnings,
            [
                "Skipped B: it holds data from before a restored version. Update and sync that device.",
                "Skipped D: it holds data from before a restored version. Update and sync that device."
            ]
        );
        only_at_key(&mut read, Some(&info(2, "a")));
        let left: Vec<&str> = read.files.iter().map(|f| f.device_id.as_str()).collect();
        assert_eq!(left, ["c"]);
        // With no restore anywhere nothing is in the way.
        let mut read = folder(vec![file("b", None)], Some(file("me", None)));
        assert_eq!(fence(&mut read, None), 0);
        assert!(read.own.is_some() && read.warnings.is_empty());
    }

    fn pinput(name: &str) -> ProjectInput {
        ProjectInput {
            name: name.into(),
            color: "#336699".into(),
            billable: true,
            budget_hours: None,
            client_zone: None,
            billing_type: Default::default(),
            fixed_fee_cents: None,
            fee_currency: None,
        }
    }

    fn tinput(name: &str, p: &str) -> TaskInput {
        TaskInput {
            name: name.into(),
            project_id: p.into(),
            description: Value::Null,
            continued_from: None,
            tags: Vec::new(),
        }
    }

    fn segment(id: &str, task: &str, device: &str, start: i64, end: Option<i64>) -> Segment {
        Segment {
            id: id.to_owned(),
            task_id: task.to_owned(),
            device_id: device.to_owned(),
            start: at(start),
            end: end.map(at),
            ..Segment::default()
        }
    }

    fn corrected(mut s: Segment, start: i64, end: i64, voided: bool) -> Segment {
        s.edit = Some(SegmentEdit {
            start: at(start),
            end: at(end),
            voided,
            reason: String::new(),
            settled: false,
        });
        s
    }

    fn gap(id: &str, s: &Segment, start: i64, end: i64, decision: GapDecision) -> IdleGap {
        IdleGap {
            id: id.to_owned(),
            segment_id: s.id.clone(),
            task_id: s.task_id.clone(),
            device_id: s.device_id.clone(),
            start: at(start),
            end: at(end),
            kind: GapKind::Sleep,
            decision,
            decision_stamp: Stamp::default(),
        }
    }

    #[test]
    fn time_since_the_restore_comes_along_with_its_time_away_and_a_run_the_version_knew_keeps_it_all()
     {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = Service::open(dir.path(), Arc::new(clock), "pc").unwrap();
        let (p, q) = (
            s.create_project(&pinput("New")).unwrap(),
            s.create_project(&pinput("Known")).unwrap(),
        );
        let t = s.create_task(&tinput("New task", &p)).unwrap();
        let known = s.create_task(&tinput("Known task", &q)).unwrap();
        let me = s.file().device_id.clone();
        let mut here = s.file().data.clone();
        // The version knows the second project and task, under an older name, and has the second run still open.
        let mut base = Shared {
            tags: here.tags.clone(),
            settings: here.settings.clone(),
            ..Shared::default()
        };
        base.projects.insert(q.clone(), here.projects[&q].clone());
        let mut old_task = here.tasks[&known].clone();
        old_task.name = "Known task, as it was".to_owned();
        base.tasks.insert(known.clone(), old_task);
        let run = segment("run", &known, &me, 30, Some(100));
        base.segments
            .insert("run".to_owned(), segment("run", &known, &me, 30, None));
        let across = segment("across", &t, &me, 30, Some(90));
        for seg in [
            segment("before", &t, &me, 0, Some(50)),
            across.clone(),
            run.clone(),
            segment("theirs", &t, "other-device", 70, Some(80)),
            corrected(segment("voided", &t, &me, 70, Some(80)), 70, 80, true),
            corrected(segment("early", &t, &me, 30, Some(90)), 30, 55, false),
            corrected(segment("fixed", &t, &me, 30, Some(95)), 40, 92, false),
        ] {
            here.segments.insert(seg.id.clone(), seg);
        }
        for g in [
            gap("gone", &across, 35, 45, GapDecision::Pending),
            gap("cut", &across, 55, 65, GapDecision::Pending),
            gap("whole", &run, 40, 50, GapDecision::Pending),
            gap("decided", &run, 70, 75, GapDecision::Drop),
        ] {
            here.gaps.insert(g.id.clone(), g);
        }
        base.gaps.insert(
            "decided".to_owned(),
            gap("decided", &run, 70, 75, GapDecision::Keep),
        );

        carry_forward(&here, &mut base, &me, at(60));

        let ids: Vec<&str> = base.segments.keys().map(String::as_str).collect();
        assert_eq!(ids, ["across", "fixed", "run"]);
        assert_eq!(base.segments["across"].start, at(60), "cut at the restore");
        assert_eq!(base.segments["across"].end, Some(at(90)));
        assert_eq!(base.segments["run"].start, at(30), "the version knew it");
        assert_eq!(base.segments["run"].end, Some(at(100)));
        let fixed = &base.segments["fixed"];
        let edit = fixed.edit.as_ref().unwrap();
        assert_eq!(
            (fixed.start, edit.start, edit.end),
            (at(60), at(60), at(92))
        );
        // What it needs comes too. What the version holds stays as it is there.
        assert_eq!(base.tasks[&t].name, "New task");
        assert_eq!(base.projects[&p].name, "New");
        assert_eq!(base.tasks[&known].name, "Known task, as it was");
        let gaps: Vec<(&str, Ts, GapDecision)> = base
            .gaps
            .values()
            .map(|g| (g.id.as_str(), g.start, g.decision))
            .collect();
        assert_eq!(
            gaps,
            [
                ("cut", at(60), GapDecision::Pending),
                ("decided", at(70), GapDecision::Keep),
                ("whole", at(40), GapDecision::Pending)
            ]
        );
        // What is left behind is counted for the notice: four segments and a gap.
        assert_eq!(records_not_in(&here, &base), 5);
    }
}
