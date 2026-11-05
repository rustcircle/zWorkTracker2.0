//! The app driven headlessly: a sample store in a temp dir (never the real data folder), the worker on the core's
//! `ManualClock` in a fixed zone, Slint's mock time, and the same `Ui` the binary uses, fed from an `Inbox` instead of
//! the event loop (zwt-bridge api.md, "UI tests without an event loop").
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers: a failed lookup is a failed test, and each test file uses a part of this module"
)]

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use i_slint_backend_testing::{
    AccessibleRole, ElementHandle, ElementRoot, TestingBackend, TestingBackendOptions,
};
use slint::platform::software_renderer::MinimalSoftwareWindow;
use slint::platform::{
    Key, Platform, PointerEventButton, WindowAdapter, WindowEvent, WindowEventDispatchResult,
};
use slint::{
    ComponentHandle, LogicalPosition, LogicalSize, Model, PlatformError, Rgba8Pixel,
    SharedPixelBuffer, SharedString,
};
use z_work_core::adjust::ManualEntryInput;
use z_work_core::service::Service;
use z_work_core::{CoreError, ManualClock, Ts};
use zwt_app::clock::{CoreTimeSource, TimeSource};
use zwt_app::ui::Ui;
use zwt_app::{AppState, AppWindow, TaskRowData};
use zwt_bridge::worker::{Command, Inbox, Mutation, Worker, WorkerConfig};

/// Wednesday 4 November 2026, 10:30 in Lisbon (UTC+0 then), as in zwt-bridge's tests.
pub const NOW: &str = "2026-11-04T10:30:00Z";
pub const ZONE: &str = "Europe/Lisbon";
/// The sample's running task once a fresh store opens, and the one draft left.
pub const RUNNING: &str = "t-social";
pub const RUNNING_NAME: &str = "Social templates";
pub const DRAFT: &str = "t-a11y";
pub const DRAFT_NAME: &str = "Accessibility fixes";

pub struct Harness {
    pub app: AppWindow,
    pub ui: Ui,
    pub worker: Worker,
    pub inbox: Inbox,
    pub clock: ManualClock,
    pub dir: tempfile::TempDir,
}

impl Harness {
    /// The testing backend without an event loop, and a window tall enough for every task card to be on screen.
    pub fn new() -> Self {
        i_slint_backend_testing::init_no_event_loop();
        Self::open(LogicalSize::new(1360.0, 3600.0))
    }

    /// The app on a fresh sample store, at `size`. The platform must be set already.
    pub fn open(size: LogicalSize) -> Self {
        Self::open_in(tempfile::tempdir().unwrap(), ManualClock::at(NOW), size)
    }

    /// The app drawn by Slint's software renderer (pixels through [`Harness::snapshot`]) at `size`. Sets the platform.
    pub fn rendered(size: LogicalSize) -> Self {
        software();
        Self::open(size)
    }

    /// [`Harness::open_empty`] drawn by the software renderer, as [`Harness::rendered`]. Sets the platform.
    pub fn rendered_empty(size: LogicalSize) -> Self {
        software();
        Self::open_empty(size)
    }

    /// The app on an empty store (no project, task or time), at `size`: the folder holds a store before the worker
    /// opens it, so no sample is seeded. The platform must be set already.
    pub fn open_empty(size: LogicalSize) -> Self {
        Self::open_empty_in(ZONE, NOW, size)
    }

    /// [`Harness::open_empty`] on a computer in `zone` (an IANA name) at `now` (ISO UTC).
    pub fn open_empty_in(zone: &str, now: &str, size: LogicalSize) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(now);
        drop(Service::open(dir.path(), Arc::new(clock.clone()), "Test computer").unwrap());
        Self::open_in_zone(dir, clock, zone, size)
    }

    /// [`Harness::open_empty`] on the testing backend without an event loop, in the window of [`Harness::new`].
    pub fn empty() -> Self {
        i_slint_backend_testing::init_no_event_loop();
        Self::open_empty(LogicalSize::new(1360.0, 3600.0))
    }

    /// Runs `run` on the worker's service as one mutation, then applies the update.
    pub fn mutate(
        &mut self,
        label: &'static str,
        run: impl FnOnce(&mut Service) -> Result<(), CoreError> + Send + 'static,
    ) {
        self.worker
            .send(Command::Mutate(Mutation::new(label, run)))
            .unwrap();
        self.pump();
    }

    /// Quits (the worker pauses what runs, as 1.x does on Quit) and starts again on the same store and clock: a cold
    /// launch, where nothing runs and Resume is refused until a task starts.
    pub fn relaunch(self) -> Self {
        self.relaunch_at(LogicalSize::new(1360.0, 3600.0))
    }

    /// [`Harness::relaunch`] with a window of `size`.
    pub fn relaunch_at(self, size: LogicalSize) -> Self {
        let Self {
            app,
            ui,
            worker,
            clock,
            dir,
            ..
        } = self;
        drop(ui);
        drop(app);
        worker.shutdown().unwrap();
        Self::open_in(dir, clock, size)
    }

    /// Every element of this component type, or of one inheriting it, in tree order.
    pub fn of_type(&self, type_name: &str) -> Vec<ElementHandle> {
        ElementHandle::find_by_element_type_name(&self.app, type_name).collect()
    }

    /// The window as the software renderer draws it now (needs [`Harness::rendered`]).
    pub fn snapshot(&self) -> Frame {
        Frame(self.app.window().take_snapshot().unwrap())
    }

    fn open_in(dir: tempfile::TempDir, clock: ManualClock, size: LogicalSize) -> Self {
        Self::open_in_zone(dir, clock, ZONE, size)
    }

    fn open_in_zone(
        dir: tempfile::TempDir,
        clock: ManualClock,
        zone: &str,
        size: LogicalSize,
    ) -> Self {
        let inbox = Inbox::new();
        let mut config = WorkerConfig::new(dir.path());
        config.clock = Arc::new(clock.clone());
        config.system_zone = Some(zone.to_owned());
        let (worker, first) = Worker::spawn(config, inbox.notify()).unwrap();
        let app = AppWindow::new().unwrap();
        app.window().set_size(size);
        let time: Rc<dyn TimeSource> = Rc::new(CoreTimeSource(Arc::new(clock.clone())));
        let mut ui = Ui::new(&app, worker.commands(), time);
        ui.apply(first);
        app.show().unwrap();
        Self {
            app,
            ui,
            worker,
            inbox,
            clock,
            dir,
        }
    }

    /// Waits for the worker to handle every command sent so far and applies its updates. Returns how many.
    pub fn pump(&mut self) -> usize {
        self.worker.flush().unwrap();
        let updates = self.inbox.take();
        let n = updates.len();
        for update in updates {
            self.ui.apply(update);
        }
        n
    }

    /// What the event loop does before it draws the next frame, with no time passing: Slint runs the `changed`
    /// handlers of properties that changed (the day axis' sub-grid follows its increment and width through them) and
    /// any timer already due.
    pub fn settle(&self) {
        settle();
    }

    /// One second of a running app: the core's clock and Slint's mock time move together, then whatever the clock
    /// sent (a minute tick) is handled.
    pub fn second(&mut self) {
        self.clock.advance(1);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_secs(1));
        self.pump();
    }

    pub fn state(&self) -> AppState<'_> {
        self.app.global::<AppState>()
    }

    /// Every task card's row in the Slint models, group by group: Active, Drafts, Stopped (the Stopped group's page).
    pub fn tasks(&self) -> Vec<TaskRowData> {
        let state = self.state();
        [
            state.get_active_tasks(),
            state.get_draft_tasks(),
            state.get_stopped_tasks(),
        ]
        .iter()
        .flat_map(|group| group.iter())
        .collect()
    }

    /// The task card's row in the Slint models.
    pub fn task(&self, id: &str) -> TaskRowData {
        self.tasks()
            .into_iter()
            .find(|t| t.id == id)
            .unwrap_or_else(|| panic!("no task {id} in the model"))
    }

    /// The text of the element with this accessible id (`elapsed:<task id>`).
    pub fn text_of(&self, accessible_id: &str) -> Option<String> {
        let id = accessible_id.to_owned();
        self.app
            .root_element()
            .query_descendants()
            .match_predicate(move |e| e.accessible_id().is_some_and(|a| a == id.as_str()))
            .find_first()
            .and_then(|e| e.accessible_label())
            .map(|s| s.to_string())
    }

    /// The elapsed or logged text of a task's card.
    pub fn elapsed(&self, task_id: &str) -> Option<String> {
        self.text_of(&format!("elapsed:{task_id}"))
    }

    /// A timer button of a task: inside the group "Controls for <task name>", the button labelled `label`.
    pub fn button(&self, task_name: &str, label: &str) -> ElementHandle {
        let group = format!("Controls for {task_name}");
        let controls = ElementHandle::find_by_accessible_label(&self.app, &group)
            .next()
            .unwrap_or_else(|| panic!("no group {group:?}"));
        let label = label.to_owned();
        controls
            .query_descendants()
            .match_accessible_role(AccessibleRole::Button)
            .match_predicate(move |e| e.accessible_label().is_some_and(|l| l == label.as_str()))
            .find_first()
            .unwrap_or_else(|| panic!("no button in {group:?}"))
    }

    /// An element of `role` labelled `label`.
    pub fn find(&self, role: AccessibleRole, label: &str) -> ElementHandle {
        let wanted = label.to_owned();
        self.app
            .root_element()
            .query_descendants()
            .match_accessible_role(role)
            .match_predicate(move |e| e.accessible_label().is_some_and(|l| l == wanted.as_str()))
            .find_first()
            .unwrap_or_else(|| panic!("no {role:?} labelled {label:?}"))
    }

    /// Every element of `role` labelled `label`.
    pub fn find_all(&self, role: AccessibleRole, label: &str) -> Vec<ElementHandle> {
        let wanted = label.to_owned();
        self.app
            .root_element()
            .query_descendants()
            .match_accessible_role(role)
            .match_predicate(move |e| e.accessible_label().is_some_and(|l| l == wanted.as_str()))
            .find_all()
    }

    /// Gives `element` the keyboard focus the way a mouse does, without activating it: press on it, then release the
    /// button away from it (a click needs the release on the element).
    pub fn focus(&self, element: &ElementHandle) {
        let at = element.absolute_position();
        let size = element.size();
        let centre = LogicalPosition::new(at.x + size.width / 2.0, at.y + size.height / 2.0);
        let away = LogicalPosition::new(2.0, 2.0);
        let window = self.app.window();
        window.dispatch_event(WindowEvent::PointerMoved { position: centre });
        window.dispatch_event(WindowEvent::PointerPressed {
            position: centre,
            button: PointerEventButton::Left,
        });
        window.dispatch_event(WindowEvent::PointerMoved { position: away });
        window.dispatch_event(WindowEvent::PointerReleased {
            position: away,
            button: PointerEventButton::Left,
        });
    }

    pub fn key(&self, event: WindowEvent) -> WindowEventDispatchResult {
        self.app.window().dispatch_event_with_result(event).unwrap()
    }

    /// Space as a keyboard sends it when pressed and held: down, three repeats, up.
    pub fn hold_space(&self) -> Vec<WindowEventDispatchResult> {
        let space = || SharedString::from(Key::Space);
        let mut out = vec![self.key(WindowEvent::KeyPressed { text: space() })];
        for _ in 0..3 {
            out.push(self.key(WindowEvent::KeyPressRepeated { text: space() }));
        }
        out.push(self.key(WindowEvent::KeyReleased { text: space() }));
        out
    }

    /// Enter, down and up.
    pub fn enter(&self) -> WindowEventDispatchResult {
        let down = self.key(WindowEvent::KeyPressed {
            text: Key::Return.into(),
        });
        self.key(WindowEvent::KeyReleased {
            text: Key::Return.into(),
        });
        down
    }

    /// Commands handled by the worker so far (it counts its own loop).
    pub fn handled(&self) -> u64 {
        self.worker.flush().unwrap();
        self.worker.stats().commands
    }

    /// Logs time by hand on "Logo concepts" from `start` to `end` (ISO UTC), through the worker, and applies the
    /// update: time on a day the sample leaves empty, e.g. before the day picker's first page.
    pub fn log_by_hand(&mut self, start: &str, end: &str) {
        let start = Ts::parse(start).unwrap();
        let end = Ts::parse(end).unwrap();
        self.worker
            .send(Command::Mutate(Mutation::new("log by hand", move |svc| {
                svc.add_manual_entry(&ManualEntryInput {
                    task_id: "t-logo".to_owned(),
                    start,
                    end,
                    reason: "Forgot to start the timer".to_owned(),
                    subtask_id: None,
                    allow_overlap: false,
                })
                .map(|_| ())
            })))
            .unwrap();
        self.pump();
    }
}

/// Lets Slint run what a change asked for (change handlers, timers due now) without moving the clock.
pub fn settle() {
    i_slint_backend_testing::mock_elapsed_time(Duration::ZERO);
}

/// The testing backend with Slint's software renderer, so a window can be drawn into pixels.
pub fn software() {
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        threading: false,
        renderer_name: Some("software".into()),
    })))
    .unwrap();
}

/// The left edges of the elements of `type_name` (`HourLine`, `SubLine`) on the day axis, left to right.
pub fn xs_of(h: &Harness, type_name: &str) -> Vec<f32> {
    let mut xs: Vec<f32> = h
        .of_type(type_name)
        .iter()
        .map(|e| e.absolute_position().x)
        .collect();
    xs.sort_by(f32::total_cmp);
    xs
}

/// The window's box of `element`: x, y, width, height in logical pixels (= pixels of a snapshot at scale 1).
pub fn rect_of(element: &ElementHandle) -> (f32, f32, f32, f32) {
    let at = element.absolute_position();
    let size = element.size();
    (at.x, at.y, size.width, size.height)
}

/// `inner` lies within `outer`.
pub fn encloses(outer: &ElementHandle, inner: &ElementHandle) -> bool {
    let (ox, oy, ow, oh) = rect_of(outer);
    let (ix, iy, iw, ih) = rect_of(inner);
    ix >= ox && iy >= oy && ix + iw <= ox + ow && iy + ih <= oy + oh
}

/// A frame drawn by the software renderer, at scale 1.
pub struct Frame(pub SharedPixelBuffer<Rgba8Pixel>);

impl Frame {
    pub fn width(&self) -> u32 {
        self.0.width()
    }

    pub fn height(&self) -> u32 {
        self.0.height()
    }

    /// The colour of the pixel at `x, y`.
    pub fn rgb(&self, x: u32, y: u32) -> [u8; 3] {
        let p = self.0.as_slice()[(y * self.0.width() + x) as usize];
        [p.r, p.g, p.b]
    }

    /// How far the colour at `x, y` is from `rgb`: the largest difference of a channel.
    pub fn distance(&self, x: u32, y: u32, rgb: [u8; 3]) -> u8 {
        let c = self.rgb(x, y);
        (0..3).map(|i| c[i].abs_diff(rgb[i])).max().unwrap()
    }

    /// The area (px squared) of the box `x, y, w, h` covered by `ink` over `paper`, from the pixels' green channel:
    /// a pixel of `paper` counts 0, one of `ink` 1, a blend of both its share.
    pub fn ink(&self, (x, y, w, h): (u32, u32, u32, u32), paper: [u8; 3], ink: [u8; 3]) -> f32 {
        let span = f32::from(paper[1]) - f32::from(ink[1]);
        let mut sum = 0.0;
        for py in y..y + h {
            for px in x..x + w {
                let g = f32::from(self.rgb(px, py)[1]);
                sum += ((f32::from(paper[1]) - g) / span).clamp(0.0, 1.0);
            }
        }
        sum
    }
}

/// `#rrggbb` as bytes.
pub fn hex(s: &str) -> [u8; 3] {
    let v = u32::from_str_radix(s.trim_start_matches('#'), 16).unwrap();
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

/// A clock the test sets by hand, in milliseconds.
pub struct FakeTime(pub Rc<Cell<i64>>);

impl TimeSource for FakeTime {
    fn now_ms(&self) -> i64 {
        self.0.get()
    }
}

/// A platform of one `MinimalSoftwareWindow`: it draws only when asked, and says whether a frame was due.
pub struct OneWindow(pub Rc<MinimalSoftwareWindow>);

impl Platform for OneWindow {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(Rc::clone(&self.0) as Rc<dyn WindowAdapter>)
    }
}

pub type TestResult = Result<(), Box<dyn std::error::Error>>;

/// `rgba` (8 bits a channel, `width` x `height`) as a PNG file at `path`.
pub fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> TestResult {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(rgba)?;
    writer.finish()?;
    Ok(())
}

/// The 1.x repo the drift tests read (tests/parity_*.rs): `$ZWT_1X_DIR`, else the first ancestor A of this crate with
/// `A/z-work-tracker/src/lib/themes.ts`, else the first ancestor A with `A/reference/z-work-tracker/src/lib/themes.ts`
/// (the gitignored read-only copy in the main checkout).
pub fn one_x_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("ZWT_1X_DIR") {
        return Some(PathBuf::from(dir));
    }
    // Two passes: a 1.x checkout beside any ancestor wins over the reference copy.
    ["z-work-tracker", "reference/z-work-tracker"]
        .into_iter()
        .find_map(|rel| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .map(|a| a.join(rel))
                .find(|c| c.join("src/lib/themes.ts").is_file())
        })
}
