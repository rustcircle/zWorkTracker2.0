//! Shared setup: a sample store in a temp dir, opened on a manual clock in a fixed zone.
#![allow(
    dead_code,
    reason = "each test crate uses a different part of this module"
)]

use std::sync::Arc;
use std::time::Duration;

use chrono::NaiveDate;
use chrono_tz::Tz;
use z_work_core::ManualClock;
use zwt_bridge::worker::{Command, Inbox, Update, Worker, WorkerConfig};

/// Wednesday 4 November 2026, 10:30 in Lisbon (UTC+0 then).
pub const NOW: &str = "2026-11-04T10:30:00Z";
pub const ZONE_NAME: &str = "Europe/Lisbon";
pub const ZONE: Tz = chrono_tz::Europe::Lisbon;

pub fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

pub fn today() -> NaiveDate {
    date(2026, 11, 4)
}

pub fn config(dir: &std::path::Path, clock: &ManualClock) -> WorkerConfig {
    WorkerConfig {
        dir: dir.to_path_buf(),
        clock: Arc::new(clock.clone()),
        system_zone: Some(ZONE_NAME.to_owned()),
        heartbeat: Duration::from_secs(30),
    }
}

/// A worker on a fresh sample store: the first update, and an inbox for the rest.
pub fn spawn(dir: &std::path::Path, clock: &ManualClock) -> (Worker, Update, Inbox) {
    let inbox = Inbox::new();
    let (worker, first) = Worker::spawn(config(dir, clock), inbox.notify()).unwrap();
    (worker, first, inbox)
}

/// Sends nothing: waits until the worker has handled everything sent so far, and returns what it notified.
pub fn settle(worker: &Worker, inbox: &Inbox) -> Vec<Update> {
    worker.flush().unwrap();
    inbox.take()
}

/// The one update the commands sent so far produced.
pub fn one(worker: &Worker, inbox: &Inbox) -> Update {
    let mut all = settle(worker, inbox);
    assert_eq!(all.len(), 1, "{all:#?}");
    all.pop().unwrap()
}

/// `n` minutes of a running app: the clock moves a minute at a time and the UI sends its minute tick, as the
/// Slint clock does on each minute boundary. Each tick is handled before the clock moves on, as in real time (the
/// clock is shared: moving it while the worker still has ticks queued would look like a sleep to the core).
/// Returns the updates those ticks produced.
pub fn minutes(worker: &Worker, inbox: &Inbox, clock: &ManualClock, n: u32) -> Vec<Update> {
    let mut out = Vec::new();
    for _ in 0..n {
        clock.advance(60);
        worker.send(Command::MinuteTick).unwrap();
        out.extend(settle(worker, inbox));
    }
    out
}
