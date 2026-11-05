//! How the app ends: however the event loop stops, the worker shuts down first, so it pauses what runs as 1.x
//! does on Quit (`Service::on_exit`, pause reason `appQuit`), instead of leaving the task running for the next
//! launch's crash recovery (review R1).
//!
//! - The window's close button, or `slint::quit_event_loop`: the loop ends, winit calls [`Handler::exiting`], which
//!   runs [`loop_exiting`], and `app.run()` returns to `main`.
//! - macOS Quit (Cmd+Q, the app menu, the Dock, logging out): AppKit's `terminate:` calls `exit()` inside
//!   `applicationWillTerminate:`, so `app.run()` never returns. winit delivers `LoopExiting` from there first, and
//!   [`loop_exiting`] shuts the worker down. Behind it, best effort and macOS only, the one place where the process
//!   is ended by an `exit()` inside the loop: that `exit()` also runs the UI thread's thread-local destructors (Rust
//!   registers them with `_tlv_atexit`), and the [`Worker`] held in one shuts down when it is dropped. That is why a
//!   hook that did nothing still paused the task on macOS (review RV2 F1). Nothing relies on it, and a process ended
//!   by a signal's default action runs no destructors at all.
//! - SIGTERM, SIGINT and SIGHUP (Unix, [`quit_on_signals`]): a thread waits for them and quits the event loop as the
//!   close button does. Once the worker has shut down, [`finish`] ends the process by that signal's default action,
//!   so a shell or a supervisor sees what it sent (bash stops a script after a Ctrl+C only then). Another one while
//!   the shutdown runs, [`FORCE_AFTER`] or more after the first, ends the process at once by its own default action,
//!   so a shutdown that does not end can still be ended (security audit SA2 F4); what runs then stays running, for the
//!   next launch's crash recovery. One that comes sooner is the same request delivered twice (closing a zsh terminal
//!   sends SIGHUP twice) and lets the shutdown go on.
//! - Windows: closing the window, Alt+F4 and Task Manager's End task (a `WM_CLOSE` first) end the loop as the close
//!   button does. Logging off, shutting down or restarting do not: Windows sends `WM_QUERYENDSESSION` and
//!   `WM_ENDSESSION`, which winit 0.30 does not handle, and ends the process after `WM_ENDSESSION`; the next launch's
//!   crash recovery then closes the segment at the last heartbeat (at most [`zwt_bridge::worker::HEARTBEAT`] lost),
//!   as after a crash. Ctrl+C in a debug build's console ends the process the same way. Both are milestone 3
//!   (lifecycle): they need a window subclass for `WM_ENDSESSION` and `SetConsoleCtrlHandler`, unsafe FFI for the
//!   platform crate, each calling [`shut_down`].
//!
//! The worker lives in a thread-local of the UI thread from start-up ([`hold`]) until the first [`shut_down`];
//! later calls find nothing to do, so `main` calls it again after `app.run()` and reads the result with
//! [`outcome`].

use std::cell::RefCell;
use std::process::ExitCode;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

use slint::winit_030::winit::event_loop::ActiveEventLoop;
use slint::winit_030::{CustomApplicationHandler, EventResult};
use zwt_bridge::worker::{ShutdownError, Worker};

use crate::{launch, renderer};

thread_local! {
    /// The running worker, until it is shut down.
    static WORKER: RefCell<Option<Worker>> = const { RefCell::new(None) };
    /// What the shutdown gave, until `main` takes it.
    static OUTCOME: RefCell<Option<Result<(), ShutdownError>>> = const { RefCell::new(None) };
}

/// The number of the signal that ended the app, or 0.
static SIGNALLED: AtomicI32 = AtomicI32::new(0);

/// How long after the first quit signal a later one asks to end at once ([`quit_on_signals`]); sooner, it is the first
/// request again. The OS can deliver one request twice: closing the terminal of a zsh job sends SIGHUP from zsh and
/// again from the kernel, about 2 ms apart, and logind sends SIGTERM and then SIGHUP. A person who presses Ctrl+C
/// again, or a supervisor that gives up on the shutdown, comes later.
pub const FORCE_AFTER: Duration = Duration::from_secs(1);

/// Keeps `worker` until the app ends. Call it on the UI thread (the one that runs the event loop).
pub fn hold(worker: Worker) {
    WORKER.set(Some(worker));
}

/// Shuts the held worker down: it pauses what runs here, ends its thread and is waited for. Only the first call
/// does anything; its result is kept for [`outcome`].
pub fn shut_down() {
    let Some(worker) = WORKER.take() else {
        return;
    };
    OUTCOME.set(Some(worker.shutdown()));
}

/// The result of the shutdown, once: `Ok` when there was no worker to shut down, or once it was taken.
pub fn outcome() -> Result<(), ShutdownError> {
    OUTCOME.take().unwrap_or(Ok(()))
}

/// What the end of the event loop does, however it ends ([`Handler::exiting`]): [`shut_down`]. `stop_asked` says
/// whether the loop was asked to stop (`ActiveEventLoop::exiting`). When it was not, the OS ends the app here (macOS
/// `terminate:`) and `main` will not run again to say what went wrong, so this says it on stderr and takes the result;
/// a loop that was asked to stop returns to `main`, which reads the result with [`outcome`] and says it.
pub fn loop_exiting(stop_asked: bool) {
    shut_down();
    if !stop_asked && let Err(e) = outcome() {
        eprintln!("{}", launch::failure(&e).message);
    }
}

/// The winit hook that runs [`loop_exiting`] when the event loop ends, however it ends. It only forwards: the tests
/// run [`loop_exiting`] itself, since an `ActiveEventLoop` exists only inside winit, and no test runs this forwarding
/// (left out, `main`'s own [`shut_down`] after `app.run()` and, on macOS, the thread-local destructor would hide it).
#[derive(Debug, Default)]
pub struct Handler;

impl CustomApplicationHandler for Handler {
    fn exiting(&mut self, event_loop: &ActiveEventLoop) -> EventResult {
        loop_exiting(event_loop.exiting());
        EventResult::Propagate
    }
}

/// Makes Slint's winit backend the platform, drawing as [`renderer::select`] picks, with [`Handler`] installed. Call
/// it before the first window is made.
pub fn select_backend() -> Result<renderer::Selected, slint::PlatformError> {
    renderer::select(&renderer::Request::from_env(), || Handler)
}

/// Ends the event loop on SIGTERM, SIGINT and SIGHUP, as the window's close button does, so the worker pauses what
/// runs before the process exits. One that comes [`FORCE_AFTER`] or more after the first, while the shutdown the
/// first one started has not ended, ends the process at once by its own default action, as if it had not been caught
/// (security audit SA2 F4); one that comes sooner is the first request again. Call it after [`select_backend`] (the
/// loop must exist to be told) and before the worker starts. Dropping the returned watch stops its thread, but the
/// three signals stay caught: signal-hook does not put their default action back, so a later one does nothing. Drop it
/// only on the way out.
#[cfg(unix)]
pub fn quit_on_signals() -> std::io::Result<SignalWatch> {
    use std::time::Instant;

    use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;

    let mut signals = Signals::new([SIGTERM, SIGINT, SIGHUP])?;
    let handle = signals.handle();
    let thread = std::thread::Builder::new()
        .name("zwt-signals".to_owned())
        .spawn(move || {
            let mut first: Option<Instant> = None;
            for signal in signals.forever() {
                match first {
                    // The process ends by the first signal, after the shutdown it starts.
                    None => {
                        first = Some(Instant::now());
                        let _ = SIGNALLED.compare_exchange(
                            0,
                            signal,
                            Ordering::Relaxed,
                            Ordering::Relaxed,
                        );
                    }
                    // A later one ends it now: the shutdown may hang (a store on a network drive that stopped
                    // answering). For TERM, INT and HUP signal-hook restores the default action and raises the signal
                    // again, and aborts if that fails.
                    Some(at) if at.elapsed() >= FORCE_AFTER => {
                        let _ = signal_hook::low_level::emulate_default_handler(signal);
                    }
                    // Sooner it is the first request again, as the OS can deliver it.
                    Some(_) => {}
                }
                // Quit from inside the loop: Slint's winit backend drops a quit sent before the loop has started
                // (it belongs to an earlier generation), and a signal can come while the store is still opening.
                // After the loop has ended there is nothing to quit, and the error says so.
                let _ = slint::invoke_from_event_loop(|| {
                    let _ = slint::quit_event_loop();
                });
            }
        })?;
    Ok(SignalWatch {
        handle,
        thread: Some(thread),
    })
}

/// The thread [`quit_on_signals`] started; dropping it stops the thread and waits for it.
#[cfg(unix)]
#[derive(Debug)]
pub struct SignalWatch {
    handle: signal_hook::iterator::Handle,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl Drop for SignalWatch {
    fn drop(&mut self) {
        self.handle.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The number of the quit signal that ended the app, or `None` when none did.
#[must_use]
pub fn quit_signal() -> Option<i32> {
    Some(SIGNALLED.load(Ordering::Relaxed)).filter(|n| *n != 0)
}

/// How a run that shut down cleanly ends: after a quit signal, by that signal's default action (the process does
/// not come back; if that fails, it exits with 128 + the signal's number); otherwise with success.
#[must_use]
pub fn finish() -> ExitCode {
    let Some(signal) = quit_signal() else {
        return ExitCode::SUCCESS;
    };
    #[cfg(unix)]
    {
        let _ = signal_hook::low_level::emulate_default_handler(signal);
    }
    exit_code_for(signal).map_or(ExitCode::FAILURE, ExitCode::from)
}

/// 128 + `signal` for a signal number from 1 to 127, as a shell reports a process that signal ended.
#[must_use]
pub fn exit_code_for(signal: i32) -> Option<u8> {
    u8::try_from(signal)
        .ok()
        .filter(|n| (1..128).contains(n))
        .map(|n| 128 + n)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test code: a failed lookup is a failed test"
)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use z_work_core::ManualClock;
    use z_work_core::model::{PauseReason, TaskState};
    use zwt_bridge::sample::{STARTED_TASK, open_or_seed};
    use zwt_bridge::worker::{Command, Commands, WorkerConfig};

    use super::*;

    /// A worker on a fresh sample folder, whose seed starts a task, held as `main` holds it; with its commands.
    fn held_worker(dir: &std::path::Path, clock: &ManualClock) -> Commands {
        let config = WorkerConfig {
            dir: dir.to_path_buf(),
            clock: Arc::new(clock.clone()),
            system_zone: Some("Europe/Lisbon".to_owned()),
            heartbeat: Duration::from_secs(30),
        };
        let (worker, _) = Worker::spawn(config, Box::new(|_| {})).unwrap();
        let commands = worker.commands();
        hold(worker);
        commands
    }

    /// Review RV2 F1: what [`Handler::exiting`] does, run without a loop (an `ActiveEventLoop` exists only inside
    /// winit). When it returns, the worker is gone from the thread-local, its result kept for `main`, and the running
    /// task paused as on Quit. On macOS a hook that did nothing went unnoticed, because AppKit's `exit()` drops the
    /// thread-local and the worker shuts down then; here the thread lives on, so only this call can have done it.
    #[test]
    fn the_loop_end_takes_the_worker_and_keeps_its_result_for_main() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at("2026-11-04T10:30:00Z");
        let commands = held_worker(dir.path(), &clock);
        assert!(WORKER.with_borrow(Option::is_some), "held");
        clock.advance(90);
        loop_exiting(true);
        assert!(
            WORKER.with_borrow(Option::is_none),
            "the loop's end took the worker"
        );
        assert!(
            OUTCOME.with_borrow(|o| matches!(o, Some(Ok(())))),
            "the result is kept for main"
        );
        assert!(
            commands.send(Command::MinuteTick).is_err(),
            "the worker has stopped"
        );
        outcome().unwrap();
        assert!(OUTCOME.with_borrow(Option::is_none), "main took it");
        let reopened = open_or_seed(dir.path(), Arc::new(clock), "Europe/Lisbon").unwrap();
        let task = reopened.service.file().data.tasks.get(STARTED_TASK);
        assert_eq!(
            task.map(|t| (t.state, t.pause_reason.clone())),
            Some((TaskState::Paused, Some(PauseReason::AppQuit)))
        );
    }

    /// Nobody asked the loop to stop (macOS `terminate:`): `main` will not run again, so the loop's end takes the
    /// result itself (and would say an error on stderr).
    #[test]
    fn a_loop_end_nobody_asked_for_takes_the_result_too() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at("2026-11-04T10:30:00Z");
        let commands = held_worker(dir.path(), &clock);
        loop_exiting(false);
        assert!(
            WORKER.with_borrow(Option::is_none),
            "the loop's end took the worker"
        );
        assert!(OUTCOME.with_borrow(Option::is_none), "and the result");
        assert!(
            commands.send(Command::MinuteTick).is_err(),
            "the worker has stopped"
        );
    }

    #[test]
    fn a_signal_gives_128_plus_its_number() {
        assert_eq!(exit_code_for(15), Some(143));
        assert_eq!(exit_code_for(2), Some(130));
        assert_eq!(exit_code_for(1), Some(129));
        assert_eq!(exit_code_for(0), None, "no signal");
        assert_eq!(exit_code_for(-1), None);
        assert_eq!(exit_code_for(128), None, "would not fit in an exit code");
    }
}
