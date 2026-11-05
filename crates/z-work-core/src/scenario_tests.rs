//! Two devices, a fake clock and a delayed folder: the lease.
//!
//! DESKTOP and LAPTOP (and TABLET where three are needed) share one cloud folder through `testkit`. Each device's lease
//! thread is a `tick`, its sync a `cycle`, and time passes for everyone with `FakeCloud::advance`. Numbers are the
//! design's test plan (section 12).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use crate::adjust::{CorrectionInput, ManualEntryInput, PauseInput};
use crate::catalog::{TagInput, TemplateInput};
use crate::error::CoreError;
use crate::lease::{
    LEASE_BEAT_SECS, LEASE_STALE_SECS, LeaseRecord, LeaseRecordState, LeaseStateName,
    MAX_GENERATION,
};
use crate::lease_io::{
    self, Intent, LEAVING_SECS, OFFER_TAKE_OVER_TO_A_LIVE_HOLDER, TakeOverExpected, TakeOverMode,
    TakeOverOutcome, TakeOverPreview, TakeOverResult,
};
use crate::machine::{RateInput, RoundingInput, ShiftInput, TaskExpected, TaskPatch};
use crate::model::{PauseReason, RoundMethod, TaskState, Theme, VersionReason};
use crate::service::span_text;
use crate::summary::SummaryRequest;
use crate::testkit::{Device, FakeCloud, LIMIT, doc, pinput, sync_all, tinput};
use crate::time::Ts;

const ROOT: &str = "/cloud/OneDrive";
const FREE: LeaseStateName = LeaseStateName::Free;
const MINE: LeaseStateName = LeaseStateName::HeldByMe;
const LIVE: LeaseStateName = LeaseStateName::HeldByOtherLive;
const STALE: LeaseStateName = LeaseStateName::HeldByOtherStale;

fn two() -> (Arc<FakeCloud>, Device, Device) {
    let cloud = FakeCloud::new(&[ROOT]);
    let a = Device::open(&cloud, "DESKTOP", &[ROOT]);
    let b = Device::open(&cloud, "LAPTOP", &[ROOT]);
    (cloud, a, b)
}

/// Time passes a minute at a time, and each device's lease thread does what is due.
fn minutes(cloud: &FakeCloud, devices: &[&Device], n: i64) {
    for _ in 0..n {
        cloud.advance(60);
        for d in devices {
            d.tick_if_due();
        }
    }
}

/// The lower id wins a tie.
fn by_id<'a>(a: &'a Device, b: &'a Device) -> (&'a Device, &'a Device) {
    if a.id() < b.id() { (a, b) } else { (b, a) }
}

fn hm(at: Ts) -> String {
    at.datetime().format("%H:%M").to_string()
}

fn record(cloud: &FakeCloud, d: &Device) -> LeaseRecord {
    cloud.lease_record(ROOT, &d.id()).unwrap()
}

fn expected(preview: &TakeOverPreview) -> TakeOverExpected {
    let h = preview.holder.as_ref().unwrap();
    TakeOverExpected {
        generation: h.generation,
        beat_seq: h.beat_seq,
    }
}

/// Takes over from the holder the dialog shows as inactive.
fn take_over_stale(d: &Device) -> TakeOverResult {
    let preview = lease_io::preview_take_over(&d.svc, &d.view, LIMIT);
    assert!(!preview.live, "shown as inactive");
    lease_io::take_over(
        &d.svc,
        &d.view,
        expected(&preview),
        TakeOverMode::Stale,
        LIMIT,
    )
    .unwrap()
}

/// DESKTOP starts timing and so holds the lease. LAPTOP has synced its data and is read-only. Returns the project, the
/// task and its segment.
fn desktop_timing(a: &Device, b: &Device) -> (String, String, String) {
    a.tick();
    b.tick();
    let (p, t) = a.task("Client", "Design");
    a.s().start_task(&t).unwrap();
    a.tick();
    assert_eq!(a.state(), MINE);
    sync_all(&[a, b]);
    b.tick();
    assert_eq!(b.state(), LIVE);
    (p, t, a.running())
}

/// As `desktop_timing`, then DESKTOP beats for half an hour and drops off the network with its timer running. Its
/// device file reached LAPTOP only at the start. LAPTOP watches until it looks inactive. Returns the project, task and
/// segment, and DESKTOP's last beat.
fn desktop_goes_silent(cloud: &FakeCloud, a: &Device, b: &Device) -> (String, String, String, Ts) {
    let (p, t, seg) = desktop_timing(a, b);
    minutes(cloud, &[a, b], 30);
    let last_beat = record(cloud, a).beat_at;
    a.view.set_offline(true);
    minutes(cloud, &[b], 16);
    assert_eq!(b.state(), STALE);
    (p, t, seg, last_beat)
}

/// Test 11.
#[test]
fn first_change_claims_and_other_goes_read_only() {
    let (cloud, a, b) = two();
    cloud.delay(&a.id(), &b.id(), 300);
    a.tick();
    b.tick();
    assert_eq!((a.state(), b.state()), (FREE, FREE));
    a.s().set_theme(Theme::Dark).unwrap();
    a.tick_if_due();
    assert_eq!(a.state(), FREE, "a preference claims nothing");
    assert!(cloud.lease_record(ROOT, &a.id()).is_none());

    a.s().create_project(&pinput("P")).unwrap();
    // A change that needs a claim doesn't wait for the next read.
    assert!(a.tick_if_due().changed);
    assert_eq!(a.state(), MINE);
    let held = record(&cloud, &a);
    assert_eq!(
        (held.state, held.generation, held.device_name.as_str()),
        (LeaseRecordState::Held, 1, "DESKTOP")
    );
    assert!(held.open_segments.is_empty());
    assert!(!a.s().read_only());
    assert_eq!(
        a.s().file().prefs.lease_memo.as_ref().unwrap().generation,
        1
    );

    // The laptop's copy of the folder is five minutes behind, and until then it's free there.
    b.tick();
    assert_eq!(b.state(), FREE);
    cloud.advance(300);
    b.tick();
    assert_eq!(b.state(), LIVE);
    assert!(b.s().read_only());
    let view = b.s().lease_view();
    let holder = view.holder.unwrap();
    assert_eq!(
        (holder.device_name.as_str(), holder.generation),
        ("DESKTOP", 1)
    );
    assert_eq!(view.generation, Some(1));
    let refused = b.s().create_project(&pinput("Q")).unwrap_err();
    assert_eq!(refused.code(), "read_only");
    assert_eq!(
        refused.to_string(),
        "DESKTOP is using zWorkTracker, so this computer is read-only. Take over from the banner to work here."
    );
    assert!(b.s().snapshot().lease.read_only);
}

/// Test 12. The refusal itself shows the inputs were good: the gate only refuses a change that went through on a copy.
#[test]
fn read_only_refuses_every_shared_change_and_allows_prefs() {
    let (cloud, a, b) = two();
    a.tick();
    b.tick();
    // The desktop holds the lease and makes one of everything.
    let (p, t) = a.task("Client", "Design");
    let (p2, t2) = a.task("Other", "Notes");
    let (p3, t3) = a.task("Billed", "Done");
    let (_, t4) = a.task("Paused", "Waiting");
    let empty = a.s().create_project(&pinput("Empty")).unwrap();
    let start = cloud.now();
    a.s().start_task(&t).unwrap();
    cloud.advance(3600);
    a.s().pause_task(&t).unwrap();
    cloud.advance(600);
    a.s().resume_task(&t).unwrap();
    cloud.advance(1800);
    a.s().pause_task(&t).unwrap();
    let mut logged: Vec<(Ts, String)> = a
        .s()
        .file()
        .data
        .segments
        .values()
        .filter(|x| x.task_id == t)
        .map(|x| (x.start, x.id.clone()))
        .collect();
    logged.sort();
    let (seg1, seg2) = (logged[0].1.clone(), logged[1].1.clone());
    a.s().void_segment(&seg2, "not work").unwrap();
    let pause = a
        .s()
        .mark_billable_pause(&PauseInput {
            task_id: t.clone(),
            start: start.later(600),
            end: start.later(1200),
            note: String::new(),
        })
        .unwrap();
    a.s().start_task(&t4).unwrap();
    cloud.advance(60);
    a.s().pause_task(&t4).unwrap();
    let by_hand = |task: &str, hours_ago: i64| ManualEntryInput {
        task_id: task.to_owned(),
        start: start.later(-hours_ago * 3600),
        end: start.later(-(hours_ago - 1) * 3600),
        reason: "forgot".into(),
        subtask_id: None,
        allow_overlap: true,
    };
    a.s().add_manual_entry(&by_hand(&t2, 3)).unwrap();
    a.s().add_manual_entry(&by_hand(&t3, 5)).unwrap();
    let today = |project: &str| SummaryRequest {
        project_id: project.to_owned(),
        from: "2026-03-02".into(),
        to: "2026-03-02".into(),
        zone: "UTC".into(),
    };
    let summary = a.s().create_summary(&today(&p3)).unwrap();
    let tag = a
        .s()
        .save_tag(&TagInput {
            id: None,
            name: "Urgent".into(),
            color: "#aa3300".into(),
        })
        .unwrap();
    let template_input = |name: &str| TemplateInput {
        id: None,
        name: name.into(),
        project_id: Some(p.clone()),
        description: doc("Plan"),
        tags: Vec::new(),
    };
    let template = a.s().save_template(&template_input("Weekly")).unwrap();
    let shift_input = |name: &str| ShiftInput {
        shift_id: None,
        effective_from: "2026-03-01".into(),
        name: name.into(),
        color: "#225588".into(),
        start: "09:00".into(),
        end: "17:00".into(),
        weekdays: vec![1, 2, 3, 4, 5],
        zone: "UTC".into(),
        break_minutes: 0,
        retired: false,
    };
    let shift = a.s().save_shift(&shift_input("Day")).unwrap();
    let rate_input = |cents: i64| RateInput {
        project_id: p.clone(),
        rate_cents: Some(cents),
        currency: "EUR".into(),
        effective_from: None,
        confirm_retroactive: true,
    };
    a.s().set_rate(&rate_input(5000)).unwrap();
    let rounding_input = |minutes: u32| RoundingInput {
        project_id: Some(p.clone()),
        effective_from: "2026-03-01".into(),
        increment_minutes: minutes,
        method: RoundMethod::Up,
        confirm_retroactive: true,
    };
    a.s()
        .set_rounding(&rounding_input(15), "2026-03-02")
        .unwrap();
    let rate = a.s().file().data.rates.keys().next().unwrap().clone();
    let rounding = a.s().file().data.rounding.keys().next().unwrap().clone();
    // And it is timing a task when the laptop looks.
    a.s().resume_task(&t).unwrap();
    let running = a.running();
    a.tick();
    sync_all(&[&a, &b]);
    b.tick();
    assert_eq!(b.state(), LIVE);
    let backup = {
        let s = b.s();
        s.run_daily_backup().unwrap();
        s.list_backups().unwrap()[0].name.clone()
    };

    let mut s = b.s();
    s.warm = true;
    // Asking the holder to pause is what a read-only computer can always do.
    s.request_remote_pause(&t).unwrap();
    let before = s.file().data.clone();
    let seg1_start = before.segments[&seg1].start;
    let refused: Vec<(&str, Result<(), CoreError>)> = vec![
        ("create_project", s.create_project(&pinput("New")).map(drop)),
        (
            "update_project",
            s.update_project(&p, &pinput("Client renamed")),
        ),
        ("set_project_archived", s.set_project_archived(&p2, true)),
        ("delete_project", s.delete_project(&empty)),
        ("create_task", s.create_task(&tinput("New", &p)).map(drop)),
        (
            "update_task",
            s.update_task(&t4, &tinput("Waiting longer", &p)),
        ),
        (
            "patch_task",
            s.patch_task(
                &t4,
                &TaskPatch {
                    name: Some("Patched".into()),
                    project_id: None,
                    description: None,
                    tags: None,
                },
                &TaskExpected::default(),
            )
            .map(drop),
        ),
        ("delete_task", s.delete_task(&t2)),
        ("start_task", s.start_task(&t3).map(drop)),
        ("resume_task", s.resume_task(&t4).map(drop)),
        ("stop_task", s.stop_task(&t4)),
        (
            "add_manual_entry",
            s.add_manual_entry(&by_hand(&t4, 8)).map(drop),
        ),
        (
            "correct_segment",
            s.correct_segment(&CorrectionInput {
                segment_id: seg1.clone(),
                start: seg1_start,
                end: seg1_start.later(1800),
                reason: "fix".into(),
                allow_overlap: true,
            }),
        ),
        ("void_segment", s.void_segment(&seg1, "wrong")),
        ("restore_segment", s.restore_segment(&seg2, "it was work")),
        (
            "mark_billable_pause",
            s.mark_billable_pause(&PauseInput {
                task_id: t.clone(),
                start: seg1_start.later(1800),
                end: seg1_start.later(2400),
                note: String::new(),
            })
            .map(drop),
        ),
        ("unmark_billable_pause", s.unmark_billable_pause(&pause)),
        (
            "save_tag",
            s.save_tag(&TagInput {
                id: None,
                name: "Later".into(),
                color: "#118844".into(),
            })
            .map(drop),
        ),
        ("delete_tag", s.delete_tag(&tag)),
        (
            "save_template",
            s.save_template(&template_input("Monthly")).map(drop),
        ),
        ("delete_template", s.delete_template(&template)),
        (
            "task_from_template",
            s.task_from_template(&template, Some(&p)).map(drop),
        ),
        ("save_shift", s.save_shift(&shift_input("Late")).map(drop)),
        ("delete_shift", s.delete_shift(&shift)),
        ("set_rate", s.set_rate(&rate_input(6000))),
        ("delete_rate_version", s.delete_rate_version(&rate, true)),
        (
            "set_rounding",
            s.set_rounding(&rounding_input(30), "2026-03-02"),
        ),
        ("delete_rounding", s.delete_rounding(&rounding)),
        (
            "set_setting",
            s.set_setting("myZone", json!("Europe/Lisbon")),
        ),
        ("create_summary", s.create_summary(&today(&p2)).map(drop)),
        ("void_summary", s.void_summary(&summary)),
        ("set_device_prefix", s.set_device_prefix("Q")),
        ("settle_lost_segment", s.settle_lost_segment(&running, "")),
        ("restore_backup", s.restore_backup(&backup).map(drop)),
    ];
    for (name, r) in refused {
        assert!(
            matches!(&r, Err(CoreError::ReadOnly { holder }) if holder == "DESKTOP"),
            "{name}: {r:?}"
        );
    }

    // This computer's own preferences, and what it is looking at.
    s.set_theme(Theme::Dark).unwrap();
    s.set_hide_earnings(true).unwrap();
    s.set_backup_keep(9).unwrap();
    s.mark_client_zone_prompt_done().unwrap();
    s.mark_sync_prompt_done().unwrap();
    s.mark_hide_notice_shown().unwrap();
    s.set_setting("activeZone", json!("Asia/Tokyo")).unwrap();
    s.set_setting("showBoth", json!(true)).unwrap();
    s.on_exit().unwrap();
    assert_eq!(s.file().prefs.theme, Theme::Dark);
    let after = &s.file().data;
    assert_eq!(after.projects, before.projects);
    assert_eq!(after.tasks, before.tasks);
    assert_eq!(after.segments, before.segments);
    assert_eq!(after.pauses, before.pauses);
    assert_eq!(after.tags, before.tags);
    assert_eq!(after.templates, before.templates);
    assert_eq!(after.shifts, before.shifts);
    assert_eq!(after.rates, before.rates);
    assert_eq!(after.rounding, before.rounding);
    assert_eq!(after.summaries, before.summaries);
    assert_eq!(after.prefixes, before.prefixes);
    assert_eq!(after.settings["activeZone"].value, json!("Asia/Tokyo"));
    assert!(!after.settings.contains_key("myZone"));
    assert!(!s.lease.want_claim, "nothing here asked for the lease");
    assert_eq!(s.lease.last_change_at, None);
    assert!(s.snapshot().lease.read_only);
}

/// Test 13.
#[test]
fn read_only_still_reads_merges_and_writes_own_file() {
    let (cloud, a, b) = two();
    desktop_timing(&a, &b);
    let p = a.s().create_project(&pinput("From the desktop")).unwrap();
    cloud.advance(60);
    a.cycle();
    let own_file = FakeCloud::device_file_path(ROOT, &b.id());
    let written = cloud.history(&own_file).len();
    assert!(b.cycle(), "merged");
    assert!(b.s().file().data.projects.contains_key(&p));
    assert!(b.s().read_only());
    let history = cloud.history(&own_file);
    assert_eq!(history.len(), written + 1, "and wrote its own file");
    assert_eq!(history.last().unwrap().0, b.id());
    a.cycle();
    assert!(a.s().file().remote_devices.contains_key(&b.id()));
    b.tick();
    assert_eq!(b.state(), LIVE, "still behind the desktop");
}

/// Test 14.
#[test]
fn simultaneous_claims_settle_on_one_holder_after_delay() {
    let (cloud, a, b) = two();
    a.tick();
    b.tick();
    // Neither sees what the other writes for now.
    cloud.hold_both(&a.id(), &b.id());
    a.s().create_project(&pinput("On the desktop")).unwrap();
    b.s().create_project(&pinput("On the laptop")).unwrap();
    a.tick();
    b.tick();
    assert_eq!((a.state(), b.state()), (MINE, MINE));
    assert_eq!(record(&cloud, &a).generation, 1);
    assert_eq!(record(&cloud, &b).generation, 1);

    cloud.advance(120);
    cloud.release_both(&a.id(), &b.id());
    let (winner, loser) = by_id(&a, &b);
    let out = loser.tick();
    assert!(out.lost && out.changed);
    assert_eq!(loser.state(), LIVE);
    assert!(loser.s().read_only());
    let said = record(&cloud, loser);
    assert_eq!(
        (said.state, said.generation),
        (LeaseRecordState::Released, 1),
        "the loser says so at once"
    );
    assert!(loser.notices().contains(&format!(
        "{} started making changes at the same time, so this computer is now read-only. Nothing was lost: it will sync.",
        winner.name()
    )));
    winner.tick();
    assert_eq!(winner.state(), MINE);
    assert_eq!(winner.s().lease.claim.as_ref().unwrap().generation, 1);
    loser.tick();
    assert_eq!(loser.state(), LIVE, "and it stays that way");

    // Both changes merge.
    sync_all(&[&a, &b]);
    for d in [&a, &b] {
        let names: BTreeSet<String> = d
            .s()
            .file()
            .data
            .projects
            .values()
            .map(|p| p.name.clone())
            .collect();
        assert!(names.contains("On the desktop") && names.contains("On the laptop"));
    }
}

/// Test 15.
#[test]
fn lost_race_closes_timer_keeps_work_and_notifies() {
    let (cloud, a, b) = two();
    a.tick();
    b.tick();
    cloud.hold_both(&a.id(), &b.id());
    let (_, ta) = a.task("Desk", "Desk work");
    let (_, tb) = b.task("Lap", "Lap work");
    a.s().start_task(&ta).unwrap();
    b.s().start_task(&tb).unwrap();
    a.tick();
    b.tick();
    minutes(&cloud, &[&a, &b], 10);
    assert_eq!((a.state(), b.state()), (MINE, MINE));
    cloud.release_both(&a.id(), &b.id());

    let (winner, loser) = by_id(&a, &b);
    let (task, task_name) = if loser.id() == a.id() {
        (&ta, "Desk work")
    } else {
        (&tb, "Lap work")
    };
    let seg = loser.running();
    // The winner sees the tie first, and moves past it at once.
    winner.tick();
    assert_eq!(winner.state(), MINE);
    assert_eq!(record(&cloud, winner).generation, 2);
    let out = loser.tick();
    assert!(out.lost);
    let now = loser.s().now();
    let s = loser.s();
    assert!(s.read_only());
    assert_eq!(
        s.file().data.segments[&seg].end,
        Some(now),
        "kept, up to now"
    );
    assert_eq!(s.file().data.tasks[task].state, TaskState::Paused);
    assert_eq!(
        s.file().data.tasks[task].pause_reason,
        Some(PauseReason::SyncReconciled)
    );
    let message = format!(
        "{} took over, so this computer is now read-only. \"{task_name}\" stopped timing here at {}. Nothing was lost: it will sync.",
        winner.name(),
        hm(now)
    );
    assert!(s.snapshot().notices.iter().any(|n| n.message == message));
    let audit = s.audit_entries(Some(&winner.id()));
    let lost = audit.iter().find(|e| e.action == "leaseLost").unwrap();
    assert_eq!(lost.entity, "lease");
    assert_eq!(
        lost.after,
        json!({"by": winner.name(), "generation": 2, "closed": [task]})
    );
    drop(s);
    assert_eq!(loser.s().take_pause_alerts(), vec![message]);

    // The ten minutes reach the winner.
    sync_all(&[&a, &b]);
    assert_eq!(winner.s().file().data.segments[&seg].end, Some(now));
}

/// Test 15a.
#[test]
fn start_during_checking_then_other_holder_demotes() {
    let (cloud, a, b) = two();
    a.tick();
    a.s().create_project(&pinput("P")).unwrap();
    a.tick();
    // The laptop starts work before its first look at the folder, as that never holds up a launch.
    assert_eq!(b.state(), LeaseStateName::Checking);
    let (_, t) = b.task("Lap", "Started early");
    b.s().start_task(&t).unwrap();
    let seg = b.running();
    cloud.advance(30);
    let out = b.tick();
    assert!(out.lost);
    assert_eq!(b.state(), LIVE);
    let now = b.s().now();
    assert_eq!(b.s().file().data.segments[&seg].end, Some(now));
    assert_eq!(b.s().file().data.tasks[&t].state, TaskState::Paused);
    assert!(b.notices().contains(&format!(
        "DESKTOP is using zWorkTracker, so this computer is now read-only. \"Started early\" stopped timing here at {}. Nothing was lost: it will sync.",
        hm(now)
    )));
    assert!(
        cloud.lease_record(ROOT, &b.id()).is_none(),
        "it never claimed"
    );
}

/// Test 15b.
#[test]
fn released_record_at_my_generation_keeps_my_claim_when_timer_runs() {
    let (cloud, a, b) = two();
    a.tick();
    b.tick();
    cloud.hold_both(&a.id(), &b.id());
    // The laptop makes one change and lets go fifteen minutes later.
    b.s().create_project(&pinput("Quick fix")).unwrap();
    b.tick();
    // The desktop, which can't see that, starts timing and claims the same generation.
    let (_, t) = a.task("Desk", "Long job");
    a.s().start_task(&t).unwrap();
    a.tick();
    minutes(&cloud, &[&a, &b], 16);
    assert_eq!(
        (record(&cloud, &b).state, record(&cloud, &b).generation),
        (LeaseRecordState::Released, 1)
    );
    cloud.release_both(&a.id(), &b.id());
    a.tick();
    assert_eq!(a.state(), MINE, "Held outranks Released at one generation");
    assert_eq!(a.s().lease.claim.as_ref().unwrap().generation, 1);
    b.tick();
    assert_eq!(b.state(), LIVE);

    // A release above this device's claim, while its timer still runs: it claims above that.
    cloud.hold_both(&a.id(), &b.id());
    minutes(&cloud, &[&a, &b], 16);
    assert_eq!(b.state(), STALE);
    assert_eq!(take_over_stale(&b).outcome, TakeOverOutcome::Done);
    minutes(&cloud, &[&a, &b], 16);
    let released = record(&cloud, &b);
    assert_eq!(
        (released.state, released.generation),
        (LeaseRecordState::Released, 2)
    );
    cloud.release_both(&a.id(), &b.id());
    a.tick();
    assert_eq!(a.state(), MINE);
    assert_eq!(a.s().lease.claim.as_ref().unwrap().generation, 3);
    assert_eq!(record(&cloud, &a).generation, 3);
    b.tick();
    assert_eq!(b.state(), LIVE);
}

/// Test 15c.
#[test]
fn tick_in_flight_at_quit_cannot_rewrite_held_after_released() {
    // A pass has decided to beat when the app quits, and writes after the release.
    let (cloud, a, _b) = two();
    a.tick();
    let (_, t) = a.task("Desk", "Work");
    a.s().start_task(&t).unwrap();
    a.tick();
    cloud.advance(LEASE_BEAT_SECS);
    let plan = a.s().lease_plan(true).unwrap();
    let reads = lease_io::read_all(&a.view, &plan, LIMIT);
    let step = a.s().lease_decide(&plan, &reads);
    let beat = step.write.as_ref().unwrap();
    assert_eq!(beat.intent, Intent::Beat);
    a.quit(LIMIT);
    assert_eq!(record(&cloud, &a).state, LeaseRecordState::Released);
    let wrote = lease_io::write_all(&a.view, &plan, beat, LIMIT, "lease");
    assert!(
        wrote.iter().all(|(_, w)| matches!(w, Ok(false))),
        "{wrote:?}"
    );
    a.s().lease_apply(&reads, step, &wrote);
    assert_eq!(record(&cloud, &a).state, LeaseRecordState::Released);
    assert!(a.s().lease.claim.is_none());
    // Nothing after quitting writes one either, not even for a change.
    a.s().create_project(&pinput("Late")).unwrap();
    a.tick();
    assert_eq!(record(&cloud, &a).state, LeaseRecordState::Released);

    // A beat already being written when the app quits: the release waits its turn in that folder and lands after it.
    let (cloud, a, _b) = two();
    a.tick();
    let (_, t) = a.task("Desk", "Work");
    a.s().start_task(&t).unwrap();
    a.tick();
    cloud.advance(LEASE_BEAT_SECS);
    let gate = a
        .view
        .gate_next_write(&FakeCloud::lease_path(ROOT, &a.id()));
    std::thread::scope(|scope| {
        let beating = scope.spawn(|| a.tick());
        assert!(gate.wait_entered(), "the beat is being written");
        a.s().on_exit().unwrap();
        lease_io::release(&a.svc, &a.view, Duration::from_millis(200));
        gate.open();
        beating.join().unwrap();
    });
    // The release outlived its limit waiting for the folder, and lands on its own thread: up to 10 s for a busy machine.
    let mut waited = 0;
    while record(&cloud, &a).state != LeaseRecordState::Released && waited < 500 {
        std::thread::sleep(Duration::from_millis(20));
        waited += 1;
    }
    let written = cloud.lease_history(ROOT, &a.id());
    let n = written.len();
    assert_eq!(written[n - 1].state, LeaseRecordState::Released);
    assert_eq!(
        written[n - 2].state,
        LeaseRecordState::Held,
        "the beat landed first"
    );
    assert!(a.s().lease.closing);
}

/// Test 16.
#[test]
fn idle_release_after_15_minutes_without_timer() {
    let (cloud, a, b) = two();
    a.tick();
    b.tick();
    a.s().create_project(&pinput("P")).unwrap();
    a.tick();
    minutes(&cloud, &[&a, &b], 14);
    assert_eq!((a.state(), b.state()), (MINE, LIVE));
    minutes(&cloud, &[&a, &b], 1);
    assert_eq!(a.state(), FREE);
    let released = record(&cloud, &a);
    assert_eq!(
        (released.state, released.generation),
        (LeaseRecordState::Released, 1)
    );
    assert_eq!(
        a.s().file().prefs.lease_memo.as_ref().unwrap().generation,
        1,
        "remembered"
    );
    b.tick();
    assert_eq!(b.state(), FREE);
    b.s().create_project(&pinput("Q")).unwrap();
    b.tick();
    assert_eq!(b.state(), MINE);
    assert_eq!(record(&cloud, &b).generation, 2);
    a.tick();
    assert_eq!(a.state(), LIVE);
}

/// Test 17.
#[test]
fn timer_keeps_lease_past_idle_limit() {
    let (cloud, a, b) = two();
    a.tick();
    b.tick();
    let (_, t) = a.task("Desk", "Long job");
    a.s().start_task(&t).unwrap();
    let seg = a.running();
    a.tick();
    minutes(&cloud, &[&a, &b], 40);
    assert_eq!((a.state(), b.state()), (MINE, LIVE));
    let held = record(&cloud, &a);
    assert_eq!(held.open_segments, vec![seg]);
    assert!(held.beat_seq >= 20, "a beat every two minutes");
    // Once it pauses, the idle clock runs from that change.
    a.s().pause_task(&t).unwrap();
    minutes(&cloud, &[&a, &b], 14);
    assert_eq!(a.state(), MINE);
    assert!(record(&cloud, &a).open_segments.is_empty());
    minutes(&cloud, &[&a, &b], 1);
    assert_eq!(a.state(), FREE);
}

/// Test 18.
#[test]
fn clean_quit_releases_and_other_becomes_free() {
    let (cloud, a, b) = two();
    let (_, t, _) = desktop_timing(&a, &b);
    cloud.advance(60);
    a.quit(Duration::from_millis(1500));
    let released = record(&cloud, &a);
    assert_eq!(released.state, LeaseRecordState::Released);
    assert!(released.open_segments.is_empty());
    assert_eq!(a.s().file().data.tasks[&t].state, TaskState::Paused);
    b.tick();
    assert_eq!(b.state(), FREE);
    assert!(!b.s().read_only());
    b.s().create_project(&pinput("Q")).unwrap();
    b.tick();
    assert_eq!(b.state(), MINE);
    assert_eq!(record(&cloud, &b).generation, 2);
}

/// Test 19.
#[test]
fn crash_relaunch_resumes_same_generation() {
    let (cloud, a, b) = two();
    a.tick();
    b.tick();
    a.s().create_project(&pinput("P")).unwrap();
    a.tick();
    minutes(&cloud, &[&a, &b], 6);
    let before = record(&cloud, &a);
    let a = a.relaunch();
    assert_eq!(a.state(), LeaseStateName::Checking);
    cloud.advance(30);
    a.tick();
    assert_eq!(a.state(), MINE);
    let claim = a.s().lease.claim.clone().unwrap();
    assert_eq!(
        (claim.generation, claim.acquired_at),
        (before.generation, before.acquired_at)
    );
    let after = record(&cloud, &a);
    assert_eq!(after.generation, before.generation);
    assert!(
        after.beat_seq > before.beat_seq,
        "beats go on from the record"
    );
    b.tick();
    assert_eq!(b.state(), LIVE, "the laptop never saw it go");
    // The idle clock starts at the launch.
    minutes(&cloud, &[&a, &b], 14);
    assert_eq!(a.state(), MINE);
    minutes(&cloud, &[&a, &b], 1);
    assert_eq!(a.state(), FREE);
}

/// Test 20.
#[test]
fn stale_takeover_settles_at_last_lease_beat_and_audits() {
    let (cloud, a, b) = two();
    let (_, t, seg, last_beat) = desktop_goes_silent(&cloud, &a, &b);
    let preview = lease_io::preview_take_over(&b.svc, &b.view, LIMIT);
    assert!(!preview.live && !preview.unreachable);
    let holder = preview.holder.clone().unwrap();
    assert_eq!(holder.device_name, "DESKTOP");
    assert!(!holder.clock_suspect, "gone quiet, not skewed");
    assert_eq!(preview.segments.len(), 1);
    let shown = &preview.segments[0];
    assert_eq!(
        (shown.segment_id.as_str(), shown.task_name.as_str()),
        (seg.as_str(), "Design")
    );
    let start = b.s().file().data.segments[&seg].start;
    assert_eq!(
        shown.end, last_beat,
        "its last lease beat, fresher than its file"
    );
    assert_eq!(shown.secs, last_beat.secs() - start.secs());

    let result = lease_io::take_over(
        &b.svc,
        &b.view,
        expected(&preview),
        TakeOverMode::Stale,
        LIMIT,
    )
    .unwrap();
    assert_eq!(result.outcome, TakeOverOutcome::Done);
    assert!(result.preview.is_none());
    assert_eq!(result.snapshot.lease.state, MINE);
    assert!(!result.snapshot.lease.read_only);
    let s = b.s();
    let settled = &s.file().data.segments[&seg];
    let edit = settled.edit.clone().unwrap();
    assert!(edit.settled);
    assert_eq!(edit.end, last_beat);
    assert_eq!(settled.end, None, "the raw end is the desktop's to write");
    assert_eq!(s.file().data.tasks[&t].state, TaskState::Paused);
    let audit = s.audit_entries(Some(&seg));
    let closed = audit.iter().find(|e| e.action == "settleLost").unwrap();
    assert_eq!(
        closed.reason,
        "Closed from LAPTOP: DESKTOP stopped responding. Ended at its last known moment."
    );
    assert!(s.snapshot().notices.iter().any(|n| n.message
        == "You took over from DESKTOP. 1 running task was closed at its last known time."));
    drop(s);
    assert_eq!(b.s().take_version_due(), Some(VersionReason::TakeOver));
    let claimed = record(&cloud, &b);
    assert_eq!(
        (claimed.state, claimed.generation),
        (LeaseRecordState::Held, holder.generation + 1)
    );
}

/// Test 21.
#[test]
fn takeover_refused_as_changed_when_holder_beats_during_dialog() {
    let (cloud, a, b) = two();
    let (_, _, seg, _) = desktop_goes_silent(&cloud, &a, &b);
    let shown = lease_io::preview_take_over(&b.svc, &b.view, LIMIT);
    // The desktop comes back while the dialog is open, and beats.
    a.view.set_offline(false);
    a.tick();
    let result = lease_io::take_over(
        &b.svc,
        &b.view,
        expected(&shown),
        TakeOverMode::Stale,
        LIMIT,
    )
    .unwrap();
    assert_eq!(result.outcome, TakeOverOutcome::Changed);
    let fresh = result.preview.unwrap();
    assert!(fresh.live);
    assert!(fresh.holder.unwrap().beat_seq > expected(&shown).beat_seq);
    assert!(result.snapshot.lease.read_only);
    assert!(
        cloud.lease_record(ROOT, &b.id()).is_none(),
        "nothing written"
    );
    assert!(
        b.s().file().data.segments[&seg].edit.is_none(),
        "nothing settled"
    );
}

/// Test 22.
#[test]
fn holder_returns_after_takeover_goes_read_only_and_real_end_comes_back() {
    let (cloud, a, b) = two();
    let (_, _, seg, last_beat) = desktop_goes_silent(&cloud, &a, &b);
    take_over_stale(&b);
    // The app syncs right after a take over, so the settlement is in the folder when the desktop returns.
    b.cycle();
    minutes(&cloud, &[&b], 14);
    // The desktop was working offline all along, and comes back.
    a.view.set_offline(false);
    let out = a.tick();
    assert!(out.lost);
    assert_eq!(a.state(), LIVE);
    let real_end = a.s().now();
    assert_eq!(a.s().file().data.segments[&seg].end, Some(real_end));
    assert!(a.notices().contains(&format!(
        "LAPTOP took over, so this computer is now read-only. \"Design\" stopped timing here at {}. Nothing was lost: it will sync.",
        hm(real_end)
    )));
    sync_all(&[&a, &b]);
    for d in [&a, &b] {
        let s = d.s();
        let back = &s.file().data.segments[&seg];
        assert_eq!(back.effective(), Some((back.start, Some(real_end))));
    }
    let told = a.notices();
    assert!(
        told.contains(&format!(
            "\"Design\": {} logged here after LAPTOP closed it is back. Correct it in task details if that time wasn't work.",
            span_text(real_end.secs() - last_beat.secs())
        )),
        "{told:#?}"
    );
    assert_eq!(b.state(), MINE);
}

/// Test 23.
#[test]
fn return_with_billed_segment_keeps_settlement_and_says_so() {
    let (cloud, a, b) = two();
    let (p, _, seg, last_beat) = desktop_goes_silent(&cloud, &a, &b);
    take_over_stale(&b);
    let summary = b
        .s()
        .create_summary(&SummaryRequest {
            project_id: p,
            from: "2026-03-02".into(),
            to: "2026-03-02".into(),
            zone: "UTC".into(),
        })
        .unwrap();
    let reference = b.s().file().data.summaries[&summary].reference.clone();
    cloud.advance(600);
    a.view.set_offline(false);
    assert!(a.tick().lost);
    let real_end = a.s().now();
    sync_all(&[&a, &b]);
    for d in [&a, &b] {
        let s = d.s();
        let kept = &s.file().data.segments[&seg];
        assert_eq!(kept.effective(), Some((kept.start, Some(last_beat))));
    }
    let told = format!(
        "\"Design\": the time on {reference} differs from what this computer recorded by {}. To change it, void the summary, then restore the segment or add the time by hand.",
        span_text(real_end.secs() - last_beat.secs())
    );
    assert_eq!(a.notices().iter().filter(|n| **n == told).count(), 1);
    sync_all(&[&a, &b]);
    assert_eq!(
        a.notices().iter().filter(|n| **n == told).count(),
        1,
        "said once"
    );
    assert!(!b.notices().contains(&told), "only its owner hears it");
}

/// Test 25a, first part: the desktop paused long ago and only went on editing, so its beats say nothing of the timer.
#[test]
fn beat_ignored_when_open_segments_omits_it_or_clock_suspect() {
    let (cloud, a, b) = two();
    a.tick();
    b.tick();
    let (p, t) = a.task("Client", "Design");
    a.s().start_task(&t).unwrap();
    let seg = a.running();
    a.tick();
    sync_all(&[&a, &b]);
    b.tick();
    let start = b.s().file().data.segments[&seg].start;
    cloud.advance(600);
    a.s().pause_task(&t).unwrap();
    for i in 0..30 {
        cloud.advance(60);
        if i % 5 == 0 {
            a.s()
                .update_project(&p, &pinput(&format!("Client {i}")))
                .unwrap();
        }
        a.tick_if_due();
        b.tick_if_due();
    }
    let last = record(&cloud, &a);
    assert!(last.open_segments.is_empty());
    a.view.set_offline(true);
    minutes(&cloud, &[&b], 16);
    let preview = lease_io::preview_take_over(&b.svc, &b.view, LIMIT);
    assert_eq!(
        preview.segments[0].end, start,
        "what its file said, not its beat at {}",
        last.beat_at
    );
    take_over_stale(&b);
    assert_eq!(
        b.s().file().data.segments[&seg].edit.as_ref().unwrap().end,
        start
    );

    // Second part: the desktop's clock is an hour behind, so its beats can't be believed, whatever they list.
    let (cloud, a, b) = two();
    a.clock.advance(-3600);
    let (_, _, seg, last_beat) = desktop_goes_silent(&cloud, &a, &b);
    assert!(record(&cloud, &a).open_segments.contains(&seg));
    let preview = lease_io::preview_take_over(&b.svc, &b.view, LIMIT);
    assert!(preview.holder.as_ref().unwrap().clock_suspect);
    let start = b.s().file().data.segments[&seg].start;
    assert_eq!(
        preview.segments[0].end, start,
        "not its beat at {last_beat}"
    );
    take_over_stale(&b);
    assert_eq!(
        b.s().file().data.segments[&seg].edit.as_ref().unwrap().end,
        start
    );
}

/// Test 25b. The desktop hung after its last heartbeat while its lease thread went on beating, then was killed: the
/// settlement at the beat is later than the end the desktop records when it opens again.
#[test]
fn late_device_file_with_earlier_end_lifts_settlement() {
    let (cloud, a, b) = two();
    let (_, _, seg) = desktop_timing(&a, &b);
    minutes(&cloud, &[&a, &b], 20);
    a.s().heartbeat().unwrap();
    let heartbeat = a.s().now();
    minutes(&cloud, &[&a, &b], 10);
    let last_beat = record(&cloud, &a).beat_at;
    assert!(last_beat > heartbeat);
    a.view.set_offline(true);
    minutes(&cloud, &[&b], 16);
    take_over_stale(&b);
    assert_eq!(
        b.s().file().data.segments[&seg].edit.as_ref().unwrap().end,
        last_beat
    );

    let a = a.relaunch();
    a.view.set_offline(false);
    assert_eq!(a.s().file().data.segments[&seg].end, Some(heartbeat));
    a.tick();
    assert_eq!(a.state(), LIVE, "nothing to stop: it only reads");
    sync_all(&[&a, &b]);
    for d in [&a, &b] {
        let s = d.s();
        let lifted = &s.file().data.segments[&seg];
        assert_eq!(lifted.effective(), Some((lifted.start, Some(heartbeat))));
    }
    assert!(
        !a.notices().iter().any(|n| n.contains("is back")),
        "no time comes back"
    );
}

/// Test 26.
#[test]
fn two_takeovers_at_once_converge() {
    let cloud = FakeCloud::new(&[ROOT]);
    let a = Device::open(&cloud, "DESKTOP", &[ROOT]);
    let b = Device::open(&cloud, "LAPTOP", &[ROOT]);
    let c = Device::open(&cloud, "TABLET", &[ROOT]);
    c.tick();
    let (_, _, seg) = desktop_timing(&a, &b);
    sync_all(&[&a, &c]);
    minutes(&cloud, &[&a, &b, &c], 30);
    a.view.set_offline(true);
    minutes(&cloud, &[&b, &c], 16);
    cloud.hold_both(&b.id(), &c.id());
    for d in [&b, &c] {
        assert_eq!(take_over_stale(d).outcome, TakeOverOutcome::Done);
        assert_eq!(record(&cloud, d).generation, 2);
    }
    cloud.release_both(&b.id(), &c.id());
    let (winner, loser) = by_id(&b, &c);
    assert!(loser.tick().lost);
    assert_eq!(record(&cloud, loser).state, LeaseRecordState::Released);
    assert!(loser.notices().contains(&format!(
        "{} started making changes at the same time, so this computer is now read-only. Nothing was lost: it will sync.",
        winner.name()
    )));
    winner.tick();
    assert_eq!(winner.state(), MINE);
    sync_all(&[&b, &c]);
    let edit = |d: &Device| d.s().file().data.segments[&seg].edit.clone();
    assert!(edit(&b).is_some_and(|e| e.settled));
    assert_eq!(edit(&b), edit(&c), "the settlements converge");
    for d in [&b, &c] {
        let by: BTreeSet<String> = d
            .s()
            .audit_entries(Some(&seg))
            .into_iter()
            .filter(|e| e.action == "settleLost")
            .map(|e| e.by)
            .collect();
        assert_eq!(by, BTreeSet::from([b.id(), c.id()]), "both are recorded");
    }
}

/// Test 27. Take over is offered for a holder that looks inactive, so the holder here has been silent for longer than it
/// takes to look that way when the folder comes back.
#[test]
fn offline_takeover_claims_on_return_unless_overtaken() {
    // The folder comes back with the same holder, which looks inactive by then: the take over goes through then.
    let (cloud, a, b) = two();
    let (_, _, seg) = desktop_timing(&a, &b);
    b.view.set_offline(true);
    b.tick();
    assert_eq!(b.state(), LeaseStateName::Unknown);
    assert!(b.s().read_only(), "the desktop held it a moment ago");
    let preview = lease_io::preview_take_over(&b.svc, &b.view, LIMIT);
    assert!(preview.unreachable && preview.segments.is_empty());
    let holder = preview.holder.clone().unwrap();
    assert_eq!(
        (holder.device_name.as_str(), holder.beat_seq),
        ("DESKTOP", 0)
    );
    let mode = |m| lease_io::take_over(&b.svc, &b.view, expected(&preview), m, LIMIT);
    assert_eq!(
        mode(TakeOverMode::Stale).unwrap().outcome,
        TakeOverOutcome::Unreachable
    );
    assert_eq!(
        mode(TakeOverMode::Offline).unwrap().outcome,
        TakeOverOutcome::Done
    );
    assert!(!b.s().read_only());
    let p = b.s().create_project(&pinput("Offline work")).unwrap();
    assert!(
        b.s().file().data.segments[&seg].edit.is_none(),
        "nothing settled yet"
    );
    cloud.advance(LEASE_STALE_SECS + 60);
    b.view.set_offline(false);
    b.tick();
    assert_eq!(b.state(), MINE);
    assert_eq!(b.s().lease.claim.as_ref().unwrap().generation, 2);
    assert!(b.s().lease.offline_claim.is_none());
    assert!(
        b.s().file().data.segments[&seg]
            .edit
            .as_ref()
            .is_some_and(|e| e.settled)
    );
    assert!(b.notices().contains(
        &"You took over from DESKTOP. 1 running task was closed at its last known time.".to_owned()
    ));
    assert!(a.tick().lost);
    sync_all(&[&a, &b]);
    assert!(a.s().file().data.projects.contains_key(&p));

    // Another took over meanwhile: the claim is dropped, the timer here stops, and the work merges.
    let cloud = FakeCloud::new(&[ROOT]);
    let a = Device::open(&cloud, "DESKTOP", &[ROOT]);
    let b = Device::open(&cloud, "LAPTOP", &[ROOT]);
    let c = Device::open(&cloud, "TABLET", &[ROOT]);
    c.tick();
    desktop_timing(&a, &b);
    sync_all(&[&a, &c]);
    c.tick();
    b.view.set_offline(true);
    b.tick();
    let preview = lease_io::preview_take_over(&b.svc, &b.view, LIMIT);
    let chosen = lease_io::take_over(
        &b.svc,
        &b.view,
        expected(&preview),
        TakeOverMode::Offline,
        LIMIT,
    )
    .unwrap();
    assert_eq!(chosen.outcome, TakeOverOutcome::Done);
    let (_, tb) = b.task("Lap", "Offline work");
    b.s().start_task(&tb).unwrap();
    a.view.set_offline(true);
    minutes(&cloud, &[&b, &c], 16);
    assert_eq!(take_over_stale(&c).outcome, TakeOverOutcome::Done);
    cloud.advance(300);
    b.view.set_offline(false);
    assert!(b.tick().lost);
    let now = b.s().now();
    assert_eq!(b.state(), LIVE);
    assert!(b.s().lease.offline_claim.is_none());
    assert_eq!(b.s().file().data.tasks[&tb].state, TaskState::Paused);
    assert!(b.notices().contains(&format!(
        "TABLET took over while this computer was offline, so this computer is now read-only. \"Offline work\" stopped timing here at {}. What you did here is kept and will merge.",
        hm(now)
    )));
    assert!(
        cloud.lease_record(ROOT, &b.id()).is_none(),
        "it never claimed"
    );
    sync_all(&[&b, &c]);
    assert!(c.s().file().data.tasks.contains_key(&tb));
}

/// Test 27, with a holder that still looks in use when the folder comes back. "Work here anyway" is no way past a computer
/// in use unless D2 is on: then the claim made offline goes through, as it does for an inactive one. As it ships, take
/// over is offered once the holder looks inactive, so the claim is dropped, this computer goes back to read-only with a
/// notice, DESKTOP is left as it was, and what this computer did meanwhile is kept and merges.
#[test]
fn offline_takeover_from_a_holder_still_in_use_follows_d2_on_return() {
    let (cloud, a, b) = two();
    let (_, _, seg) = desktop_timing(&a, &b);
    b.view.set_offline(true);
    b.tick();
    assert_eq!(b.state(), LeaseStateName::Unknown);
    let preview = lease_io::preview_take_over(&b.svc, &b.view, LIMIT);
    let chosen = lease_io::take_over(
        &b.svc,
        &b.view,
        expected(&preview),
        TakeOverMode::Offline,
        LIMIT,
    )
    .unwrap();
    assert_eq!(chosen.outcome, TakeOverOutcome::Done);
    assert!(!b.s().read_only());
    let p = b.s().create_project(&pinput("Offline work")).unwrap();
    // The folder is back after ten minutes, and DESKTOP, last seen a moment before it went away, still looks in use.
    cloud.advance(600);
    b.view.set_offline(false);
    let out = b.tick();
    if OFFER_TAKE_OVER_TO_A_LIVE_HOLDER {
        assert_eq!(b.state(), MINE);
        assert_eq!(b.s().lease.claim.as_ref().unwrap().generation, 2);
        assert!(b.s().lease.offline_claim.is_none());
        assert!(
            b.s().file().data.segments[&seg]
                .edit
                .as_ref()
                .is_some_and(|e| e.settled)
        );
        assert!(
            b.notices().contains(
                &"You took over from DESKTOP. 1 running task was closed at its last known time."
                    .to_owned()
            )
        );
        assert!(a.tick().lost);
    } else {
        assert!(out.lost, "the claim made offline is dropped");
        assert_eq!(b.state(), LIVE);
        assert!(b.s().read_only());
        assert!(b.s().lease.offline_claim.is_none());
        assert!(
            cloud.lease_record(ROOT, &b.id()).is_none(),
            "it never claimed"
        );
        assert!(
            b.s().file().data.segments[&seg].edit.is_none(),
            "DESKTOP's timer is left as it was"
        );
        assert!(b.notices().contains(
            &"DESKTOP is using zWorkTracker, so this computer is now read-only. What you did here is kept and will merge."
                .to_owned()
        ));
        assert!(!a.tick().lost, "DESKTOP goes on holding it");
        assert_eq!(a.state(), MINE);
    }
    sync_all(&[&a, &b]);
    assert!(
        a.s().file().data.projects.contains_key(&p),
        "what was done here merges either way"
    );
}

/// Test 28.
#[test]
fn sleep_resume_reads_before_beating() {
    let (cloud, a, b) = two();
    let (_, _, seg) = desktop_timing(&a, &b);
    // The desktop sleeps: its lease thread doesn't run, and the laptop takes over meanwhile.
    minutes(&cloud, &[&b], 16);
    assert_eq!(take_over_stale(&b).outcome, TakeOverOutcome::Done);
    minutes(&cloud, &[&b], 4);
    let written = cloud.lease_history(ROOT, &a.id()).len();
    // It wakes. Its beat is long overdue, but it reads first.
    let out = a.tick_if_due();
    assert!(out.lost);
    assert_eq!(
        cloud.lease_history(ROOT, &a.id()).len(),
        written,
        "nothing written"
    );
    assert_eq!(a.state(), LIVE);
    let now = a.s().now();
    assert_eq!(a.s().file().data.segments[&seg].end, Some(now));
}

/// Test 30.
#[test]
fn lease_conflict_copy_ignored_and_never_deleted() {
    let (cloud, a, b) = two();
    a.tick();
    b.tick();
    a.s().create_project(&pinput("P")).unwrap();
    a.tick();
    b.tick();
    let copy = cloud.conflict_copy(&FakeCloud::lease_path(ROOT, &a.id()), " (1)");
    // A copy that would outrank everything if it were read.
    let third = "c0000000-0000-4000-8000-000000000003";
    let forged = LeaseRecord {
        device_id: third.to_owned(),
        generation: 99,
        ..record(&cloud, &a)
    };
    let forged_path =
        lease_io::lease_dir(std::path::Path::new(ROOT)).join(format!("{third}-PC2.json"));
    cloud.put(&forged_path, &serde_json::to_vec(&forged).unwrap());
    cloud.advance(60);
    a.tick();
    b.tick();
    assert_eq!((a.state(), b.state()), (MINE, LIVE));
    assert_eq!(b.s().lease.holder.as_ref().unwrap().device_id, a.id());
    for d in [&a, &b] {
        assert_eq!(d.s().snapshot().sync.targets[0].status.ignored_files, 2);
    }
    // Nothing ever removes them, through a release and a new claim.
    a.quit(LIMIT);
    b.tick();
    b.s().create_project(&pinput("Q")).unwrap();
    b.tick();
    assert_eq!(b.state(), MINE);
    assert!(cloud.exists(&copy) && cloud.exists(&forged_path));
}

/// Test 32.
#[test]
fn deleted_lease_file_recovers() {
    let (cloud, a, b) = two();
    desktop_timing(&a, &b);
    let path = FakeCloud::lease_path(ROOT, &a.id());
    cloud.delete(&path);
    b.tick();
    assert_eq!(b.state(), FREE, "the holder vanished from view");
    // The holder puts it back at once, not at its next beat.
    a.tick();
    assert_eq!(record(&cloud, &a).generation, 1);
    b.tick();
    assert_eq!(b.state(), LIVE);

    // Deleted again, and the laptop claims meanwhile: above what it last knew, so the desktop stops.
    cloud.delete(&path);
    b.tick();
    assert_eq!(b.state(), FREE);
    b.s().create_project(&pinput("Q")).unwrap();
    b.tick();
    assert_eq!(b.state(), MINE);
    assert_eq!(record(&cloud, &b).generation, 2);
    assert!(a.tick().lost);
    assert!(a.s().read_only());
}

/// Test 33.
#[test]
fn old_lease_file_restored_by_cloud_does_not_demote() {
    let (cloud, a, b) = two();
    a.tick();
    b.tick();
    a.s().create_project(&pinput("P")).unwrap();
    a.tick();
    minutes(&cloud, &[&a, &b], 15);
    assert_eq!(a.state(), FREE);
    a.s().create_project(&pinput("Q")).unwrap();
    a.tick();
    minutes(&cloud, &[&a, &b], 2);
    assert_eq!(record(&cloud, &a).generation, 2);
    let path = FakeCloud::lease_path(ROOT, &a.id());
    // The cloud client brings back the first copy, from generation 1.
    let versions = cloud.history(&path).len();
    cloud.restore_old(&path, versions - 1);
    assert_eq!(record(&cloud, &a).generation, 1);
    b.tick();
    assert!(b.s().read_only());
    // The claim in memory decides, and the file is put right at once.
    assert!(!a.tick().lost);
    assert_eq!(a.state(), MINE);
    assert_eq!(record(&cloud, &a).generation, 2);
    // The same with the copy of its idle release, which would say it was free.
    let released = cloud
        .history(&path)
        .iter()
        .rposition(|(_, bytes)| {
            bytes.as_deref().is_some_and(|b| {
                crate::lease::parse_record(&format!("{}.json", a.id()), b)
                    .is_some_and(|r| r.state == LeaseRecordState::Released)
            })
        })
        .unwrap();
    cloud.restore_old(&path, cloud.history(&path).len() - 1 - released);
    assert_eq!(record(&cloud, &a).state, LeaseRecordState::Released);
    assert!(!a.tick().lost);
    assert_eq!(record(&cloud, &a).state, LeaseRecordState::Held);
    b.tick();
    assert_eq!(b.state(), LIVE);
    assert!(
        !a.notices().iter().any(|n| n.contains("read-only")),
        "it never stopped"
    );
}

/// `release_one`, as removing a folder runs it: the lease is let go there and nowhere else, and no pass writes there
/// again before the folder goes, not even one that was waiting its turn.
#[test]
fn leaving_one_folder_frees_it_there_only_and_no_pass_writes_there_before_it_goes() {
    const DRIVE: &str = "/cloud/Drive";
    let cloud = FakeCloud::new(&[ROOT, DRIVE]);
    let a = Device::open(&cloud, "DESKTOP", &[ROOT, DRIVE]);
    let b = Device::open(&cloud, "LAPTOP", &[ROOT, DRIVE]);
    let c = Device::open(&cloud, "TABLET", &[DRIVE]);
    for d in [&a, &b, &c] {
        d.tick();
    }
    a.s().create_project(&pinput("P")).unwrap();
    a.tick();
    let in_drive = |d: &Device| cloud.lease_record(DRIVE, &d.id()).unwrap();
    assert_eq!(in_drive(&a).state, LeaseRecordState::Held);
    cloud.advance(LEASE_BEAT_SECS);
    lease_io::release_one(&a.svc, &a.view, DRIVE, LIMIT);
    let (kept, left) = (record(&cloud, &a), in_drive(&a));
    assert_eq!(kept.state, LeaseRecordState::Held);
    assert_eq!(left.state, LeaseRecordState::Released);
    assert!(
        left.beat_seq < kept.beat_seq,
        "below the beat in the folder it keeps"
    );
    assert_eq!(a.state(), MINE);
    // A pass that was waiting its turn runs before the folder is removed, and leaves that folder alone.
    a.tick();
    assert_eq!(in_drive(&a), left);
    assert_eq!(record(&cloud, &a), kept);
    // Reading only that folder, it's free; reading both, this one still holds.
    c.tick();
    assert_eq!(c.state(), FREE);
    b.tick();
    assert_eq!(b.state(), LIVE);
    a.s().remove_sync_target(DRIVE).unwrap();
    minutes(&cloud, &[&a, &b], 3);
    assert_eq!((a.state(), b.state()), (MINE, LIVE));
    assert_eq!(in_drive(&a), left, "nor after it went");

    // A folder still connected a minute later is in use after all, and gets the record back.
    let cloud = FakeCloud::new(&[ROOT, DRIVE]);
    let a = Device::open(&cloud, "DESKTOP", &[ROOT, DRIVE]);
    a.tick();
    a.s().create_project(&pinput("P")).unwrap();
    a.tick();
    lease_io::release_one(&a.svc, &a.view, DRIVE, LIMIT);
    let in_drive = |d: &Device| cloud.lease_record(DRIVE, &d.id()).unwrap();
    assert_eq!(in_drive(&a).state, LeaseRecordState::Released);
    cloud.advance(LEAVING_SECS - 1);
    a.tick();
    assert_eq!(in_drive(&a).state, LeaseRecordState::Released);
    cloud.advance(1);
    a.tick();
    assert_eq!(in_drive(&a).state, LeaseRecordState::Held);
    assert_eq!(a.state(), MINE);
}

/// D2: take over from a holder that looks in use, and only while the named condition allows it.
#[test]
fn a_live_holder_is_taken_over_from_only_while_d2_allows_it() {
    let (cloud, a, b) = two();
    let (_, _, seg) = desktop_timing(&a, &b);
    let shown = lease_io::preview_take_over(&b.svc, &b.view, LIMIT);
    assert!(shown.live);
    let refused = lease_io::take_over_offering(
        &b.svc,
        &b.view,
        expected(&shown),
        TakeOverMode::Live,
        LIMIT,
        false,
    )
    .unwrap_err();
    assert_eq!(refused.code(), "conflict");
    assert_eq!(
        refused.to_string(),
        "DESKTOP looks like it's still in use. Take over is offered once it looks inactive."
    );
    assert!(cloud.lease_record(ROOT, &b.id()).is_none());
    assert!(b.s().read_only());
    assert!(b.s().file().data.segments[&seg].edit.is_none());
    // A dialog that showed it as inactive gets the fresh view instead, whatever D2 says.
    let changed = lease_io::take_over(
        &b.svc,
        &b.view,
        expected(&shown),
        TakeOverMode::Stale,
        LIMIT,
    )
    .unwrap();
    assert_eq!(changed.outcome, TakeOverOutcome::Changed);
    // The app's own call follows the switch: as it ships, it goes through, behind the dialog's stronger words.
    let public = lease_io::take_over(&b.svc, &b.view, expected(&shown), TakeOverMode::Live, LIMIT);
    assert_eq!(
        public.is_ok(),
        OFFER_TAKE_OVER_TO_A_LIVE_HOLDER,
        "{:?}",
        public.as_ref().err().map(ToString::to_string)
    );
    let done = match public {
        Ok(done) => done,
        Err(_) => lease_io::take_over_offering(
            &b.svc,
            &b.view,
            expected(&shown),
            TakeOverMode::Live,
            LIMIT,
            true,
        )
        .unwrap(),
    };
    assert_eq!(done.outcome, TakeOverOutcome::Done);
    assert!(
        b.s().file().data.segments[&seg]
            .edit
            .as_ref()
            .is_some_and(|e| e.settled)
    );
    assert!(a.tick().lost);
}

/// D2's two switches are one decision: the page's and this one have to say the same.
#[test]
fn the_backend_allows_a_live_take_over_exactly_when_the_page_offers_it() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/1x-src/components/TakeOverDialog.tsx"
    );
    let text = std::fs::read_to_string(path).unwrap();
    let line = text
        .lines()
        .find(|l| l.starts_with("export const OFFER_TAKE_OVER_TO_A_LIVE_HOLDER = "))
        .unwrap();
    let page = match line.trim_end_matches(';').rsplit(' ').next() {
        Some("true") => true,
        Some("false") => false,
        other => panic!("{other:?} in {line}"),
    };
    assert_eq!(page, OFFER_TAKE_OVER_TO_A_LIVE_HOLDER, "flip both together");
}

/// Review R2 (a): a claim the lease thread writes just as the app quits, before that pass has taken in that it landed, is
/// released by the quit all the same. Otherwise the other computer goes on reading a holder that has gone.
#[test]
fn a_claim_that_lands_as_quitting_starts_is_released_by_the_quit() {
    let (cloud, a, b) = two();
    a.tick();
    b.tick();
    // The last edit, flushed as the app quits, needs the lease, and the pass that claims it is under way.
    a.task("Work", "Notes");
    let plan = a.s().lease_plan(true).unwrap();
    let reads = lease_io::read_all(&a.view, &plan, LIMIT);
    let step = a.s().lease_decide(&plan, &reads);
    let wrote = step
        .write
        .as_ref()
        .map(|p| lease_io::write_all(&a.view, &plan, p, LIMIT, "lease"))
        .unwrap_or_default();
    assert_eq!(
        record(&cloud, &a).state,
        LeaseRecordState::Held,
        "the claim landed"
    );
    // Quitting: timers close and the lease is released, before the pass takes in what it wrote.
    a.quit(LIMIT);
    let _ = a.s().lease_apply(&reads, step, &wrote);
    assert_eq!(record(&cloud, &a).state, LeaseRecordState::Released);
    cloud.advance(60);
    b.tick();
    assert_eq!(b.state(), FREE);
    assert!(!b.s().read_only());
}

/// Review R2 (b): "Work here anyway", then quitting. A pass that finds the folder back after the quit's release claims
/// nothing, so the computer still working isn't told that one which has gone took over.
#[test]
fn work_here_anyway_claims_nothing_after_the_quit_released_the_lease() {
    let (cloud, a, b) = two();
    let (_, t) = b.task("Laptop", "Work");
    b.s().start_task(&t).unwrap();
    b.tick();
    sync_all(&[&a, &b]);
    a.tick();
    assert!(a.s().read_only());
    a.view.set_offline(true);
    cloud.advance(LEASE_BEAT_SECS);
    a.tick();
    assert_eq!(a.state(), LeaseStateName::Unknown);
    let preview = lease_io::preview_take_over(&a.svc, &a.view, LIMIT);
    let chosen = lease_io::take_over(
        &a.svc,
        &a.view,
        expected(&preview),
        TakeOverMode::Offline,
        LIMIT,
    )
    .unwrap();
    assert_eq!(chosen.outcome, TakeOverOutcome::Done);
    assert!(!a.s().read_only());
    a.quit(LIMIT);
    let before = cloud.lease_history(ROOT, &a.id()).len();
    // The folder is back as the lease thread's last pass reads it, after the release.
    a.view.set_offline(false);
    a.tick();
    let written: Vec<LeaseRecord> = cloud
        .lease_history(ROOT, &a.id())
        .into_iter()
        .skip(before)
        .collect();
    assert!(
        written.iter().all(|r| r.state != LeaseRecordState::Held),
        "{written:?}"
    );
    cloud.advance(60);
    b.tick();
    assert_eq!(b.state(), MINE);
    assert_eq!(b.s().file().data.tasks[&t].state, TaskState::Running);
}

/// Review R5, found to lose nothing: a take over whose own read finds that another computer took over meanwhile stops
/// the timer here and keeps its pause alert, which the sync the shell starts after every take over then shows.
#[test]
fn a_take_over_that_finds_this_computer_overtaken_keeps_its_pause_alert_for_the_next_sync() {
    let cloud = FakeCloud::new(&[ROOT]);
    let a = Device::open(&cloud, "DESKTOP", &[ROOT]);
    let b = Device::open(&cloud, "LAPTOP", &[ROOT]);
    let c = Device::open(&cloud, "TABLET", &[ROOT]);
    c.tick();
    desktop_timing(&a, &b);
    sync_all(&[&a, &c]);
    c.tick();
    b.view.set_offline(true);
    b.tick();
    let preview = lease_io::preview_take_over(&b.svc, &b.view, LIMIT);
    let take = || {
        lease_io::take_over(
            &b.svc,
            &b.view,
            expected(&preview),
            TakeOverMode::Offline,
            LIMIT,
        )
        .unwrap()
    };
    assert_eq!(take().outcome, TakeOverOutcome::Done);
    let (_, tb) = b.task("Lap", "Offline work");
    b.s().start_task(&tb).unwrap();
    a.view.set_offline(true);
    minutes(&cloud, &[&b, &c], 16);
    assert_eq!(take_over_stale(&c).outcome, TakeOverOutcome::Done);
    cloud.advance(300);
    b.view.set_offline(false);
    // Asked again as the folder comes back: the read before it finds that TABLET took over meanwhile.
    assert_eq!(take().outcome, TakeOverOutcome::Changed);
    assert_eq!(b.s().file().data.tasks[&tb].state, TaskState::Paused);
    let alerts = b.s().take_pause_alerts();
    assert_eq!(alerts.len(), 1, "{alerts:?}");
    assert!(
        alerts[0].starts_with("TABLET took over while this computer was offline"),
        "{alerts:?}"
    );
}

/// Audit SEC-1: a lease record planted at the highest generation a reader takes can't be outranked, as no reader takes
/// a record above it. Taking over from it is refused and names the file to delete, rather than leaving this computer
/// with a claim no other computer sees. Once the file is gone, claims start again from the beginning.
#[test]
fn a_lease_record_at_the_highest_generation_is_not_taken_over_and_stops_nothing_once_deleted() {
    let (cloud, a, _b) = two();
    let planted = "0e0e0e0e-0000-4000-8000-000000000000";
    let now = cloud.now();
    let forged = LeaseRecord {
        lease_version: 1,
        device_id: planted.to_owned(),
        device_name: "PLANTED".to_owned(),
        app_version: String::new(),
        generation: MAX_GENERATION,
        state: LeaseRecordState::Held,
        acquired_at: now,
        beat_at: now,
        beat_seq: 1,
        open_segments: Vec::new(),
    };
    let path = FakeCloud::lease_path(ROOT, planted);
    cloud.put(&path, &serde_json::to_vec(&forged).unwrap());
    a.tick();
    assert!(a.s().read_only());
    minutes(&cloud, &[&a], 16);
    assert_eq!(a.state(), STALE);
    let preview = lease_io::preview_take_over(&a.svc, &a.view, LIMIT);
    let refused = lease_io::take_over(
        &a.svc,
        &a.view,
        expected(&preview),
        TakeOverMode::Stale,
        LIMIT,
    )
    .unwrap_err();
    assert_eq!(refused.code(), "conflict");
    assert!(
        refused.to_string().contains(&format!("{planted}.json")),
        "{refused}"
    );
    assert!(
        a.s().read_only(),
        "no claim held here that no other computer sees"
    );
    assert!(
        cloud.lease_record(ROOT, &a.id()).is_none(),
        "nothing written"
    );
    // With the file deleted, a change claims as ever.
    cloud.delete(&path);
    a.tick();
    assert_eq!(a.state(), FREE);
    a.task("Work", "Notes");
    a.tick();
    assert_eq!(a.state(), MINE);
    assert_eq!(record(&cloud, &a).generation, 1);
}
