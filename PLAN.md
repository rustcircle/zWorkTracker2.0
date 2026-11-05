# zWorkTracker 2.0: native Slint UI, plan

Status: **plan only, no app code yet** (2026-11-04). This file is written so a fresh session can start from it
with no other context. Read it top to bottom before writing code, then start at [Milestone 1](#milestone-1).

## 1. Context

- **zWorkTracker** is a local-first time tracker for freelancers by Pedro Miguel Rodrigues Jorge (the owner).
- **Current app (1.x):** `/Users/p/Z/ZCode/z-work-tracker`. Tauri 2 + React/TypeScript UI + Rust core, at 1.3.3,
  MIT, remote https://github.com/rustcircle/z-work-tracker. It never goes to the Store: **the owner decided on 2026-11-04 that 2.0 (Slint) is the first
  version on the Microsoft Store** (2.0.0.0, the first submission); Tauri 1.4.0 is not finished and is not released.
  The owner finds it heavy and sluggish (WebView2). Since 2026-11-04 the 1.x repo is **reference only and may be
  deleted** (the owner's words: "I might remove or delete z-work-tracker repo"); a gitignored read-only copy at
  `a88c1aa` sits in the main checkout at `reference/z-work-tracker`.
- **This repo (2.0):** `/Users/p/Z/ZCode/zWorkTracker`. A native Rust desktop app with a **Slint** UI over the
  **same Rust core**, reading and writing the **same data** as the Tauri app. The core is **vendored** at
  `crates/z-work-core`, copied from 1.x `a88c1aa` ([§4](#4-dependency-strategy-for-z-work-core)).
- The owner accepted Slint's **Royalty-free License 2.0** on 2026-11-04 (see [§9](#9-license-obligations)).
- Dev host: Apple M3 Mac, rustc/cargo 1.99.1, rustup targets `aarch64-apple-darwin` and `x86_64-pc-windows-gnu`,
  MinGW at `/opt/homebrew/bin/x86_64-w64-mingw32-gcc`. No `cargo-xwin`, no Wine, no Windows VM.

### Ground rules (from the owner, hold for every session)

1. **Never modify `/Users/p/Z/ZCode/z-work-tracker`.** Read it, never edit, commit or build inside it (builds of it
   go to a `CARGO_TARGET_DIR` outside it). Nothing here builds from it any more: it may be gone, so a check that
   needs 1.x sources looks for `reference/z-work-tracker` too.
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

The 1.x repo is reference only and may be deleted; read it, or its copy `reference/z-work-tracker` (main checkout,
gitignored, at `a88c1aa`), for behaviour and design. The core and the shared fixtures are in this repo
([§4](#4-dependency-strategy-for-z-work-core)).

**The newest 1.x code is in the worktree `z-work-tracker/.claude/worktrees/task-deletion-timeline-ad2298`
(W)**: 1.3.3 plus the unfinished 1.4.0 work (sticky Share of time level, live updates, subtask tags). Read W in
preference to the main checkout. The 1.4.0 designs (live updates, subtask tags) are **2.0 features built
natively here**, from the spec and plan below, not "followed" from a Tauri release. The full feature list with sources
is **[docs/PARITY.md](docs/PARITY.md)** (420 lines).

| Source | What for |
|---|---|
| [docs/from-1.4.0/README.md](docs/from-1.4.0/README.md) | Index of the 1.4.0 requirements carried into 2.0 (the owner: carry momentum and requirements from 1.4.0 to 2.0); `live-updates-tauri-plan.md` there is the stopped Tauri plan, reference only |
| [docs/from-1.4.0/live-updates-spec.md](docs/from-1.4.0/live-updates-spec.md) | 1.4.0 live-updates design (per-data subscriptions, 1 s clock, no polling, hidden-window catch-up, tray elapsed); 2.0 builds this design natively ([§5](#5-reactive-architecture)): its Change 2/3 rules are requirements |
| [docs/from-1.4.0/subtask-tags-plan.md](docs/from-1.4.0/subtask-tags-plan.md) | Subtask tags, a **2.0 feature** built natively (milestone 2) from this plan incl. "Owner answers": subset of the task's tags with Select all/Deselect all, empty = No tag, unknown-field catch-all, copies reset to Same as task |
| W/`STORE_LISTING_NOTICE.md` | Store feature list, notices, privacy (no network calls, no telemetry) |
| W/`src/lib/themes.ts`, `src/styles/base.css`, `src/features/**/*.css` | Theme tokens, fonts, spacing (visual parity, [§7.2](#72-visual-parity)) |
| W/`assets/logo/`, `src-tauri/icons/` | Logo master, icon generator, tray icons |
| `z-work-tracker/README.md` §User guide (lines ~151-526) | Behaviour spec: timer states, tray, time away, timeline, analysis, billing, sync |
| `z-work-tracker/site/howto.html` | The user-facing how-to, same features in plain words |
| `crates/z-work-core/src/` (here; copied from 1.x `src-tauri/crates/z-work-core/src/` at `a88c1aa`) | The core we reuse: `service.rs` (transactions, `Service`), `view.rs` (UI snapshot), `machine.rs`, `analytics.rs`, `billing.rs`, `sync.rs`, `lease.rs`, `changes.rs`, `tray.rs`, `report.rs`/`pdf.rs`/`xlsx.rs`/`csv.rs` |
| `z-work-tracker/src-tauri/src/lib.rs` | How the Tauri shell opens the store, runs heartbeat/sync threads, the tray, `data_dir()` (~line 1854) |
| `z-work-tracker/src/lib/`, `src/features/` | Logic that today lives **only in TypeScript** (see risk R1) |
| `fixtures/*.json` (here; copied from 1.x `fixtures/` at `a88c1aa`) | Parity fixtures shared by Rust and TS (`analytics-parity`, `billing-fixtures`, `patch-task-parity`, `tz-fixtures`) |

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
    z-work-core/          the core, vendored: copied from 1.x src-tauri/crates/z-work-core at a88c1aa (§4)
      tests/1x-src/       two 1.x TypeScript files the core's tests compare against (copied with it)
    zwt-app/              the binary: main.rs, window, tray, menus, platform glue
      ui/                 .slint files: app.slint, timeline.slint, share.slint, about.slint, ...
      build.rs            slint-build
    zwt-bridge/           UI-independent adapter over z-work-core: the event hub, per-topic view models,
                          the 1 s clock, the background worker (sync, heartbeat). Unit-testable without Slint.
    zwt-ui-logic/         (if needed) ports of TS-only logic, tested against fixtures/*.json
  fixtures/               the 1.x parity fixtures (*.json), copied from 1.x fixtures/ at a88c1aa
  assets/                 the 1.x icon sources (logo.svg, build-icons.py, tray-*.svg), copied verbatim in M1
  site/                   the public website (z.rustcircle.com), moved here from the 1.x repo (done, [§11](#11-website))
  .github/workflows/pages.yml   deploys site/ to GitHub Pages (done, copied verbatim from 1.x)
  docs/
    PARITY.md             the exhaustive feature checklist (done)
    site-slint-badge.html attribution badge snippet (done; the badge is live in site/index.html)
    DEPENDENCIES.md       the vendored core (provenance, local edits, pulling 1.x changes), Slint, MinGW; M1
    MEASUREMENTS.md       Slint vs Tauri numbers, written in M1 and updated per milestone
  LICENSE, THIRD_PARTY_NOTICES.md, README.md, PLAN.md
```

## 4. Dependency strategy for z-work-core

**Vendored at `crates/z-work-core`, copied from 1.x `a88c1aa`.** The owner decided on 2026-11-04 ("could you copy
over core into this repo and link against/reference that instead because I might remove or delete z-work-tracker
repo"): the 1.x repo is reference only and may be deleted, and nothing here builds from it.

```toml
# Cargo.toml
[workspace]
members = ["crates/z-work-core", "crates/zwt-app", "crates/zwt-bridge", "crates/zwt-ui-logic"]

[workspace.dependencies]
z-work-core = { path = "crates/z-work-core" }
```

- **What was copied**, with `git archive` from commit `a88c1aa82d5e14179f18349172e0b4c6e6715f5f` of
  https://github.com/rustcircle/z-work-tracker (not from a working tree): the crate `src-tauri/crates/z-work-core`
  (sources, examples, the PDF fonts, proptest regressions, its cloud fixtures), the parity fixtures `fixtures/*.json`
  into `fixtures/`, and the two 1.x TypeScript files its tests compare against into
  `crates/z-work-core/tests/1x-src/`. Every copied file equals its `a88c1aa` blob.
- **Local edits** are paths (fixtures, the licence, the two TypeScript files) and the `repository` URL. The list,
  the commands and how to pull a later 1.x change by hand are in [docs/DEPENDENCIES.md](docs/DEPENDENCIES.md).
- The crate is a workspace member: it inherits edition 2024, rust-version 1.99 and the lints from this workspace
  (the same as 1.x `src-tauri/Cargo.toml`), and `cargo test --workspace` runs its tests too. `Cargo.lock` holds it
  as a path package; its dev-dependency `proptest` (1.12.0, as in the 1.x lock) is locked here now.
- The package keeps its 1.x version (1.3.3), which the core writes as the app version into lease records, version
  files and the store. Changing it is a decision of its own.
- **Core changes 2.0 needs are made here**, in `crates/z-work-core`, each in its own commit.
  [docs/CORE_REQUESTS.md](docs/CORE_REQUESTS.md) lists what 2.0 has worked around in `zwt-bridge` so far.

History: until this change (Milestone 1) the core was a git dependency pinned to `a88c1aa` plus a gitignored
`.cargo/config.toml` path override to the sibling checkout, so that no code was copied. It worked, but every build
needed the 1.x repo or its GitHub copy.

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
2. Slint backend/renderer that cross-compiles with MinGW: the `winit` backend with the **femtovg-wgpu** renderer
   (FemtoVG on wgpu), chosen in code (`crates/zwt-app/src/renderer.rs`): Metal on macOS; Direct3D 12 on Windows,
   then Vulkan; never OpenGL (owner decision 2026-11-04: OpenGL is deprecated on macOS and weak in Windows VMs).
   The **software** renderer draws where no GPU adapter is found. Avoid Skia unless its prebuilt download works
   (that download would need approval).
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

**Owner rules of 2026-11-04** (M1 fix round, docs/PARITY.md status `2.0 change`): the task list in 1.x's Active /
Drafts / Stopped groups (restored 2026-11-05), each with the running task on top, then by latest counted time,
never-run tasks last (row 416); Timeline days from the past to the future with
page arrows (row 417); an hour sub-grid at the rounding increment (row 418); an empty Timeline keeps its days
(row 419). Row 420 (the now line moves once a minute while nothing runs) came from the code review and awaits the
owner's confirmation.

### Milestone 2: tasks, projects, corrections

Projects (price per hour, currency, fixed fee, budget alerts, archive), tasks and subtasks (forms, drafts, cold
resume = new linked task form, continued tasks, delete rules), tags and templates, colours, corrections and
manual entries, billable pauses, subtask time, and **subtask tags** (a 2.0 feature built natively from the 1.4.0
subtask-tags plan and its Owner answers: a subtask carries a subset of its task's tags, default "Same as task",
popover with one Select all / Deselect all toggle, empty subset = "No tag", catch-all for unknown fields on Task
and Segment, copies and templates reset to "Same as task"; docs/PARITY.md rows 70-80). AC: each matches the README section of the same name;
`patch-task-parity.json` passes through the Rust path.

### Milestone 3: tray, lifecycle, time away

Tray icon (idle/running icons from the Tauri repo's assets, referenced or re-exported, not hand-copied code),
menu Show / current task h:mm / Pause-Resume / Quit, left-click shows; **close hides to tray, timing continues,
only Quit pauses** (closes the open segment, marks paused); first-hide notice; single instance (second launch
focuses the window); heartbeat every ~30 s and crash recovery notice; time away (sleep, clock jump > 2 min, lock,
idle limit default 10 min) with Keep / Drop / Thinking; reminders (long run, after shift) as notifications;
Start with Windows (installer: Run key; Store: StartupTask). Crates to evaluate: `tray-icon` + `muda`,
`single-instance` or a named mutex/socket, `user-idle`, `notify-rust`/Windows toasts.

**Window frame** (owner decision, 2026-11-04; docs/PARITY.md rows 403-409): no native title bar; the app header is
the title bar. Minimise, maximise and close are drawn in the header in the Windows 11 style, each with an
accessible label (close hides to tray, as above). Dragging the header moves the window, with Aero Snap;
double-clicking it maximises; every edge and corner resizes. Windows 11 Snap Layouts on the maximise button come
through a small Windows platform module, the one exception to `unsafe_code = "forbid"` that [§8](#8-gates) allows.
On macOS the traffic lights sit inside the content, in the header. Verified in the owner's Windows test pass (R4).

AC: README §Tray behaviour and §Time away hold, checked by tests where possible and the manual checklist otherwise;
the window has no native title bar on Windows or macOS, and UI tests find minimise, maximise and close by their
accessible labels; the owner's Windows test pass confirms drag with Aero Snap, double-click to maximise, resizing
from every edge and corner, and Snap Layouts on the maximise button; on macOS the traffic lights sit in the header.

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

Settings, themes (light/dark/system plus the existing palettes), backups and restore, folder sync on OneDrive,
Google Drive and the providers below (detection, two folders, versions and restoring), one computer at a time
(lease banner, Release now, Take over, Pause it from here), first-run setup.

**Sync providers** (owner decision, 2026-11-04; docs/PARITY.md rows 411-415): besides OneDrive and Google Drive,
detect the desktop folders of **Dropbox, iCloud Drive, Box, pCloud, MEGA, Proton Drive, Nextcloud/ownCloud and
Sync.com**, listed like the 1.x ones (checkboxes, none on silently) in Settings and in the setup sync step; plus an
explicit **Other provider** folder choice with an optional typed name that messages use for that folder. A new
provider is stored so a 1.x device still reads the store (as `Custom` plus a label, or with a serde fallback),
proved by a round-trip test. Online-only (placeholder) files are detected for every provider and shown with a clear
notice, never treated as damage. The wording is per provider: status lines and notices name the folder's provider.
Detection and the provider type live in the core (`detect.rs`, `SyncProvider`), so this is a core change
([§4](#4-dependency-strategy-for-z-work-core)) in its own commit.

AC: a store written by the Tauri app opens in 2.0 and vice versa (sample data, both directions); sync between a
Tauri device and a Slint device converges (core cloud round-trip scenario reproduced with two sample dirs); tests
detect each new provider from its sample folder layout, and an Other provider folder with a typed name shows that
name in its status line; a round-trip test shows the 1.x core (`a88c1aa`) reads a store holding each new provider
and an Other provider name, and 2.0 reads that store back after the 1.x core has written it; an online-only
placeholder file gives the placeholder notice, never a damage warning.

### Milestone 7: packaging and Store release preparation

2.0 is the **first version on the Microsoft Store** (2.0.0.0, the first submission to the app already reserved in
Partner Center; no Tauri build is ever submitted). This milestone takes over the release steps planned for Tauri
1.4.0. The source of truth for the identity is **[docs/STORE_IDENTITY.md](docs/STORE_IDENTITY.md)** (never edit
its values without the owner): Identity Name `PedroMiguelRodriguesJorge.zWorkTracker`, Publisher
`CN=1DEF8D7B-1D68-4604-8047-048DC92CF3A9`, display name "Pedro Miguel Rodrigues Jorge", family
`PedroMiguelRodriguesJorge.zWorkTracker_54k2cbaqgvhqp`, Store ID `9P9RM9G0QBRC`, x64, version `2.0.0.0` (each
later submission higher, last part 0). From the 1.x manifest (W/scripts/msix-manifest.mjs, verified in
docs/PARITY.md §Store identity): Application Id `zWorkTracker`, StartupTask `zWorkTrackerStartup`,
`runFullTrust`, MinVersion 10.0.19041.0, and the separate test identity. Store data-redirect rules from the 1.x
README apply unchanged.

Steps (the 1.x scripts `package-msix.mjs`, `make-pri.mjs`, `msix-vm.mjs` are the model; port or reuse their logic,
never modify them in the old repo):

1. **MSIX packages**, Store (unsigned: the Store signs it) and test identity; the 60 images incl.
   `Square44x44Logo.targetsize-*_altform-unplated` cut from the same `icon.png` by the same rules; toast AUMID.
2. **Icon index (PRI)** made with `makepri` in the owner's Windows VM (UTM), sent back and checked (all 60 image
   paths, its own identity), kept in `packaging/msix/{store,test}/resources.pri`.
3. **WACK**: run the Windows App Certification Kit in the VM on a **copy** of the Store package signed with an
   ephemeral certificate (RSA 3072, subject exactly the Store Publisher, 7 days, code signing only, private key
   deleted after signing). The Store package itself stays unsigned. Save the report.
4. **Test install** in the VM with the test identity (signed with the owner's own test key, outside the repo).
5. **10 Store screenshots**, 1920x1080, from the sample-data personas: web developer 4, software engineer 3, data
   scientist 3 (screens chosen to show Timeline, Analysis incl. Share of time, Billing, exports, sync).
6. **Listing text**: revise [docs/STORE_LISTING_NOTICE.md](docs/STORE_LISTING_NOTICE.md) (copied from the Tauri
   repo, 1.3.2 wording) for 2.0: features, notices, requirements (no WebView2), version, privacy statements.
7. **Privacy policy page** on the site (`site/privacy.html`), updated for 2.0 and linked from the listing.
8. Version 2.0.0, full third-party licence list (`cargo about` or `cargo deny`), README user guide, the 2.0 site
   changes ([§11](#11-website)), the manual Windows test checklist adapted from the 1.x README.

AC: a release build passes every gate; WACK passes; the test install starts and runs in the VM; the owner runs it
on real Windows; **an identity test** (in `cargo test`, run on every build) reads the generated
`AppxManifest.xml` and asserts that `Identity/@Name`, `Identity/@Publisher` and
`Properties/PublisherDisplayName` equal the values in docs/STORE_IDENTITY.md, and that the Package Family Name
computed from Name and Publisher (Microsoft's publisher-id hash: SHA-256 of the UTF-16LE publisher, first 8 bytes,
Crockford base32, 13 characters) is exactly `PedroMiguelRodriguesJorge.zWorkTracker_54k2cbaqgvhqp`. Port the 1.x
test in W/scripts/msix-manifest.test.ts.

### Milestone 8: parity gate (release blocker)

**No 2.0 release until all of these pass:**

1. Re-run the inventory against the 1.x repo (in case it gained features) and update docs/PARITY.md; every
   `2.0 native (1.4.0 design)` row is checked against its design (live-updates spec, subtask-tags plan with
   Owner answers) and the 1.x code where it exists; every `2.0 addition` row against its milestone text here.
2. **Every row of docs/PARITY.md is checked against the Tauri app** (side by side, same sample store) and ticked,
   or carries an owner-approved exception; a `2.0 addition` row (1.x has nothing to compare) is checked as in
   item 1 instead. The 7 `1.x-only` rows each have their 2.0 equivalent (e.g. the
   WebView2-missing message becomes a friendly start-up check for what 2.0 needs: a GPU/renderer fallback to the
   software renderer, a damaged or locked store, a missing data folder, with the same "Your data is safe." tone).
3. **Visual parity** ([§7.2](#72-visual-parity)) passes for every screen and theme.
4. **Responsiveness targets** ([§10.2](#102-targets-20-must-beat)) all pass on the Windows VM, with the Tauri baseline
   (the newest 1.x build, 1.3.3 or W) recorded first.
5. Data round trip: a store and sync folder written by 1.3.3 (and W) open in 2.0 and vice versa, and a 1.x device
   and a 2.0 device sync and converge (sample data only); subtask-tag overrides survive a 1.3.3 device.

### Milestone 9: Store submission (owner)

After the parity gate passes: the owner's **guided Partner Center submission** of 2.0.0.0 (the stack prepares
the package, the listing text, screenshots, privacy URL and certification notes, and walks the owner through it;
it never submits, signs in or publishes itself). Then the site switches to the 2.0 wording ([§11](#11-website)).

## 7. Parity

### 7.1 Feature parity

The checklist is **[docs/PARITY.md](docs/PARITY.md)**: 420 lines, one per feature or rule, each with its source
(README/howto line, spec, plan, commit, or owner decision) and the milestone that delivers it. Status counts at
2026-11-05 (each is `grep -c '| <status> |' docs/PARITY.md`): 361 `shipped 1.3.3`, 33 `2.0 native (1.4.0 design)`
(sticky Share level, live updates, subtask tags with Select all/Deselect all, No tag when empty, unknown-field
catch-all, copies reset to Same as task), 1 mixed (row 223, `shipped 1.3.3 (subtask part: 2.0 native)`), 7
`1.x-only, needs a 2.0 equivalent` (WebView2 checks, CSP, snapshot IPC, show-after-first-paint), 13 `2.0 addition`
(owner decisions of 2026-11-04 beyond 1.x: the drawn window frame, rows 403-409; the period label on Share of time,
row 410; more sync providers and Other provider, rows 411-415), 5 `2.0 change` (1.x behaviours 2.0 replaces on
purpose, each row stating the 1.x one: the owner rules of 2026-11-04 for the task-list order, row 416, the Timeline's
days from the past to the future with page arrows, row 417, the hour sub-grid, row 418, and the empty Timeline,
row 419; the now line moving while nothing runs, row 420, from the code review, owner to confirm). Also there:
the verified Store identity, the icon inventory, and what the app deliberately never does (no network calls, no
telemetry, no Mica, OS theme not followed, ...). Those "never" rules are parity rules too.

Each milestone ticks nothing; it implements its rows and tests them. Ticking happens only at the parity gate.

Contradictions the inventory found in the 1.x docs (resolve against the code, not the docs):
- Refresh model: the uncommitted 1.4.0 README/howto still describe a 30 s refresh; the live-updates spec replaces
  it with no polling. 2.0 builds the spec (no polling), never the 30 s draft.
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
  weight 650), `Cascadia Mono` for code/changes text, with the same fallbacks as W/src/styles/base.css.
  **Not bundled** (owner, 2026-11-04): the app uses the fonts installed on the system. On macOS dev builds the
  fallback is the system font; parity is judged on Windows.
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
- Parity fixtures live in this repo: the core's tests read `fixtures/*.json` (copied from 1.x at `a88c1aa`) and
  never skip. The UI drift tests (theme, strings) read 1.x sources from `$ZWT_1X_DIR`, else `z-work-tracker`, else
  `reference/z-work-tracker` in an ancestor folder, and skip with a message when none is there.
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
`pnpm msix:vm` flow already uses), Tauri build (the newest 1.x: 1.3.3 or W) vs Slint build, both x64, same VM, same settings, same
sample store holding **one year of data** (generated by a script in this repo, e.g. 12 projects, ~200 tasks,
~2,500 segments; also the 1.x `store_bench` 50k stress size). **Record the Tauri baselines first**, in
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
| Idle memory (private working set, all processes) | ≤ 60 MB | vs typical WebView2 100–200 MB; ⚑ femtovg-wgpu/GPU drivers can add hundreds of MB (M3 Mac: phys_footprint 1.21 GB with wgpu, 0.66 GB with OpenGL; docs/DEPENDENCIES.md); the software renderer is the fallback |
| Input-to-redraw, clicks and typing | ≤ 16 ms | one frame at 60 Hz, p95 |
| Tab / screen switch | ≤ 50 ms | p95, to the fully drawn screen |
| Analysis full redraw, one year of data | ≤ 100 ms | ⚑ depends on core analytics cost; measure the core alone first |
| CPU idle, window visible, no task running | ≈ 0 %; no wakeups, except one per minute while the Timeline shows today's axis | that wake moves the now line (docs/PARITY.md row 420, pending the owner); 1.x's now line stands still until midnight, so 1.x has none; it also wakes while minimised or hidden (§12) |
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
unchanged). From now on **all site changes are made here**, not in the old repo. The Slint badge is in the
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
- Open (owner), from the milestone 1 review: the minute clock that moves the now line (docs/PARITY.md row 420)
  wakes the app once a minute while the Timeline shows today's axis, also while the window is minimised or hidden,
  where row 347 says nothing is recomputed. Keep that wake, or stop the clock while the window is not visible? Window
  visibility is milestone 3.
- Open, review N2 (tie order in the Share legend): rows with equal seconds are ordered by name
  (`crates/zwt-ui-logic/src/collate.rs`), and some of these ties differ from 1.x: `æ` and `Æ` (also `œ` and `Œ`)
  compare equal, `STRASSE` sorts after `Straße`, typographic punctuation sorts after all ASCII punctuation, and some
  scripts and the dotless `ı` are off; 1.x's `localeCompare()` follows the webview's locale. Fix: secondary weights
  for `æ`, `œ` and `ß`, or `icu_collator` once the parity target is decided.
