//! How the app ends, for review findings R1 and N3 of milestone 1 (2026-11-04).
//!
//! - R1: a quit that does not come back through `app.run()` (macOS `terminate:`; SIGTERM, SIGINT, SIGHUP) skipped the
//!   worker's shutdown, so a running task stayed `running` with an open segment until the next launch's crash
//!   recovery closed it. The signal test runs the built binary on a fresh sample folder (whose seed starts
//!   `t-social`), waits for its first frame, sends the signal, and reads the store.
//! - N3: errors after the start (a store that cannot be opened, a worker panic, a Slint platform error) went to
//!   stderr only, which a Windows release build does not have, and repeated a cause whose text the error above
//!   already held.
//! - Security audit SA2 F4 (2026-11-05): a signal after the first one was swallowed, so a shutdown that hung could
//!   not be ended by another Ctrl+C or `kill`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

#[cfg(unix)]
mod app {
    use std::cell::Cell;
    use std::io::{BufRead, BufReader};
    use std::path::Path;
    use std::process::{Command, ExitStatus, Stdio};
    use std::sync::mpsc::{self, Receiver};
    use std::thread;
    use std::time::{Duration, Instant};

    /// The built app, started on a sample folder with its first-frame line on (`ZWT_MEASURE=1`).
    pub struct App {
        pid: u32,
        lines: Receiver<String>,
        exited: Receiver<std::io::Result<ExitStatus>>,
        ended: Cell<bool>,
    }

    /// The app binary with a clean environment for the variables it reads. `ZWT_TEST_APP` names another build to
    /// run instead (how the red runs used the unfixed binary).
    pub fn command(dir: &Path) -> Command {
        let app = std::env::var_os("ZWT_TEST_APP")
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_zwt-app").into());
        let mut c = Command::new(app);
        c.arg("--sample")
            .arg(dir)
            .env_remove("ZWT_DATA_DIR")
            .env_remove("ZWT_MEASURE")
            .env_remove("ZWT_MEASURE_EXIT")
            .env_remove("ZWT_MEASURE_LATENCY")
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        c
    }

    /// False on Linux without X11 or Wayland: the app cannot open its window there.
    pub fn display_available() -> bool {
        !cfg!(target_os = "linux")
            || std::env::var_os("DISPLAY").is_some()
            || std::env::var_os("WAYLAND_DISPLAY").is_some()
    }

    impl App {
        pub fn start(dir: &Path) -> Self {
            let mut child = command(dir)
                .env("ZWT_MEASURE", "1")
                .stderr(Stdio::piped())
                .spawn()
                .expect("the app starts");
            let stderr = child.stderr.take().unwrap();
            let (line_tx, lines) = mpsc::channel();
            thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if line_tx.send(line).is_err() {
                        break;
                    }
                }
            });
            let pid = child.id();
            let (exit_tx, exited) = mpsc::channel();
            thread::spawn(move || {
                let _ = exit_tx.send(child.wait());
            });
            Self {
                pid,
                lines,
                exited,
                ended: Cell::new(false),
            }
        }

        /// Waits for the first rendered frame: by then the event loop runs and the seeded task runs in the worker.
        pub fn wait_for_first_frame(&self) {
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                match self.lines.recv_timeout(left) {
                    Ok(line) if line.starts_with("zwt-measure first-frame") => return,
                    Ok(line) => eprintln!("app: {line}"),
                    Err(e) => panic!("no first frame within 60 s ({e:?}): is a display available?"),
                }
            }
        }

        /// Sends `signal` (a name `kill` knows: TERM, INT, HUP, KILL).
        pub fn signal(&self, signal: &str) {
            let sent = Command::new("kill")
                .arg(format!("-{signal}"))
                .arg(self.pid.to_string())
                .status()
                .unwrap();
            assert!(sent.success(), "kill -{signal} {}", self.pid);
        }

        pub fn wait_for_exit(&self) -> ExitStatus {
            let status = self
                .exited
                .recv_timeout(Duration::from_secs(30))
                .expect("the app ends within 30 s")
                .unwrap();
            self.ended.set(true);
            for line in self.lines.try_iter() {
                eprintln!("app: {line}");
            }
            status
        }
    }

    impl Drop for App {
        /// A failed test must not leave a window behind.
        fn drop(&mut self) {
            if !self.ended.get() {
                let _ = Command::new("kill")
                    .arg("-KILL")
                    .arg(self.pid.to_string())
                    .status();
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn r1_a_quit_signal_pauses_the_running_task_as_quit_does() {
    use std::os::unix::process::ExitStatusExt;
    use std::sync::Arc;

    use z_work_core::model::{PauseReason, TaskState};
    use zwt_bridge::SystemClock;
    use zwt_bridge::sample::{STARTED_TASK, open_or_seed};

    if !app::display_available() {
        eprintln!("skipped: no display to open the app's window on");
        return;
    }
    for (signal, number) in [("TERM", 15), ("INT", 2), ("HUP", 1)] {
        let dir = tempfile::tempdir().unwrap();
        let running = app::App::start(dir.path());
        running.wait_for_first_frame();
        running.signal(signal);
        let status = running.wait_for_exit();
        let reopened = open_or_seed(dir.path(), Arc::new(SystemClock), "UTC").unwrap();
        assert!(!reopened.seeded, "{signal}: the store was there");
        let task = &reopened.service.file().data.tasks[STARTED_TASK];
        assert_eq!(
            task.pause_reason,
            Some(PauseReason::AppQuit),
            "SIG{signal}: paused on the way out (1.x Quit), not by the next launch's crash recovery"
        );
        assert_eq!(task.state, TaskState::Paused, "SIG{signal}");
        // After the shutdown the app ends by the signal's default action, so its parent sees that signal.
        assert_eq!(status.signal(), Some(number), "SIG{signal}: {status:?}");
    }
}

/// A store that cannot be read (`CoreError::Read`, whose message holds the I/O error that is also its source): the
/// run error reaches the user as one line, each cause said once, and the exit code is 1.
#[cfg(unix)]
#[test]
fn n3_a_run_error_is_printed_with_each_cause_once() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("z-work-data.json");
    std::fs::write(&legacy, b"{}").unwrap();
    std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&legacy).is_ok() {
        eprintln!("skipped: permissions are not enforced for this user (root?)");
        return;
    }
    let out = app::command(dir.path()).output().unwrap();
    std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o644)).unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    let line = stderr
        .lines()
        .find(|l| l.starts_with("zwt-app: "))
        .unwrap_or_else(|| panic!("no error line in {stderr:?}"));
    let denied = std::io::Error::from_raw_os_error(13).to_string();
    // The guard hands the worker the folder's canonical path.
    let shown = dir.path().canonicalize().unwrap();
    assert_eq!(
        line,
        format!(
            "zwt-app: could not open the data in {}: could not read data: {denied}",
            shown.display()
        )
    );
}

/// What [`zwt_app::exit::Handler`] does when the event loop ends, without a loop (an `ActiveEventLoop` exists only
/// inside winit): the held worker shuts down once, pausing the seeded running task with the quit reason, and a
/// second end (`main` after `app.run()`) finds nothing left to do.
#[test]
fn r1_the_loop_end_shuts_the_worker_down_once_and_pauses_what_runs() {
    use std::sync::Arc;
    use std::time::Duration;

    use slint::winit_030::CustomApplicationHandler;
    use z_work_core::ManualClock;
    use z_work_core::model::{PauseReason, TaskState};
    use zwt_app::exit;
    use zwt_bridge::sample::{STARTED_TASK, open_or_seed};
    use zwt_bridge::worker::{Command, Worker, WorkerConfig};

    let _installed: Box<dyn CustomApplicationHandler> = Box::new(exit::Handler);
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at("2026-11-04T10:30:00Z");
    let config = WorkerConfig {
        dir: dir.path().to_path_buf(),
        clock: Arc::new(clock.clone()),
        system_zone: Some("Europe/Lisbon".to_owned()),
        heartbeat: Duration::from_secs(30),
    };
    let (worker, first) = Worker::spawn(config, Box::new(|_| {})).unwrap();
    assert!(
        first
            .tasks
            .unwrap()
            .iter()
            .any(|t| t.state == TaskState::Running),
        "the seed starts a task"
    );
    let commands = worker.commands();
    exit::hold(worker);
    clock.advance(90);
    exit::shut_down();
    assert!(
        commands.send(Command::MinuteTick).is_err(),
        "the worker has stopped"
    );
    exit::shut_down();
    exit::outcome().unwrap();
    exit::outcome().unwrap();
    let reopened = open_or_seed(dir.path(), Arc::new(clock), "Europe/Lisbon").unwrap();
    let task = &reopened.service.file().data.tasks[STARTED_TASK];
    assert_eq!(task.state, TaskState::Paused);
    assert_eq!(task.pause_reason, Some(PauseReason::AppQuit));
}

/// The errors `run()` can end with, as `main` reports them on Windows: in the refusal window, one line each.
#[test]
fn n3_on_windows_a_run_error_is_shown_in_the_refusal_window() {
    use std::error::Error;
    use std::io;
    use std::path::PathBuf;

    use i_slint_backend_testing::ElementHandle;
    use slint::ComponentHandle;
    use z_work_core::CoreError;
    use zwt_app::launch;
    use zwt_bridge::datadir::Platform;
    use zwt_bridge::worker::{ShutdownError, StartError};

    assert!(
        launch::shows_window(Platform::Windows),
        "a Windows release build has no console"
    );
    assert!(!launch::shows_window(Platform::MacOs));
    assert!(!launch::shows_window(Platform::Linux));
    let dir = PathBuf::from("/samples/m1");
    let open = |source| -> Box<dyn Error> {
        Box::new(StartError::Open {
            dir: dir.clone(),
            source,
        })
    };
    let cases: Vec<(Box<dyn Error>, &str)> = vec![
        (
            open(CoreError::Read(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "access denied",
            ))),
            "zwt-app: could not open the data in /samples/m1: could not read data: access denied",
        ),
        (
            open(CoreError::FutureVersion {
                found: 3,
                supported: 2,
            }),
            "zwt-app: could not open the data in /samples/m1: data file was written by a newer app version (3, this app supports 2); it was left untouched",
        ),
        (
            Box::new(StartError::Stopped),
            "zwt-app: the worker thread stopped while opening the data",
        ),
        (
            Box::new(ShutdownError::Panicked),
            "zwt-app: the worker thread panicked",
        ),
        (
            Box::new(ShutdownError::Exit(CoreError::Io(io::Error::other(
                "disk full",
            )))),
            "zwt-app: could not pause the running task on the way out: could not save data: disk full",
        ),
        (
            Box::new(slint::PlatformError::Other("no display".to_owned())),
            "zwt-app: no display",
        ),
    ];
    i_slint_backend_testing::init_no_event_loop();
    for (e, want) in cases {
        let failure = launch::failure(&*e);
        assert_eq!(failure.message, want);
        assert_eq!(failure.code, launch::FAILED);
        let window = launch::message_window(&failure.message).unwrap();
        assert_eq!(window.get_message(), failure.message.as_str());
        window.show().unwrap();
        assert_eq!(
            ElementHandle::find_by_accessible_label(&window, &failure.message).count(),
            1,
            "{want}"
        );
    }
}

/// The child process of the SA2 F4 tests: this test binary, run again on one test with [`signal_child::CHILD`] set. It
/// watches the quit signals as the app does (`exit::quit_on_signals`) but has no event loop to quit, so the shutdown
/// the first signal asks for never ends there, as a hung one would not.
#[cfg(unix)]
mod signal_child {
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, ExitStatus, Stdio};
    use std::sync::mpsc::{self, Receiver};
    use std::thread;
    use std::time::{Duration, Instant};

    use zwt_app::exit;

    pub const CHILD: &str = "ZWT_TEST_SIGNAL_CHILD";

    /// In the child, which `test` was started as: watch, say so, say the first signal, then wait to be ended (exit
    /// code 3 after 20 s). Elsewhere nothing.
    pub fn run_if_child() {
        if std::env::var_os(CHILD).is_none() {
            return;
        }
        let _watch = exit::quit_on_signals().unwrap();
        eprintln!("child: watching");
        let deadline = Instant::now() + Duration::from_secs(20);
        while exit::quit_signal().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        eprintln!("child: first {:?}", exit::quit_signal());
        while Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        std::process::exit(3);
    }

    /// A child that watches the signals; killed when dropped, so a failed test leaves nothing behind.
    pub struct Watching {
        child: Child,
        lines: Receiver<String>,
    }

    impl Watching {
        /// The child of `test` (this binary's test of that name), once it watches the signals.
        pub fn start(test: &str) -> Self {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([test, "--exact", "--nocapture", "--test-threads=1"])
                .env(CHILD, "1")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let stderr = child.stderr.take().unwrap();
            let (tx, lines) = mpsc::channel();
            thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
            let watching = Self { child, lines };
            watching.wait_for("child: watching");
            watching
        }

        /// The child's first line, within 30 s, that starts with `prefix`.
        pub fn wait_for(&self, prefix: &str) -> String {
            loop {
                match self.lines.recv_timeout(Duration::from_secs(30)) {
                    Ok(line) if line.starts_with(prefix) => return line,
                    Ok(_) => {}
                    Err(e) => panic!("no {prefix:?} line from the child ({e:?})"),
                }
            }
        }

        /// Sends `signal` (a name `kill` knows: TERM, INT, HUP).
        pub fn send(&self, signal: &str) {
            let pid = self.child.id();
            let sent = Command::new("kill")
                .arg(format!("-{signal}"))
                .arg(pid.to_string())
                .status()
                .unwrap();
            assert!(sent.success(), "kill -{signal} {pid}");
        }

        /// How the child ended, if it did within `within`.
        pub fn end_within(&mut self, within: Duration) -> Option<ExitStatus> {
            let deadline = Instant::now() + within;
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    return Some(status);
                }
                if Instant::now() > deadline {
                    return None;
                }
                thread::sleep(Duration::from_millis(20));
            }
        }
    }

    impl Drop for Watching {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Security audit SA2 F4 (2026-11-05): once the first TERM, INT or HUP has started the shutdown, a later one ends the
/// process at once by its own default action, so a shutdown that does not end can still be ended from a shell or a
/// supervisor; before, every later signal was swallowed. The second one here comes [`zwt_app::exit::FORCE_AFTER`]
/// after the first, as a person's or a supervisor's does.
#[cfg(unix)]
#[test]
fn sa2_f4_a_second_quit_signal_ends_a_shutdown_that_does_not_end() {
    use std::os::unix::process::ExitStatusExt;
    use std::time::Duration;

    use zwt_app::exit;

    signal_child::run_if_child();
    for ((first, first_number), (second, second_number)) in [
        (("TERM", 15), ("TERM", 15)),
        (("INT", 2), ("INT", 2)),
        (("HUP", 1), ("TERM", 15)),
        (("TERM", 15), ("HUP", 1)),
    ] {
        let mut child = signal_child::Watching::start(
            "sa2_f4_a_second_quit_signal_ends_a_shutdown_that_does_not_end",
        );
        child.send(first);
        assert_eq!(
            child.wait_for("child: first "),
            format!("child: first Some({first_number})"),
            "SIG{first} is the one the app ends by after a clean shutdown"
        );
        std::thread::sleep(exit::FORCE_AFTER + Duration::from_millis(200));
        child.send(second);
        let status = child
            .end_within(Duration::from_secs(10))
            .unwrap_or_else(|| {
                panic!("SIG{second} after SIG{first}: the child still runs 10 s later")
            });
        assert_eq!(
            status.signal(),
            Some(second_number),
            "SIG{second} after SIG{first}: {status:?}"
        );
    }
}

/// The review of the SA2 F4 fix (2026-11-05, R1): the OS can deliver one request twice. Closing the terminal of a zsh
/// job sends SIGHUP twice, about 2 ms apart (zsh forwards it, then the kernel sends it as the session ends), and
/// logind sends SIGTERM and then SIGHUP. A repeat that soon is the same request, so the shutdown the first one started
/// goes on (and the task is paused as on Quit); only a signal [`zwt_app::exit::FORCE_AFTER`] after the first ends the
/// app at once.
#[cfg(unix)]
#[test]
fn sa2_f4_a_signal_the_os_sends_twice_lets_the_shutdown_go_on() {
    use std::os::unix::process::ExitStatusExt;
    use std::time::{Duration, Instant};

    use zwt_app::exit;

    signal_child::run_if_child();
    let mut child =
        signal_child::Watching::start("sa2_f4_a_signal_the_os_sends_twice_lets_the_shutdown_go_on");
    let sent = Instant::now();
    child.send("HUP");
    child.wait_for("child: first Some(1)");
    child.send("HUP");
    assert!(
        sent.elapsed() < exit::FORCE_AFTER,
        "this machine took {:?} to send two signals, too slow to send a repeat",
        sent.elapsed()
    );
    let ended = child.end_within(exit::FORCE_AFTER + Duration::from_millis(500));
    assert!(ended.is_none(), "the repeat ended the shutdown: {ended:?}");
    child.send("TERM");
    let status = child
        .end_within(Duration::from_secs(10))
        .unwrap_or_else(|| panic!("SIGTERM after the window: the child still runs 10 s later"));
    assert_eq!(status.signal(), Some(15), "{status:?}");
}
