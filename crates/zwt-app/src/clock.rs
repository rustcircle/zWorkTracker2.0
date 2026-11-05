//! The app's clocks (PLAN.md 5), each one single-shot `slint::Timer` that runs only while something on screen needs
//! the time, and nothing polls:
//! - [`Clock1s`], while a task runs: fires on each whole second and writes `now-sec` (the only value that changes
//!   every second; elapsed texts, the running block and the now line derive from it in Slint) and, when a minute
//!   boundary has passed, sends `MinuteTick` to the worker.
//! - [`MinuteClock`], while nothing runs and today's axis is on screen: fires on each whole minute and writes the
//!   minute the now line shows (review R2: `now-sec` stands still then).
//! - [`Countdown`], while a notice shows: fires once, when the notice is due to go (review R2).
//!
//! The clocks are re-armed from their own shot for the next boundary of the clock they read, so they stay on it
//! instead of drifting by the time each shot takes.

use std::cell::Cell;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use slint::{Timer, TimerMode};
use z_work_core::Clock;

/// Where the clock reads the time, in milliseconds since the Unix epoch.
pub trait TimeSource {
    fn now_ms(&self) -> i64;
}

/// The system clock, which the worker's `SystemClock` also reads (to the second).
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemTimeSource;

impl TimeSource for SystemTimeSource {
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
    }
}

/// The core's clock, whole seconds: in UI tests the same `ManualClock` the worker reads.
#[derive(Clone)]
pub struct CoreTimeSource(pub Arc<dyn Clock>);

impl TimeSource for CoreTimeSource {
    fn now_ms(&self) -> i64 {
        self.0.now().secs().saturating_mul(1000)
    }
}

/// A shot this many milliseconds before a whole second still names that second.
const EARLY_MS: i64 = 50;

struct Inner {
    timer: Timer,
    time: Rc<dyn TimeSource>,
    on_second: Box<dyn Fn(i64)>,
    on_minute: Box<dyn Fn()>,
    minute: Cell<i64>,
    shots: Cell<u64>,
}

/// The 1 s clock. `follow` starts it when a task starts running and stops it when nothing runs.
pub struct Clock1s {
    inner: Rc<Inner>,
}

impl Clock1s {
    /// `on_second(unix_secs)` runs on each whole second while running; `on_minute()` when a minute boundary passed.
    pub fn new(
        time: Rc<dyn TimeSource>,
        on_second: impl Fn(i64) + 'static,
        on_minute: impl Fn() + 'static,
    ) -> Self {
        Self {
            inner: Rc::new(Inner {
                timer: Timer::default(),
                time,
                on_second: Box::new(on_second),
                on_minute: Box::new(on_minute),
                minute: Cell::new(0),
                shots: Cell::new(0),
            }),
        }
    }

    /// Runs the clock exactly while `running`. Starting writes the current second at once, so an elapsed text is
    /// right from the first frame, then waits for the next whole second.
    pub fn follow(&self, running: bool) {
        let inner = &self.inner;
        if !running {
            inner.timer.stop();
            return;
        }
        if inner.timer.running() {
            return;
        }
        let now = inner.time.now_ms();
        let sec = now.div_euclid(1000);
        inner.minute.set(sec.div_euclid(60));
        (inner.on_second)(sec);
        arm(inner, (sec + 1).saturating_mul(1000).saturating_sub(now));
    }

    /// The clock is running (a task runs).
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.inner.timer.running()
    }

    /// Shots fired so far.
    #[must_use]
    pub fn shots(&self) -> u64 {
        self.inner.shots.get()
    }
}

fn arm(inner: &Rc<Inner>, wait_ms: i64) {
    let wait = u64::try_from(wait_ms.clamp(1, 2000)).unwrap_or(1000);
    let weak: Weak<Inner> = Rc::downgrade(inner);
    inner.timer.start(
        TimerMode::SingleShot,
        Duration::from_millis(wait),
        move || {
            if let Some(inner) = weak.upgrade() {
                shot(&inner);
            }
        },
    );
}

fn shot(inner: &Rc<Inner>) {
    inner.shots.set(inner.shots.get().saturating_add(1));
    let now = inner.time.now_ms();
    let sec = now.saturating_add(EARLY_MS).div_euclid(1000);
    (inner.on_second)(sec);
    let minute = sec.div_euclid(60);
    if minute != inner.minute.get() {
        inner.minute.set(minute);
        (inner.on_minute)();
    }
    arm(inner, (sec + 1).saturating_mul(1000).saturating_sub(now));
}

const MINUTE_MS: i64 = 60_000;

struct MinuteInner {
    timer: Timer,
    time: Rc<dyn TimeSource>,
    on_time: Box<dyn Fn(i64)>,
    shots: Cell<u64>,
}

/// The now line's clock while nothing runs: `follow` runs it exactly while the line is on screen with no task
/// running, so an idle app with today's axis shown wakes once a minute, and one showing anything else not at all.
pub struct MinuteClock {
    inner: Rc<MinuteInner>,
}

impl MinuteClock {
    /// `on_time(unix_secs)` runs when the clock starts, with the current second, then on each whole minute with it.
    pub fn new(time: Rc<dyn TimeSource>, on_time: impl Fn(i64) + 'static) -> Self {
        Self {
            inner: Rc::new(MinuteInner {
                timer: Timer::default(),
                time,
                on_time: Box::new(on_time),
                shots: Cell::new(0),
            }),
        }
    }

    /// Runs the clock exactly while `wanted`. Starting writes the current second at once (the line is right from
    /// the first frame, where the 1 s clock left it or wherever time has moved to since), then waits for the next
    /// whole minute.
    pub fn follow(&self, wanted: bool) {
        let inner = &self.inner;
        if !wanted {
            inner.timer.stop();
            return;
        }
        if inner.timer.running() {
            return;
        }
        let now = inner.time.now_ms();
        (inner.on_time)(now.div_euclid(1000));
        let next = now.div_euclid(MINUTE_MS).saturating_add(1);
        arm_minute(inner, next.saturating_mul(MINUTE_MS).saturating_sub(now));
    }

    /// The clock is running (nothing runs and the now line is on screen).
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.inner.timer.running()
    }

    /// Shots fired so far.
    #[must_use]
    pub fn shots(&self) -> u64 {
        self.inner.shots.get()
    }
}

fn arm_minute(inner: &Rc<MinuteInner>, wait_ms: i64) {
    let wait = u64::try_from(wait_ms.clamp(1, MINUTE_MS + 1000)).unwrap_or(60_000);
    let weak: Weak<MinuteInner> = Rc::downgrade(inner);
    inner.timer.start(
        TimerMode::SingleShot,
        Duration::from_millis(wait),
        move || {
            if let Some(inner) = weak.upgrade() {
                minute_shot(&inner);
            }
        },
    );
}

fn minute_shot(inner: &Rc<MinuteInner>) {
    inner.shots.set(inner.shots.get().saturating_add(1));
    let now = inner.time.now_ms();
    // A shot a little early still names the minute it was armed for, as the 1 s clock does with its second.
    let minute = now.saturating_add(EARLY_MS).div_euclid(MINUTE_MS);
    (inner.on_time)(minute.saturating_mul(60));
    let next = minute.saturating_add(1);
    arm_minute(inner, next.saturating_mul(MINUTE_MS).saturating_sub(now));
}

/// A timer for something shown for a while (a notice): `start` runs `on_due` once, `after` from now, unless `cancel`
/// or another `start` comes first. Nothing runs while nothing is shown.
pub struct Countdown {
    timer: Timer,
    after: Duration,
}

impl Countdown {
    #[must_use]
    pub fn new(after: Duration) -> Self {
        Self {
            timer: Timer::default(),
            after,
        }
    }

    /// Starts over: `on_due` runs `after` from now, in place of whatever was due.
    pub fn start(&self, on_due: impl FnMut() + 'static) {
        self.timer.start(TimerMode::SingleShot, self.after, on_due);
    }

    /// Nothing is due any more.
    pub fn cancel(&self) {
        self.timer.stop();
    }

    /// Something is due.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.timer.running()
    }
}
