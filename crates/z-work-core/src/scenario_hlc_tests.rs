//! The hybrid logical clock and the clock-behind warning on whole `Service`s: design D1 §7.2 tests 5 to 15, with
//! addendum A1.5 (P6, P13) and A1.4 P15.
//!
//! Devices share one folder through `testkit`'s fake cloud, each with a clock of its own that moves with the cloud's, so
//! a skew is one `advance` of that clock alone. Every commit is a real save, so the properties run 64 cases, not 600,
//! and their stores skip the syncs to disk (`NoSyncStores`), which are nearly all a save costs and not their subject.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use serde_json::{Value, json};

use crate::hlc::{CLOCK_BEHIND_WARN_SECS, HLC_MAX_LEAD_SECS};
use crate::machine::{TaskExpected, TaskPatch};
use crate::merge::merge;
use crate::model::{Shared, Stamp};
use crate::service::Service;
use crate::store::NoSyncStores;
use crate::testkit::{Device, FakeCloud, T0, pinput, sync_all, tinput};
use crate::time::{Clock, ManualClock, Ts};
use crate::validate::keep_valid_records;
use crate::view::ClockBehindView;

const ROOT: &str = "/cloud/OneDrive";
const CAP: i64 = HLC_MAX_LEAD_SECS;
const HOUR: i64 = 3600;
const DAY: i64 = 86_400;

fn config() -> ProptestConfig {
    ProptestConfig {
        cases: 64,
        // Each case opens real stores, so a failure shrinks for a few minutes at most.
        max_shrink_iters: 64,
        ..ProptestConfig::default()
    }
}

fn two() -> (Arc<FakeCloud>, Device, Device) {
    let cloud = FakeCloud::new(&[ROOT]);
    let a = Device::open(&cloud, "DESKTOP", &[ROOT]);
    let b = Device::open(&cloud, "LAPTOP", &[ROOT]);
    (cloud, a, b)
}

/// One computer with no folder, its data in `dir`.
fn alone(dir: &Path, clock: &ManualClock) -> Service {
    Service::open(dir, Arc::new(clock.clone()), "pc").unwrap()
}

fn rename_task(s: &mut Service, task: &str, name: &str) {
    s.patch_task(
        task,
        &TaskPatch {
            name: Some(name.to_owned()),
            ..TaskPatch::default()
        },
        &TaskExpected::default(),
    )
    .unwrap();
}

fn task_name(d: &Device, task: &str) -> String {
    d.s().file().data.tasks[task].name.clone()
}

/// A rename of the task (0) or of the project (1), or a setting (2).
fn edit(s: &mut Service, (project, task): (&str, &str), kind: u8, n: u16) {
    match kind {
        0 => rename_task(s, task, &format!("task {n}")),
        1 => s
            .update_project(project, &pinput(&format!("project {n}")))
            .unwrap(),
        _ => s
            .set_setting("idleMinutes", Value::from(u64::from(n % 600) + 1))
            .unwrap(),
    }
}

fn order(a: &Stamp, b: &Stamp) -> std::cmp::Ordering {
    a.at.cmp(&b.at).then_with(|| a.by.cmp(&b.by))
}

/// Every stamp of the records, by where it is (`/tasks/<id>/nameStamp`), read from their JSON rather than through
/// `Shared::for_each_stamp`. The record of changes is left out: what it quotes are copies, not clocks.
fn stamps_at(data: &Shared) -> BTreeMap<String, Stamp> {
    fn walk(v: &Value, path: String, out: &mut BTreeMap<String, Stamp>) {
        match v {
            Value::Object(m) => {
                if let (2, Some(Value::String(at)), Some(Value::String(by))) =
                    (m.len(), m.get("at"), m.get("by"))
                {
                    let at = Ts::parse(at).unwrap();
                    out.insert(path, Stamp { at, by: by.clone() });
                    return;
                }
                for (k, x) in m {
                    if path.is_empty() && k == "auditLog" {
                        continue;
                    }
                    walk(x, format!("{path}/{k}"), out);
                }
            }
            Value::Array(a) => {
                for (i, x) in a.iter().enumerate() {
                    walk(x, format!("{path}/{i}"), out);
                }
            }
            _ => {}
        }
    }
    let mut out = BTreeMap::new();
    walk(
        &serde_json::to_value(data).unwrap(),
        String::new(),
        &mut out,
    );
    out
}

/// After one operation on `d`, whose store held `before`: every stamp it wrote is later than every other stamp the
/// store now holds, unless that newest other stamp is past the lead cap. A stamp it wrote is one of its own whose value
/// the store didn't hold before; a copy of an older one, as `Project::pin_clocks` makes, is not.
fn check_commit(d: &Device, before: &BTreeMap<String, Stamp>) -> Result<(), TestCaseError> {
    let s = d.s();
    let me = s.file().device_id.clone();
    let now = s.now();
    let old: Vec<&Stamp> = before.values().collect();
    let after = stamps_at(&s.file().data);
    let (written, others): (Vec<&Stamp>, Vec<&Stamp>) =
        after.values().partition(|v| v.by == me && !old.contains(v));
    let (Some(first), Some(newest_other)) = (
        written.iter().copied().min_by(|a, b| order(a, b)),
        others.iter().copied().max_by(|a, b| order(a, b)),
    ) else {
        return Ok(());
    };
    // Under the cap: one second past the newest other stamp is at most a day past the clock.
    if newest_other.at.secs() < now.secs() + CAP {
        prop_assert!(
            first.at > newest_other.at,
            "a stamp written at {} is not after {} held already (now {})",
            first.at,
            newest_other.at,
            now
        );
    }
    // Never past the cap, unless it beats the version it replaced (`Ctx::next`).
    Ok(())
}

#[derive(Clone, Debug)]
enum Step {
    /// This device's clock moves by this many seconds, either way.
    Clock(i64),
    /// This device edits (see `edit`).
    Edit(u8, u16),
    /// The other device, its clock this many seconds from this one's, edits and syncs, then this one syncs.
    Peer(i64, u8, u16),
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        (-HOUR..=HOUR).prop_map(Step::Clock),
        (0u8..3, any::<u16>()).prop_map(|(k, n)| Step::Edit(k, n)),
        (-(DAY - 1)..=DAY - 1, 0u8..3, any::<u16>()).prop_map(|(o, k, n)| Step::Peer(o, k, n)),
    ]
}

proptest! {
    #![proptest_config(config())]

    /// 5.
    #[test]
    fn stamps_strictly_increase_per_device_across_commits(
        steps in prop::collection::vec(step(), 1..=30),
    ) {
        let _fast = NoSyncStores::on();
        let (_cloud, a, peer) = two();
        let (p, t) = a.task("P", "T");
        sync_all(&[&a, &peer]);
        for step in steps {
            match step {
                Step::Clock(secs) => a.clock.advance(secs),
                Step::Edit(kind, n) => {
                    let before = stamps_at(&a.s().file().data);
                    edit(&mut a.s(), (&p, &t), kind, n);
                    check_commit(&a, &before)?;
                }
                Step::Peer(offset, kind, n) => {
                    let to = a.clock.now().secs() + offset;
                    peer.clock.advance(to - peer.clock.now().secs());
                    edit(&mut peer.s(), (&p, &t), kind, n);
                    peer.cycle();
                    let before = stamps_at(&a.s().file().data);
                    a.cycle();
                    check_commit(&a, &before)?;
                }
            }
        }
    }

    /// 6.
    #[test]
    fn merge_laws_hold_for_hlc_made_states(
        offsets in prop::array::uniform3(-7200i64..=7200),
        steps in prop::collection::vec((0usize..3, 0u8..4, any::<u16>()), 1..=16),
    ) {
        let _fast = NoSyncStores::on();
        let cloud = FakeCloud::new(&[ROOT]);
        let all = [
            Device::open(&cloud, "DESKTOP", &[ROOT]),
            Device::open(&cloud, "LAPTOP", &[ROOT]),
            Device::open(&cloud, "TABLET", &[ROOT]),
        ];
        let [d0, d1, d2] = &all;
        let (p, t) = d0.task("P", "T");
        sync_all(&[d0, d1, d2]);
        for (d, offset) in all.iter().zip(offsets) {
            d.clock.advance(offset);
        }
        for (who, what, n) in steps {
            let d = &all[who];
            if what == 3 {
                // A partial sync: this device alone reads the folder and writes its file.
                d.cycle();
            } else {
                edit(&mut d.s(), (&p, &t), what, n);
            }
        }
        let data = || all.each_ref().map(|d| d.s().file().data.clone());
        let [a, b, c] = data();
        prop_assert_eq!(merge(&a, &a), a.clone());
        prop_assert_eq!(merge(&a, &b), merge(&b, &a));
        prop_assert_eq!(merge(&b, &c), merge(&c, &b));
        prop_assert_eq!(merge(&merge(&a, &b), &c), merge(&a, &merge(&b, &c)));
        sync_all(&[d0, d1, d2]);
        let [a, b, c] = data();
        prop_assert_eq!(&a, &b);
        prop_assert_eq!(&b, &c);
    }

    /// 7. B's clock is behind A's by less than a day, or by more, up to the two days a reader allows.
    #[test]
    fn the_latest_save_after_seeing_wins(behind in prop_oneof![1..CAP, CAP..2 * DAY - 120]) {
        latest_save_after_seeing(behind)?;
    }
}

/// A renames T, B syncs, B renames T, A syncs: both have B's name.
fn latest_save_after_seeing(behind: i64) -> Result<(), TestCaseError> {
    let _fast = NoSyncStores::on();
    let (_cloud, a, b) = two();
    let (_, t) = a.task("P", "T");
    sync_all(&[&a, &b]);
    b.clock.advance(-behind);
    rename_task(&mut a.s(), &t, "A's");
    let renamed = a.s().file().data.tasks[&t].name_stamp.clone();
    a.cycle();
    b.cycle();
    let b_now = b.s().now();
    rename_task(&mut b.s(), &t, "B's");
    b.cycle();
    a.cycle();
    prop_assert_eq!(task_name(&a, &t), "B's");
    prop_assert_eq!(task_name(&b, &t), "B's");
    let won = b.s().file().data.tasks[&t].name_stamp.clone();
    prop_assert_eq!(won.at, renamed.at.later(1));
    if behind >= CAP {
        // Past the cap B's commit is dated a day past its clock, before A's rename: its name comes after A's only
        // because a new edit always beats the version it replaces (`Ctx::next`).
        prop_assert!(won.at > b_now.later(CAP));
        // And the file B writes is still taken whole by A.
        let mut file = b.s().device_file();
        let left_out = keep_valid_records(&mut file, a.s().now()).unwrap();
        prop_assert!(left_out.is_empty(), "{:?}", left_out);
    }
    Ok(())
}

/// 8.
///
/// B's clock is an hour behind. A renames T, which B doesn't see yet; then B sees a later change from C, and renames T
/// after that. B's rename is the later one, though its clock reads before A's: having seen C's change, B dates its own
/// after it, so it wins everywhere once the folder is read through. By its clock alone it would lose.
#[test]
fn a_device_an_hour_behind_still_wins_with_its_later_edit() {
    let cloud = FakeCloud::new(&[ROOT]);
    let a = Device::open(&cloud, "DESKTOP", &[ROOT]);
    let b = Device::open(&cloud, "LAPTOP", &[ROOT]);
    let c = Device::open(&cloud, "TABLET", &[ROOT]);
    let (_, t) = a.task("P", "T");
    sync_all(&[&a, &b, &c]);
    b.clock.advance(-HOUR);
    cloud.hold(&a.id(), &b.id());
    cloud.hold(&a.id(), &c.id());
    cloud.advance(60);
    rename_task(&mut a.s(), &t, "A's");
    let a_renamed = a.s().file().data.tasks[&t].name_stamp.clone();
    a.cycle();
    cloud.advance(300);
    c.s().set_setting("idleMinutes", json!(15)).unwrap();
    c.cycle();
    b.cycle();
    assert_eq!(task_name(&b, &t), "T", "A's rename hasn't reached B");
    cloud.advance(60);
    rename_task(&mut b.s(), &t, "B's");
    let b_renamed = b.s().file().data.tasks[&t].name_stamp.clone();
    assert!(b.s().now() < a_renamed.at, "by its clock B renames first");
    assert!(b_renamed.at > a_renamed.at);
    b.cycle();
    cloud.release(&a.id(), &b.id());
    cloud.release(&a.id(), &c.id());
    sync_all(&[&a, &b, &c]);
    for d in [&a, &b, &c] {
        assert_eq!(task_name(d, &t), "B's", "on {}", d.name());
    }
}

/// 9.
#[test]
fn a_bulk_edit_moves_the_stamp_one_second_not_one_per_record() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(T0);
    let mut s = alone(dir.path(), &clock);
    let p = s.create_project(&pinput("P")).unwrap();
    // A second save within the same second: the floor is already past the clock.
    s.create_task(&tinput("first", &p)).unwrap();
    let floor = s.newest_seen().cloned().unwrap();
    assert_eq!(floor.at, s.now().later(1));
    let ids = s
        .mutate(|f, c| {
            (0..500)
                .map(|i| f.data.create_task(c, &tinput(&format!("T{i}"), &p)))
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap();
    let data = &s.file().data;
    let stamps: BTreeSet<Ts> = ids
        .iter()
        .flat_map(|id| {
            let t = &data.tasks[id];
            [t.name_stamp.at, t.state_stamp.at, t.description_stamp.at]
        })
        .collect();
    assert_eq!(stamps, BTreeSet::from([floor.at.later(1)]));
    assert_eq!(s.newest_seen().unwrap().at, floor.at.later(1));
}

/// 10.
#[test]
fn segment_times_stay_on_the_wall_clock() {
    let (cloud, a, b) = two();
    let (_, t) = a.task("P", "T");
    sync_all(&[&a, &b]);
    b.clock.advance(-HOUR);
    cloud.advance(60);
    rename_task(&mut a.s(), &t, "renamed on A");
    let a_newest = a.s().newest_seen().cloned().unwrap();
    a.cycle();
    b.cycle();
    b.s().start_task(&t).unwrap();
    let seg = b.running();
    let s = b.s();
    let data = &s.file().data;
    assert_eq!(
        data.segments[&seg].start,
        s.now(),
        "logged time is wall time"
    );
    assert_eq!(data.tasks[&t].state_stamp.at, a_newest.at.later(1));
    assert_eq!(data.segments[&seg].edit_stamp.at, a_newest.at.later(1));
}

/// 11.
///
/// An edit made while A's clock said 300 days later puts A's floor there. Once the clock is right, A's next stamps stop
/// a day past it, so B, whose clock is right, still takes them; only the far-future record is left out there.
#[test]
fn a_far_future_stamp_caps_the_lead_and_the_file_still_reaches_others() {
    let (_cloud, a, b) = two();
    let (p, t) = a.task("P", "T");
    sync_all(&[&a, &b]);
    let ahead = 300 * DAY;
    a.clock.advance(ahead);
    rename_task(&mut a.s(), &t, "dated far ahead");
    a.clock.advance(-ahead);
    a.s().update_project(&p, &pinput("still syncs")).unwrap();
    let now = a.s().now();
    let project = a.s().file().data.projects[&p].clone();
    assert_eq!(project.identity_stamp.as_ref().unwrap().at, now.later(CAP));
    assert_eq!(project.stamp.at, now.later(CAP));
    a.cycle();
    b.cycle();
    let on_b = b.s().file().data.clone();
    assert_eq!(on_b.projects[&p].name, "still syncs");
    assert_eq!(on_b.tasks[&t].name, "T", "the far-future rename stays out");
    // A was told: its clock is behind a change saved here, by more than the lead allows.
    let view = a.s().clock_behind().unwrap();
    assert_eq!(view.cause_device, None);
    assert!(view.capped);
    assert!(view.behind_secs >= ahead - 60, "{view:?}");
    // B has A's change dated a day past its clock, which is the cap. B's clock is right, and A's file, written at A's
    // clock, which is right again, says so: nothing is said to B (review F3: clocks are held against the times files
    // were written, never against stamps, which A's floor put ahead).
    let b_now = b.s().now();
    assert_eq!(b.s().newest_seen().unwrap().at, b_now.later(CAP));
    assert_eq!(b.s().clock_behind(), None);
}

/// 12.
#[test]
fn the_clock_behind_view_names_the_other_device_and_clears_when_the_clock_catches_up() {
    let (_cloud, a, b) = two();
    let (_, t) = a.task("P", "T");
    sync_all(&[&a, &b]);
    a.clock.advance(HOUR);
    rename_task(&mut a.s(), &t, "renamed ahead");
    a.cycle();
    b.cycle();
    let view = b.s().clock_behind().unwrap();
    assert!((3590..=3600).contains(&view.behind_secs), "{view:?}");
    assert_eq!(view.cause_device.as_deref(), Some("DESKTOP"));
    assert!(!view.capped);
    assert_eq!(b.s().snapshot().clock_behind, Some(view.clone()));
    // B's own change is dated after A's, and the warning is still about A's: B's clock hasn't moved.
    rename_task(&mut b.s(), &t, "renamed behind");
    assert!(b.s().file().data.tasks[&t].name_stamp.at > view.newest_at);
    b.cycle();
    assert_eq!(b.s().clock_behind(), Some(view));
    assert_eq!(a.s().clock_behind(), None, "the clock ahead is not told");
    b.clock.advance(HOUR);
    assert_eq!(b.s().clock_behind(), None);
    assert_eq!(b.s().snapshot().clock_behind, None);
}

/// 13.
#[test]
fn a_clock_set_back_warns_as_this_computer() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(T0);
    let mut s = alone(dir.path(), &clock);
    let p = s.create_project(&pinput("P")).unwrap();
    let saved = s.now();
    clock.set(&saved.later(-HOUR).iso());
    let set_back = ClockBehindView {
        behind_secs: HOUR,
        newest_at: saved,
        cause_device: None,
        capped: false,
    };
    assert_eq!(s.clock_behind(), Some(set_back.clone()));
    // A change made on the clock as it reads now is still dated after the one before, and the warning stays.
    s.update_project(&p, &pinput("Q")).unwrap();
    assert!(s.file().data.projects[&p].stamp.at > saved);
    assert_eq!(s.clock_behind(), Some(set_back.clone()));
    // It is kept with the data, so a restart still says it.
    drop(s);
    let s = alone(dir.path(), &clock);
    assert_eq!(s.clock_behind(), Some(set_back));
    clock.set(&saved.later(-(CLOCK_BEHIND_WARN_SECS - 1)).iso());
    assert_eq!(s.clock_behind(), None);
}

/// 14 (addendum P13, review F3). 200 commits within one second of the clock date their stamps 200 seconds past it, which
/// is past the warning's 120. The warning never compares the clock with a stamp, only with when changes were saved here
/// by this clock, and with when other devices wrote their files by theirs, so nothing is said, before a restart or
/// after. The same burst on another device is `another_devices_burst_never_warns`.
#[test]
fn own_bursts_never_warn() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(T0);
    let mut s = alone(dir.path(), &clock);
    let p = s.create_project(&pinput("P")).unwrap();
    let now = s.now();
    for i in 0..200 {
        s.update_project(&p, &pinput(&format!("P{i}"))).unwrap();
    }
    assert_eq!(s.newest_seen().unwrap().at, now.later(200));
    assert_eq!(s.clock_behind(), None);
    assert_eq!(s.snapshot().clock_behind, None);
    drop(s);
    let s = alone(dir.path(), &clock);
    assert_eq!(s.newest_seen().unwrap().at, now.later(200));
    assert_eq!(s.clock_behind(), None);
}

/// Review F3 (a). Another device's burst dates its stamps past its clock as this device's own do (14), and its clock is
/// this one's: its file says so, and nothing is said here.
#[test]
fn another_devices_burst_never_warns() {
    let (_cloud, a, b) = two();
    let (_, t) = b.task("P", "T");
    sync_all(&[&a, &b]);
    let now = b.s().now();
    for i in 0..200 {
        rename_task(&mut b.s(), &t, &format!("n{i}"));
    }
    assert!(
        b.s().newest_seen().unwrap().at >= now.later(200),
        "the burst dated B's stamps past its clock"
    );
    b.cycle();
    a.cycle();
    assert_eq!(task_name(&a, &t), "n199");
    assert!(a.s().newest_seen().unwrap().at >= now.later(200));
    assert_eq!(a.clock.now(), b.clock.now(), "the clocks agree");
    assert_eq!(a.s().clock_behind(), None);
    assert_eq!(a.s().snapshot().clock_behind, None);
}

/// Review F3 (b). A clock two hours fast warns the other computer while it is fast. Put right, its next file says so:
/// the warning goes there, and a computer that joins after never has it. Only the computer whose clock was fast is still
/// told about itself, as it saved a change at a time its clock hasn't reached again (13).
#[test]
fn a_fast_clock_put_right_is_believed_at_its_next_write() {
    let (cloud, a, b) = two();
    let (_, t) = a.task("P", "T");
    sync_all(&[&a, &b]);
    b.clock.advance(2 * HOUR);
    rename_task(&mut b.s(), &t, "fast");
    b.cycle();
    a.cycle();
    let view = a.s().clock_behind().unwrap();
    assert_eq!(view.behind_secs, 2 * HOUR);
    assert_eq!(view.cause_device.as_deref(), Some("LAPTOP"));
    assert!(!view.capped);
    b.clock.advance(-2 * HOUR);
    b.cycle();
    a.cycle();
    assert_eq!(task_name(&a, &t), "fast", "the change stands");
    assert_eq!(a.s().clock_behind(), None);
    let c = Device::open(&cloud, "THIRD", &[ROOT]);
    sync_all(&[&b, &c]);
    assert_eq!(task_name(&c, &t), "fast");
    assert_eq!(c.s().clock_behind(), None);
    let own = b.s().clock_behind().unwrap();
    assert_eq!(own.cause_device, None);
    assert_eq!(own.behind_secs, 2 * HOUR);
}

/// Review F3. A clock really two hours slow goes on being told for as long as the other computer writes, each file dated
/// by a clock that is right, and by as much: what the warning holds it against isn't a change that ages.
#[test]
fn a_slow_clock_is_told_for_as_long_as_the_others_write() {
    let (cloud, a, b) = two();
    let (_, t) = b.task("P", "T");
    sync_all(&[&a, &b]);
    a.clock.advance(-2 * HOUR);
    rename_task(&mut b.s(), &t, "x");
    b.cycle();
    a.cycle();
    assert_eq!(a.s().clock_behind().map(|v| v.behind_secs), Some(2 * HOUR));
    cloud.advance(HOUR);
    b.cycle();
    a.cycle();
    let view = a.s().clock_behind().unwrap();
    assert_eq!(view.behind_secs, 2 * HOUR);
    assert_eq!(view.cause_device.as_deref(), Some("LAPTOP"));
}

/// Review F8. A restored backup holding a stamp dated a month ahead, past any reader's clock, doesn't take the floor
/// there, before a restart or after: stamps made here stay on the clock rather than at the lead cap, where they would
/// beat every other computer's later edits.
#[test]
fn a_restored_stamp_past_every_readers_clock_does_not_hold_the_floor_ahead() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(T0);
    let mut s = alone(dir.path(), &clock);
    let p = s.create_project(&pinput("P")).unwrap();
    let t = s.create_task(&tinput("T", &p)).unwrap();
    assert!(s.run_daily_backup().unwrap());
    let name = s.list_backups().unwrap()[0].name.clone();
    let path = dir.path().join("backups").join(&name);
    let far = s.now().later(30 * DAY);
    let mut file: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    file["data"]["tasks"][&t]["nameStamp"]["at"] = json!(far.iso());
    std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
    s.restore_backup(&name).unwrap();
    assert_eq!(
        s.file().data.tasks[&t].name_stamp.at,
        far,
        "restored as it was"
    );
    let lead = |s: &Service| s.newest_seen().unwrap().at.secs() - s.now().secs();
    assert!(lead(&s) < HOUR, "{}", lead(&s));
    s.update_project(&p, &pinput("Q")).unwrap();
    let at = s.file().data.projects[&p].stamp.at;
    assert!(at < s.now().later(HOUR), "dated by the clock: {at}");
    drop(s);
    let mut s = alone(dir.path(), &clock);
    assert!(lead(&s) < HOUR, "and after a restart: {}", lead(&s));
    s.update_project(&p, &pinput("R")).unwrap();
    assert_eq!(s.file().data.projects[&p].stamp.at, at.later(1));
}

/// 15.
#[test]
fn the_newest_stamp_survives_a_restart_and_a_restore() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(T0);
    let mut s = alone(dir.path(), &clock);
    let p = s.create_project(&pinput("P")).unwrap();
    s.run_daily_backup().unwrap();
    let backup = s.list_backups().unwrap()[0].name.clone();
    for i in 0..5 {
        s.update_project(&p, &pinput(&format!("P{i}"))).unwrap();
    }
    let floor = s.newest_seen().cloned().unwrap();
    assert_eq!(floor.at, s.now().later(5));
    drop(s);
    let mut s = alone(dir.path(), &clock);
    assert_eq!(s.file().newest_stamp.as_ref(), Some(&floor));
    assert_eq!(s.newest_seen(), Some(&floor));
    s.restore_backup(&backup).unwrap();
    assert!(
        s.file()
            .data
            .newest_stamp()
            .is_some_and(|n| n.at < floor.at),
        "the data restored is older"
    );
    assert_eq!(s.newest_seen(), Some(&floor), "a restore never lowers it");
    assert_eq!(s.file().newest_stamp.as_ref(), Some(&floor));
    drop(s);
    let mut s = alone(dir.path(), &clock);
    assert_eq!(s.newest_seen(), Some(&floor), "nor does a restart after it");
    s.update_project(&p, &pinput("after the restore")).unwrap();
    assert_eq!(s.file().data.projects[&p].stamp.at, floor.at.later(1));
}

/// Addendum A1.4 P15: a damaged database set aside still gives its newest stamp, and when a change was last saved
/// here, to the one that replaces it. The clock was put back meanwhile, so both are ahead of it.
#[test]
fn a_damaged_database_set_aside_still_gives_its_newest_stamp() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(T0);
    let mut s = alone(dir.path(), &clock);
    let p = s.create_project(&pinput("P")).unwrap();
    for i in 0..3 {
        s.update_project(&p, &pinput(&format!("P{i}"))).unwrap();
    }
    let floor = s.newest_seen().cloned().unwrap();
    let changed = s.file().latest_change_here;
    assert_eq!(changed, Some(s.now()));
    drop(s);
    rusqlite::Connection::open(dir.path().join(crate::store::DB_FILE))
        .unwrap()
        .execute(
            "INSERT INTO records(kind, id, json) VALUES ('widgets', 'w', '{}')",
            [],
        )
        .unwrap();
    clock.advance(-HOUR);
    let s = alone(dir.path(), &clock);
    assert!(
        s.snapshot()
            .notices
            .iter()
            .any(|n| n.message.contains("The data file was damaged")),
        "it was set aside"
    );
    assert!(
        s.file().data.projects.is_empty(),
        "no backup to restore from"
    );
    assert_eq!(s.newest_seen(), Some(&floor));
    assert_eq!(s.file().newest_stamp.as_ref(), Some(&floor));
    assert_eq!(s.file().latest_change_here, changed);
    assert_eq!(s.clock_behind().map(|v| v.behind_secs), Some(HOUR));
}

/// The fields of `export interface <name>` in 1.x `src/lib/types.ts` (copied from a88c1aa to `tests/1x-src/lib/`):
/// name (with `?` when optional) and TS type.
fn ts_interface(name: &str) -> Vec<(String, String)> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/1x-src/lib/types.ts");
    let text = std::fs::read_to_string(path).unwrap();
    let body = text
        .split_once(&format!("export interface {name} {{"))
        .unwrap()
        .1
        .split_once("\n}")
        .unwrap()
        .0;
    body.lines()
        .map(str::trim)
        .filter(|l| {
            !(l.is_empty() || l.starts_with("/*") || l.starts_with('*') || l.starts_with("//"))
        })
        .filter_map(|l| {
            let (field, ty) = l.strip_suffix(';')?.split_once(':')?;
            Some((field.trim().to_owned(), ty.trim().to_owned()))
        })
        .collect()
}

/// Acceptance 4: the view serialises to exactly the fields and types of the TS `ClockBehindView` (design §6.3, addendum
/// P6), `causeDevice` as null rather than left out, and the snapshot always carries `clockBehind`.
#[test]
fn the_clock_behind_view_serialises_as_the_ts_type_says() {
    let ts = ts_interface("ClockBehindView");
    assert_eq!(
        ts,
        [
            ("behindSecs", "number"),
            ("newestAt", "string"),
            ("causeDevice", "string | null"),
            ("capped", "boolean"),
        ]
        .map(|(n, t)| (n.to_owned(), t.to_owned()))
    );
    let newest_at = Ts::parse(T0).unwrap();
    for view in [
        ClockBehindView {
            behind_secs: 5400,
            newest_at,
            cause_device: Some("LAPTOP".to_owned()),
            capped: false,
        },
        ClockBehindView {
            behind_secs: 2 * DAY,
            newest_at,
            cause_device: None,
            capped: true,
        },
    ] {
        let json = serde_json::to_value(&view).unwrap();
        let fields = json.as_object().unwrap();
        let names: BTreeSet<&str> = fields.keys().map(String::as_str).collect();
        let declared: BTreeSet<&str> = ts.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, declared, "every field always sent, none other");
        for (name, ty) in &ts {
            let value = &fields[name.as_str()];
            let fits = match ty.as_str() {
                "number" => value.is_i64(),
                "string" => value.is_string(),
                "string | null" => value.is_string() || value.is_null(),
                "boolean" => value.is_boolean(),
                other => panic!("no rule for the TS type {other}"),
            };
            assert!(fits, "{name}: {value} is not a {ty}");
        }
        assert_eq!(fields["newestAt"], json!("2026-03-02T09:00:00Z"));
    }
    let snapshot_field = ts_interface("Snapshot")
        .into_iter()
        .find(|(n, _)| n.trim_end_matches('?') == "clockBehind");
    assert_eq!(
        snapshot_field,
        Some((
            "clockBehind?".to_owned(),
            "ClockBehindView | null".to_owned()
        ))
    );
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(T0);
    let mut s = alone(dir.path(), &clock);
    let snapshot = serde_json::to_value(s.snapshot()).unwrap();
    assert_eq!(snapshot.get("clockBehind"), Some(&Value::Null));
    s.create_project(&pinput("P")).unwrap();
    clock.advance(-2 * HOUR);
    let snapshot = serde_json::to_value(s.snapshot()).unwrap();
    assert_eq!(
        snapshot["clockBehind"],
        json!({"behindSecs": 7200, "newestAt": "2026-03-02T09:00:00Z", "causeDevice": null, "capped": false})
    );
}
