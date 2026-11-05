//! Measurement hooks for milestone 1's numbers (PLAN.md 10). Inert unless one of these is set; [`hooks`] gives none
//! then:
//!
//! - `ZWT_MEASURE=1`: one line on stderr with the epoch milliseconds of the first rendered frame, the time since
//!   `main` started, and what drew it ([`renderer::Picked::fields`]).
//! - `ZWT_MEASURE_EXIT=1`: quit right after the first frame.
//! - `ZWT_MEASURE_LATENCY=<n>`: `n` rounds of Pause, Resume and two Share level switches, each sent through the same
//!   callback a click invokes, timed from the callback's entry to the end of the next frame rendered after the
//!   worker's answer was applied; then per-action statistics on stderr, and quit. When no frame begins within
//!   [`FRAME_DEADLINE`] of an applied answer, the run stops with one `zwt-measure latency stalled` line instead.
//!
//! Frames are seen through the window's rendering notifier, which [`renderer::report`] installs and which runs
//! [`hooks`]; no polling is added. The latency rounds arm a single-shot timer at each applied answer, so that a frame
//! that never comes ends the run instead of hanging it.
//!
//! On macOS, Slint 1.18 draws from the window's `CADisplayLink`, which does not tick while the display sleeps: a
//! redraw requested then waits until the display wakes, although the app has done its work. Such a wait times the
//! display, not the app, so it voids the run. `scripts/measure/measure.py` keeps the display awake (`caffeinate`).
//! Likewise, with FemtoVG on wgpu (renderer.rs) a covered window gets no drawable from Metal, so no frame is drawn
//! until the window is uncovered: a measured window must be in front (start the app as a bundle with `open -n`, which
//! brings it to the front, or check that nothing covers it).
//!
//! While a hook is on, the renderer's rendering notifier is set, which costs FemtoVG an extra pass per frame that a
//! run without hooks does not have.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use slint::{ComponentHandle, Model, RenderingState, Timer, TimerMode};

use crate::{AppState, AppWindow, renderer};

/// Which hooks are on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Measure {
    pub first_frame: bool,
    pub exit_after_first_frame: bool,
    pub latency_rounds: Option<u32>,
}

impl Measure {
    /// From this process's environment.
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_vars(|key| std::env::var(key).ok())
    }

    /// From `get(name)`, for tests.
    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> Self {
        let on = |key: &str| get(key).is_some_and(|v| v.trim() == "1");
        Self {
            first_frame: on("ZWT_MEASURE"),
            exit_after_first_frame: on("ZWT_MEASURE_EXIT"),
            latency_rounds: get("ZWT_MEASURE_LATENCY")
                .and_then(|v| v.trim().parse::<u32>().ok())
                .filter(|n| *n > 0),
        }
    }

    /// No hook is on.
    #[must_use]
    pub const fn is_inert(&self) -> bool {
        !self.first_frame && !self.exit_after_first_frame && self.latency_rounds.is_none()
    }
}

/// Milliseconds since the Unix epoch.
fn epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

#[derive(Clone, Debug)]
enum Action {
    Start(String),
    Pause(String),
    Resume(String),
    Level(i32),
}

impl Action {
    const fn name(&self) -> &'static str {
        match self {
            Self::Start(_) => "start",
            Self::Pause(_) => "pause",
            Self::Resume(_) => "resume",
            Self::Level(_) => "share-level",
        }
    }

    /// Invokes the callback a click on the matching button invokes.
    fn invoke(&self, state: &AppState<'_>) {
        match self {
            Self::Start(id) => state.invoke_start_task(id.as_str().into()),
            Self::Pause(id) => state.invoke_pause_task(id.as_str().into()),
            Self::Resume(id) => state.invoke_resume_task(id.as_str().into()),
            Self::Level(level) => state.invoke_set_share_level(*level),
        }
    }
}

/// Longest wait from an applied answer to the start of the frame that shows it. One frame is normal (about 10 ms;
/// at most 316 ms was seen at 50k segments under machine load); longer means no frame is being drawn.
pub const FRAME_DEADLINE: Duration = Duration::from_secs(1);

/// The action in flight.
struct Pending {
    action: Action,
    /// The callback's entry.
    started: Instant,
    /// When the worker's answer was applied.
    applied: Option<Instant>,
    /// When the first frame after that began.
    began: Option<Instant>,
}

/// An applied answer that no frame followed in time.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Stall {
    action: &'static str,
    /// From the applied answer to the start of the next frame, or to now when none began.
    waited: Duration,
}

/// What a finished frame leads to.
#[derive(Debug)]
enum Step {
    /// The pending answer is not on screen yet.
    Wait,
    /// Invoke this action next.
    Invoke(Action),
    /// Every action is done.
    Done,
    /// The run is void.
    Stalled(Stall),
}

#[derive(Default)]
struct Rounds {
    actions: Vec<Action>,
    next: usize,
    pending: Option<Pending>,
    samples: BTreeMap<&'static str, Vec<f64>>,
}

impl Rounds {
    /// The worker's answer to the pending action was applied at `now`; the first application counts.
    fn applied(&mut self, now: Instant) {
        if let Some(p) = &mut self.pending {
            p.applied.get_or_insert(now);
        }
    }

    /// A frame began at `now`.
    fn frame_began(&mut self, now: Instant) {
        if let Some(p) = &mut self.pending
            && p.applied.is_some()
        {
            p.began.get_or_insert(now);
        }
    }

    /// A frame ended at `now`: records the pending action once a frame that began after its answer has ended, then
    /// starts the next action at `now`.
    fn frame_ended(&mut self, now: Instant) -> Step {
        if let Some(p) = self.pending.take() {
            let (Some(applied), Some(began)) = (p.applied, p.began) else {
                // A frame before the answer (the clock's second, say): keep waiting.
                self.pending = Some(p);
                return Step::Wait;
            };
            let waited = began.saturating_duration_since(applied);
            if waited > FRAME_DEADLINE {
                return Step::Stalled(Stall {
                    action: p.action.name(),
                    waited,
                });
            }
            let ms = now.saturating_duration_since(p.started).as_secs_f64() * 1000.0;
            self.samples.entry(p.action.name()).or_default().push(ms);
        }
        let Some(action) = self.actions.get(self.next).cloned() else {
            return Step::Done;
        };
        self.next += 1;
        self.pending = Some(Pending {
            action: action.clone(),
            started: now,
            applied: None,
            began: None,
        });
        Step::Invoke(action)
    }

    /// At `now`: the pending answer was applied at least [`FRAME_DEADLINE`] ago and no frame has begun since.
    fn stalled(&self, now: Instant) -> Option<Stall> {
        let p = self.pending.as_ref()?;
        let applied = p.applied?;
        if p.began.is_some() {
            return None;
        }
        let waited = now.saturating_duration_since(applied);
        (waited >= FRAME_DEADLINE).then(|| Stall {
            action: p.action.name(),
            waited,
        })
    }
}

/// Statistics of one action's samples, in milliseconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stats {
    pub n: usize,
    pub median: f64,
    pub p95: f64,
    pub min: f64,
    pub max: f64,
}

/// Median (the mean of the middle two for an even count), p95 by nearest rank, min and max.
#[must_use]
pub fn stats(samples: &[f64]) -> Option<Stats> {
    let mut v = samples.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let min = *v.first()?;
    let max = *v.last()?;
    let median = if n % 2 == 1 {
        *v.get(n / 2)?
    } else {
        (*v.get(n / 2 - 1)? + *v.get(n / 2)?) / 2.0
    };
    // Nearest rank: the smallest sample with at least 95% of the samples at or below it.
    let rank = (n * 95).div_ceil(100).max(1);
    let p95 = *v.get(rank - 1)?;
    Some(Stats {
        n,
        median,
        p95,
        min,
        max,
    })
}

/// The frame hooks `measure` turns on, for [`renderer::report`] to run on each rendering state (the window's one
/// rendering notifier is the renderer's); `None` when none is on. `started` is when `main` began. `set_applied_hook`
/// hands the UI a closure to run after each applied update (the latency rounds wait for it).
pub fn hooks(
    app: &AppWindow,
    measure: Measure,
    started: Instant,
    set_applied_hook: impl FnOnce(Box<dyn FnMut()>),
) -> Option<renderer::Frames> {
    if measure.is_inert() {
        return None;
    }
    let rounds: Rc<RefCell<Rounds>> = Rc::default();
    let deadline = Rc::new(Timer::default());
    if measure.latency_rounds.is_some() {
        let rounds = Rc::clone(&rounds);
        let deadline = Rc::clone(&deadline);
        let weak = app.as_weak();
        set_applied_hook(Box::new(move || {
            rounds.borrow_mut().applied(Instant::now());
            // Fires only when no frame follows in time (each applied answer restarts it).
            let rounds = Rc::clone(&rounds);
            deadline.start(TimerMode::SingleShot, FRAME_DEADLINE, move || {
                let stall = rounds.borrow().stalled(Instant::now());
                if let Some(stall) = stall {
                    report_stall(stall, &rounds.borrow());
                    let _ = slint::quit_event_loop();
                }
            });
            // A frame must follow even when the answer changed nothing on screen.
            if let Some(app) = weak.upgrade() {
                app.window().request_redraw();
            }
        }));
    }
    let weak = app.as_weak();
    let mut first = true;
    Some(Box::new(move |state: &RenderingState| {
        match state {
            RenderingState::BeforeRendering if measure.latency_rounds.is_some() => {
                rounds.borrow_mut().frame_began(Instant::now());
                return;
            }
            RenderingState::AfterRendering => {}
            _ => return,
        }
        let Some(app) = weak.upgrade() else {
            return;
        };
        if first {
            first = false;
            if measure.first_frame {
                // What drew, as the renderer recorded it when this frame was set up.
                let drawn = renderer::picked().map(|p| format!(" {}", p.fields()));
                eprintln!(
                    "zwt-measure first-frame epoch-ms={} since-main-ms={:.1}{}",
                    epoch_ms(),
                    started.elapsed().as_secs_f64() * 1000.0,
                    drawn.unwrap_or_default()
                );
            }
            if let Some(n) = measure.latency_rounds {
                plan(&app, n, &mut rounds.borrow_mut());
            } else if measure.exit_after_first_frame {
                let _ = slint::quit_event_loop();
                return;
            }
        }
        if measure.latency_rounds.is_some() && step(&app, &rounds) {
            deadline.stop();
            let _ = slint::quit_event_loop();
        }
    }))
}

/// The actions of `n` rounds: Pause and Resume of the running task (a draft is started first when nothing runs),
/// then Tags and Projects on the Share level switch.
fn plan(app: &AppWindow, n: u32, rounds: &mut Rounds) {
    let state = app.global::<AppState>();
    // A running task is under Active, a draft under Drafts.
    let running = state
        .get_active_tasks()
        .iter()
        .find(|t| t.state == 1)
        .map(|t| t.id.to_string());
    let draft = state
        .get_draft_tasks()
        .iter()
        .find(|t| t.state == 0)
        .map(|t| t.id.to_string());
    let task = match (running, draft) {
        (Some(id), _) => Some(id),
        (None, Some(id)) => {
            rounds.actions.push(Action::Start(id.clone()));
            Some(id)
        }
        (None, None) => {
            eprintln!(
                "zwt-measure latency: nothing runs and no draft is left; timing the Share level only"
            );
            None
        }
    };
    for _ in 0..n {
        if let Some(id) = &task {
            rounds.actions.push(Action::Pause(id.clone()));
            rounds.actions.push(Action::Resume(id.clone()));
        }
        rounds.actions.push(Action::Level(3));
        rounds.actions.push(Action::Level(0));
    }
}

/// Called after each frame: records the pending action once its answer was applied and drawn, then starts the next.
/// True when the run is over (reported).
fn step(app: &AppWindow, rounds: &RefCell<Rounds>) -> bool {
    let step = rounds.borrow_mut().frame_ended(Instant::now());
    match step {
        Step::Wait => false,
        Step::Invoke(action) => {
            action.invoke(&app.global::<AppState>());
            false
        }
        Step::Done => {
            report(&rounds.borrow());
            true
        }
        Step::Stalled(stall) => {
            report_stall(stall, &rounds.borrow());
            true
        }
    }
}

/// A stalled run prints no statistics: its samples would mix the app's latency with the wait for a frame.
fn report_stall(stall: Stall, rounds: &Rounds) {
    eprintln!(
        "zwt-measure latency stalled action={} no-frame-ms={:.0} done={}/{} (no frame began within {} ms of the applied \
         answer; on macOS the display may be asleep)",
        stall.action,
        stall.waited.as_secs_f64() * 1000.0,
        rounds.next.saturating_sub(1),
        rounds.actions.len(),
        FRAME_DEADLINE.as_millis()
    );
}

fn report(rounds: &Rounds) {
    for (name, samples) in &rounds.samples {
        if let Some(s) = stats(samples) {
            eprintln!(
                "zwt-measure latency action={name} n={} median-ms={:.2} p95-ms={:.2} min-ms={:.2} max-ms={:.2}",
                s.n, s.median, s.p95, s.min, s.max
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_are_off_without_their_variables() {
        let m = Measure::from_vars(|_| None);
        assert!(m.is_inert());
        let m = Measure::from_vars(|k| (k == "ZWT_MEASURE_LATENCY").then(|| "0".to_owned()));
        assert!(m.is_inert(), "zero rounds is off");
        let m = Measure::from_vars(|k| (k == "ZWT_MEASURE").then(|| "yes".to_owned()));
        assert!(m.is_inert(), "only 1 turns a hook on");
    }

    #[test]
    fn each_variable_turns_on_its_hook() {
        let m = Measure::from_vars(|k| match k {
            "ZWT_MEASURE" | "ZWT_MEASURE_EXIT" => Some("1".to_owned()),
            "ZWT_MEASURE_LATENCY" => Some(" 20 ".to_owned()),
            _ => None,
        });
        assert_eq!(
            m,
            Measure {
                first_frame: true,
                exit_after_first_frame: true,
                latency_rounds: Some(20),
            }
        );
    }

    #[test]
    fn statistics_use_nearest_rank_and_the_middle_pair() {
        let s = stats(&[4.0, 1.0, 3.0, 2.0]).unwrap_or_else(|| unreachable!());
        assert_eq!(
            (s.n, s.median, s.p95, s.min, s.max),
            (4, 2.5, 4.0, 1.0, 4.0)
        );
        let twenty: Vec<f64> = (1..=20).map(f64::from).collect();
        let s = stats(&twenty).unwrap_or_else(|| unreachable!());
        assert_eq!((s.median, s.p95), (10.5, 19.0));
        assert!(stats(&[]).is_none());
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Rounds of Pause and Resume of one task, the first frame at `t0` (which invokes the Pause).
    fn pause_resume(t0: Instant) -> Rounds {
        let mut r = Rounds {
            actions: vec![
                Action::Pause("t".to_owned()),
                Action::Resume("t".to_owned()),
            ],
            ..Rounds::default()
        };
        assert!(matches!(r.frame_ended(t0), Step::Invoke(Action::Pause(_))));
        r
    }

    fn sample(r: &Rounds, action: &str) -> Option<f64> {
        r.samples.get(action).and_then(|v| v.first()).copied()
    }

    #[test]
    fn a_sample_runs_from_the_callback_to_the_end_of_the_frame_after_the_answer() {
        let t0 = Instant::now();
        let mut r = pause_resume(t0);
        r.frame_began(t0 + ms(5));
        assert!(
            matches!(r.frame_ended(t0 + ms(6)), Step::Wait),
            "a frame before the answer"
        );
        r.applied(t0 + ms(20));
        r.frame_began(t0 + ms(25));
        assert!(matches!(
            r.frame_ended(t0 + ms(30)),
            Step::Invoke(Action::Resume(_))
        ));
        assert!(sample(&r, "pause").is_some_and(|x| (x - 30.0).abs() < 1e-6));
    }

    #[test]
    fn an_answer_no_frame_follows_stops_the_run_instead_of_hanging_it() {
        // Display asleep (runs E1, loopA 8-10): the answer is applied, then no frame ever comes.
        let t0 = Instant::now();
        let mut r = pause_resume(t0);
        r.applied(t0 + ms(31));
        assert_eq!(r.stalled(t0 + ms(500)), None, "within the deadline");
        assert_eq!(
            r.stalled(t0 + ms(31) + FRAME_DEADLINE),
            Some(Stall {
                action: "pause",
                waited: FRAME_DEADLINE
            })
        );
    }

    #[test]
    fn a_frame_that_waited_for_the_display_to_wake_is_not_a_sample() {
        // Run E3: answer applied at +32 ms, the display woken at +7.5 s, the next frame began at +8.11 s.
        let t0 = Instant::now();
        let mut r = pause_resume(t0);
        r.applied(t0 + ms(32));
        r.frame_began(t0 + ms(8112));
        let step = r.frame_ended(t0 + ms(8131));
        assert!(
            matches!(
                step,
                Step::Stalled(Stall {
                    action: "pause",
                    ..
                })
            ),
            "{step:?}"
        );
        assert_eq!(sample(&r, "pause"), None, "no 8131 ms sample");
    }

    #[test]
    fn a_slow_answer_that_a_frame_follows_at_once_is_still_a_sample() {
        // Seen at 50k segments under machine load with the display on: 3.77 s to the applied answer.
        let t0 = Instant::now();
        let mut r = pause_resume(t0);
        r.applied(t0 + ms(3769));
        assert_eq!(r.stalled(t0 + ms(3800)), None);
        r.frame_began(t0 + ms(3826));
        assert!(matches!(r.frame_ended(t0 + ms(3997)), Step::Invoke(_)));
        assert!(sample(&r, "pause").is_some_and(|x| (x - 3997.0).abs() < 1e-6));
    }
}
