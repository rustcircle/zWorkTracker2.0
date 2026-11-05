//! The worker: one thread owns `Service`; the UI talks to it over a channel and hears back through a callback.
//!
//! The thread blocks in `recv` (or `recv_timeout` to its next real deadline: a heartbeat every
//! [`HEARTBEAT`] while a task runs here, and the next local midnight, when the day picker and "Today" move on).
//! A heartbeat also ticks the core's away detector (`Service::observe_activity`), which takes more than two
//! minutes without a tick for a sleep; [`Command::MinuteTick`] ticks it too.
//! After each command it compares `Service::revision()`, diffs the sections of the file into [`Topics`]
//! ([`crate::topics::diff`]), recomputes only the views of those topics, and drops any whose output did not change.
//! The callback is called only when something is left (or for a notice or an error). Nothing here polls.

use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use chrono::NaiveDate;
use chrono_tz::Tz;
use z_work_core::machine::Transition;
use z_work_core::model::LocalFile;
use z_work_core::service::Service;
use z_work_core::{Clock, CoreError, SystemClock};
use zwt_ui_logic::day::DayChip;
use zwt_ui_logic::share::ShareLevel;

use crate::sample;
use crate::topics::{Scope, Topics, diff};
use crate::views::{
    self, DayPage, DayView, Period, Running, ShareData, StoppedPage, TaskRow, day_of, day_view,
    page_last, scope_of, task_list, week_of,
};

/// How often a running task's heartbeat is saved: a crash loses at most this much (the 1.x shell's interval).
pub const HEARTBEAT: Duration = Duration::from_secs(30);

/// What the worker needs to open the data.
#[derive(Clone)]
pub struct WorkerConfig {
    /// The data folder, already checked by [`crate::datadir::resolve`].
    pub dir: PathBuf,
    /// The clock of the core: `SystemClock` in the app, a `ManualClock` in tests.
    pub clock: Arc<dyn Clock>,
    /// The computer's zone as an IANA name; `None` asks the OS (and falls back to UTC).
    pub system_zone: Option<String>,
    /// The heartbeat interval while a task runs.
    pub heartbeat: Duration,
}

impl WorkerConfig {
    /// The app's settings for `dir`: system clock, the OS zone, a 30 s heartbeat.
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            clock: Arc::new(SystemClock),
            system_zone: None,
            heartbeat: HEARTBEAT,
        }
    }
}

impl fmt::Debug for WorkerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkerConfig")
            .field("dir", &self.dir)
            .field("clock_now", &self.clock.now())
            .field("system_zone", &self.system_zone)
            .field("heartbeat", &self.heartbeat)
            .finish()
    }
}

/// Called on the worker thread with each update. The UI wraps `slint::invoke_from_event_loop` in it. It should not
/// panic (ignore the error `invoke_from_event_loop` returns once the event loop has ended); a panic is caught and
/// counted in [`Stats::lost`], and that update is lost.
pub type Notify = Box<dyn Fn(Update) + Send + 'static>;

type MutateFn = Box<dyn FnOnce(&mut Service) -> Result<(), CoreError> + Send + 'static>;

/// A change made directly on the `Service`, for edits milestone 1 has no command for (tests; later forms).
pub struct Mutation {
    label: &'static str,
    run: MutateFn,
}

impl Mutation {
    pub fn new(
        label: &'static str,
        run: impl FnOnce(&mut Service) -> Result<(), CoreError> + Send + 'static,
    ) -> Self {
        Self {
            label,
            run: Box::new(run),
        }
    }
}

impl fmt::Debug for Mutation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Mutation").field(&self.label).finish()
    }
}

/// What the UI asks of the worker.
#[derive(Debug)]
#[non_exhaustive]
pub enum Command {
    /// Starts a draft. A task running here is paused first; the update carries [`Notice::AutoPaused`].
    Start(String),
    Pause(String),
    /// Resumes a paused task (pausing a running one, as `Start` does). Refused on a cold launch:
    /// [`ErrorKind::ColdStartRequired`].
    Resume(String),
    Stop(String),
    /// Shows another day on the day axis. Only the `DAY` topic moves. Today's date means "follow today".
    SelectDay(NaiveDate),
    /// Moves the day picker one page ([`views::CHIP_DAYS`] days) into the past: its left arrow. Only the `CHIPS`
    /// topic moves; the day on the axis stays. Ignored while no day with time lies before the page
    /// ([`DayPage::older`]).
    OlderDays,
    /// Moves the day picker one page towards today: its right arrow. Only the `CHIPS` topic moves; the day on the axis
    /// stays. Ignored on the page that ends today ([`DayPage::newer`]): there are no future days.
    NewerDays,
    /// Picks the Share of time level; it stays until the next pick. Only the `SHARE` topic moves.
    SetShareLevel(ShareLevel),
    /// The link under the Stopped group (1.x TaskRail.tsx): `true` lists every stopped task ("Show all N stopped
    /// tasks"), `false` only the first [`views::STOPPED_PAGE`] ("Show fewer"); it stays so until the next. Only the
    /// `TASKS` topic moves, and only when the page changes.
    ShowAllStopped(bool),
    /// A minute boundary passed: recomputes the minute-level figures (Share of time) with the current time, and
    /// ticks the core's away detector as a heartbeat does. Nothing else is recomputed, except at the first tick
    /// after local midnight or when the core found a sleep (a clock jump of over two minutes since the last tick).
    /// Also the command to send when the computer wakes (milestone 3's OS hook): the worker's own midnight
    /// deadline runs on a clock that stops during sleep, so after a night asleep with nothing running the day
    /// rolls over at the first command, not on waking.
    MinuteTick,
    /// Runs a change on the `Service`, then updates whatever it changed.
    Mutate(Mutation),
    /// Pauses what runs here (`Service::on_exit`, the 1.x Quit) and ends the thread. No update follows.
    Shutdown,
}

/// The command an error answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CommandKind {
    Start,
    Pause,
    Resume,
    Stop,
    Mutate,
    MinuteTick,
    /// The worker's own heartbeat (an activity tick, then the heartbeat save).
    Heartbeat,
}

/// What went wrong, for the UI to word.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// Resume on a cold launch: 1.x opens the form for a new linked task instead (PARITY 86).
    ColdStartRequired,
    /// The task's state does not allow it (e.g. pausing a draft).
    IllegalTransition,
    NotFound,
    /// A required field is missing, or a value is not valid.
    Validation,
    RunningElsewhere,
    ReadOnly,
    Conflict,
    /// The store could not be read or written.
    Storage,
    Other,
}

/// A refused or failed command. Never a panic: the worker carries on.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{command:?} failed ({code}): {message}")]
pub struct CommandError {
    pub command: CommandKind,
    pub task_id: Option<String>,
    pub kind: ErrorKind,
    /// The core's error code (`CoreError::code`).
    pub code: &'static str,
    /// The core's message, for logs and tests.
    pub message: String,
}

impl CommandError {
    fn new(command: CommandKind, task_id: Option<String>, e: &CoreError) -> Self {
        let code = e.code();
        let kind = match code {
            "cold_start_required" => ErrorKind::ColdStartRequired,
            "illegal_transition" => ErrorKind::IllegalTransition,
            "not_found" => ErrorKind::NotFound,
            "validation" => ErrorKind::Validation,
            "running_elsewhere" => ErrorKind::RunningElsewhere,
            "read_only" => ErrorKind::ReadOnly,
            "conflict" | "overlap" => ErrorKind::Conflict,
            "io" | "store_unavailable" | "future_version" => ErrorKind::Storage,
            _ => ErrorKind::Other,
        };
        Self {
            command,
            task_id,
            kind,
            code,
            message: e.to_string(),
        }
    }
}

/// Something to tell the user that is not an error.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Notice {
    /// Starting or resuming `by_task` paused `task`, which was running here (PARITY 91).
    AutoPaused {
        task_id: String,
        task_name: String,
        by_task_id: String,
        by_task_name: String,
    },
}

impl Notice {
    /// The words 1.x shows (`useCommands.ts`).
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::AutoPaused {
                task_name,
                by_task_name,
                ..
            } => format!(
                "Paused \"{task_name}\" because you started \"{by_task_name}\". Only one task is timed at a time."
            ),
        }
    }
}

/// What changed, with the new data of each changed view topic. `topics` names every topic that moved; a view
/// topic is in it exactly when its fields below are `Some` (`TASKS`: `tasks` and `stopped_page`, `CHIPS`: `chips`
/// and `page`, each pair always together).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Update {
    pub topics: Topics,
    /// The task list as the rail lists it: grouped Active, Drafts, Stopped, the Stopped group cut to its page
    /// ([`views::task_list`]).
    pub tasks: Option<Vec<TaskRow>>,
    /// The Stopped group's paging: how many stopped tasks there are, and whether all of them are listed.
    pub stopped_page: Option<StoppedPage>,
    pub running: Option<Running>,
    pub day: Option<DayView>,
    /// The day picker's chips, the oldest first.
    pub chips: Option<Vec<DayChip>>,
    /// The page those chips are: its days, and whether the picker can show older or newer ones.
    pub page: Option<DayPage>,
    pub share: Option<ShareData>,
    pub notice: Option<Notice>,
    pub error: Option<CommandError>,
}

impl Update {
    /// Nothing to tell.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.topics.is_empty() && self.notice.is_none() && self.error.is_none()
    }
}

/// Why the worker did not start.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StartError {
    #[error("could not open the data in {}", dir.display())]
    Open {
        dir: PathBuf,
        #[source]
        source: CoreError,
    },
    #[error("could not start the worker thread")]
    Thread(#[source] std::io::Error),
    #[error("the worker thread stopped while opening the data")]
    Stopped,
}

/// The worker has stopped (shut down, or its thread ended).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the worker has stopped")]
pub struct Gone;

/// Why `shutdown` did not end cleanly.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ShutdownError {
    #[error("could not pause the running task on the way out")]
    Exit(#[source] CoreError),
    #[error("the worker thread panicked")]
    Panicked,
}

/// Counters of the worker loop, for tests and diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Times the loop woke: a message or a deadline.
    pub wakeups: u64,
    pub commands: u64,
    pub heartbeats: u64,
    /// Calls of the callback.
    pub updates: u64,
    /// Calls of the callback that panicked (the panic is caught; the worker carries on).
    pub lost: u64,
}

#[derive(Default)]
struct Counters {
    wakeups: AtomicU64,
    commands: AtomicU64,
    heartbeats: AtomicU64,
    updates: AtomicU64,
    lost: AtomicU64,
}

impl Counters {
    fn bump(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    fn read(&self) -> Stats {
        Stats {
            wakeups: self.wakeups.load(Ordering::Relaxed),
            commands: self.commands.load(Ordering::Relaxed),
            heartbeats: self.heartbeats.load(Ordering::Relaxed),
            updates: self.updates.load(Ordering::Relaxed),
            lost: self.lost.load(Ordering::Relaxed),
        }
    }
}

enum Msg {
    Command(Command),
    /// Answered once every message sent before it has been handled and its update delivered.
    Flush(SyncSender<()>),
}

/// A cloneable handle for sending commands, e.g. from UI callbacks.
#[derive(Clone, Debug)]
pub struct Commands(Sender<Msg>);

impl Commands {
    /// Queues `command`; never blocks.
    pub fn send(&self, command: Command) -> Result<(), Gone> {
        self.0.send(Msg::Command(command)).map_err(|_| Gone)
    }
}

type Exit = Result<(), CoreError>;

/// The running worker. Dropping it shuts it down (pausing what runs here) and waits for the thread.
pub struct Worker {
    commands: Commands,
    thread: Option<JoinHandle<Exit>>,
    counters: Arc<Counters>,
    dir: PathBuf,
}

impl fmt::Debug for Worker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Worker")
            .field("dir", &self.dir)
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

impl Worker {
    /// Opens (or seeds) the store on a new thread and returns once it is open, with the first update: every view
    /// topic, for the UI to fill its models. Later updates go to `notify`, on the worker thread.
    pub fn spawn(config: WorkerConfig, notify: Notify) -> Result<(Self, Update), StartError> {
        let (tx, rx) = mpsc::channel::<Msg>();
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<Update, StartError>>(1);
        let counters = Arc::new(Counters::default());
        let thread_counters = Arc::clone(&counters);
        let dir = config.dir.clone();
        let thread = thread::Builder::new()
            .name("zwt-worker".to_owned())
            .spawn(move || -> Exit {
                match State::open(&config, notify, thread_counters) {
                    Ok((state, first)) => {
                        if ready_tx.send(Ok(first)).is_err() {
                            return Ok(());
                        }
                        run(state, &rx)
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        Ok(())
                    }
                }
            })
            .map_err(StartError::Thread)?;
        match ready_rx.recv() {
            Ok(Ok(first)) => Ok((
                Self {
                    commands: Commands(tx),
                    thread: Some(thread),
                    counters,
                    dir,
                },
                first,
            )),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err(StartError::Stopped)
            }
        }
    }

    /// Queues `command`; never blocks.
    pub fn send(&self, command: Command) -> Result<(), Gone> {
        self.commands.send(command)
    }

    /// A handle for sending commands from elsewhere (UI callbacks).
    #[must_use]
    pub fn commands(&self) -> Commands {
        self.commands.clone()
    }

    /// Waits until every command sent before this call has been handled and its update delivered.
    pub fn flush(&self) -> Result<(), Gone> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.commands.0.send(Msg::Flush(tx)).map_err(|_| Gone)?;
        rx.recv().map_err(|_| Gone)
    }

    /// The loop's counters so far.
    #[must_use]
    pub fn stats(&self) -> Stats {
        self.counters.read()
    }

    /// Pauses what runs here, ends the thread and waits for it.
    pub fn shutdown(mut self) -> Result<(), ShutdownError> {
        self.stop()
    }

    fn stop(&mut self) -> Result<(), ShutdownError> {
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        let _ = self.commands.send(Command::Shutdown);
        match thread.join() {
            Ok(exit) => exit.map_err(ShutdownError::Exit),
            Err(_) => Err(ShutdownError::Panicked),
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// The views as last sent, so an unchanged recompute sends nothing.
#[derive(Default)]
struct Sent {
    tasks: Vec<TaskRow>,
    stopped_page: Option<StoppedPage>,
    running: Running,
    day: Option<DayView>,
    chips: Vec<DayChip>,
    page: Option<DayPage>,
    share: Option<ShareData>,
}

struct State {
    svc: Service,
    notify: Notify,
    counters: Arc<Counters>,
    heartbeat_every: Duration,
    zone: Tz,
    today: NaiveDate,
    /// The day the user picked; `None` follows today.
    picked_day: Option<NaiveDate>,
    /// How many pages of the day picker back from the page that ends today ([`page_last`]); 0 on open, so today shows.
    days_back: u32,
    /// The Share of time level the user picked. The core has no preference for it, so it lives here.
    picked_level: Option<ShareLevel>,
    /// Every stopped task is listed (1.x `showAllStopped`): false on open, as 1.x's rail starts.
    all_stopped: bool,
    period: Period,
    scope: Scope,
    /// The file as the views last saw it, its revision, and whether a task had started in this launch.
    seen: LocalFile,
    seen_revision: u64,
    seen_warm: bool,
    sent: Sent,
    next_beat: Option<Instant>,
    next_midnight: Option<Instant>,
}

impl State {
    fn open(
        config: &WorkerConfig,
        notify: Notify,
        counters: Arc<Counters>,
    ) -> Result<(Self, Update), StartError> {
        let zone_name = config
            .system_zone
            .clone()
            .or_else(|| iana_time_zone::get_timezone().ok())
            .unwrap_or_else(|| "UTC".to_owned());
        let opened = sample::open_or_seed(&config.dir, Arc::clone(&config.clock), &zone_name)
            .map_err(|source| StartError::Open {
                dir: config.dir.clone(),
                source,
            })?;
        let svc = opened.service;
        let zone = svc.viewing_zone();
        let today = day_of(svc.now(), zone);
        let period = week_of(today, zone);
        let scope = scope_of(today, today, &period, zone);
        let mut state = Self {
            seen: svc.file().clone(),
            seen_revision: svc.revision(),
            seen_warm: svc.is_warm(),
            svc,
            notify,
            counters,
            heartbeat_every: config.heartbeat,
            zone,
            today,
            picked_day: None,
            days_back: 0,
            picked_level: None,
            all_stopped: false,
            period,
            scope,
            sent: Sent::default(),
            next_beat: None,
            next_midnight: None,
        };
        let mut first = Update::default();
        state.recompute(Topics::VIEWS, true, &mut first);
        state.schedule();
        Ok((state, first))
    }

    fn handle(&mut self, command: Command) -> Update {
        let mut forced = Topics::NONE;
        let mut update = Update::default();
        let task_error =
            |kind, id: &str, e: &CoreError| CommandError::new(kind, Some(id.to_owned()), e);
        match command {
            Command::Start(id) => match self.svc.start_task(&id) {
                Ok(t) => update.notice = self.auto_paused(t, &id),
                Err(e) => update.error = Some(task_error(CommandKind::Start, &id, &e)),
            },
            Command::Resume(id) => match self.svc.resume_task(&id) {
                Ok(t) => update.notice = self.auto_paused(t, &id),
                Err(e) => update.error = Some(task_error(CommandKind::Resume, &id, &e)),
            },
            Command::Pause(id) => {
                if let Err(e) = self.svc.pause_task(&id) {
                    update.error = Some(task_error(CommandKind::Pause, &id, &e));
                }
            }
            Command::Stop(id) => {
                if let Err(e) = self.svc.stop_task(&id) {
                    update.error = Some(task_error(CommandKind::Stop, &id, &e));
                }
            }
            Command::Mutate(m) => {
                if let Err(e) = (m.run)(&mut self.svc) {
                    update.error = Some(CommandError::new(CommandKind::Mutate, None, &e));
                }
            }
            Command::SelectDay(day) => {
                self.picked_day = Some(day);
                forced |= Topics::DAY;
            }
            Command::OlderDays => {
                let last = page_last(self.today, self.days_back);
                if views::day_page(self.svc.file(), last, self.today, self.zone).older {
                    self.days_back = self.days_back.saturating_add(1);
                    forced |= Topics::CHIPS;
                }
            }
            Command::NewerDays => {
                if let Some(back) = self.days_back.checked_sub(1) {
                    self.days_back = back;
                    forced |= Topics::CHIPS;
                }
            }
            Command::SetShareLevel(level) => {
                self.picked_level = Some(level);
                forced |= Topics::SHARE;
            }
            Command::ShowAllStopped(all) => {
                if all != self.all_stopped {
                    self.all_stopped = all;
                    forced |= Topics::TASKS;
                }
            }
            Command::MinuteTick => {
                update.error = self.tick(CommandKind::MinuteTick).err();
                forced |= Topics::SHARE;
            }
            // Handled by the loop; nothing to do here.
            Command::Shutdown => {}
        }
        self.refresh(forced, update)
    }

    fn auto_paused(&self, t: Transition, by: &str) -> Option<Notice> {
        let task_id = t.auto_paused_task_id?;
        Some(Notice::AutoPaused {
            task_name: t.auto_paused_task_name.unwrap_or_default(),
            task_id,
            by_task_id: by.to_owned(),
            by_task_name: self
                .svc
                .file()
                .data
                .tasks
                .get(by)
                .map_or_else(String::new, |t| t.name.clone()),
        })
    }

    /// Brings the views up to date after a command or a deadline: rolls the day and zone over, diffs the file if
    /// its revision moved, recomputes the topics that moved (plus `forced`), and keeps only changed output.
    fn refresh(&mut self, forced: Topics, mut update: Update) -> Update {
        let mut topics = forced;
        let zone = self.svc.viewing_zone();
        let today = day_of(self.svc.now(), zone);
        if zone != self.zone || today != self.today {
            self.zone = zone;
            self.today = today;
            self.period = week_of(today, zone);
            topics |= Topics::DAY | Topics::CHIPS | Topics::SHARE;
        }
        if self.picked_day == Some(self.today) {
            self.picked_day = None;
        }
        let day = self.picked_day.unwrap_or(self.today);
        let chips_last = page_last(self.today, self.days_back);
        self.scope = scope_of(day, chips_last, &self.period, self.zone);
        if self.svc.revision() != self.seen_revision {
            topics |= diff(&self.seen, self.svc.file(), &self.scope);
            self.seen = self.svc.file().clone();
            self.seen_revision = self.svc.revision();
        }
        let warm = self.svc.is_warm();
        if warm != self.seen_warm {
            self.seen_warm = warm;
            topics |= Topics::TASKS;
        }
        update.topics |= topics - Topics::VIEWS;
        self.recompute(topics, false, &mut update);
        self.schedule();
        update
    }

    /// Recomputes the view topics in `topics` and puts those whose output changed (all of them with `all`) in
    /// `update`.
    fn recompute(&mut self, topics: Topics, all: bool, update: &mut Update) {
        let file = self.svc.file();
        let sent = &mut self.sent;
        if topics.contains(Topics::TASKS) {
            let list = task_list(file, self.svc.is_warm(), self.all_stopped);
            if all || list.rows != sent.tasks || sent.stopped_page != Some(list.stopped) {
                sent.tasks.clone_from(&list.rows);
                sent.stopped_page = Some(list.stopped);
                update.tasks = Some(list.rows);
                update.stopped_page = Some(list.stopped);
                update.topics |= Topics::TASKS;
            }
        }
        if topics.contains(Topics::RUNNING) {
            let running = views::running(file);
            if all || running != sent.running {
                sent.running = running.clone();
                update.running = Some(running);
                update.topics |= Topics::RUNNING;
            }
        }
        if topics.contains(Topics::DAY) {
            let day = self.picked_day.unwrap_or(self.today);
            let view = day_view(file, day, self.today, self.zone);
            if all || sent.day.as_ref() != Some(&view) {
                sent.day = Some(view.clone());
                update.day = Some(view);
                update.topics |= Topics::DAY;
            }
        }
        if topics.contains(Topics::CHIPS) {
            let last = page_last(self.today, self.days_back);
            let chips = views::chips(file, last, self.zone);
            let page = views::day_page(file, last, self.today, self.zone);
            if all || chips != sent.chips || sent.page != Some(page) {
                sent.chips.clone_from(&chips);
                sent.page = Some(page);
                update.chips = Some(chips);
                update.page = Some(page);
                update.topics |= Topics::CHIPS;
            }
        }
        if topics.contains(Topics::SHARE) {
            let share = views::share(file, &self.period, self.svc.now(), self.picked_level);
            if all || sent.share.as_ref() != Some(&share) {
                sent.share = Some(share.clone());
                update.share = Some(share);
                update.topics |= Topics::SHARE;
            }
        }
    }

    /// Sets the deadlines: a heartbeat while a task runs here, and the next local midnight.
    fn schedule(&mut self) {
        let running = crate::topics::own_open(self.svc.file()).is_some();
        let now = Instant::now();
        match (running, self.next_beat) {
            (true, None) => self.next_beat = now.checked_add(self.heartbeat_every),
            (false, Some(_)) => self.next_beat = None,
            _ => {}
        }
        let tomorrow = views::day_span(self.today, self.zone).end;
        let wait = u64::try_from(tomorrow - self.svc.now().secs())
            .unwrap_or(0)
            .max(1);
        self.next_midnight = now.checked_add(Duration::from_secs(wait));
    }

    fn deadline(&self) -> Option<Instant> {
        match (self.next_beat, self.next_midnight) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// A deadline passed: a heartbeat if one is due, then catch up with the clock (a new day, at midnight).
    fn on_deadline(&mut self, now: Instant) -> Update {
        let mut update = Update::default();
        if self.next_beat.is_some_and(|at| at <= now) {
            Counters::bump(&self.counters.heartbeats);
            update.error = self.tick(CommandKind::Heartbeat).err();
            self.next_beat = now.checked_add(self.heartbeat_every);
        }
        self.refresh(Topics::NONE, update)
    }

    /// Time passed while a task may run: tells the core's away detector the app is alive (it counts more than
    /// two minutes without such a tick as a sleep, 1.x ticks every 5 s), then saves the heartbeat. With no OS idle
    /// figure before milestone 3, the tick says "unknown", which never makes an idle gap; a real sleep, seen as a
    /// clock jump between ticks, still does.
    fn tick(&mut self, kind: CommandKind) -> Result<(), CommandError> {
        self.svc
            .observe_activity(None)
            .map_err(|e| CommandError::new(kind, None, &e))?;
        self.svc
            .heartbeat()
            .map_err(|e| CommandError::new(kind, None, &e))
    }

    /// Calls the callback. A panic in it is caught, so the worker carries on and still pauses what runs at
    /// shutdown; the update is lost, as is any the UI could not take.
    fn deliver(&self, update: Update) {
        if update.is_empty() {
            return;
        }
        Counters::bump(&self.counters.updates);
        let notify = &self.notify;
        if panic::catch_unwind(AssertUnwindSafe(|| notify(update))).is_err() {
            Counters::bump(&self.counters.lost);
        }
    }
}

fn run(mut state: State, rx: &Receiver<Msg>) -> Exit {
    loop {
        Counters::bump(&state.counters.wakeups);
        let message = match state.deadline() {
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(at) => match at.checked_duration_since(Instant::now()) {
                Some(wait) if !wait.is_zero() => rx.recv_timeout(wait),
                _ => Err(RecvTimeoutError::Timeout),
            },
        };
        match message {
            Ok(Msg::Command(Command::Shutdown)) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(Msg::Command(command)) => {
                Counters::bump(&state.counters.commands);
                let update = state.handle(command);
                state.deliver(update);
            }
            Ok(Msg::Flush(done)) => {
                let _ = done.send(());
            }
            Err(RecvTimeoutError::Timeout) => {
                let update = state.on_deadline(Instant::now());
                state.deliver(update);
            }
        }
    }
    state.svc.on_exit()
}

/// A [`Notify`] that keeps every update, with a way to wait for the next one: for tests here and in the UI crate.
#[derive(Clone, Default)]
pub struct Inbox {
    updates: Arc<Mutex<Vec<Update>>>,
}

impl Inbox {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The callback to give [`Worker::spawn`].
    pub fn notify(&self) -> Notify {
        let updates = Arc::clone(&self.updates);
        Box::new(move |u| {
            updates
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(u);
        })
    }

    /// Every update received since the last call.
    #[must_use]
    pub fn take(&self) -> Vec<Update> {
        std::mem::take(&mut *self.updates.lock().unwrap_or_else(PoisonError::into_inner))
    }
}
