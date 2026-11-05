//! Measures the local store at several sizes: opening it, saving after one user action, and a sync cycle that
//! merges another device's file. Written at 1.3.1, whose store is one JSON file, so that the store replacing it
//! can be measured by the same driver on the same machine and the two compared.
//!
//! Run, from `src-tauri`:
//!
//! ```sh
//! cargo run --release -p z-work-core --example store_bench -- out.json
//! cargo run --release -p z-work-core --example store_bench -- out.json --sizes 1000,10000 --save-reps 60
//! ```
//!
//! Options: `--sizes` (default 1000,10000,50000), `--open-reps` (20), `--save-reps` (40), `--sync-reps` (20),
//! `--merge-reps` (20). Fewer than 10 open, sync or merge samples or 30 save samples is allowed, for a smoke run,
//! with a warning. From 20 samples on, the nearest-rank p95 is not the largest sample, so one stalled sample (an
//! fsync held up by other work on the disk) can't be the p95 by itself. The results go to the JSON file named
//! first, every sample included, and a table is printed.
//!
//! What each operation measures, by wall clock (`Instant`), one sample per repetition, after one untimed run:
//! - `open`: `Service::open` on a store of N segments already on disk. A new `Service` each time, in this process:
//!   nothing is kept in memory between samples, but the OS file cache is warm. The untimed run first settles what
//!   only a first open does (starter tags). Dropping the service is not timed.
//! - `save`: one user action with its save: a task renamed through `Service::patch_task`. Names differ every time,
//!   so every sample writes, and the clock moves a second between samples, as a person would.
//! - `sync`: `sync::run_cycle` over one cloud folder holding another device's file with N segments, half of them
//!   new to this store. That is: reading and checking the file, the merge, the local save, and writing this device's
//!   own file to the folder. Each sample starts from a newly seeded store and folder, set up untimed.
//! - `merge`: `merge::merge` of the same two data sets, in memory only, for reference.
//!
//! The data is synthetic and deterministic: 12 projects, 400 tasks and N segments over three years (2024-2026),
//! as in `perf_bench`, with time frozen at 2027-01-01T03:00:00Z. Every file lives in a temporary directory.
//!
//! Only `seed_store` knows how the store is kept on disk. Everything measured goes through `Service`, `sync` and
//! `merge`, which outlive a change of storage, so such a change adapts `seed_store` and nothing else.

use std::error::Error;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{Map, Value, json};
use z_work_core::machine::{TaskExpected, TaskPatch};
use z_work_core::merge::merge;
use z_work_core::model::{DeviceFile, LocalFile, SCHEMA_VERSION, Shared, SyncProvider, SyncTarget};
use z_work_core::service::Service;
use z_work_core::sync::{RealIo, devices_dir, run_cycle};
use z_work_core::time::{ManualClock, Ts};

type Res<T> = Result<T, Box<dyn Error>>;

/// Frozen time: every synthetic record is older than this.
const NOW: &str = "2027-01-01T03:00:00Z";
/// This device, as in `perf_bench`, so its store holds the same data.
const LOCAL: &str = "11111111-1111-4111-8111-111111111111";
/// The other device whose file the sync cycle merges.
const REMOTE: &str = "22222222-2222-4222-8222-222222222222";
/// 2024-01-01.
const START: i64 = 1_704_067_200;
const DAYS: i64 = 1095;

const DEFAULT_SIZES: [usize; 3] = [1_000, 10_000, 50_000];
/// The fewest samples the comparison this driver serves asks for.
const MIN_OPEN: usize = 10;
const MIN_SAVE: usize = 30;
const MIN_SYNC: usize = 10;
const MIN_MERGE: usize = 10;

/// A small LCG, as in `perf_bench`: the data must not depend on a crate's random stream.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % bound
    }

    fn secs(&mut self, bound: u64) -> i64 {
        i64::try_from(self.next(bound)).unwrap_or(0)
    }
}

fn iso(secs: i64) -> String {
    Ts(secs).iso()
}

fn stamp(at: i64, by: &str) -> Value {
    json!({ "at": iso(at), "by": by })
}

fn now() -> Res<Ts> {
    Ts::parse(NOW).ok_or_else(|| "bad NOW".into())
}

fn description(i: usize) -> Value {
    json!({ "type": "doc", "content": [{ "type": "orderedList", "attrs": { "start": 1 }, "content": [
        { "type": "listItem", "content": [{ "type": "paragraph", "content": [{ "type": "text", "text": format!("Step one of task {i}") }] }] },
        { "type": "listItem", "content": [{ "type": "paragraph", "content": [{ "type": "text", "text": format!("Step two of task {i}") }] }] }
    ] }] })
}

/// Segments, time away and billable pauses of `device`: `n` segments spread over three years, as in `perf_bench`.
fn work(
    n: usize,
    device: &str,
    seed: u64,
) -> (Map<String, Value>, Map<String, Value>, Map<String, Value>) {
    let mut rng = Lcg(seed);
    let per_day = n.div_ceil(usize::try_from(DAYS).unwrap_or(1)).max(1);
    let (mut segs, mut gaps, mut pauses) = (Map::new(), Map::new(), Map::new());
    let mut made = 0usize;
    'days: for d in 0..DAYS {
        let mut cursor = START + d * 86_400 + 21_600 + rng.secs(7200);
        for _ in 0..per_day {
            if made >= n {
                break 'days;
            }
            let len = 120 + rng.secs(1800);
            let task = format!("t{:03}", rng.next(400));
            let id = format!("{seed:08x}-0000-4000-8000-{made:012x}");
            segs.insert(
                id.clone(),
                json!({ "id": id, "taskId": task, "deviceId": device, "start": iso(cursor), "end": iso(cursor + len) }),
            );
            if made % 250 == 7 {
                let gap = format!("g{seed:x}-{made}");
                gaps.insert(
                    gap.clone(),
                    json!({ "id": gap, "segmentId": id, "taskId": task, "deviceId": device, "start": iso(cursor + 30),
                        "end": iso(cursor + 60), "kind": "idle", "decision": "keep", "decisionStamp": stamp(cursor + 70, device) }),
                );
            }
            if made % 250 == 11 {
                let pause = format!("h{seed:x}-{made}");
                pauses.insert(
                    pause.clone(),
                    json!({ "id": pause, "taskId": task, "start": iso(cursor + len + 1), "end": iso(cursor + len + 60),
                        "stamp": stamp(cursor + len + 70, device) }),
                );
            }
            cursor += len + rng.secs(600);
            made += 1;
        }
    }
    (segs, gaps, pauses)
}

/// This device's data at `n` segments: the store `perf_bench` writes, with nothing running.
fn local_data(n: usize) -> Res<Shared> {
    let created = START - 86_400;
    let (mut projects, mut rates, mut tasks) = (Map::new(), Map::new(), Map::new());
    for i in 0..12usize {
        let id = format!("p{i:02}");
        projects.insert(
            id.clone(),
            json!({ "id": id, "name": format!("Project {i}"), "color": "#336699", "billable": true,
                "budgetHours": if i % 3 == 0 { json!(800.0) } else { Value::Null },
                "createdAt": iso(created), "stamp": stamp(created, LOCAL) }),
        );
        let rate = format!("r{i:02}");
        rates.insert(
            rate.clone(),
            json!({ "id": rate, "projectId": id, "effectiveFrom": iso(created), "rateCents": 4000 + i * 500,
                "currency": if i % 2 == 0 { "EUR" } else { "USD" }, "stamp": stamp(created, LOCAL) }),
        );
    }
    let end = START + DAYS * 86_400;
    for i in 0..400usize {
        let id = format!("t{i:03}");
        let s = stamp(created, LOCAL);
        let state = if i % 13 == 0 { "paused" } else { "stopped" };
        tasks.insert(
            id.clone(),
            json!({ "id": id, "name": format!("Task {i}"), "nameStamp": s, "projectId": format!("p{:02}", i % 12),
                "projectStamp": s, "description": description(i), "descriptionStamp": s, "deletedStamp": s,
                "createdAt": iso(created), "state": state, "stateStamp": s,
                "pauseReason": if state == "paused" { json!({ "kind": "user" }) } else { Value::Null },
                "stoppedAt": if state == "stopped" { json!(iso(end)) } else { Value::Null } }),
        );
    }
    let (segments, gaps, pauses) = work(n, LOCAL, 1);
    Ok(serde_json::from_value(json!({
        "projects": projects, "tasks": tasks, "segments": segments, "rates": rates, "gaps": gaps, "pauses": pauses
    }))?)
}

/// What the other device holds: everything of this device's but the second half of its segments (and the time away
/// inside those), plus `n - n/2` segments of its own on the same tasks, with their time away and pauses.
fn remote_data(local: &Shared, n: usize) -> Res<Shared> {
    let mut data = local.clone();
    let keep = n / 2;
    data.segments = local
        .segments
        .iter()
        .take(keep)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    data.gaps
        .retain(|_, g| data.segments.contains_key(&g.segment_id));
    let (segments, gaps, pauses) = work(n - keep, REMOTE, 2);
    let theirs: Shared =
        serde_json::from_value(json!({ "segments": segments, "gaps": gaps, "pauses": pauses }))?;
    data.segments.extend(theirs.segments);
    data.gaps.extend(theirs.gaps);
    data.pauses.extend(theirs.pauses);
    Ok(data)
}

/// STORAGE-SPECIFIC: the only part of this driver that knows how the local store is kept on disk, and the only part
/// a change of storage may adapt. It puts a store of this device holding `local_data(n)` into `dir`, as the app
/// would find it at a launch. Everything measured goes through `Service`, `sync` and `merge` instead.
///
/// 1.3.2 keeps the store as `z-work-data.db`, a SQLite database, written here as the app writes a complete one, by
/// `store::seed_store`. The app opens it as an existing store: no first start, no notices. A store that is filled
/// another way (from a cloud folder at its first start, say) seeds itself that way here, untimed.
fn seed_store(dir: &Path, n: usize) -> Res<()> {
    std::fs::create_dir_all(dir)?;
    let file: LocalFile = serde_json::from_value(json!({
        "version": SCHEMA_VERSION, "deviceId": LOCAL, "deviceName": "bench",
        "prefs": { "syncPromptDone": true, "clientZonePromptDone": true, "hideNoticeShown": true },
        "data": local_data(n)?,
    }))?;
    z_work_core::store::seed_store(dir, &file, now()?)?;
    Ok(())
}

/// The other device's file, as 1.3.1 writes one to `<root>/zWorkTracker/devices/<id>.json`. This is the cloud
/// format, which a change of the local store keeps byte for byte.
fn remote_file_bytes(local: &Shared, n: usize) -> Res<Vec<u8>> {
    let file = DeviceFile {
        schema_version: SCHEMA_VERSION,
        device_id: REMOTE.to_owned(),
        device_name: "bench remote".to_owned(),
        written_at: now()?,
        last_heartbeat: None,
        epoch_info: None,
        data: remote_data(local, n)?,
    };
    Ok(serde_json::to_vec(&file)?)
}

fn put_remote_file(cloud: &Path, bytes: &[u8]) -> Res<()> {
    let dir = devices_dir(cloud);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(format!("{REMOTE}.json")), bytes)?;
    Ok(())
}

fn elapsed_ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn service(dir: &Path, clock: &ManualClock) -> Res<Service> {
    Ok(Service::open(dir, Arc::new(clock.clone()), "bench")?)
}

/// Bytes and files under `dir`, whatever the store keeps there.
fn dir_size(dir: &Path) -> Res<(u64, usize)> {
    let (mut bytes, mut files) = (0u64, 0usize);
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_dir() {
            let (b, f) = dir_size(&entry.path())?;
            bytes += b;
            files += f;
        } else {
            bytes += meta.len();
            files += 1;
        }
    }
    Ok((bytes, files))
}

fn bench_open(dir: &Path, clock: &ManualClock, reps: usize) -> Res<Vec<f64>> {
    drop(service(dir, clock)?);
    let mut out = Vec::with_capacity(reps);
    for _ in 0..reps {
        let t = Instant::now();
        let svc = service(dir, clock)?;
        out.push(elapsed_ms(t));
        drop(std::hint::black_box(svc));
    }
    Ok(out)
}

fn bench_save(dir: &Path, clock: &ManualClock, reps: usize) -> Res<Vec<f64>> {
    let mut svc = service(dir, clock)?;
    let task = svc
        .file()
        .data
        .tasks
        .keys()
        .next()
        .cloned()
        .ok_or("the store has no task")?;
    let mut out = Vec::with_capacity(reps);
    // Sample 0 is the untimed run.
    for i in 0..=reps {
        clock.advance(1);
        let patch = TaskPatch {
            name: Some(format!("Renamed task {i}")),
            ..TaskPatch::default()
        };
        let t = Instant::now();
        let outcome = svc.patch_task(&task, &patch, &TaskExpected::default())?;
        let ms = elapsed_ms(t);
        if !outcome.conflicts.is_empty() {
            return Err("the rename was refused".into());
        }
        if i > 0 {
            out.push(ms);
        }
    }
    Ok(out)
}

/// One sample of a sync cycle, from a newly seeded store and folder. Returns its time and the segments it left.
fn sync_once(n: usize, clock: &ManualClock, remote: &[u8]) -> Res<(f64, usize)> {
    let tmp = tempfile::Builder::new()
        .prefix("z-work-store-bench-sync-")
        .tempdir()?;
    let (store, cloud) = (tmp.path().join("store"), tmp.path().join("cloud"));
    seed_store(&store, n)?;
    put_remote_file(&cloud, remote)?;
    let root = cloud.to_string_lossy().into_owned();
    let mut svc = service(&store, clock)?;
    svc.add_sync_target(SyncTarget {
        provider: SyncProvider::Custom,
        root: root.clone(),
    })?;
    let shared = Mutex::new(svc);
    let t = Instant::now();
    let changed = run_cycle(&shared, &RealIo);
    let ms = elapsed_ms(t);
    let mut svc = shared.into_inner().map_err(|_| "poisoned")?;
    let status = svc.target_status_mut(&root).clone();
    if let Some(e) = status.last_error.or(svc.local_error().map(str::to_owned)) {
        return Err(format!("the sync cycle failed: {e}").into());
    }
    if !changed || status.devices_seen != 1 {
        return Err("the sync cycle merged nothing".into());
    }
    let left = svc.file().data.segments.len();
    drop(svc);
    drop(tmp);
    Ok((ms, left))
}

fn bench_sync(n: usize, clock: &ManualClock, reps: usize, remote: &[u8]) -> Res<(Vec<f64>, usize)> {
    let (_, left) = sync_once(n, clock, remote)?;
    let mut out = Vec::with_capacity(reps);
    for _ in 0..reps {
        let (ms, after) = sync_once(n, clock, remote)?;
        if after != left {
            return Err("two sync cycles from the same start ended differently".into());
        }
        out.push(ms);
    }
    Ok((out, left))
}

fn bench_merge(local: &Shared, remote: &Shared, reps: usize) -> Vec<f64> {
    drop(std::hint::black_box(merge(local, remote)));
    let mut out = Vec::with_capacity(reps);
    for _ in 0..reps {
        let t = Instant::now();
        let merged = merge(std::hint::black_box(local), std::hint::black_box(remote));
        out.push(elapsed_ms(t));
        drop(std::hint::black_box(merged));
    }
    out
}

struct Stats {
    n: usize,
    median: f64,
    p95: f64,
    min: f64,
    max: f64,
    mean: f64,
    samples: Vec<f64>,
}

impl Stats {
    /// Median (mean of the two middle samples for an even count) and the nearest-rank p95: the smallest sample
    /// with at least 95% of all samples at or below it.
    fn of(samples: Vec<f64>) -> Res<Self> {
        let mut sorted = samples.clone();
        sorted.sort_by(f64::total_cmp);
        let n = sorted.len();
        let at = |i: usize| sorted.get(i).copied().ok_or("no samples");
        let median = if n % 2 == 1 {
            at(n / 2)?
        } else {
            (at(n / 2 - 1)? + at(n / 2)?) / 2.0
        };
        let rank = (n * 95).div_ceil(100).max(1);
        Ok(Self {
            n,
            median,
            p95: at(rank - 1)?,
            min: at(0)?,
            max: at(n - 1)?,
            mean: sorted.iter().sum::<f64>() / n as f64,
            samples,
        })
    }

    fn json(&self) -> Value {
        let round = |x: f64| (x * 1000.0).round() / 1000.0;
        json!({
            "n": self.n, "medianMs": round(self.median), "p95Ms": round(self.p95), "minMs": round(self.min),
            "maxMs": round(self.max), "meanMs": round(self.mean),
            "samplesMs": self.samples.iter().map(|x| round(*x)).collect::<Vec<_>>(),
        })
    }
}

/// The first line a command prints, or `None` when it can't be run here.
fn first_line(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program)
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
}

/// The 1, 5 and 15 minute load averages, as a sign of what else the machine was doing.
fn load_average() -> Option<String> {
    first_line("sysctl", &["-n", "vm.loadavg"])
        .map(|l| l.trim_matches(|c| c == '{' || c == '}').trim().to_owned())
        .or_else(|| {
            std::fs::read_to_string("/proc/loadavg")
                .ok()
                .map(|l| l.split_whitespace().take(3).collect::<Vec<_>>().join(" "))
        })
}

/// What the numbers were measured on. Each field is `null` where this OS can't say.
fn machine() -> Value {
    let power =
        first_line("pmset", &["-g", "batt"]).and_then(|l| l.split('\'').nth(1).map(str::to_owned));
    let os = first_line("sw_vers", &["-productVersion"])
        .map(|v| format!("macOS {v}"))
        .or_else(|| first_line("uname", &["-sr"]));
    json!({
        "cpu": first_line("sysctl", &["-n", "machdep.cpu.brand_string"]),
        "threads": std::thread::available_parallelism().map(|n| n.get()).ok(),
        "power": power,
        "os": os,
        "kernel": first_line("uname", &["-r"]),
        "rustc": first_line("rustc", &["-V"]),
        "loadAverageAtStart": load_average(),
    })
}

fn source() -> Value {
    let dirty = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| !o.stdout.is_empty());
    json!({
        "crateVersion": env!("CARGO_PKG_VERSION"),
        "commit": first_line("git", &["rev-parse", "--short", "HEAD"]),
        "trackedFilesChanged": dirty,
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
    })
}

struct Options {
    out: Option<String>,
    sizes: Vec<usize>,
    open: usize,
    save: usize,
    sync: usize,
    merge: usize,
}

fn options() -> Res<Options> {
    let mut o = Options {
        out: None,
        sizes: DEFAULT_SIZES.to_vec(),
        open: 20,
        save: 40,
        sync: 20,
        merge: 20,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--sizes" => {
                o.sizes = value()?
                    .split(',')
                    .map(|s| s.trim().replace('_', "").parse::<usize>())
                    .collect::<Result<_, _>>()?;
            }
            "--open-reps" => o.open = value()?.parse()?,
            "--save-reps" => o.save = value()?.parse()?,
            "--sync-reps" => o.sync = value()?.parse()?,
            "--merge-reps" => o.merge = value()?.parse()?,
            "-h" | "--help" => {
                return Err(
                    "usage: store_bench [out.json] [--sizes 1000,10000,50000] [--open-reps 20] \
                    [--save-reps 40] [--sync-reps 20] [--merge-reps 20]"
                        .into(),
                );
            }
            path if !path.starts_with('-') && o.out.is_none() => o.out = Some(path.to_owned()),
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    if o.sizes.iter().any(|n| *n < 2) || [o.open, o.save, o.sync, o.merge].contains(&0) {
        return Err(
            "sizes need at least 2 segments and every operation at least one sample".into(),
        );
    }
    Ok(o)
}

fn label(op: &str) -> &'static str {
    match op {
        "open" => "open (Service::open)",
        "save" => "save after one action (rename)",
        "sync" => "sync cycle, N/2 new segments",
        _ => "merge::merge in memory",
    }
}

fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn main() -> Res<()> {
    let o = options()?;
    if cfg!(debug_assertions) {
        eprintln!("warning: a debug build; these numbers can't be compared with a release build's");
    }
    for (what, got, min) in [
        ("open", o.open, MIN_OPEN),
        ("save", o.save, MIN_SAVE),
        ("sync", o.sync, MIN_SYNC),
        ("merge", o.merge, MIN_MERGE),
    ] {
        if got < min {
            eprintln!("warning: {got} {what} samples, fewer than the {min} a comparison needs");
        }
    }
    let started = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let (mut machine, source) = (machine(), source());
    let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("?").to_owned();
    println!(
        "store_bench {} ({}{}), {}, {} threads, {}, {}, {}, {}, load {}",
        text(&source, "crateVersion"),
        text(&source, "commit"),
        if source.get("trackedFilesChanged") == Some(&json!(true)) {
            ", tracked files changed"
        } else {
            ""
        },
        text(&machine, "cpu"),
        machine.get("threads").and_then(Value::as_u64).unwrap_or(0),
        text(&machine, "power"),
        text(&machine, "os"),
        text(&machine, "rustc"),
        text(&source, "profile"),
        text(&machine, "loadAverageAtStart"),
    );
    println!(
        "{:>8}  {:<32} {:>4} {:>11} {:>11}",
        "segments", "operation", "n", "median ms", "p95 ms"
    );

    let clock = ManualClock::at(NOW);
    let mut results = Vec::new();
    for &n in &o.sizes {
        let work = tempfile::Builder::new()
            .prefix("z-work-store-bench-")
            .tempdir()?;
        let dir = work.path().join("store");
        seed_store(&dir, n)?;
        let local = local_data(n)?;
        let remote_bytes = remote_file_bytes(&local, n)?;
        let remote = remote_data(&local, n)?;

        let open = Stats::of(bench_open(&dir, &clock, o.open)?)?;
        // After the untimed open: what the store keeps on disk once it has settled.
        let (data_bytes, data_files) = dir_size(&dir)?;
        let save = Stats::of(bench_save(&dir, &clock, o.save)?)?;
        let (sync_samples, after_sync) = bench_sync(n, &clock, o.sync, &remote_bytes)?;
        let sync = Stats::of(sync_samples)?;
        let merge_ms = Stats::of(bench_merge(&local, &remote, o.merge))?;
        let merged_segments = merge(&local, &remote).segments.len();
        if after_sync != merged_segments {
            return Err(format!(
                "the sync cycle left {after_sync} segments where the merge makes {merged_segments}"
            )
            .into());
        }
        let mut ops = Map::new();
        for (key, s) in [
            ("open", &open),
            ("save", &save),
            ("sync", &sync),
            ("merge", &merge_ms),
        ] {
            println!(
                "{:>8}  {:<32} {:>4} {:>11.2} {:>11.2}",
                grouped(n),
                label(key),
                s.n,
                s.median,
                s.p95
            );
            ops.insert(key.to_owned(), s.json());
        }
        results.push(json!({
            "segments": n,
            "dataDirBytes": data_bytes,
            "dataDirFiles": data_files,
            "remoteFileBytes": remote_bytes.len(),
            "segmentsAfterSync": after_sync,
            "ops": ops,
        }));
        drop(work);
    }

    if let Some(m) = machine.as_object_mut() {
        m.insert("loadAverageAtEnd".to_owned(), json!(load_average()));
    }
    let report = json!({
        "tool": "store_bench",
        "formatVersion": 1,
        "startedAt": started,
        "finishedAt": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "source": source,
        "machine": machine,
        "method": {
            "clock": "wall clock (std::time::Instant), one sample per repetition, after one untimed run of the same operation",
            "statistics": "median (mean of the middle two for an even count) and nearest-rank p95 of all samples; every sample is listed",
            "data": "synthetic and deterministic: 12 projects, 400 tasks, N segments over 2024-2026 on one device, time frozen at 2027-01-01T03:00:00Z",
            "open": "Service::open on a store of N segments on disk; a new Service each time in one process, OS file cache warm; drop not timed",
            "save": "Service::patch_task renaming one task, including its save; a new name and one second later each time",
            "sync": "sync::run_cycle with one folder holding another device's file of N segments, N/2 of them new here: read, check, merge, local save, own file written; newly seeded store and folder for each sample, set up untimed",
            "merge": "merge::merge of the same two data sets in memory",
            "seeding": "seed_store is the only storage-specific step and is never timed",
        },
        "results": results,
    });
    if let Some(path) = &o.out {
        std::fs::write(path, serde_json::to_vec_pretty(&report)?)?;
        println!("results: {path}");
    }
    Ok(())
}
