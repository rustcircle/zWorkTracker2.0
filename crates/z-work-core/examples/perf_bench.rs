//! Measures the backend at a given data size and prints digests of its outputs, so two revisions can
//! be compared both for speed and for byte-identical results. Time is frozen, so digests are stable.
//!
//! Run: cargo run --release -p z-work-core --example perf_bench -- [segments] [json-out]
//!
//! The store is synthetic and deterministic: one device, 12 projects, 400 tasks, three years.
//! `write-store <dir> <segments> [running|paused]` writes it as a real store for the native app.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::error::Error;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{Value, json};
use z_work_core::machine::{TaskExpected, TaskPatch};
use z_work_core::merge::merge;
use z_work_core::model::{LocalFile, Segment, Shared, SyncProvider, SyncTarget, TaskState};
use z_work_core::pdf::{self, PdfRequest};
use z_work_core::persist;
use z_work_core::report::ExportRequest;
use z_work_core::service::Service;
use z_work_core::store::{self, Store};
use z_work_core::sync::{RealIo, run_cycle};
use z_work_core::time::{ManualClock, Ts};
use z_work_core::tray::tray_state;
use z_work_core::{csv, xlsx};

type Res<T> = Result<T, Box<dyn Error>>;

const NOW: &str = "2027-01-01T03:00:00Z";
const DEVICE: &str = "11111111-1111-4111-8111-111111111111";
const START: i64 = 1_704_067_200; // 2024-01-01

/// A small LCG: the fixture must not depend on a crate's random stream.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self, bound: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % bound
    }
}

fn iso(secs: i64) -> String {
    Ts(secs).iso()
}

fn stamp(at: i64, by: &str) -> Value {
    json!({ "at": iso(at), "by": by })
}

fn description(i: usize) -> Value {
    json!({ "type": "doc", "content": [{ "type": "orderedList", "attrs": { "start": 1 }, "content": [
        { "type": "listItem", "content": [{ "type": "paragraph", "content": [{ "type": "text", "text": format!("Step one of task {i}") }] }] },
        { "type": "listItem", "content": [{ "type": "paragraph", "content": [{ "type": "text", "text": format!("Step two of task {i}") }] }] }
    ] }] })
}

/// The shared data of one device. `running` adds an open segment on `DEVICE`.
fn shared(segments: usize, device: &str, seed: u64, running: bool) -> Value {
    let mut rng = Lcg(seed);
    let created = START - 86_400;
    let mut projects = serde_json::Map::new();
    let mut rates = serde_json::Map::new();
    for i in 0..12 {
        let id = format!("p{i:02}");
        projects.insert(
            id.clone(),
            json!({ "id": id, "name": format!("Project {i}"), "color": "#336699", "billable": true,
                "budgetHours": if i % 3 == 0 { json!(800.0) } else { Value::Null },
                "createdAt": iso(created), "stamp": stamp(created, device) }),
        );
        let rate = format!("r{i:02}");
        rates.insert(
            rate.clone(),
            json!({ "id": rate, "projectId": id, "effectiveFrom": iso(created), "rateCents": 4000 + i * 500,
                "currency": if i % 2 == 0 { "EUR" } else { "USD" }, "stamp": stamp(created, device) }),
        );
    }
    let days = 1095i64;
    let end = START + days * 86_400;
    let mut tasks = serde_json::Map::new();
    for i in 0..400usize {
        let id = format!("t{i:03}");
        let s = stamp(created, device);
        let state = if i == 0 && running {
            "running"
        } else if i % 13 == 0 {
            "paused"
        } else {
            "stopped"
        };
        tasks.insert(
            id.clone(),
            json!({ "id": id, "name": format!("Task {i}"), "nameStamp": s, "projectId": format!("p{:02}", i % 12),
                "projectStamp": s, "description": description(i), "descriptionStamp": s, "deletedStamp": s,
                "createdAt": iso(created), "state": state, "stateStamp": s,
                "pauseReason": if state == "paused" { json!({ "kind": "user" }) } else { Value::Null },
                "stoppedAt": if state == "stopped" { json!(iso(end)) } else { Value::Null } }),
        );
    }
    let per_day = segments.div_ceil(usize::try_from(days).unwrap_or(1)).max(1);
    let mut segs = serde_json::Map::new();
    let mut gaps = serde_json::Map::new();
    let mut pauses = serde_json::Map::new();
    let mut n = 0usize;
    'days: for d in 0..days {
        let mut cursor = START + d * 86_400 + 21_600 + i64::try_from(rng.next(7200)).unwrap_or(0);
        for _ in 0..per_day {
            if n >= segments {
                break 'days;
            }
            let len = 120 + i64::try_from(rng.next(1800)).unwrap_or(0);
            let task = format!("t{:03}", rng.next(400));
            // Ids look like the real ones: a UUID is what the map is keyed by.
            let id = format!("{seed:08x}-0000-4000-8000-{n:012x}");
            segs.insert(
                id.clone(),
                json!({ "id": id, "taskId": task, "deviceId": device, "start": iso(cursor), "end": iso(cursor + len) }),
            );
            if n % 250 == 7 {
                let gap = format!("g{seed:x}-{n}");
                gaps.insert(
                    gap.clone(),
                    json!({ "id": gap, "segmentId": id, "taskId": task, "deviceId": device, "start": iso(cursor + 30),
                        "end": iso(cursor + 60), "kind": "idle", "decision": "keep", "decisionStamp": stamp(cursor + 70, device) }),
                );
            }
            if n % 250 == 11 {
                let pause = format!("h{seed:x}-{n}");
                pauses.insert(
                    pause.clone(),
                    json!({ "id": pause, "taskId": task, "start": iso(cursor + len + 1), "end": iso(cursor + len + 60), "stamp": stamp(cursor + len + 70, device) }),
                );
            }
            cursor += len + i64::try_from(rng.next(600)).unwrap_or(0);
            n += 1;
        }
    }
    if running {
        let now = Ts::parse(NOW).map_or(0, Ts::secs);
        let id = "ffffffff-0000-4000-8000-000000000000";
        segs.insert(
            id.to_owned(),
            json!({ "id": id, "taskId": "t000", "deviceId": device, "start": iso(now - 1800), "end": Value::Null }),
        );
    }
    json!({ "projects": projects, "tasks": tasks, "segments": segs, "rates": rates, "gaps": gaps, "pauses": pauses })
}

fn local_file(segments: usize, running: bool) -> Res<LocalFile> {
    let v = json!({ "version": 2, "deviceId": DEVICE, "deviceName": "bench",
        "prefs": { "syncPromptDone": true, "clientZonePromptDone": true, "hideNoticeShown": true },
        "data": shared(segments, DEVICE, 1, running) });
    Ok(serde_json::from_value(v)?)
}

fn fnv(bytes: &[u8]) -> String {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

struct Sample {
    median_ms: f64,
    min_ms: f64,
    cv: f64,
    n: usize,
}

/// Runs `f` until `n` samples or the time budget is spent, whichever comes first (at least 3).
fn time<T>(n: usize, mut f: impl FnMut() -> T) -> Sample {
    let started = Instant::now();
    let mut all = Vec::with_capacity(n);
    while all.len() < n && (all.len() < 3 || started.elapsed().as_secs() < 20) {
        let t = Instant::now();
        std::hint::black_box(f());
        all.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    all.sort_by(f64::total_cmp);
    let mean = all.iter().sum::<f64>() / all.len() as f64;
    let var = all.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / all.len() as f64;
    Sample {
        median_ms: all[all.len() / 2],
        min_ms: all[0],
        cv: if mean > 0.0 { var.sqrt() / mean } else { 0.0 },
        n: all.len(),
    }
}

/// Writes the store into `dir` as the app saves a complete one, replacing a store already there.
fn write_store(dir: &Path, segments: usize, running: bool) -> Res<()> {
    std::fs::create_dir_all(dir)?;
    for suffix in ["", "-wal", "-shm"] {
        let old = dir.join(format!("{}{suffix}", store::DB_FILE));
        if old.exists() {
            std::fs::remove_file(old)?;
        }
    }
    let file = local_file(segments, running)?;
    store::seed_store(dir, &file, Ts::parse(NOW).unwrap_or_default())?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn main() -> Res<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("write-store") {
        let dir = args
            .get(1)
            .ok_or("write-store <dir> <segments> [running|paused]")?;
        let n = args.get(2).ok_or("segments")?.parse()?;
        return write_store(
            Path::new(dir),
            n,
            args.get(3).map(String::as_str) != Some("paused"),
        );
    }
    let segments: usize = args.first().map_or(Ok(50_000), |s| s.parse())?;
    let out_path = args.get(1).cloned();
    let work = std::env::temp_dir().join(format!("z-work-bench-{}", std::process::id()));
    std::fs::create_dir_all(&work)?;

    let results: RefCell<BTreeMap<String, Value>> = RefCell::new(BTreeMap::new());
    let mut digests: BTreeMap<String, String> = BTreeMap::new();
    let record = |name: &str, s: Sample| {
        println!(
            "{name:<34} median {:>10.2} ms  min {:>10.2} ms  cv {:>5.1}%  n {}",
            s.median_ms,
            s.min_ms,
            s.cv * 100.0,
            s.n
        );
        results.borrow_mut().insert(
            name.to_owned(),
            json!({ "medianMs": s.median_ms, "minMs": s.min_ms, "cv": s.cv, "n": s.n }),
        );
    };

    let clock = ManualClock::at(NOW);
    let now = Ts::parse(NOW).unwrap_or_default();
    let dir = work.join("store");
    write_store(&dir, segments, false)?;
    let store_bytes = std::fs::metadata(dir.join(store::DB_FILE))?.len();
    // The same store as JSON, as a backup holds it.
    let main_bytes = serde_json::to_vec(&local_file(segments, false)?)?;
    println!(
        "store: {segments} segments, {store_bytes} bytes ({} as JSON)",
        main_bytes.len()
    );

    record(
        "open (load, normalize, save)",
        time(10, || {
            Service::open(&dir, Arc::new(clock.clone()), "bench").unwrap()
        }),
    );
    let mut svc = Service::open(&dir, Arc::new(clock.clone()), "bench")?;
    svc.set_system_zone("Europe/Lisbon".to_owned());
    // A task runs from here on, half an hour in, as the app is most of the working day.
    // Set by hand rather than through a resume, whose random segment id would change the digests.
    let open: Segment =
        serde_json::from_value(json!({ "id": "ffffffff-0000-4000-8000-000000000000",
        "taskId": "t013", "deviceId": DEVICE, "start": NOW, "end": Value::Null }))?;
    svc.mutate(|f, _| {
        if let Some(task) = f.data.tasks.get_mut("t013") {
            task.state = TaskState::Running;
            task.pause_reason = None;
        }
        f.data.segments.insert(open.id.clone(), open);
        Ok(())
    })?;
    clock.advance(1800);
    let now = now.later(1800);
    let file: LocalFile = svc.file().clone();

    record("clone store", time(30, || file.clone()));
    record(
        "serialize store",
        time(30, || serde_json::to_vec(&file).unwrap()),
    );
    record(
        "parse + check store",
        time(20, || persist::parse_checked(&main_bytes).unwrap()),
    );
    // A store of its own, as the service holds this one's lock. Each save changes one segment, as a real one does:
    // an unchanged store is not written at all.
    let saved_dir = work.join("saved");
    write_store(&saved_dir, segments, false)?;
    let mut saved = Store::open(&saved_dir, "bench", now)?;
    let mut last = saved.file;
    let one = last
        .data
        .segments
        .keys()
        .next()
        .cloned()
        .ok_or("no segment")?;
    let mut beat = 0i64;
    record(
        "store::save",
        time(20, || {
            beat += 1;
            let mut next = last.clone();
            if let Some(s) = next.data.segments.get_mut(&one) {
                s.end = s.end.map(|e| e.later(1));
            }
            next.last_heartbeat = Some(now.later(beat));
            saved.store.save(&last, &next).unwrap();
            last = next;
        }),
    );
    record(
        "store::save, unchanged",
        time(20, || saved.store.save(&last, &last).unwrap()),
    );
    record(
        "heartbeat (running)",
        time(20, || {
            clock.advance(1);
            svc.heartbeat().unwrap();
        }),
    );
    let mut keep = 30;
    record(
        "mutate (one setting)",
        time(20, || {
            keep = if keep == 30 { 31 } else { 30 };
            svc.set_backup_keep(keep).unwrap();
        }),
    );
    svc.set_backup_keep(30)?;
    clock.set(&now.iso());
    svc.heartbeat()?;
    record("snapshot build", time(20, || svc.snapshot()));
    let snap = svc.snapshot();
    let snap_json = serde_json::to_vec(&snap)?;
    println!("snapshot json: {} bytes", snap_json.len());
    results
        .borrow_mut()
        .insert("snapshotBytes".into(), json!(snap_json.len()));
    results
        .borrow_mut()
        .insert("storeBytes".into(), json!(store_bytes));
    record(
        "snapshot serialize",
        time(20, || serde_json::to_vec(&snap).unwrap()),
    );
    digests.insert("snapshot".into(), fnv(&snap_json));
    // An autosave, what the page used to pay and what it pays now: the old check read a whole snapshot
    // first, and the expected value now travels with the save and is compared inside the write.
    let task_id = snap.tasks[0].id.clone();
    let mut typed = 0u32;
    let mut next_doc = || {
        typed += 1;
        json!({ "type": "doc", "content": [{ "type": "paragraph", "content": [{ "type": "text", "text": format!("Words {typed}") }] }] })
    };
    record(
        "autosave, read first then write (before)",
        time(10, || {
            let shown = svc.snapshot();
            let shown_bytes = serde_json::to_vec(&shown).unwrap();
            svc.patch_task(
                &task_id,
                &TaskPatch {
                    description: Some(next_doc()),
                    ..TaskPatch::default()
                },
                &TaskExpected::default(),
            )
            .unwrap();
            let reply = svc.snapshot();
            (shown_bytes, serde_json::to_vec(&reply).unwrap())
        }),
    );
    record(
        "autosave, compare in the write (after)",
        time(10, || {
            let expected = svc.file().data.tasks[&task_id].description.clone();
            let out = svc
                .patch_task(
                    &task_id,
                    &TaskPatch {
                        description: Some(next_doc()),
                        ..TaskPatch::default()
                    },
                    &TaskExpected {
                        description: Some(expected),
                        ..TaskExpected::default()
                    },
                )
                .unwrap();
            assert!(out.conflicts.is_empty());
            let reply = svc.snapshot();
            serde_json::to_vec(&json!({ "conflicts": out.conflicts, "stored": out.stored, "bytes": serde_json::to_vec(&reply).unwrap().len() })).unwrap()
        }),
    );

    record(
        "activity tick (lock + idle)",
        time(200, || {
            svc.observe_lock(Some(false)).unwrap();
            svc.observe_activity(Some(1)).unwrap();
        }),
    );
    record("chores (take_alerts)", time(20, || svc.take_alerts()));
    let tz: chrono_tz::Tz = "Europe/Lisbon".parse()?;
    record(
        "tray tick, running",
        time(50, || tray_state(svc.file(), now, tz)),
    );
    digests.insert(
        "tray_running".into(),
        fnv(format!("{:?}", tray_state(svc.file(), now, tz)).as_bytes()),
    );

    // Nothing running: the tray looks for the latest paused task.
    let paused_dir = work.join("paused");
    write_store(&paused_dir, segments, false)?;
    let paused = Service::open(&paused_dir, Arc::new(clock.clone()), "bench")?;
    record(
        "tray tick, paused tasks",
        time(10, || tray_state(paused.file(), now, tz)),
    );
    digests.insert(
        "tray_paused".into(),
        fnv(format!("{:?}", tray_state(paused.file(), now, tz)).as_bytes()),
    );
    drop(paused);

    // Exports go through the same copy the commands take.
    let request: ExportRequest = serde_json::from_value(json!({
        "dayZone": "America/New_York", "myZone": "Europe/Lisbon", "clientZone": "America/New_York",
        "includeEarnings": true, "segmentShifts": {}, "analytics": []
    }))?;
    let pdf_request: PdfRequest = serde_json::from_value(json!({
        "from": "2024-01-01", "to": "2026-12-31", "zone": "America/New_York", "title": "Timesheet", "includePrices": true
    }))?;
    record("export copy", time(20, || svc.export_source()));
    let source = svc.export_source();
    record(
        "export xlsx",
        time(5, || xlsx::build(&source, &request).unwrap()),
    );
    record(
        "export csv",
        time(10, || csv::build(&source, &request).unwrap()),
    );
    record(
        "export pdf",
        time(5, || pdf::build(&source, &pdf_request, None).unwrap().bytes),
    );
    let xlsx_bytes = xlsx::build(&source, &request)?;
    results
        .borrow_mut()
        .insert("xlsxBytes".into(), json!(xlsx_bytes.len()));
    let pdf_bytes = pdf::build(&source, &pdf_request, None)?.bytes;
    results
        .borrow_mut()
        .insert("pdfBytes".into(), json!(pdf_bytes.len()));
    // An xlsx holds its creation time, so two runs are compared by unzipping these.
    if let Ok(dir) = std::env::var("PERF_ARTIFACTS") {
        std::fs::write(Path::new(&dir).join("export.xlsx"), &xlsx_bytes)?;
    }
    digests.insert("csv".into(), fnv(csv::build(&source, &request)?.as_bytes()));
    digests.insert("pdf".into(), fnv(&pdf_bytes));
    digests.insert("xlsx_len".into(), xlsx_bytes.len().to_string());

    // Sync: three other devices, each holding the shared history plus its own recent work.
    let remote: Vec<Shared> = (2u64..5)
        .map(|seed| {
            let device = format!("{seed:08}-1111-4111-8111-111111111111");
            let mut own: Shared =
                serde_json::from_value(shared(1000, &device, seed, false)).unwrap();
            own = merge(&svc.file().data, &own);
            own
        })
        .collect();
    let first = remote[0].clone();
    record(
        "merge (two files, 50k)",
        time(20, || merge(&svc.file().data, &first)),
    );
    let merged_all = remote
        .iter()
        .fold(svc.file().data.clone(), |acc, r| merge(&acc, r));
    digests.insert("merge".into(), fnv(&serde_json::to_vec(&merged_all)?));

    let sync_root = work.join("sync");
    let devices = sync_root.join("zWorkTracker").join("devices");
    std::fs::create_dir_all(&devices)?;
    for (i, data) in remote.iter().enumerate() {
        let id = format!("{:08}-2222-4222-8222-222222222222", i + 2);
        let v = json!({ "schemaVersion": 2, "deviceId": id, "deviceName": format!("Device {i}"),
            "writtenAt": NOW, "data": data });
        std::fs::write(devices.join(format!("{id}.json")), serde_json::to_vec(&v)?)?;
    }
    svc.add_sync_target(SyncTarget {
        provider: SyncProvider::Custom,
        root: sync_root.to_string_lossy().into_owned(),
    })?;
    let shared_svc = Mutex::new(svc);
    let t = Instant::now();
    run_cycle(&shared_svc, &RealIo);
    let first_ms = t.elapsed().as_secs_f64() * 1000.0;
    println!("sync cycle, first (data changes):  {first_ms:.2} ms");
    results
        .borrow_mut()
        .insert("sync cycle first ms".into(), json!(first_ms));
    record(
        "sync cycle, settled (3 devices)",
        time(5, || run_cycle(&shared_svc, &RealIo)),
    );
    {
        let guard = shared_svc.lock().map_err(|_| "poisoned")?;
        digests.insert(
            "after_sync".into(),
            fnv(&serde_json::to_vec(&guard.file().data)?),
        );
    }
    drop(shared_svc);

    println!("digests: {}", serde_json::to_string(&digests)?);
    let report = json!({ "segments": segments, "results": results, "digests": digests });
    if let Some(path) = out_path {
        std::fs::write(path, serde_json::to_vec_pretty(&report)?)?;
    }
    std::fs::remove_dir_all(&work)?;
    Ok(())
}
