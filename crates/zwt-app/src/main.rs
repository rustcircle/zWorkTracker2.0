#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! zWorkTracker 2.0 desktop app, milestone 1: runs on a sample folder only (`--sample <folder>` or
//! `ZWT_DATA_DIR=<folder>`); without one it refuses to start.

use std::cell::RefCell;
use std::error::Error;
use std::process::ExitCode;
use std::rc::Rc;
use std::time::Instant;

use slint::ComponentHandle;
use zwt_app::AppWindow;
use zwt_app::clock::SystemTimeSource;
use zwt_app::exit;
use zwt_app::launch::{self, Refused};
use zwt_app::measure::{self, Measure};
use zwt_app::renderer::{self, Selected};
use zwt_app::ui::Ui;
use zwt_bridge::datadir::{DataDir, Launch, Platform};
use zwt_bridge::worker::{Notify, Update, Worker, WorkerConfig};

thread_local! {
    /// The UI thread's state. The worker's updates reach it through `slint::invoke_from_event_loop`, whose closure must
    /// be `Send` and so cannot hold the `Rc`s inside.
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

fn main() -> ExitCode {
    let started = Instant::now();
    // A diagnosis switch, which also has the renderer line name the GPU adapter (renderer.rs).
    let diagnosing = renderer::log_from_env();
    let measure = Measure::from_env();
    let dir = launch::data_dir(&Launch::from_env());
    // Before any window, the refusal window included, so that each draws as renderer.rs picks and the end of the event
    // loop shuts the worker down however it comes (exit.rs). A refusal is said even when this fails (no display).
    let selected = exit::select_backend();
    let dir = match dir {
        Ok(dir) => dir,
        Err(refused) => return stop(&refused, selected.ok(), diagnosing),
    };
    let selected = match selected {
        Ok(selected) => selected,
        Err(e) => return stop(&launch::failure(&e), None, diagnosing),
    };
    match run(dir, measure, started, selected.clone(), diagnosing) {
        Ok(()) => exit::finish(),
        Err(e) => stop(&launch::failure(&*e), Some(selected), diagnosing),
    }
}

/// Prints why the app does not start or could not go on. A Windows build may have no console to print to, so it also
/// shows the message in a small window there, drawn as `selected` says (as Slint picks when nothing was selected).
fn stop(stopped: &Refused, selected: Option<Selected>, diagnosing: bool) -> ExitCode {
    eprintln!("{}", stopped.message);
    if launch::shows_window(Platform::host())
        && let Ok(window) = launch::message_window(&stopped.message)
    {
        if let Some(selected) = selected {
            let _ = renderer::report(window.window(), selected, None, diagnosing);
        }
        let _ = window.run();
    }
    ExitCode::from(stopped.code)
}

fn apply(update: Update) {
    UI.with_borrow_mut(|ui| {
        if let Some(ui) = ui {
            ui.apply(update);
        }
    });
}

/// Runs the app on `dir`, on the platform `main` selected.
fn run(
    dir: DataDir,
    measure: Measure,
    started: Instant,
    selected: Selected,
    diagnosing: bool,
) -> Result<(), Box<dyn Error>> {
    #[cfg(unix)]
    let _signals = exit::quit_on_signals()?;
    let app = AppWindow::new()?;
    // Called on the worker thread, only when something changed: hop to the UI thread. After the loop has ended the
    // call fails, and the update is dropped with the window.
    let notify: Notify = Box::new(|update| {
        let _ = slint::invoke_from_event_loop(move || apply(update));
    });
    let (worker, first) = Worker::spawn(WorkerConfig::new(dir.path), notify)?;
    UI.set(Some(Ui::new(
        &app,
        worker.commands(),
        Rc::new(SystemTimeSource),
    )));
    exit::hold(worker);
    apply(first);
    // None unless a ZWT_MEASURE* variable is set.
    let hooks = measure::hooks(&app, measure, started, |hook| {
        UI.with_borrow_mut(|ui| {
            if let Some(ui) = ui {
                ui.set_applied_hook(hook);
            }
        });
    });
    let measuring = hooks.is_some();
    // Says on stderr what draws the window, then runs the hooks on each frame. The software renderer has no
    // rendering notifier: it only loses the numbers.
    if let Err(e) = renderer::report(app.window(), selected, hooks, diagnosing)
        && measuring
    {
        eprintln!("zwt-measure: no frame timing with this renderer: {e}");
    }
    let ran = app.run();
    // The loop's end has shut the worker down already (exit::Handler), pausing what runs as 1.x does on Quit. The
    // clock and the models go before the window; then, if the loop failed before it ended cleanly, the worker goes.
    UI.set(None);
    drop(app);
    exit::shut_down();
    let shut = exit::outcome();
    if let (Err(_), Err(e)) = (&ran, &shut) {
        // main reports the loop's error; this one would be lost otherwise.
        eprintln!("{}", launch::failure(e).message);
    }
    ran?;
    shut?;
    Ok(())
}
