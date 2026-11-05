//! The UI thread's half of the app (PLAN.md 5): the Slint models bound to `AppState`, zwt-bridge's hub writing them
//! row by row, the clocks (src/clock.rs), and the callbacks that send commands to the worker.
//!
//! [`Ui`] needs no event loop: the binary keeps it in a thread-local and feeds it the worker's updates through
//! `slint::invoke_from_event_loop`; UI tests own it and feed it the updates of a `zwt_bridge::worker::Inbox`.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

use chrono::NaiveDate;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use zwt_bridge::hub::{Applied, GroupedSink, Hub, RowSink, Sinks, Writes};
use zwt_bridge::views::{DayHead, DayPage, Running, ShareHead, StoppedPage, TaskGroup, TaskRow};
use zwt_bridge::worker::{Command, Commands, ErrorKind, Update};
use zwt_bridge::{DayChip, DayPiece, ShareLevel, ShareRow};

use crate::clock::{Clock1s, Countdown, MinuteClock, TimeSource};
use crate::convert::{self, int, iso};
use crate::{
    AppState, AppWindow, ChipData, PieceData, ShareArc, ShareRowData, Strings, TaskRowData,
};

/// The third-party notices the About screen shows (the repository's THIRD_PARTY_NOTICES.md).
pub const NOTICES: &str = include_str!("../../../THIRD_PARTY_NOTICES.md");

/// How long a notice stays on screen, in seconds, whether or not a task runs (1.x Toasts.tsx: eight seconds).
pub const NOTICE_SECS: u64 = 8;

/// A [`RowSink`] over a Slint `VecModel`: converts each row on write and counts the writes.
pub struct ModelSink<R, S: Clone + 'static> {
    model: Rc<VecModel<S>>,
    convert: Box<dyn Fn(&R) -> S>,
    writes: Writes,
}

impl<R, S: Clone + 'static> ModelSink<R, S> {
    fn new(convert: impl Fn(&R) -> S + 'static) -> Self {
        Self {
            model: Rc::new(VecModel::default()),
            convert: Box::new(convert),
            writes: Writes::default(),
        }
    }

    fn rc(&self) -> ModelRc<S> {
        ModelRc::from(Rc::clone(&self.model))
    }

    /// Every write this sink made to its model.
    #[must_use]
    pub const fn writes(&self) -> Writes {
        self.writes
    }
}

impl<R, S: Clone + 'static> RowSink<R> for ModelSink<R, S> {
    fn len(&self) -> usize {
        self.model.row_count()
    }

    fn set_row(&mut self, index: usize, row: &R) {
        if index < self.model.row_count() {
            self.model.set_row_data(index, (self.convert)(row));
            self.writes.set += 1;
        }
    }

    fn insert(&mut self, index: usize, row: &R) {
        let index = index.min(self.model.row_count());
        self.model.insert(index, (self.convert)(row));
        self.writes.inserted += 1;
    }

    fn remove(&mut self, index: usize) {
        if index < self.model.row_count() {
            self.model.remove(index);
            self.writes.removed += 1;
        }
    }
}

/// Writes to each model so far (tests count them: a clock tick must write none to the task list or the day axis).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModelWrites {
    pub tasks: Writes,
    pub pieces: Writes,
    pub chips: Writes,
    pub share_rows: Writes,
    pub share_arcs: Writes,
}

type Sender = Rc<dyn Fn(Command)>;

/// How many sent commands [`Ui::sent`] keeps (a minute tick a minute must not grow it for as long as the app runs).
pub const SENT_KEPT: usize = 256;

/// The commands sent: a count, and the last few for tests and diagnostics.
#[derive(Default)]
struct SentLog {
    total: u64,
    recent: VecDeque<String>,
}

impl SentLog {
    fn push(&mut self, command: String) {
        self.total = self.total.saturating_add(1);
        if self.recent.len() == SENT_KEPT {
            self.recent.pop_front();
        }
        self.recent.push_back(command);
    }
}

/// The now line's minute clock and what it runs on (review R2): today's axis on screen, which is the Timeline tab
/// (the window says when it changes: `timeline-shown`) with today picked, and no task running (the updates say).
struct NowLine {
    clock: MinuteClock,
    timeline_shown: Cell<bool>,
    today_idle: Cell<bool>,
}

impl NowLine {
    fn refresh(&self) {
        self.clock
            .follow(self.timeline_shown.get() && self.today_idle.get());
    }
}

/// The models, the hub and the clocks of one `AppWindow`.
pub struct Ui {
    app: slint::Weak<AppWindow>,
    hub: Hub,
    /// The task list, one model per group (AppState `active-tasks`, `draft-tasks`, `stopped-tasks`).
    tasks: GroupedSink<TaskRow, ModelSink<TaskRow, TaskRowData>>,
    pieces: ModelSink<DayPiece, PieceData>,
    chips: ModelSink<DayChip, ChipData>,
    share_rows: ModelSink<ShareRow, ShareRowData>,
    arcs: Rc<VecModel<ShareArc>>,
    arc_writes: Writes,
    clock: Clock1s,
    now_line: Rc<NowLine>,
    notice: Rc<Countdown>,
    sent: Rc<RefCell<SentLog>>,
    applied_hook: Option<Box<dyn FnMut()>>,
    lanes: i32,
}

/// The index of a Share level in `AppState` (0 Projects, 1 Tasks, 2 Subtasks, 3 Tags).
#[must_use]
pub fn level_index(level: ShareLevel) -> i32 {
    ShareLevel::ALL
        .iter()
        .position(|l| *l == level)
        .and_then(|i| i32::try_from(i).ok())
        .unwrap_or(0)
}

impl Ui {
    /// Binds fresh models to `app`, wires its callbacks to `commands`, and sets the values that never change.
    /// `time` is the clock the clocks read: the system clock in the app, the test's `ManualClock` in tests.
    pub fn new(app: &AppWindow, commands: Commands, time: Rc<dyn TimeSource>) -> Self {
        let sent: Rc<RefCell<SentLog>> = Rc::default();
        let send: Sender = {
            let sent = Rc::clone(&sent);
            Rc::new(move |command: Command| {
                sent.borrow_mut().push(format!("{command:?}"));
                if commands.send(command).is_err() {
                    eprintln!("zwt-app: the worker has stopped; the command was not sent");
                }
            })
        };
        // One model per group of the task list, in `TaskGroup::index` order: Active, Drafts, Stopped.
        let [active, drafts, stopped] = TaskGroup::ALL.map(|_| ModelSink::new(convert::task_row));
        let pieces = ModelSink::new(convert::piece);
        let chips = ModelSink::new(convert::chip);
        let share_rows = ModelSink::new(convert::share_row);
        let arcs: Rc<VecModel<ShareArc>> = Rc::new(VecModel::default());

        let state = app.global::<AppState>();
        state.set_active_tasks(active.rc());
        state.set_draft_tasks(drafts.rc());
        state.set_stopped_tasks(stopped.rc());
        let tasks = GroupedSink::new(|r: &TaskRow| r.group.index(), vec![active, drafts, stopped]);
        state.set_pieces(pieces.rc());
        state.set_chips(chips.rc());
        state.set_share_rows(share_rows.rc());
        state.set_share_arcs(ModelRc::from(Rc::clone(&arcs)));
        state.set_app_version(env!("CARGO_PKG_VERSION").into());
        state.set_notices_text(NOTICES.into());
        state.set_live_lines(convert::live_lines().into());
        state.set_pause_lines(convert::pause_lines().into());
        let start_sec = int(time.now_ms().div_euclid(1000));
        state.set_now_sec(start_sec);
        state.set_now_min(start_sec);

        let on = |f: fn(String) -> Command| {
            let send = Rc::clone(&send);
            move |id: SharedString| send(f(id.to_string()))
        };
        state.on_start_task(on(Command::Start));
        state.on_pause_task(on(Command::Pause));
        state.on_resume_task(on(Command::Resume));
        state.on_stop_task(on(Command::Stop));
        // The link under the Stopped group: the worker lists every stopped task or the first page, and sends the rows.
        {
            let send = Rc::clone(&send);
            state.on_show_all_stopped(move |all| send(Command::ShowAllStopped(all)));
        }
        {
            let send = Rc::clone(&send);
            state.on_select_day(move |date| {
                if let Ok(day) = NaiveDate::parse_from_str(date.as_str(), "%Y-%m-%d") {
                    send(Command::SelectDay(day));
                }
            });
        }
        // The day picker's arrows: the worker moves the page and sends the new chips (and refuses a move past the first
        // day with time or past today, should a disabled arrow ever be activated).
        {
            let send = Rc::clone(&send);
            state.on_show_older_days(move || send(Command::OlderDays));
        }
        {
            let send = Rc::clone(&send);
            state.on_show_newer_days(move || send(Command::NewerDays));
        }
        // The day axis' sub-grid, asked by the axis when its increment or its pixels a minute change
        // (ui/timeline.slint), never on a clock tick.
        state.on_sub_grid(convert::sub_grid);
        {
            let send = Rc::clone(&send);
            state.on_set_share_level(move |index| {
                let level = usize::try_from(index)
                    .ok()
                    .and_then(|i| ShareLevel::ALL.get(i).copied());
                if let Some(level) = level {
                    send(Command::SetShareLevel(level));
                }
            });
        }

        let clock = {
            let weak = app.as_weak();
            let send = Rc::clone(&send);
            Clock1s::new(
                Rc::clone(&time),
                move |sec| {
                    if let Some(app) = weak.upgrade() {
                        app.global::<AppState>().set_now_sec(int(sec));
                    }
                },
                move || send(Command::MinuteTick),
            )
        };

        // The now line while nothing runs: a minute clock, on only while today's axis is on screen (the first
        // update says what runs and which day is shown; until then it is off).
        let now_line = Rc::new(NowLine {
            clock: {
                let weak = app.as_weak();
                let send = Rc::clone(&send);
                // The day end the worker was last asked to catch up with, so it is asked once per day.
                let asked_at = Cell::new(i32::MIN);
                MinuteClock::new(Rc::clone(&time), move |sec| {
                    let Some(app) = weak.upgrade() else {
                        return;
                    };
                    let state = app.global::<AppState>();
                    let sec = int(sec);
                    state.set_now_min(sec);
                    // Past the end of the day shown as today: the computer slept across midnight, and the worker's
                    // midnight deadline runs on a clock that stops during sleep. MinuteTick is the command for a
                    // wake: the day rolls over now, not at the worker's next deadline (review R2, after midnight).
                    let day_end = state.get_day_end();
                    if sec >= day_end
                        && state.get_day_date() == state.get_today_date()
                        && asked_at.get() != day_end
                    {
                        asked_at.set(day_end);
                        send(Command::MinuteTick);
                    }
                })
            },
            timeline_shown: Cell::new(app.get_tab() == 0),
            today_idle: Cell::new(false),
        });
        {
            let line = Rc::downgrade(&now_line);
            state.on_timeline_shown(move |shown| {
                if let Some(line) = line.upgrade() {
                    line.timeline_shown.set(shown);
                    line.refresh();
                }
            });
        }

        // A notice goes after NOTICE_SECS or when dismissed (the window clears the text, then says so here).
        let notice = Rc::new(Countdown::new(Duration::from_secs(NOTICE_SECS)));
        {
            let notice = Rc::downgrade(&notice);
            state.on_notice_dismissed(move || {
                if let Some(notice) = notice.upgrade() {
                    notice.cancel();
                }
            });
        }

        Self {
            app: app.as_weak(),
            hub: Hub::new(),
            tasks,
            pieces,
            chips,
            share_rows,
            arcs,
            arc_writes: Writes::default(),
            clock,
            now_line,
            notice,
            sent,
            applied_hook: None,
            lanes: -1,
        }
    }

    /// Applies one update of the worker: writes the rows and values that changed, shows a notice or an error, and
    /// starts or stops the clocks when what runs or the day shown changed. Returns what the hub wrote.
    pub fn apply(&mut self, update: Update) -> Applied {
        let Some(app) = self.app.upgrade() else {
            return Applied::default();
        };
        let state = app.global::<AppState>();
        if let Some(notice) = &update.notice {
            self.notice(&state, notice.text());
        }
        if let Some(error) = &update.error {
            let text = match error.kind {
                // 1.x opens the form of a new, linked task instead (PARITY 86): milestone 2. Until then, say why.
                ErrorKind::ColdStartRequired => app
                    .global::<Strings>()
                    .get_task_cold_resume_note()
                    .to_string(),
                _ => error.message.clone(),
            };
            self.notice(&state, text);
        }
        let day_moved = update.day.is_some();
        let share_moved = update.share.is_some();

        let clock = &self.clock;
        let mut stopped_page = |p: &StoppedPage| {
            state.set_stopped_total(int(i64::try_from(p.total).unwrap_or(i64::MAX)));
            state.set_stopped_paged(p.has_link());
            state.set_stopped_all(p.all);
        };
        let mut running = |r: &Running| {
            state.set_running_id(r.task_id.as_deref().unwrap_or_default().into());
            clock.follow(r.is_running());
        };
        let mut day_head = |h: &DayHead| {
            state.set_day_date(iso(h.date));
            state.set_today_date(iso(h.today));
            state.set_day_label(h.label.as_str().into());
            state.set_day_start(int(h.start));
            state.set_day_end(int(h.end));
            let increment = int(i64::from(h.increment_minutes));
            if increment != state.get_day_increment() {
                state.set_day_increment(increment);
                // Nothing drawn reads the increment: the day axis follows it in a `changed` handler, which Slint runs
                // when it draws. Ask for a frame, or a new increment alone would wait for the next one (review F1).
                app.window().request_redraw();
            }
        };
        let mut page = |p: &DayPage| {
            state.set_days_older(p.older);
            state.set_days_newer(p.newer);
        };
        let mut share_head = |h: &ShareHead| {
            state.set_share_picked(h.picked.map_or(-1, level_index));
            state.set_share_shown(level_index(h.shown));
            state.set_share_subtasks_available(h.subtasks_available);
            state.set_share_total_text(h.total_text.as_str().into());
            state.set_share_note(h.note.as_deref().unwrap_or_default().into());
            state.set_share_footnote(h.footnote.as_deref().unwrap_or_default().into());
            state.set_share_period(h.period.label.as_str().into());
        };
        let applied = self.hub.apply(
            update,
            &mut Sinks {
                tasks: &mut self.tasks,
                stopped_page: &mut stopped_page,
                running: &mut running,
                day_head: &mut day_head,
                day_pieces: &mut self.pieces,
                chips: &mut self.chips,
                page: &mut page,
                share_head: &mut share_head,
                share_rows: &mut self.share_rows,
            },
        );
        if day_moved {
            self.write_lanes(&state);
        }
        let today_idle = self.hub.day_head().is_some_and(|h| h.date == h.today)
            && !self.hub.running().is_some_and(Running::is_running);
        self.now_line.today_idle.set(today_idle);
        self.now_line.refresh();
        if share_moved {
            self.write_arcs();
        }
        if let Some(hook) = self.applied_hook.as_mut() {
            hook();
        }
        applied
    }

    /// Shows `text` for [`NOTICE_SECS`] from now, in place of any notice shown (1.x Toasts.tsx: a timeout of its own,
    /// so it goes whether or not a task runs).
    fn notice(&self, state: &AppState<'_>, text: String) {
        state.set_notice_text(text.into());
        let app = self.app.clone();
        self.notice.start(move || {
            if let Some(app) = app.upgrade() {
                app.global::<AppState>()
                    .set_notice_text(SharedString::default());
            }
        });
    }

    /// The axis is as tall as its highest lane plus one (Axis.tsx).
    fn write_lanes(&mut self, state: &AppState<'_>) {
        let lanes = self
            .hub
            .day_pieces()
            .iter()
            .map(|p| int(i64::from(p.lane)).saturating_add(1))
            .max()
            .unwrap_or(1);
        if lanes != self.lanes {
            state.set_day_lanes(lanes);
            self.lanes = lanes;
        }
    }

    /// Where each slice starts and ends (charts.tsx Donut: each row's share of the total seconds, clockwise from the
    /// top), with the stripes of a combination of tags cut to its slice. Rewrites only the arcs that moved: the
    /// stripes follow from the angles and the number of colours.
    fn write_arcs(&mut self) {
        let rows = self.hub.share_rows();
        let total: i64 = rows.iter().map(|r| r.secs.max(0)).sum();
        let mut next = Vec::with_capacity(rows.len());
        let mut acc: i64 = 0;
        for row in rows {
            let from = degrees(acc, total);
            acc = acc.saturating_add(row.secs.max(0));
            next.push((from, degrees(acc, total), row.stripes.len()));
        }
        let model = &self.arcs;
        for (i, &(from, to, colors)) in next.iter().enumerate() {
            let striped = if colors >= 2 { colors } else { 0 };
            let old = model.row_data(i);
            if old
                .as_ref()
                .is_some_and(|o| o.from == from && o.to == to && o.stripes.row_count() == striped)
            {
                continue;
            }
            let arc = ShareArc {
                from,
                to,
                stripes: convert::arc_stripes(from, to, colors),
            };
            if old.is_some() {
                model.set_row_data(i, arc);
                self.arc_writes.set += 1;
            } else {
                model.push(arc);
                self.arc_writes.inserted += 1;
            }
        }
        while model.row_count() > next.len() {
            model.remove(model.row_count() - 1);
            self.arc_writes.removed += 1;
        }
    }

    /// The rows and values the hub holds.
    #[must_use]
    pub const fn hub(&self) -> &Hub {
        &self.hub
    }

    /// The 1 s clock.
    #[must_use]
    pub const fn clock(&self) -> &Clock1s {
        &self.clock
    }

    /// The now line's minute clock.
    #[must_use]
    pub fn now_line(&self) -> &MinuteClock {
        &self.now_line.clock
    }

    /// The timer that takes the notice away.
    #[must_use]
    pub fn notice_timer(&self) -> &Countdown {
        &self.notice
    }

    /// The last commands the callbacks and the clock sent (at most [`SENT_KEPT`]), oldest first, as `Debug` text
    /// (`Start("t-a11y")`, `MinuteTick`).
    #[must_use]
    pub fn sent(&self) -> Vec<String> {
        self.sent.borrow().recent.iter().cloned().collect()
    }

    /// How many commands the callbacks and the clock sent so far.
    #[must_use]
    pub fn sent_count(&self) -> u64 {
        self.sent.borrow().total
    }

    /// Writes to each model so far (the task list's: its three groups' together).
    #[must_use]
    pub fn writes(&self) -> ModelWrites {
        ModelWrites {
            tasks: self
                .tasks
                .sinks()
                .iter()
                .fold(Writes::default(), |all, group| all + group.writes()),
            pieces: self.pieces.writes(),
            chips: self.chips.writes(),
            share_rows: self.share_rows.writes(),
            share_arcs: self.arc_writes,
        }
    }

    /// Runs `hook` after each applied update (the latency measurement of `crate::measure`).
    pub fn set_applied_hook(&mut self, hook: Box<dyn FnMut()>) {
        self.applied_hook = Some(hook);
    }
}

/// `part` of `total` as degrees clockwise from the top; 0 when there is no total.
fn degrees(part: i64, total: i64) -> f32 {
    if total <= 0 {
        return 0.0;
    }
    // Seconds are far below 2^52, so f64 holds them exactly; an angle in f32 is finer than a pixel of the ring.
    (part as f64 / total as f64 * 360.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sent_log_counts_everything_and_keeps_the_last_few() {
        let mut log = SentLog::default();
        for i in 0..300 {
            log.push(i.to_string());
        }
        assert_eq!(log.total, 300);
        assert_eq!(log.recent.len(), SENT_KEPT);
        assert_eq!(log.recent.front().map(String::as_str), Some("44"));
        assert_eq!(log.recent.back().map(String::as_str), Some("299"));
    }

    #[test]
    fn slices_turn_seconds_into_degrees() {
        assert!((degrees(1, 4) - 90.0).abs() < f32::EPSILON);
        assert!((degrees(4, 4) - 360.0).abs() < f32::EPSILON);
        assert!(degrees(5, 0).abs() < f32::EPSILON, "no total, no angle");
        assert_eq!(level_index(ShareLevel::Tags), 3);
        assert_eq!(level_index(ShareLevel::Projects), 0);
    }
}
