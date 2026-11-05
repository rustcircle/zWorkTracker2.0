# Measurements: zWorkTracker 2.0 (Slint) against 1.x (Tauri)

PLAN.md section 10. This file holds the **Mac stage** (Milestone 1, node M1, 2026-11-04): like-for-like numbers on one
Mac to steer the work. They are **not binding**. The binding numbers come from the owner's Windows 11 VM; those cells
say "pending owner".

## Summary

- **Renderer:** the points below and every table up to the renderer-switch section were measured with the
  **femtovg-OpenGL** build. 2.0 now ships **femtovg-wgpu** (Metal here; see "Renderer switch to femtovg-wgpu"
  below). Known so far for wgpu, from quick checks with a task running (n = 3): `phys_footprint` 1,212 MB against
  661 MB with OpenGL, RSS 227 against 137 MB, and binaries of 20.3 MB on macOS and 24.1 MB for the Windows .exe
  (final tree). The full re-measure of the final tree is still to do: it needs an unlocked session with the display
  on.
- **Memory:** Slint uses about half the RSS of Tauri: 128 MB against 280-287 MB, whether idle or with a task running,
  and still after 5 minutes. Measured as `phys_footprint` (what Activity Monitor shows), the two are close: 183 against
  194 MB idle, and 663 against 717 MB with a task running. Most of that is GPU memory: about 500 MB of graphics in
  Slint's own process, and 490 MB in WebKit's GPU process.
- **Cold start:** the first frame comes at about the same time, 526 ms for Slint and 510 ms for Tauri's first React
  render. Tauri's window, though, only appears at 4.45 s on macOS. It starts hidden and waits for a
  `requestAnimationFrame` that WebKit does not run in a hidden window, so the backend's 4 s fallback shows it. Slint's
  window is visible at its first frame. Slint spends 512 ms between the start of `main` and the first frame (not yet
  profiled).
- **CPU:** with nothing running, Slint uses about 0 % (no ticks at all) against Tauri's 0.07 %. With a task running,
  Slint uses 0.28 % against Tauri's 0.06 %, so Slint costs about 5 times more per second there: its 1 s clock redraws
  the whole window. Both are under the 0.5 % target. Whether the wgpu build is too is not known until the re-measure:
  the renderer node's CPU looks (docs/DEPENDENCIES.md: OpenGL 2.7-3.0 %, wgpu 2.6-6.9 %) were taken on the evening of
  2026-11-04, when even the unchanged M1 binary measured about 5 times higher than here (1.54 % against 0.28 %; Tauri
  2.07 % against 0.058 %), so absolute CPU numbers from that evening cannot be compared with this file's tables.
- **Input to frame:** on the 14-day sample, Pause takes 13.7 ms in Slint and 15 ms in Tauri (medians). At 5k
  segments it is 19.7 ms against 32.5 ms, and at 50k segments 98 ms against 209 ms. The Share level switch is 5.5 ms
  against 7 ms on the sample, but 12 ms against 7 ms at 50k: Slint's switch goes through the worker and recomputes
  over every segment, while Tauri's is a client-side React state change.
- **Size:** the Slint binary is larger: 16.7 MB against 13.0 MB on macOS, and 18.5 MB against 12.1 MB for the Windows
  .exe. Tauri leaves the web engine to the operating system.

## Machine, method and source state

| | |
|---|---|
| Machine | Apple M3 Ultra (Mac15,14), 32 cores, 512 GB, macOS 27.2 (26B123), AC power, Low Power Mode off, 4K display |
| Toolchain | rustc 1.99.1, cargo 1.99.1; Windows builds `x86_64-pc-windows-gnu` (MinGW) for both apps |
| Load | Other agents (a validator running cargo, a reviewer) ran on the same machine; load average 1.9-4.1 is recorded with every run in the raw data |
| Slint source | the worktree at HEAD `fd0bbc5` plus uncommitted changes (`crates/` is untracked). `git status --short \| shasum -a 256` = `b1c9d137…`; the sha256 of the sha256s of every file under `crates/` = `7e20f82a…`. Both were recorded at 10:53:53Z, before the build, and again at 10:59:13Z, after it, and were the same both times. Built with `CARGO_TARGET_DIR=<M1>/target cargo build --release -p zwt-app` (profile: lto, codegen-units 1, strip, opt-level 3), and again with `--target x86_64-pc-windows-gnu`. The copies in `<M1>/bin/` were the ones measured. The orchestrator records the final sizes again after the fix node. |
| Tauri source | W = `z-work-tracker` worktree `task-deletion-timeline-ad2298`, HEAD `fd51d79` (clean apart from the untracked `.claude/`). Built from a **copy** in `<M1>/tauri-src/` (`src-tauri/` without `target/`, LICENSE, THIRD_PARTY_NOTICES.md, `scripts/msix-manifest.mjs`, fixtures). W's `dist/` dates from 00:20 UTC, before the last two frontend commits, so the frontend was rebuilt in the copy (`tsc --noEmit && vite build`, from an APFS clone of W's `node_modules`). Then `CARGO_TARGET_DIR=.stack/out/tauri-target cargo build --release --features smoke,tauri/custom-protocol` (the one measured at runtime) and `--features tauri/custom-protocol` (for the plain size). W's release profile: lto, codegen-units 1, opt-level "s", strip. Nothing was built or written inside W, the main checkout or `reference/`. |
| Sample store | made by `zwt-storegen gen` (scratch helper, `<M1>/storegen/`): the zwt-bridge sample (`sample::build`: 3 projects, 10 tasks, 14 days, 44 closed segments, Europe/Lisbon) with the 1.x first-launch prompts marked done, written with `z_work_core::store::seed_store`. It opens **cold**, with nothing running and the draft `t-social` startable. `s5k` and `s50k` add 5,000 and 50,000 closed synthetic segments, 10 a day, reaching 1.4 and 13.7 years back (the 1.x `store_bench` sizes). Every launch gets a fresh copy of the `.db`. |
| Task running | Slint: a fresh `--sample` folder, which is seeded and then starts `t-social`. Tauri: the copied sample store, then `invoke('start_task', {id: 't-social'})` from the smoke build's `ZWT_SMOKE_SCRIPT`, which is evaluated 1.5 s after setup. The same task runs in both. |
| Order | Every metric alternates the apps run by run (Slint, Tauri, Slint, …) and one launch at a time (`scripts/measure/run_all.sh`). Wall time: 11:14-13:31 plus a 5-minute footprint pass. |
| Windows | Both apps' windows open at their default size and were frontmost and unobscured in spot checks (`scripts/measure/windows.py`, CoreGraphics window list: z = 0, nothing overlapping in front; `raw/window-spotchecks.jsonl`) |

Statistics: the median, then the interquartile range (IQR), then min-max, then n = number of runs. "Cold" here means
a new process with the binary in the page cache. A true cold cache needs `sudo purge`, which was not run. The first
launch of each app is reported on its own, and launches 2-11 give the median.

## Results

The Windows VM cells are "pending owner": they need the owner's Windows 11 VM run of PLAN 10.1.

| Metric | Slint (macOS) | Tauri (macOS) | Slint / Tauri | Windows VM Slint | Windows VM Tauri |
|---|---|---|---|---|---|
| **1. Binary, host** (already stripped by both release profiles; `nm` lists only imports) | 16,655,936 B, the M1 measurement build (femtovg-OpenGL) (gzip -9: 8,209,460); n/a spread (deterministic) | 13,007,664 B plain, 13,073,888 B smoke (gzip -9: 5,824,027); plus the OS's WebKit | 1.28 | pending owner (install size) | pending owner |
| **1. Windows .exe** (`file`: PE32+ executable (GUI) x86-64 (stripped to external PDB)) | `zwt-app.exe` 18,537,984 B (gzip -9 8,860,040), MinGW | `reference/…/windows-exe/zWorkTracker.exe` 12,076,032 B (gzip -9 5,180,521), MinGW, existing build (its commit is not recorded) | 1.54 | same file | same file; plus WebView2 runtime |
| **2. Cold start, exec to first frame** | **526 ms** (IQR 513-538, 466-551, n=10); first launch 523 ms. Exec stamp is taken just before `sandbox-exec`; the end is the `AfterRendering` epoch (before present). `main` to first frame: 512 ms (IQR 500-525). | **510 ms** to the `z-first-render` mark, the README §Performance method of `scripts/perf-native.mjs`: React's first effect, with the window still hidden (IQR 501-514, 463-517, n=10); first launch 504 ms. Page start: 402 ms. | 1.03 | pending owner | pending owner |
| **2b. Exec to window visible** | = first frame: 526 ms (n=10) | **4,451 ms** (IQR 4429-4477, 4425-4507, n=10). The smoke script polled `smoke_window_visible` every 5 ms. The page's `window_ready` waits for a `requestAnimationFrame` that WebKit does not run in a hidden window, so the 4 s fallback shows the window. | 0.12 | pending owner | pending owner (WebView2 may run rAF in a hidden window) |
| **3. Idle RSS after 10 s**, nothing running, sum of all processes (`ps -o rss`) | **128 MB** (IQR 127-133, 127-134, n=10); one process | **280 MB** (IQR 279-283, 276-287, n=10): main 120, WebContent 116, GPU 33, Networking 11 | 0.46 | pending owner (private working set) | pending owner |
| 3b. Idle `phys_footprint` after 10 s (`footprint -j`, all processes) | 183 MB (183-185, n=3; 4 more suite runs 183-183) | 194 MB (192-194, n=3; 3 more suite runs 192-194): main 45, WebContent 116, GPU 26, Networking 6 | 0.94 | - | - |
| **4. RSS with a task running, after 5 min** | **128 MB** (127-128, n=3); 128 MB at 10 s (n=10): no growth | **286 MB** (IQR 283-286, 280-286, n=3); 287 MB at 10 s (n=10) | 0.45 | pending owner | pending owner |
| 4b. `phys_footprint`, task running, 10 s | 663 MB (IQR 662-672, 662-681, n=3); about 500 MB is "owned physical footprint (graphics)" (femtovg/OpenGL). A spot check went from 661 to 680 MB in 5 s. 5-minute footprint: not measured (see doubts). | 717 MB (IQR 715-719, 713-722, n=3): WebKit GPU 490, WebContent 176, main 45, Networking 6 | 0.92 | - | - |
| **5. CPU, task running, window visible**, % of one core over 60 s (ps cputime delta, all processes; `top -l 2 -s 60` agrees) | **0.28 %** (IQR 0.26-0.31, 0.26-0.33, n=10) | **0.058 %** (IQR 0.050-0.066, 0.033-0.083, n=10) | 4.9 | pending owner | pending owner |
| 5b. Idle wakeups/s, task running (top `IDLEW`, summed) | 0.05 (IQR 0.05-0.07, 0-0.17, n=10) | 0.27 (IQR 0.20-0.40, 0.17-0.47, n=10) | 0.19 | pending owner (powermetrics needs sudo) | pending owner |
| 5c. CPU idle, nothing running, window visible | **0.000 %** (IQR 0-0.017, n=10); wakeups/s 0.017 | **0.066 %** (IQR 0.050-0.215, 0.050-0.447, n=9; one run dropped because its 60 s window started 20 min late after a hung `footprint`); wakeups/s 0.18 | ~0 | pending owner | pending owner |
| **6. Input to frame, Pause**, sample store, 20 rounds a run | median 13.7 ms (IQR 13.65-13.73 over runs, n=10 runs × 20); p95 22.4 ms (16.0-26.1) | median 15 ms (14-15, n=9 runs × 20; one run timed out at start); p95 17 ms (16-20) | 0.91 | pending owner | pending owner |
| 6. Resume | 12.3 ms (12.2-13.2, n=10); p95 21.5 ms (18.3-23.1) | 15 ms (14-15, n=9); p95 17 ms (16-19) | 0.82 | pending owner | pending owner |
| 6. Share level switch (Tags and Projects alternately) | 5.5 ms (5.39-5.81, n=10 × 40); p95 9.5 ms (8.3-12.4) | 7 ms (7-7, n=9 × 40); p95 8 ms (7-9) | 0.79 | pending owner | pending owner |
| **7. Day axis / Share redraw on data change, 5k segments** (Pause and Resume redraw the day axis, chips, Share and the task list) | Pause 19.7 ms (19.6-19.8, n=10 × 20), p95 22.7; Resume 18.3 ms, p95 24.4; Share switch 5.6 ms, p95 7.7; Start (draft) 95.5 ms (n=10 × 1) | Pause 32.5 ms (32-33.5, n=10), p95 36; Resume 33 ms, p95 37.5; Share switch 7 ms, p95 7 | 0.61 (pause) | pending owner | pending owner |
| **7. Same, 50k segments** | Pause 98.4 ms (98.3-98.6, n=10 × 20), p95 103 (max over runs 326); Resume 91.1 ms, p95 96.9; Share switch 12.0 ms, p95 19.6; Start 98.8 ms | Pause 209 ms (205.5-212, n=10), p95 265.5; Resume 210 ms, p95 242.5; Share switch 7 ms, p95 8 | 0.47 (pause); 1.71 (share) | pending owner | pending owner |
| 7b. Worker view cost alone, 50k (`zwt-storegen bench`, 50 reps, median ms) | `views::share` 6.2-6.7 per level, `day_view` (today) 6.2, `chips` 6.3, `task_rows` 7.4; at 5k all 0.6-1.1; store open 87 ms (5k: 18.5) | n/a (1.x computes these in TypeScript) | - | - | - |

How each was taken (all launches under `sandbox-exec -f scripts/measure/sandbox.sb`):

- **Processes:** Tauri's WebKit helpers are started by launchd (ppid 1), and their responsible pid is the terminal,
  not the app. They are found in the app's own launchd XPC domain: `launchctl print pid/<app pid>` lists
  `com.apple.WebKit.WebContent`, `.GPU` and `.Networking` with their pids. Every Tauri run found exactly these three
  plus the main process (`procs` in `raw/idle-tauri.jsonl`). Slint is one process.
- **Latency, Slint:** `ZWT_MEASURE_LATENCY=20` (see U1's `measure.md`). This is **handler-to-frame, not
  OS-input-to-frame**: from the click callback's entry to the end of the `AfterRendering` callback of the next frame
  after the worker's answer is applied. Hit-testing, buffer swap and vsync are excluded.
- **Latency, Tauri (a comparable method):** a smoke script calls the real DOM button's `.click()`, which runs React's
  handler synchronously. The time is from just before the click to the first `requestAnimationFrame` in which the
  card's `data-state` (or the radio's `aria-checked`) shows the new state, plus a `setTimeout(0)`.
  `performance.now()` has a 1 ms resolution in WebKit. No rAF misses were counted (a 250 ms fallback would have
  counted them). This is also handler-to-frame, but the end is the start of the frame that paints the change, about
  one frame earlier than Slint's end.
- **Latency, the difference in the Share switch:** in Tauri it is a local React state change in the Analysis tab. In
  Slint it is a worker command that recomputes `views::share` over the store, which is why 50k costs Slint 12 ms and
  Tauri 7 ms.
- **CPU:** the ps `TIME` delta over a measured 60.4 s window that starts 10 s after launch, summed over the app's
  processes. `top` ran over the same window for `IDLEW`.

## Redirect proof for 1.x (Tauri) and data safety

- **Data folder:** the smoke build honours `ZWT_SMOKE_DATA_DIR` (lib.rs `data_dir`, `#[cfg(feature = "smoke")]`). The
  harness also sets `HOME` and `CFFIXED_USER_HOME` to a temporary folder for each run.
  `scripts/measure/measure.py proof` (`raw/proof-tauri.jsonl`) launched it with a start action. The script answered
  `running: ["t-social"], segments: 45`. The temporary `data/z-work-data.db` was modified after launch, and
  `data/backups/z-work-data-2026-11-04.json` and `-wal` were created there. All of WebKit's folders landed in the
  temporary home: `home/Library/WebKit/zWorkTracker-tauri-smoke/…` and `home/Library/Caches/zWorkTracker-tauri-smoke/…`.
  All 14 Tauri "running" run folders show the same layout (WebKit folders only under their temporary home).
- **Real folders:** `sandbox.sb` denies `file-read*` and `file-write*` on
  `~/Library/Application Support/com.rustcircle.zworktracker`, on `~/Library/{WebKit,Caches,HTTPStorages}` for both
  `com.rustcircle.zworktracker` and `zWorkTracker`, on Saved Application State, on `~/Library/LaunchAgents`, and on
  `com.rustcircle.zworktracker` and `zWorkTracker` under the per-user cache and temp folders (paths resolved through
  `/private`). It also denies IP networking. A self-test against a fake home returned "Operation not permitted" for
  read, list and write. The real data folder was never listed, read or stat'ed by this node. The modification times
  of `~/Library/WebKit/com.rustcircle.zworktracker`, `…/zWorkTracker` and the Caches equivalents were the same before
  and after all runs (`raw/real-dirs-mtime-*.txt`; a directory-level stat only).
- **What the sandbox does not cover:** WebKit's XPC helpers are not confined by `sandbox-exec`. They write where the
  app's UI process tells them to, which here was the temporary home, as shown above.
- **Single instance:** 1.x's single-instance socket is `/tmp/com_rustcircle_zworktracker_si.sock`, so a real 1.x
  instance would have received the launch. The harness refuses to start if any `zWorkTracker` process it did not
  start is running, and none was. The socket left behind by the SIGTERM'd test runs was moved to the Trash afterwards.
  The real app cleans up a stale socket on its own anyway.
- Every launch was killed by pid: only the pids the harness started, and the WebKit helpers in their launchd domain.

## PLAN 10.2 targets: first Mac reading (not binding)

| Target (Windows VM, one year of data) | Mac reading, Slint | Mac reading, Tauri | Reading |
|---|---|---|---|
| Cold start to first interactive window ≤ 0.5 s | 0.53 s to the first frame (14-day sample) | 0.51 s first render, 4.45 s window visible | Slint is just over; 512 ms of it is inside `main`, to profile |
| Idle memory ≤ 60 MB (private working set) | RSS 128 MB, footprint 183 MB | RSS 280 MB, footprint 194 MB | Missed on Mac. macOS RSS/footprint is not a Windows private working set; femtovg's graphics memory dominates footprint (⚑ in the plan) |
| Input-to-redraw ≤ 16 ms p95 | Pause p95 22 ms, Share p95 9.5 ms (sample); 5k Pause p95 23 ms | 17 ms / 8 ms | Clicks that write miss on Mac; the Share switch passes |
| Tab / screen switch ≤ 50 ms p95 | not measured: milestone 1 has no tabs | not measured | - |
| Analysis full redraw ≤ 100 ms, one year | Share at 5k (1.4 years): 5.6 ms switch, 19.7 ms after a write; core view cost 0.6-1.1 ms | 7 ms switch | Passes at 5k; the 50k write path is 98 ms |
| CPU idle, no task ≈ 0 % | 0.000 % (0.017 wakeups/s) | 0.066 % | Slint meets it |
| CPU, task running, window visible: < 0.5 %, one wake per second | 0.28 % | 0.058 % | Both meet < 0.5 %, but Tauri is about 5 times lower. By the "at least as good as Tauri" rule, Slint misses on Mac |
| CPU hidden in the tray, task running ≈ 0 % | not measured: milestone 1 has no tray | not measured | - |

## Caveats

- **macOS is not Windows.** WebKit is not WebView2. The Tauri numbers include WebKit's helper processes and its
  hidden-window rAF behaviour (the 4 s window), and WebView2's behaviour differs. The femtovg renderer runs OpenGL
  over Metal here and will drive a different driver on Windows. Absolute numbers on an M3 Ultra are optimistic for a
  typical Windows laptop or VM.
- **What "first frame" means differs.** Slint's is the end of rendering its first frame, visible. Tauri's README
  marker is React's first effect in a still-hidden window. Row 2b gives the user-visible time.
- **The screens differ.** Slint milestone 1 shows the day view, Share and the task list. 1.x opens on Timeline and
  has Share in the Analysis tab. The comparison is like-for-like on store and actions, not on the screen drawn.
- **Footprint:** `footprint` hung or took minutes on these GPU-using processes during the suite. It is bounded
  (`--footprint-timeout`) and was taken in a separate pass with a 90 s timeout (n=3 per app and state).
- **Latency outlier:** in a diagnostic run outside the suite, one Slint Pause waited **30.8 s** for its frame (10
  rounds: `p95-ms=30805.88`). Another 20-round run hung past 300 s. Cause: the display was asleep. Slint 1.18 on
  macOS 14+ draws from the window's `CADisplayLink`, which does not tick while the display is off; the worker's
  answer was applied within about 30 ms and the redraw was requested, but the frame came only when the display woke
  (8.1 s in a run whose display was woken after 7.5 s), or never. With the display held on the runs were normal.
  A user sees no stale frame: there is nothing to see until the display wakes, and the frame follows the wake. So it
  is a measurement artefact. The hook now stops a run with a `zwt-measure latency stalled` line when no frame begins
  within 1 s of an applied answer (recorded as an error, no statistics), and `measure.py` and `run_all.sh` keep the
  display awake. That deadline firing while the display is off was not observed end to end (both display-off runs
  were woken by user input), so a run whose display never wakes may still wait for the harness's stop at 120 s.
  The per-run p95 maxima of 326 and 331 ms at 50k are a different thing: re-runs at 50k with the display on, while
  other builds loaded the machine (load 8-12), gave samples of 0.2-4.3 s. There the time was spent before the answer
  was applied (worker and UI apply, both slowed), and the frame followed within 316 ms. Those are load on the
  measuring machine, and they stay in the numbers.
- **Wakeups:** top's `IDLEW` counts wakeups from idle. `powermetrics` would give package wakeups, but it needs sudo
  and was not run (pending owner).
- `SLINT_BACKEND=winit-software` with `ZWT_MEASURE_EXIT=1` printed no first-frame line within 120 s, so the
  software-renderer comparison was dropped (`raw/backend-diag.txt`).

## Renderer switch to femtovg-wgpu (2026-11-05): provisional Mac numbers

The owner switched the on-screen renderer from femtovg on OpenGL to femtovg on wgpu (Metal here; Direct3D 12, then
Vulkan on Windows) during milestone 1. The tables above were measured with the OpenGL build and stay as they are
(that is the femtovg-OpenGL column). The full re-measure of the final tree is still to do: it needs an unlocked
session with the display on, because a covered or locked wgpu window draws no frame on macOS. Until then these quick
checks from the renderer node stand (release builds started as an .app bundle, task running, n = 3 each, fresh sample
folders; raw lines in `.stack/out/orchestrator/20261104-2050-m1-resume/nodes/REN/e2e/`, gitignored):

| Measure | femtovg (OpenGL) | femtovg-wgpu (Metal) | software renderer |
|---|---|---|---|
| main to first frame, median | 683 ms (674-702) | 717 ms (698-734) | not measured |
| RSS at 10 s | 137 MB (137-138) | 227 MB (226-229) | 142 MB (n = 1) |
| phys_footprint at 10 s | 661 MB (660-663) | 1,212 MB (1,211-1,213); 907 MB of it is graphics | 59 MB (n = 1) |
| CPU, task running | 0.28 % in the table above; a quick look on 2026-11-04 evening: 2.7 and 3.0 % | a quick look on 2026-11-04 evening: 2.6-6.9 % (n = 5) | not measured |
| macOS binary (release) | 16,838,208 B: the build with the fix round's lifecycle, guard and measurement changes, just before the switch (row 1 above, 16,655,936 B, is the M1 measurement build) | 20,331,392 B (final tree) | - |
| Windows .exe (release, windows-gnu) | 18,537,984 B: the M1 measurement build (row 1 above) | 24,135,680 B (final tree) | - |

The CPU quick looks (docs/DEPENDENCIES.md) are not comparable with the 0.28 %: on that evening the unchanged M1
binary measured 1.54 % (n = 3) and Tauri 2.07 %, against 0.28 % and 0.058 % in the table above (likely causes: the
window partly covered, two users logged in, a 165 Hz display). Only comparisons made within that evening hold. Whether
the wgpu build meets the < 0.5 % target is known only after the re-measure.

Windows numbers for the wgpu build: pending owner. The CPU node also found that each 1 s tick drew two full frames
before the rail became a Flickable; on the final tree one tick draws one frame (`crates/zwt-app/tests/ui_frames.rs`).

## Reproduce

```bash
RUN=.stack/out/orchestrator/20261104-0756-m1-slint/nodes/M1        # inputs and raw data (gitignored)
CARGO_TARGET_DIR=$RUN/target cargo build --release -p zwt-app && cp $RUN/target/release/zwt-app $RUN/bin/zwt-app-macos
# 1.x: copy W/src-tauri (no target/), LICENSE, THIRD_PARTY_NOTICES.md, scripts/msix-manifest.mjs, the frontend sources
# and node_modules (cp -c) to $RUN/tauri-src; there: tsc --noEmit && vite build; then in src-tauri:
#   CARGO_TARGET_DIR=.stack/out/tauri-target cargo build --release --features smoke,tauri/custom-protocol
#   cp .stack/out/tauri-target/release/z-work-tracker $RUN/bin/zWorkTracker-tauri-smoke
# stores: (cd $RUN/storegen && cargo build --release); zwt-storegen gen $RUN/stores/{sample,s5k,s50k} {0,5000,50000} Europe/Lisbon
python3 scripts/measure/measure.py proof                  # the redirect proof, first
scripts/measure/run_all.sh "$PWD/$RUN"                    # cold idle running latency views (about 2 h here)
python3 scripts/measure/summarize.py "$PWD/$RUN"          # Markdown tables from raw/*.jsonl
```

Scripts: `scripts/measure/sandbox.sb` (profile), `measure.py` (harness), `run_all.sh` (driver), `summarize.py`,
`windows.py` (on-screen check). Raw data: `.stack/out/orchestrator/20261104-0756-m1-slint/nodes/M1/raw/`
(`cold-*.jsonl`, `idle-*.jsonl`, `running-*.jsonl`, `latency-*-{sample,s5k,s50k}.jsonl`, `views.txt`,
`footprint-pass/`, `sizes.txt`, `source-state-*.txt`, `environment.txt`, `summary.md`).
