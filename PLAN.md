# zWorkTracker 2.0: native Slint UI, plan

Status: **plan only, no app code yet** (2026-11-04). This file is written so a fresh session can start from it
with no other context. Read it top to bottom before writing code, then start at [Milestone 1](#milestone-1).

## 1. Context

- **zWorkTracker** is a local-first time tracker for freelancers by Pedro Miguel Rodrigues Jorge (the owner).
- **Current app (1.x):** `/Users/p/Z/ZCode/z-work-tracker`. Tauri 2 + React/TypeScript UI + Rust core, at 1.3.3,
  MIT, remote https://github.com/rustcircle/z-work-tracker. It goes to the Microsoft Store as **1.4.0 on Tauri**.
  The owner finds it heavy and sluggish (WebView2).
- **This repo (2.0):** `/Users/p/Z/ZCode/zWorkTracker`. A native Rust desktop app with a **Slint** UI over the
  **same Rust core**, reading and writing the **same data** as the Tauri app.
- The owner accepted Slint's **Royalty-free License 2.0** on 2026-11-04 (see [§9](#9-license-obligations)).
- Dev host: Apple M3 Mac, rustc/cargo 1.99.1, rustup targets `aarch64-apple-darwin` and `x86_64-pc-windows-gnu`,
  MinGW at `/opt/homebrew/bin/x86_64-w64-mingw32-gcc`. No `cargo-xwin`, no Wine, no Windows VM.

### Ground rules (from the owner, hold for every session)

1. **Never modify `/Users/p/Z/ZCode/z-work-tracker`.** Read it, depend on it, never edit, commit or build inside it
   (builds of it go to a `CARGO_TARGET_DIR` outside it).
2. Never add a git remote or push (the owner adds the remote). Never publish, never delete user data, never enter
   passwords. Use `trash`, not `rm`, for anything that is not a build output.
3. **Never touch the owner's real app data.** Development, tests and measurements use sample stores in a temp or
   gitignored folder.
4. Downloads beyond normal cargo crates (Wine, VMs, Homebrew packages, SDKs) need the owner's approval first.
5. Windows builds are **x86_64** only (`x86_64-pc-windows-gnu`, the same target the Tauri build uses on this Mac).
6. Commit at milestones only, on `main`. Every commit message ends with
   `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
7. Show the owner the result of milestone 1 (and this plan, revised by what M1 learned) before building past it.

## 2. What the existing app is (read these first)

**The newest 1.x code is in the worktree `z-work-tracker/.claude/worktrees/task-deletion-timeline-ad2298`
(W)**: 1.3.3 plus what is landing in 1.4.0 (sticky Share of time level, live updates, subtask tags). Read W in
preference to the main checkout until 1.4.0 is merged; after that read `main`. The full feature list with sources
is **[docs/PARITY.md](docs/PARITY.md)** (402 lines).

| Source | What for |
|---|---|
| W/`.stack/out/delegator/live-updates-spec.md` | 1.4.0 live-updates design (per-data subscriptions, 1 s clock, no polling, hidden-window catch-up, tray elapsed); 2.0's reactive model must match its final merged form |
| W/`.stack/out/planner/20261104-0638-140-subtask-tags/plan.md` | 1.4.0 subtask tags incl. "Owner answers" |
| W/`STORE_LISTING_NOTICE.md` | Store feature list, notices, privacy (no network calls, no telemetry) |
| W/`src/lib/themes.ts`, `src/styles/base.css`, `src/features/**/*.css` | Theme tokens, fonts, spacing (visual parity, [§7.2](#72-visual-parity)) |
| W/`assets/logo/`, `src-tauri/icons/` | Logo master, icon generator, tray icons |
| `z-work-tracker/README.md` §User guide (lines ~151-526) | Behaviour spec: timer states, tray, time away, timeline, analysis, billing, sync |
| `z-work-tracker/site/howto.html` | The user-facing how-to, same features in plain words |
| `z-work-tracker/src-tauri/crates/z-work-core/src/` | The core we reuse: `service.rs` (transactions, `Service`), `view.rs` (UI snapshot), `machine.rs`, `analytics.rs`, `billing.rs`, `sync.rs`, `lease.rs`, `changes.rs`, `tray.rs`, `report.rs`/`pdf.rs`/`xlsx.rs`/`csv.rs` |
| `z-work-tracker/src-tauri/src/lib.rs` | How the Tauri shell opens the store, runs heartbeat/sync threads, the tray, `data_dir()` (~line 1854) |
| `z-work-tracker/src/lib/`, `src/features/` | Logic that today lives **only in TypeScript** (see risk R1) |
| `z-work-tracker/fixtures/*.json` | Parity fixtures shared by Rust and TS (`analytics-parity`, `billing-fixtures`, `patch-task-parity`, `tz-fixtures`) |

Key facts found so far:

- `z-work-core` is Tauri-free with an injected `Clock`. `Service::open(dir, clock, device_name)` opens the SQLite
  store (`z-work-data.db`), `Service::mutate` applies a change, saves, then adopts it and bumps
  `Service::revision()`. Sync merges go through `mutate_system` (crate-private) and also bump the revision.
- The Tauri shell resolves the data folder as `app.path().app_data_dir()` for identifier
  `com.rustcircle.zworktracker` (macOS `~/Library/Application Support/com.rustcircle.zworktracker`, Windows
  `%APPDATA%\com.rustcircle.zworktracker`; the MSIX build gets a package-private redirect of that). An env var
  `ZWT_SMOKE_DATA_DIR` overrides it for tests. **2.0 must resolve the same folder** and honour an override.
- `testkit` in the core is `#[cfg(test)]` only, so sample data cannot come from it. Use `store::seed_store(dir,
  &LocalFile, now)` with a sample `LocalFile` built in this repo, or the JSON fixtures.
- The README says "Billing and time zone rules exist in both Rust (for exports and the tray) and TS (for the live
  UI)". The Slint app has no TS, so **every live-UI rule must come from Rust**.

## 3. Repository layout (target)

```
zWorkTracker/
  Cargo.toml              workspace (resolver 3, edition 2024, rust-version 1.99, same lints as the core)
  rust-toolchain.toml     stable + rustfmt, clippy, target x86_64-pc-windows-gnu
  crates/
    zwt-app/              the binary: main.rs, window, tray, menus, platform glue
      ui/                 .slint files: app.slint, timeline.slint, share.slint, about.slint, ...
      build.rs            slint-build
    zwt-bridge/           UI-independent adapter over z-work-core: the event hub, per-topic view models,
                          the 1 s clock, the background worker (sync, heartbeat). Unit-testable without Slint.
    zwt-ui-logic/         (if needed) ports of TS-only logic, tested against z-work-tracker/fixtures/*.json
  assets/                 the 1.x icon sources (logo.svg, build-icons.py, tray-*.svg), copied verbatim in M1
  site/                   the public website (z.rustcircle.com), moved here from the 1.x repo (done, [§12](#11-website))
  .github/workflows/pages.yml   deploys site/ to GitHub Pages (done, copied verbatim from 1.x)
  docs/
    PARITY.md             the exhaustive feature checklist (done)
    site-slint-badge.html attribution badge snippet (done; the badge is live in site/index.html)
    DEPENDENCIES.md       the core dependency strategy, written in M1
    MEASUREMENTS.md       Slint vs Tauri numbers, written in M1 and updated per milestone
  LICENSE, THIRD_PARTY_NOTICES.md, README.md, PLAN.md
```

## 4. Dependency strategy for z-work-core

Requirement: builds **locally** against the sibling checkout and **from a fresh clone** on another machine or CI,
without editing the other repo and without copying its code.

**Chosen: a git dependency pinned to a revision, plus a gitignored local override.**

```toml
# Cargo.toml (committed)
[workspace.dependencies]
z-work-core = { git = "https://github.com/rustcircle/z-work-tracker", rev = "<full sha>" }
z-work-session = { git = "https://github.com/rustcircle/z-work-tracker", rev = "<full sha>" }  # only if used
```

```toml
# .cargo/config.toml (gitignored, local only)
[patch."https://github.com/rustcircle/z-work-tracker"]
z-work-core = { path = "../z-work-tracker/src-tauri/crates/z-work-core" }
z-work-session = { path = "../z-work-tracker/src-tauri/crates/z-work-session" }
```

Why: Cargo finds a package by name anywhere in a git repo, and the core crates inherit `edition`, `rust-version`
and `lints` from the other repo's workspace root (`src-tauri/Cargo.toml`), which Cargo reads from the git checkout.
The pin makes a fresh clone reproducible. The patch makes local work use the sibling checkout, so core changes made
in the other repo (by its own sessions) show up here without bumping the pin. Ship and CI builds use the pin only.
Commit `Cargo.lock` (it's an application).

Facts and checks for M1:

- **The other repo's local `main` (0154df5, release 1.3.3) is 2 commits ahead of `origin/main` (19d15ff).**
  Until the owner pushes it, a fresh clone pinned to 0154df5 cannot build. Pin the newest revision that is **on
  GitHub** if it has the API needed, otherwise pin 0154df5 and write in README "push z-work-tracker main first".
  Bump the pin deliberately (one commit, "Bump z-work-core to <sha>") when the core changes.
- Verify whether Cargo needs to fetch the git source even when every package from it is patched (it may, to build
  the lock file). If it does and the rev isn't pushed, use `net.offline`/`--offline` with a lock produced from the
  patch, or fall back to option B. Record the result in `docs/DEPENDENCIES.md`.
- Check that the core builds as a dependency: the core's `[lints] workspace = true` and `unsafe_code = "forbid"`
  come along; its `#[cfg(test)]` licence test reads `../../../LICENSE` and never runs from here.

Alternatives if the above fails: (B) git submodule of z-work-tracker at `vendor/z-work-tracker` with path deps
(works offline and fresh, costs submodule friction, is not a copy); (C) publish `z-work-core` to crates.io or a
private registry (cleanest long-term, needs the owner's decision); (D) a plain `path` dep to `../z-work-tracker`
(local only, fresh clones must check both repos out side by side; the worst one).

Core changes that 2.0 needs (e.g. a public change hook, a public `mutate_system`, a non-test sample-data builder,
a Rust port of TS-only rules) are **requested from the other repo as notes for the owner**, not edited there. Until
they land, wrap or work around them in `zwt-bridge`.

## 5. Reactive architecture

Goals from the owner: each component subscribes only to the data it shows; the core emits change events after
every write and every sync merge; the running clock is its own 1 s signal; **no polling**; redraw only when output
changes.

```
            ┌──────────── worker thread (owns Service) ─────────────┐
 UI action ─▶ command channel ─▶ Service::mutate / sync merge / heartbeat
            │        after each commit: revision changed?           │
            │        ─▶ compute Diff { topics: bitset }             │
            └───────────────┬───────────────────────────────────────┘
                            │ slint::invoke_from_event_loop (only when something changed)
                            ▼
   EventHub on the UI thread: per-topic view models (Projects, Tasks, Running, DayPieces(date),
   Share(level, period), Billing, Settings, Lease, Sync status, Tray)
     each recomputes ONLY if its topic bit is set, compares with its previous value,
     and sets the Slint property / VecModel row ONLY if the value differs
                            │
   Clock: one slint::Timer, 1 s, running ONLY while a task runs, aligned to the next whole second;
   it sets ONE property (`now-sec`); the elapsed labels bind to it; stopped when nothing runs
```

- **Single owner of `Service`:** a worker thread holds it (timing never depends on the window, as in the Tauri
  shell). The UI sends commands (`Start(id)`, `Pause`, ...) over a channel and never blocks on I/O.
- **Change events:** after each `mutate` and each sync merge, the worker compares `revision()` and diffs the
  `LocalFile` sections it cares about (projects, tasks, segments of affected days, tags, prefs, lease) into a topic
  bitset. If the core gains a public change event later, use it instead of the diff.
- **Topics, not snapshots:** the Tauri UI receives a full snapshot per change. Here each screen's model subscribes
  to the topics it shows; a timer tick never rebuilds the task list, a project rename never touches the day axis.
- **Models:** Slint `VecModel`/`ModelRc` with row-level `set_row_data` only for changed rows (keyed by id).
- **Clock:** `now-sec` is the only thing that changes every second. Elapsed text is derived in Slint
  (`elapsed-base + now-sec - started-at`) so Rust isn't called per tick. The tray's text updates on the minute
  boundary only (README: tray shows h:mm and changes only when the minute or the data changes).
- **Background events** with deadlines (heartbeat every ~30 s, sync cycle, idle/lock checks, reminders, billing step
  due) are scheduled on the worker with `recv_timeout` to the next deadline, which is event-driven waiting, not
  polling the store. Time-away detection needs OS signals (sleep/wake, lock, idle) as in the Tauri shell.
- **Tests:** `zwt-bridge` unit tests assert that a given change sets exactly the expected topic bits and that an
  unchanged recompute does not touch the model (count `set_row_data`/property writes).

## 6. Milestones

Every milestone ends with: gates green ([§8](#8-gates)), its docs/PARITY.md rows implemented and tested, the visual
comparison ([§7.2](#72-visual-parity)) done for the screens it built, `docs/MEASUREMENTS.md` updated if it touched
a hot path, README updated, one milestone commit. Each milestone lists its **acceptance criteria (AC)**.

### Milestone 1

**Repo + core wired + Timeline screen with running timer, day axis and Share of time chart, on sample data;
cross-compiled for x86_64-pc-windows-gnu; measured against the Tauri build.** Then stop and show the owner.

Already done in the setup commit: `git init -b main`, `.gitignore`, MIT `LICENSE`, `THIRD_PARTY_NOTICES.md`,
`docs/site-slint-badge.html`, `docs/PARITY.md`, `site/` with the badge, `.github/workflows/pages.yml`, this plan.

1. Workspace, `rust-toolchain.toml`, crates `zwt-app` and `zwt-bridge`, the core dependency per [§4](#4-dependency-strategy-for-z-work-core),
   `docs/DEPENDENCIES.md`.
2. Slint backend/renderer that cross-compiles with MinGW: start with the `winit` backend and the **femtovg** or
   **software** renderer; avoid Skia unless its prebuilt download works (that download would need approval).
3. Sample store: `zwt-app --sample <dir>` (or `ZWT_DATA_DIR`) seeds a store with `seed_store` from a sample
   `LocalFile` (3 projects, ~10 tasks with subtasks and tags, 2 weeks of segments, one task running) and opens it.
   Without the flag the app refuses to start in M1 (so it can never open real data by accident).
4. Navigation shell with a top-level menu: Timeline, About. Copy the icon sources verbatim ([§7.2](#72-visual-parity)),
   generate the Slint theme global from W/src/lib/themes.ts (all four themes), use the 1.x fonts and wording.
5. **Timeline screen:** task list with Start/Pause/Resume/Stop per the state table (Draft: Start; Running: Pause,
   Stop; Paused: Resume, Stop; Stopped: none). One task runs at a time; starting another pauses the running one.
   1 s ticking elapsed time. **Space never toggles a timer** (pressed or held), Enter on a focused button works;
   mouse-first. A **day axis** of today's work pieces (and a day picker), coloured by project. **Share of time**
   chart with levels Projects / Tasks / Subtasks / Tags; the picked level stays until the user switches it (persist
   it in prefs if the core has a field, else in memory and note it).
6. **About screen** with `AboutSlint` (std-widgets), app version, MIT notice, link to THIRD_PARTY_NOTICES.
7. Reactive wiring per [§5](#5-reactive-architecture) for the topics this screen uses.
8. Builds: host release, `cargo build --release --target x86_64-pc-windows-gnu`; note the .exe path and size.
   Windows subsystem `windows` (no console window) in release.
9. Measurements per [§10](#10-measurement-plan-against-tauri), written to `docs/MEASUREMENTS.md`.
10. Gates, commit, then show the owner the result and a revised version of this plan.

AC: the gates pass on both targets; UI tests prove (a) the elapsed label changes once per second while running and
not when paused, (b) Space on Start/Pause/Resume/Stop does nothing, (c) switching the Share level to Tags then
changing data keeps Tags, (d) a timer tick causes no write to the task list or day-axis models; the Windows .exe
exists and is PE x86-64 (`file` says so); `MEASUREMENTS.md` has a table with numbers or a stated reason per cell.

### Milestone 2: tasks, projects, corrections

Projects (price per hour, currency, fixed fee, budget alerts, archive), tasks and subtasks (forms, drafts, cold
resume = new linked task form, continued tasks, delete rules), tags and templates, colours, corrections and
manual entries, billable pauses, subtask time. AC: each matches the README section of the same name;
`patch-task-parity.json` passes through the Rust path.

### Milestone 3: tray, lifecycle, time away

Tray icon (idle/running icons from the Tauri repo's assets, referenced or re-exported, not hand-copied code),
menu Show / current task h:mm / Pause-Resume / Quit, left-click shows; **close hides to tray, timing continues,
only Quit pauses** (closes the open segment, marks paused); first-hide notice; single instance (second launch
focuses the window); heartbeat every ~30 s and crash recovery notice; time away (sleep, clock jump > 2 min, lock,
idle limit default 10 min) with Keep / Drop / Thinking; reminders (long run, after shift) as notifications;
Start with Windows (installer: Run key; Store: StartupTask). Crates to evaluate: `tray-icon` + `muda`,
`single-instance` or a named mutex/socket, `user-idle`, `notify-rust`/Windows toasts. AC: README §Tray behaviour
and §Time away hold, checked by tests where possible and the manual checklist otherwise.

### Milestone 4: Timeline complete, time zones, shifts

Full timeline (bands, hour shifts, DST, Show both zones, the map if kept), work shifts, increments and rounding
display. Port the TS tz engine and band splitting to Rust (or use the core's) and test against
`tz-fixtures.json`. AC: fixtures pass; DST days render 23/25 hours correctly.

### Milestone 5: Analysis and billing

Analysis (periods and comparison, metrics per README §Metrics reference, Share of time at all four levels),
billing summaries, earnings and Hide earnings, quote helper, timesheet PDF, Excel and CSV exports **via the core**
(`report.rs`, `pdf.rs`, `xlsx.rs`, `csv.rs`) with native save dialogs (`rfd`). AC: `analytics-parity.json` and
`billing-fixtures.json` pass; exported files are byte-identical or semantically identical to the Tauri app's for
the same sample store and selection.

### Milestone 6: settings, themes, sync, data

Settings, themes (light/dark/system plus the existing palettes), backups and restore, folder sync on OneDrive and
Google Drive (detection, two folders, versions and restoring), one computer at a time (lease banner, Release now,
Take over, Pause it from here), first-run setup. AC: a store written by the Tauri app opens in 2.0 and vice versa
(sample data, both directions); sync between a Tauri device and a Slint device converges (core cloud round-trip
scenario reproduced with two sample dirs).

### Milestone 7: packaging and release

Windows installer and MSIX that ships as an **update of the same Store app**: Identity Name
`PedroMiguelRodriguesJorge.zWorkTracker`, Publisher `CN=1DEF8D7B-1D68-4604-8047-048DC92CF3A9`, display name
"Pedro Miguel Rodrigues Jorge", family `PedroMiguelRodriguesJorge.zWorkTracker_54k2cbaqgvhqp`, Store ID
`9P9RM9G0QBRC`, Application Id `zWorkTracker`, StartupTask `zWorkTrackerStartup`, `runFullTrust`, MinVersion
10.0.19041.0 (all verified in docs/PARITY.md §Store identity, from W/scripts/msix-manifest.mjs), the same test
identity, the 60 MSIX images incl. `_altform-unplated`/targetsize variants cut from the same `icon.png` by the
same rules (port or call `package-msix.mjs`/`make-pri.mjs` logic; the UTM Windows VM flow `pnpm msix:vm` is the
model), toast AUMID; the Store data-redirect rules from the 1.x README apply unchanged. Version 2.0.0 must be
higher than the last 1.x Store version (1.4.x). Icons, signing hand-off (the owner signs; no keys in this repo), full
third-party licence list (`cargo about` or `cargo deny`), README user guide, the 2.0 site changes
([§12](#11-website)), manual Windows test checklist adapted from the 1.x README. AC: a release build passes every
gate; the owner runs it on real Windows.

### Milestone 8: parity gate (release blocker)

**No 2.0 release until all of these pass:**

1. Re-run the inventory against the merged 1.4.0 (and any later 1.x) and update docs/PARITY.md; every
   `1.4.0, follow as merged` row is rewritten to the merged behaviour.
2. **Every row of docs/PARITY.md is checked against the Tauri app** (side by side, same sample store) and ticked,
   or carries an owner-approved exception. The 7 `1.x-only` rows each have their 2.0 equivalent (e.g. the
   WebView2-missing message becomes a friendly start-up check for what 2.0 needs: a GPU/renderer fallback to the
   software renderer, a damaged or locked store, a missing data folder, with the same "Your data is safe." tone).
3. **Visual parity** ([§7.2](#72-visual-parity)) passes for every screen and theme.
4. **Responsiveness targets** ([§10.2](#102-targets-20-must-beat)) all pass on the Windows VM, with the Tauri 1.4.0
   baseline recorded first.
5. Data round trip: a store and sync folder written by 1.4.0 open in 2.0 and vice versa, and a 1.4.0 device and a
   2.0 device sync and converge (sample data only).

## 7. Parity

### 7.1 Feature parity

The checklist is **[docs/PARITY.md](docs/PARITY.md)**: 402 lines, one per feature or rule, each with its source
(README/howto line, spec, plan, or commit) and the milestone that delivers it. Status counts at 2026-11-04:
361 `shipped 1.3.3`, 34 `1.4.0, follow as merged` (sticky Share level, live updates, subtask tags with Select
all/Deselect all, No tag when empty, unknown-field catch-all, copies reset to Same as task), 7
`1.x-only, needs a 2.0 equivalent` (WebView2 checks, CSP, snapshot IPC, show-after-first-paint). Also there:
the verified Store identity, the icon inventory, and what the app deliberately never does (no network calls, no
telemetry, no Mica, OS theme not followed, ...). Those "never" rules are parity rules too.

Each milestone ticks nothing; it implements its rows and tests them. Ticking happens only at the parity gate.

Contradictions the inventory found in the 1.x docs (resolve against the code, not the docs):
- Refresh model: the uncommitted 1.4.0 README/howto still describe a 30 s refresh; the live-updates spec replaces
  it with no polling. Follow what 1.4.0 merges.
- Tray icon wording in the README (stopwatch) is stale; commit 47a7aff made it a round clock at 10:10, green dial
  with a play triangle while running. Use the icon files, not the README text.
- README names root `app-icon.svg`/`app-icon-small.svg` as icon sources; they are stale. `assets/logo/logo.svg`
  is the only master (assets/logo/README.md).
- STORE_LISTING_NOTICE.md is written for 1.3.2 and marks shipped features "[check in the build]".

### 7.2 Visual parity

2.0 looks like 1.x, only faster: **reuse the exact assets, never redraw them.**

- **Icons:** copy verbatim `assets/logo/logo.svg`, `build-icons.py`, `tray-idle.svg`, `tray-running.svg` and the
  generated `src-tauri/icons/*` (32, 128, 256, 512 PNG, `icon.ico`, `icon.icns`, `tray-idle.png`,
  `tray-running.png`); regenerate with the same script, never by hand. The MSIX set (60 images incl. unplated and
  targetsize variants) is cut by the same rules (M7).
- **Themes:** Light, Solarized Light, Solarized Dark, Dark (`THEMES` and `THEME_LABELS` in W/src/lib/themes.ts).
  Generate a Slint `global Theme` from that file's tokens by a script (so they can't drift), including the chart
  tokens (`axis`, `grid`, `shiftBand`, heat ramps, `--c-*` colours). Opaque window, no Mica.
- **Fonts:** UI `Segoe UI Variable Text` / `Segoe UI Variable Display` for headings and the ticker clock (26 px,
  weight 650), `Cascadia Mono` for code/changes text, with the same fallbacks as W/src/styles/base.css. On macOS
  dev builds the fallback is the system font; parity is judged on Windows.
- **Layout, spacing, radii, wording:** every screen, label, button text, message and notice copied from the React
  components and CSS (W/src/features/**, W/src/components/**), not paraphrased. Keep a strings table
  (`ui/strings.slint` or a Rust table) with a test that diffs it against the 1.x source strings.
- **Comparison step per screen:** for each screen and state (Setup, Timeline incl. selection, shift bands, Show
  both, live earnings; Tasks incl. filter, inline edit, subtasks, corrections; Analysis tabs; Billing; Export
  scope; Settings incl. backups, sync folders, versions, read-only banner; Time-away dialog; colour picker; About)
  and each theme: take the Tauri screenshot (the 1.x e2e already produces them: 32 in W/docs/images/, 218 in
  W/test-results/screenshots/, from the Playwright specs in W/e2e/) and a Slint screenshot of the same sample
  state and window size (Slint's testing/software renderer can render to an image headlessly), save both under
  `docs/visual/<screen>-<theme>-{tauri,slint}.png`, and review them side by side (visual-reviewer or the owner).
  Pass = same layout, same wording, same colours (token-exact), same icons; differences only where Slint
  cannot render something, each listed and approved by the owner.

## 8. Gates

Run at every milestone (and in CI later):

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --target x86_64-pc-windows-gnu -- -D warnings
cargo test --workspace
cargo build --release --target x86_64-pc-windows-gnu
```

- UI tests: Slint's testing backend (`i-slint-backend-testing`, `slint::testing` APIs: element handles by
  accessible label, simulated clicks and key events, mock time) drives components headlessly in `cargo test`.
  Pin the testing crate to the exact Slint version (it has no stability guarantee).
- `zwt-bridge` tests are plain Rust: topic bits, no-op recomputes, command handling with a `ManualClock`.
- Parity tests read `../z-work-tracker/fixtures/*.json` when present (skip with a message when absent; a fresh
  clone can fetch them from the pinned git rev if needed).
- Lints: inherit the core's set (`unsafe_code = "forbid"` except a platform crate that needs it, clippy
  `unwrap_used`, `expect_used`, `panic`, `indexing_slicing` as warn, so `-D warnings` makes them errors).

## 9. License obligations

- App code: **MIT**, `LICENSE` = MIT text with "Copyright (c) 2026 Pedro Miguel Rodrigues Jorge" (done). Cargo
  `license = "MIT"` in every crate.
- Slint: **Slint Royalty-free Desktop, Mobile, and Web Applications License 2.0**
  (https://slint.dev/terms-and-conditions#royalty-free; text:
  https://github.com/slint-ui/slint/blob/master/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md), accepted by the
  owner 2026-11-04. The owner chose to meet the attribution requirement **both** ways:
  1. **AboutSlint** widget on an About screen reachable from the top-level menu (M1).
  2. **"Made with Slint" badge** on the public download page: it is in the footer of this repo's
     `site/index.html` (done) and **always visible**, not gated on 2.0 (the owner: "don't hide the badge").
     The reference snippet is `docs/site-slint-badge.html`. It loads Slint's official SVGs from
     raw.githubusercontent.com; vendoring them into `site/assets/` (so the page makes no third-party request) is a
     small download that needs the owner's OK.
- `THIRD_PARTY_NOTICES.md` (done) lists Slint and the core; M7 adds the generated full crate list.
- Re-check the Slint licence text at each Slint upgrade (the licence version is part of the obligation).

## 10. Measurement plan against Tauri

### 10.1 Method

Two stages: quick like-for-like numbers on this Mac (macOS release builds of both, same sample store) from M1
on, to steer the work; then the **binding numbers on the owner's Windows 11 VM** (UTM, the one the 1.x
`pnpm msix:vm` flow already uses), Tauri 1.4.0 build vs Slint build, both x64, same VM, same settings, same
sample store holding **one year of data** (generated by a script in this repo, e.g. 12 projects, ~200 tasks,
~2,500 segments; also the 1.x `store_bench` 50k stress size). **Record the Tauri 1.4.0 baselines first**, in
`docs/MEASUREMENTS.md`, then measure 2.0 against the targets below. If the VM is Arm, x64 builds run under
emulation for both apps: the comparison stays fair, but absolute numbers are pessimistic; say so next to them.
Each number is the median of 10 runs with the spread.

| Metric | How |
|---|---|
| Binary / install size | release binary size (and stripped size) for host and for the Windows .exe |
| Cold start to first frame | wall time from `exec` to the first presented frame (log a timestamp in the first rendering callback; for Tauri use the existing "cold start to first render" method from its README §Performance) |
| Idle RSS after 10 s | `ps -o rss` / `footprint`, both apps, all their processes (Tauri includes the WebView processes) |
| RSS with a task running, after 5 min | same |
| CPU while idle with a task running | `top -l` or `ps` average over 60 s; target ≈ one wake per second |
| Wakeups per second | `powermetrics` or Instruments (needs sudo: owner runs it, or skip) |
| Input-to-frame latency | timestamp the click handler and the next rendered frame; Start/Pause and switching Share level |
| Day axis / Share redraw cost on 5k and 50k segment stores | time from data change to frame, using the core's `store_bench`-sized synthetic store |

Rules: the Tauri app is built into a `CARGO_TARGET_DIR` outside its repo or an existing build is measured as is;
if building it would write inside the other repo (frontend `dist/`), don't, and use an existing build or skip and
say so. Point the Tauri app at a temp data dir via `ZWT_SMOKE_DATA_DIR` only if that build honours it; never at
real data. Windows runs happen on the owner's VM; driving it from the Mac follows the 1.x msix-vm pattern (the
owner types one line in the VM). No Wine download without approval.

### 10.2 Targets 2.0 must beat

Proposed by the owner via the 1.x session; **all are owner-to-confirm after the Tauri baselines are in**, and the
ones marked ⚑ especially:

| Metric (Windows VM, one year of sample data) | Target | Note |
|---|---|---|
| Cold start to first interactive window | ≤ 0.5 s | from process start to a window that accepts a click |
| Idle memory (private working set, all processes) | ≤ 60 MB | vs typical WebView2 100–200 MB; ⚑ femtovg/GPU drivers can add tens of MB; the software renderer is the fallback |
| Input-to-redraw, clicks and typing | ≤ 16 ms | one frame at 60 Hz, p95 |
| Tab / screen switch | ≤ 50 ms | p95, to the fully drawn screen |
| Analysis full redraw, one year of data | ≤ 100 ms | ⚑ depends on core analytics cost; measure the core alone first |
| CPU idle, window visible, no task running | ≈ 0 % (no wakeups) | |
| CPU with a task running, window visible | one wake per second, < 0.5 % | |
| CPU hidden in the tray, task running | ≈ 0 %, one wake per minute for the tray h:mm | hidden window draws nothing |

A target that the Tauri baseline already beats is raised to "at least as good as Tauri". Results go into
`docs/MEASUREMENTS.md` with the commit, VM spec and method.

## 11. Website

The public site (z.rustcircle.com) now **lives in this repo**: `site/` was copied on 2026-11-04 from W/site
(index.html, howto.html, privacy.html, styles.css, liquid.js, assets/) with paths unchanged, plus
`.github/workflows/pages.yml` copied verbatim from 1.x (deploys `site/` through GitHub Actions on pushes to main
that touch `site/**`; pinned action SHAs; no secrets). **No CNAME file**: with Actions deployment the custom
domain is set in the repo's Pages settings, and the owner moves z.rustcircle.com to this repo himself (DNS
unchanged). From now on **site changes for 1.4.0 are made here**, not in the old repo. The Slint badge is in the
footer and always visible.

Lines that must change for 2.0 (wording left as is for now):

| File:line (2026-11-04) | Now | Change for 2.0 |
|---|---|---|
| site/index.html:46, :179, :188; howto.html:34, :41, :259, :266; privacy.html:33, :51 | links to github.com/rustcircle/z-work-tracker | this repo, https://github.com/rustcircle/zWorkTracker2.0 (remote added by the owner 2026-11-04) (README links: point to this repo's README sections) |
| site/index.html:171 | "**WebView2.** The app needs the Microsoft Edge WebView2 Runtime…" | remove; replace with 2.0's real requirements if any (e.g. graphics fallback) |
| site/howto.html:74 | "It needs the Microsoft Edge WebView2 Runtime…" | remove |
| site/index.html:49 | "New in 1.3.2: saving is about 3 to 5 times faster…" | "New in 2.0: native app, …" with the measured numbers from §10 |
| site/index.html:48 | "Windows 10 version 2004 or later, and Windows 11 (x64)" | keep if MinVersion stays 10.0.19041.0 |
| screenshots in site/assets/*.webp | 1.x UI | retake from 2.0 after visual parity passes |
| privacy.html | check it says nothing about WebView/Edge data | keep the no-network, no-telemetry wording |

2.0 launch steps for the site: (1) before launch, compare `site/` with the old repo's `site/` and bring over
anything changed there meanwhile, then apply the 2.0 wording; (2) the owner sets the custom domain on this repo's
Pages and removes it from the old repo (DNS unchanged); (3) the old repo drops its `site/`, `CNAME` and
`pages.yml`, and its README points to this repo (done by the owner or a session in that repo, never from here).

## 12. Risks and open questions

- **R1: logic that lives only in TypeScript** (tz engine, band splitting, live billing, shifts, colour, themes in
  `z-work-tracker/src/lib/`). Each must be ported to Rust in this repo or upstreamed to the core by the owner,
  and proved with the shared fixtures. Inventory it at the start of M2.
- **R2: unpushed core revision** (see [§4](#4-dependency-strategy-for-z-work-core)). Owner pushes z-work-tracker main.
- **R3: Slint + MinGW cross-compile**: renderer choice, font handling (the core bundles a PDF font; UI fonts
  must cover the same scripts as 1.x), DPI awareness manifest on Windows.
- **R4: the app has never run on real Windows** (neither 1.x nor 2.0). Windows behaviour (tray, toasts, lock
  detection, MSIX) needs the owner's Windows test pass.
- **R5: both apps on one store.** They must never run at the same time on one PC (single store, one writer); the
  lease handles cross-device. Decide whether 2.0 refuses to open while 1.x is running (lock file check).
- **R6: crate-private core hooks** (`mutate_system`, `testkit`) that 2.0 may need: request a public API from the
  owner rather than editing the core.
- Open: does 2.0 keep the time-zone map? Does Share of time's picked level persist across restarts in 1.x (check
  the prefs model) and should 2.0 match it?
