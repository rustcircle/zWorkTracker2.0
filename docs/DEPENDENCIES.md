# Dependencies

How zWorkTracker 2.0 holds the core it shares with the 1.x app (vendored, in this repository), how it uses Slint,
and what was checked (Milestone 1, 2026-11-04, rustc/cargo 1.99.1 on an Apple M3 Mac).

## z-work-core: vendored at `crates/z-work-core`

The core is in this repository, copied from the 1.x repo. The 1.x repo is reference only from now on and may be
deleted (the owner, 2026-11-04: "could you copy over core into this repo and link against/reference that instead
because I might remove or delete z-work-tracker repo"). Nothing here builds from it or from GitHub.

```toml
# Cargo.toml
[workspace]
members = ["crates/z-work-core", "crates/zwt-app", "crates/zwt-bridge", "crates/zwt-ui-logic"]

[workspace.dependencies]
z-work-core = { path = "crates/z-work-core" }
```

### Provenance

| | |
|---|---|
| Source repository | https://github.com/rustcircle/z-work-tracker (local checkout `/Users/p/Z/ZCode/z-work-tracker`, read only) |
| Commit | `a88c1aa82d5e14179f18349172e0b4c6e6715f5f` (`main`, pushed; contains `fd51d79`, "Share of time: a level you pick stays") |
| Copied | 2026-11-04, with `git archive`: the commit's tree, not a working tree |
| Package | `z-work-core` 1.3.3, MIT, the owner's own code |

```sh
# Run from this repository's root. git archive only reads the 1.x checkout.
git -C /Users/p/Z/ZCode/z-work-tracker archive a88c1aa src-tauri/crates/z-work-core fixtures | tar -x -C "$SCRATCH"
mv "$SCRATCH/src-tauri/crates/z-work-core" crates/z-work-core
mkdir fixtures && mv "$SCRATCH"/fixtures/*.json fixtures/
# The two 1.x TypeScript files the core's tests compare against.
git -C /Users/p/Z/ZCode/z-work-tracker archive a88c1aa src/lib/types.ts src/components/TakeOverDialog.tsx \
  | tar -x -C "$SCRATCH2"
mkdir -p crates/z-work-core/tests/1x-src
mv "$SCRATCH2/src/lib" "$SCRATCH2/src/components" crates/z-work-core/tests/1x-src/
```

95 files were copied, and each was checked against its blob at `a88c1aa` with `git hash-object`: 95 of 95 equal.

| From (1.x at `a88c1aa`) | To (here) | Files |
|---|---|---|
| `src-tauri/crates/z-work-core/` | `crates/z-work-core/` | 89: `Cargo.toml`, `src/` (46), `examples/` (3), `fonts/` (Noto Sans Regular and Bold, `OFL.txt`, `SOURCE.md`), `proptest-regressions/` (5), `tests/fixtures/` (the 1.3.1 cloud folders, 30) |
| `fixtures/*.json` | `fixtures/` | 4: `analytics-parity`, `billing-fixtures`, `patch-task-parity`, `tz-fixtures` |
| `src/lib/types.ts`, `src/components/TakeOverDialog.tsx` | `crates/z-work-core/tests/1x-src/lib/`, `…/components/` | 2, read by `src/scenario_hlc_tests.rs` and `src/scenario_tests.rs` |

Nothing else outside the crate is needed to build or test it: the PDF fonts are `include_bytes!` from its own
`fonts/`, the time zone data comes from `chrono-tz`, there is no `build.rs`, and the licence test reads this
repository's `LICENSE` (the same bytes as 1.x's).

### Local edits

Paths and the `repository` URL; every other copied byte is as it was at `a88c1aa`. (The working record of this change
is `.stack/out/orchestrator/20261104-0756-m1-slint/nodes/VC/core-edits.md`, which is gitignored; this table is the
committed copy.)

| File in `crates/z-work-core/` | Change | Why |
|---|---|---|
| `Cargo.toml` | `repository` is https://github.com/rustcircle/zWorkTracker2.0 | the crate lives here now; no manifest names the 1.x repo |
| `src/lib.rs` | the licence test reads `CARGO_MANIFEST_DIR/../../LICENSE` (was `../../../LICENSE`) | the crate is two levels below the root here, three in 1.x (`src-tauri/crates/`) |
| `src/machine.rs` | `include_str!("../../../fixtures/patch-task-parity.json")` (was `../../../../`) | the fixtures are in `fixtures/` here |
| `src/billing.rs` | the shared fixtures load from `CARGO_MANIFEST_DIR/../../fixtures/` (was `../../../`) | same |
| `src/analytics.rs` | `analytics-parity.json` from `CARGO_MANIFEST_DIR/../../fixtures/` (was `../../../`); `ZWT_WRITE_PARITY=1` now rewrites this repository's copy | same |
| `src/scenario_hlc_tests.rs` | `ts_interface` reads `CARGO_MANIFEST_DIR/tests/1x-src/lib/types.ts` (was `../../../src/lib/types.ts`, the 1.x app's source); its doc comment says so | the 1.x TypeScript is not in this repository; the copy keeps the test hermetic and as strict as before |
| `src/scenario_tests.rs` | reads `CARGO_MANIFEST_DIR/tests/1x-src/components/TakeOverDialog.tsx` (was `../../../src/components/…`) | same |

Added beside the copy: `crates/z-work-core/.gitattributes` and `fixtures/.gitattributes`, which carry the 1.x root
`.gitattributes` rules for these files (`fonts/OFL.txt -text`, `*.ttf binary`, `*.json text eol=lf`) so a Windows
checkout with `core.autocrlf=true` leaves them byte for byte; and `crates/z-work-core/tests/1x-src/README.md` (where
the two TypeScript files come from and which tests read them).

Left as copied, on purpose: the run notes of the examples (`gen_tzdata.rs` names the 1.x frontend's
`../src/generated/tzdata.json` as its output, which is an argument; `store_bench.rs` says to run from `src-tauri`,
which is the repository root here); and the `no_network` test, whose `cargo tree --offline --workspace` runs from
`CARGO_MANIFEST_DIR/../..`. That was 1.x's `src-tauri`; here it is this workspace's root, so the test now checks that
no HTTP or TLS crate is compiled into 2.0, Slint included, for `x86_64-pc-windows-gnu` and `aarch64-apple-darwin`.

### In the workspace

- Edition 2024, rust-version 1.99 and the lints (`unsafe_code = "forbid"`; clippy `all`, `unwrap_used`,
  `expect_used`, `panic`, `indexing_slicing`) come from this workspace, with the same values as 1.x
  `src-tauri/Cargo.toml` at `a88c1aa`. The core is clean under `clippy -D warnings` for the host and for
  `x86_64-pc-windows-gnu`, tests and examples included.
- `cargo test --workspace` runs the core's 831 tests too: the licence, `no_network`, the fixtures shared with 1.x's
  TypeScript, the 1.3.1 cloud round trip, the scenarios and the proptests.
- The package keeps its 1.x version, 1.3.3. The core writes `CARGO_PKG_VERSION` as the app version into lease
  records (`lease_io.rs`), version files (`versions.rs`) and the store (`store.rs`), as it did as a git dependency.
  Moving it to the workspace's 2.0.0 changes what 2.0 writes into shared data, so it is a decision of its own.
- The `test-hooks` feature is unchanged (test-only, never on in a shipped build).
- Core changes 2.0 needs are made here, each in its own commit ([CORE_REQUESTS.md](CORE_REQUESTS.md)).

### Lock

`Cargo.lock` was regenerated offline with `cargo metadata --offline` (no `cargo update`). `z-work-core` lost its
`source = "git+…"` line and is now a path package. Because the core is a member now, its dev-dependencies resolve
here, which a git dependency's never do: 14 packages were added, each at the version the 1.x lock at `a88c1aa` has,
`proptest` 1.12.0, `bit-set` 0.11.1, `bit-vec` 0.10.1, `chacha20` 0.10.2, `core_detect` 1.0.0, `cpufeatures` 0.3.1,
`quick-error` 1.2.3, `rand` 0.10.3, `rand_core` 0.10.1, `rand_xorshift` 0.5.0, `rusty-fork` 0.3.1, `unarray` 0.1.4,
`wait-timeout` 0.2.1, and `borsh-derive` 1.8.1 (only a lock entry for an optional feature of `bit-vec`'s `borsh`
dependency: `cargo tree -i borsh-derive` finds nothing that builds it). No package was removed and no version changed:
576 packages before, 590 after. All 14 are test-only; none reaches a release build.

The local override is gone: `.cargo/config.toml` (the `paths` override to the sibling checkout) went to the Trash,
and the empty `.cargo/` with it. The `/.cargo/config.toml` line stays in `.gitignore` for machine-local settings.

### Independence, checked

- `grep -rn 'z-work-tracker' Cargo.toml crates/*/Cargo.toml crates/*/build.rs .cargo` prints nothing.
- `cargo metadata --format-version 1 --locked` lists `z-work-core` with `"source": null` and this repository's
  `crates/z-work-core/Cargo.toml` as its manifest.
- With reads and writes of the 1.x checkout denied,
  `sandbox-exec -p '(version 1)(allow default)(deny file-read* file-write* (subpath "/Users/p/Z/ZCode/z-work-tracker"))'`,
  and a fresh `CARGO_TARGET_DIR`, `cargo build --workspace --locked --offline` and
  `cargo test --workspace --locked --offline` both exit 0. The UI drift tests then read
  `/Users/p/Z/ZCode/zWorkTracker/reference/z-work-tracker` (next section).

### The 1.x sources of the UI drift tests

`crates/zwt-app/tests/parity_theme.rs`, `crates/zwt-app/tests/parity_strings.rs` and `scripts/gen_theme.py` compare
2.0's theme and strings with the 1.x sources, which were not copied. They take the 1.x repo from `$ZWT_1X_DIR`, else
the first ancestor A with `A/z-work-tracker/src/lib/themes.ts` (a checkout beside this one), else the first ancestor
A with `A/reference/z-work-tracker/src/lib/themes.ts` (the gitignored read-only copy in the main checkout, at
`a88c1aa`). A folder that cannot be read counts as missing. With none of them both tests print a skip message and
pass (the strings test still checks its own format), and the script exits 2.

### Pulling a later 1.x core change by hand

Only while the 1.x repo exists, and only for a 1.x change that 2.0 wants. From this repository's root, with `NEW` the
1.x commit and `O=/Users/p/Z/ZCode/z-work-tracker`:

1. **Diff.** `git -C "$O" diff --stat a88c1aa NEW -- src-tauri/crates/z-work-core fixtures src/lib/types.ts
   src/components/TakeOverDialog.tsx`, then read the full diff of what changed.
2. **Apply as a patch** (keeps the local edits above):
   `git -C "$O" diff a88c1aa NEW -- src-tauri/crates/z-work-core | git apply -p4 --directory=crates/z-work-core`,
   `git -C "$O" diff a88c1aa NEW -- fixtures | git apply`, and for the two TypeScript files
   `git -C "$O" diff a88c1aa NEW -- src/lib/types.ts src/components/TakeOverDialog.tsx | git apply -p2 --directory=crates/z-work-core/tests/1x-src`.
   A hunk on an edited line (the paths, `repository`) is rejected: make that change by hand.
   **Or re-copy:** `git archive NEW` into an empty scratch folder as above, `diff -r` it against
   `crates/z-work-core` and `fixtures`, copy the changed files over, and redo the local edits in the table.
3. **Check.** No new path may leave the crate: `rg -n 'include_(bytes|str)!|\.\./' crates/z-work-core`. Then
   `cargo metadata --offline` for the lock (online if the change adds dependencies; never a blanket
   `cargo update`), the gates of PLAN.md section 8, and one commit, "Update z-work-core from 1.x <short sha>", with
   the 1.x range in its message. Update the provenance table above to `NEW`.

### Before: the git pin (history)

From the start of Milestone 1 until this change, the core was a git dependency,
`z-work-core = { git = "https://github.com/rustcircle/z-work-tracker", rev = "a88c1aa…" }`, plus a gitignored
`.cargo/config.toml` `paths` override to the sibling checkout for local work (a `[patch]` was tried and dropped
because it rewrote the lock). A fresh clone built from it, and offline once the git checkout was cached, but every
build needed the 1.x repo or its GitHub copy, and the core's own tests never ran here. Those checks are logged in
`.stack/out/orchestrator/20261104-0756-m1-slint/nodes/S1/`.

## Slint 1.18.1

```toml
slint = { version = "=1.18.1", default-features = false, features = [
    "std", "compat-1-2", "backend-winit", "renderer-femtovg-wgpu", "renderer-software", "accessibility",
    "unstable-winit-030", "unstable-wgpu-30",
] }
slint-build = "=1.18.1"
i-slint-backend-testing = { version = "=1.18.1", features = ["renderer-software"] }
```

- **Exact pins, moved together.** `i-slint-backend-testing` has no stability guarantee and requires the same
  version as the rest of Slint (`=1.18.1` in its own manifest), so `slint`, `slint-build` and the testing crate
  are re-pinned in one commit. Re-read the Slint licence text at each upgrade (PLAN.md section 9).
- **Features and why:**
  - `compat-1-2`: mandatory. Slint 1.18 stops with `compile_error!` unless `compat-1-18` is on; `compat-1-2`
    implies it and is the one Slint's docs call mandatory.
  - `std`: desktop. With the software renderer it also brings Path drawing (zeno) and system fonts (fontique).
  - `backend-winit`: windowing and events on macOS and Windows. Not `backend-default`, which would add the Linux
    Qt/X11/Wayland selection we do not ship.
  - `renderer-femtovg-wgpu`: FemtoVG on wgpu, GPU-drawn, the renderer the app asks for in code
    ([below](#renderer-femtovg-on-wgpu-2026-11-04)). It replaced `renderer-femtovg` (FemtoVG on OpenGL) on
    2026-11-04; no OpenGL renderer is built any more.
  - `renderer-software`: what draws where none of the platform's wgpu backends has a GPU adapter (a VM without 3D
    acceleration), forced with `SLINT_BACKEND=winit-software`; also what the UI tests use to render screenshots.
  - `accessibility`: AccessKit (UI Automation on Windows, NSAccessibility on macOS) for screen readers.
  - `unstable-winit-030`: `slint::winit_030::CustomApplicationHandler` and
    `BackendSelector::with_winit_custom_application_handler`. The app installs a handler whose `exiting` shuts the
    worker down, because macOS Quit (Cmd+Q, the Dock, logging out) ends the process inside AppKit's `terminate:`,
    where `app.run()` never returns (review R1; `crates/zwt-app/src/exit.rs`). The feature is outside Slint's
    stability promise (a minor release may change or drop it when a new winit major arrives): re-check `exit.rs`
    at each Slint upgrade.
  - `unstable-wgpu-30`: `BackendSelector::require_wgpu_30` with `slint::wgpu_30::WGPUSettings`, which is how the
    app asks for one wgpu backend, and `GraphicsAPI::WGPU30` in the rendering notifier, which names the adapter
    that draws (`crates/zwt-app/src/renderer.rs`). Also outside the stability promise: Slint says it "may be
    removed or changed in future minor releases, as new major WGPU releases become available" (wgpu 31 would come
    as `unstable-wgpu-31`). Re-check `renderer.rs` at each Slint upgrade.
- **Left out:** `renderer-skia*` (its build downloads a prebuilt Skia binary: needs approval, PLAN.md section 6),
  `system-tray` (Milestone 3 adds it with the tray), `image-default-formats` (PNG and JPEG suffice),
  `live-preview`, `mcp`, `system-testing` (developer features; `mcp` would add an HTTP server).
- **Checked on macOS** (the OpenGL build, before 2026-11-04: release, 4 s each,
  `SLINT_DEBUG_PERFORMANCE=refresh_lazy,console`): `SLINT_BACKEND=winit-femtovg` printed "Backend: FemtoVG renderer
  with OpenGL backend", `SLINT_BACKEND=winit-software` printed "Backend: software"; both drew frames and kept
  running. The wgpu build's checks are [below](#checked). Windows has only been cross-compiled, not run.
- **Style** is fixed to `fluent` in `crates/zwt-app/build.rs`, so `SLINT_STYLE` cannot change the look.
- **Element debug info:** the testing backend's `ElementHandle` queries need it. `build.rs` turns it on for every
  profile except `release` (`SLINT_EMIT_DEBUG_INFO=1` turns it on for release too).
- **Lints in `zwt-app`:** it cannot use `[lints] workspace = true`. The generated Rust expands Slint's vtable
  macros, which carry `#[allow(unsafe_code)]`, and an allow under `forbid` is a hard error (E0453). `zwt-app`
  repeats the workspace table with `unsafe_code = "deny"`, which still rejects unsafe code written in the crate
  (PLAN.md section 8 allows this exception). The generated module in `src/lib.rs` allows the clippy restriction
  lints (`unwrap_used`, `expect_used`, `panic`, `indexing_slicing`) that Slint's generated code trips; our own
  code keeps them.
- **Tests:** `png` 0.18 (already in the lock through the core) is a dev-dependency to save screenshots.

## Renderer: FemtoVG on wgpu (2026-11-04)

**Decision** (the owner, 2026-11-04, Milestone 1): the window is drawn by Slint's FemtoVG renderer on wgpu instead of
FemtoVG on OpenGL, because OpenGL is deprecated on macOS and weak in Windows VMs, while wgpu draws with Metal on
macOS and Direct3D 12 on Windows. `slint` stays at exactly 1.18.1: `renderer-femtovg-wgpu` replaced
`renderer-femtovg`, and `unstable-wgpu-30` was added.

**Chosen in code**, in `crates/zwt-app/src/renderer.rs`; `main` calls it through `exit::select_backend` before any
window, the Windows refusal window included:

| Platform | Asked of Slint, in order | Never |
|---|---|---|
| macOS | FemtoVG on wgpu with `Backends::METAL` only; then the software renderer | OpenGL |
| Windows | FemtoVG on wgpu with `Backends::DX12` only; then with `Backends::VULKAN` only; then the software renderer | OpenGL |
| Linux (not shipped) | FemtoVG on wgpu with `Backends::VULKAN` only; then the software renderer | OpenGL |

- Each wgpu attempt is `BackendSelector::backend_name("winit")`, `renderer_name("femtovg-wgpu")` and
  `require_wgpu_30(WGPUConfiguration::Automatic(settings))`, where `settings` is Slint's `WGPUSettings::default()`
  with `backends` set to the one backend. Slint refuses the attempt inside `select()`, before it makes the event
  loop, when that backend has no GPU adapter (a CPU adapter such as WARP or llvmpipe does not count;
  `SLINT_WGPU_CPU` skips the check, so the first attempt is then taken as it is). Only that refusal ("no GPU-backed
  WGPU adapter is available", `renderer::NO_GPU_ADAPTER`) leads to the next attempt; any other error ends the
  choice, since winit makes one event loop per process. `tests/renderer.rs` pins the refusal and its words.
- **What the fallback does not cover:** a failure after the choice, when the window's surface, adapter or device is
  made (at the first `resume`), ends the run with that error; the next backend cannot be tried in the same process.
  Not seen on this Mac; Windows is not run yet.
- **Why one backend per attempt:** wgpu 30 lists Vulkan adapters before Direct3D 12 ones
  (`wgpu-core-30.0.1/src/instance.rs`: Vulkan, Metal, Dx12, Gl, in that order; sorted by device type only when a
  power preference is set, and the default sets none), so `DX12 | VULKAN` in one attempt would pick Vulkan on any
  Windows machine with a Vulkan driver.
- **What Slint 1.18.1 does without this** (for the record): the femtovg-wgpu renderer asks wgpu for every backend
  from `WGPU_BACKEND` or all of them, minus `default_backends_to_avoid()` (OpenGL everywhere, Metal off Apple,
  Direct3D 12 off Windows), keeping an explicit OpenGL-only `WGPU_BACKEND`; that is Metal on macOS and Vulkan before
  Direct3D 12 on Windows. When the femtovg-wgpu renderer cannot be made, the winit backend silently falls back to the
  software renderer.
- **OpenGL** is compiled in on Windows but never asked for: Slint's FemtoVG depends on wgpu with its default
  features, which include `gles` (`glow`, `khronos-egl`, `glutin_wgl_sys`), and features only add up. Only an
  explicit `SLINT_BACKEND` together with `WGPU_BACKEND=gl` could reach it (both diagnosis switches, below).
- **No GPU adapter for any of them** (a VM without 3D acceleration): Slint's software renderer draws, and the line
  below says so with `refused=...`. Before, Slint's own fallback did the same, silently.
- **Diagnosis:** `SLINT_BACKEND`, when set and not empty, replaces the choice and Slint picks as it says, with its
  own wgpu defaults (`SLINT_BACKEND=winit-software` draws in software; `SLINT_BACKEND=winit-femtovg-wgpu` uses Slint's
  backend order). `WGPU_BACKEND` is not read by the in-code choice; `WGPU_POWER_PREF` and the
  `WGPU_VALIDATION`/`WGPU_DEBUG` flags are (Slint's `WGPUSettings::default()`), and `WGPU_ADAPTER_NAME` is, when the
  window's adapter is picked (`wgpu::util::initialize_adapter_from_env`). `ZWT_LOG=<level>` prints the `log` records
  of wgpu, naga and winit on stderr (`zwt-log WARN wgpu_hal::...`); no logger is installed without it.
- **What drew** is said once on stderr. With a measurement hook on (`ZWT_MEASURE*`), with `ZWT_LOG` set, or when
  `SLINT_BACKEND` chose, it comes from the device Slint hands the window's rendering notifier when the first frame
  is set up: `zwt-app renderer=femtovg-wgpu adapter="Apple M3 Ultra" backend=Metal type=IntegratedGpu` (plus
  `driver="..."` where the backend names one). Otherwise it comes from the choice as the window is made, before any
  frame: `zwt-app renderer=femtovg-wgpu adapter=unknown backend=Metal`. Either ends with `refused=Dx12` after a
  refused attempt, or `SLINT_BACKEND="..."` when that chose. The software renderer has no rendering notifier, so its
  line, `zwt-app renderer=software adapter=none backend=none`, always comes before the event loop runs. The
  `zwt-measure first-frame` line ends with the same renderer, adapter, backend and type fields.
- **Why the adapter is looked up only then:** while a rendering notifier is set, FemtoVG flushes the cleared
  surface before each frame (`i-slint-renderer-femtovg-1.18.1/lib.rs`, the `BeforeRendering` branch): an extra
  render pass and queue submit per frame, which only the hooks need. Measured runs pay it, as they did before (the
  hooks set the notifier); other runs do not. It made no difference to memory (below).
- **Windows shader compiler:** wgpu's `static-dxc` feature is off, so Direct3D 12 compiles shaders with the system's
  FXC (`d3dcompiler_47.dll`), or with a `dxcompiler.dll` if one is found by name on the DLL search path (wgpu's
  default `Dx12Compiler::Auto`). Left as wgpu's default; not run on Windows yet.

### Checked

On this Mac (Apple M3 Ultra, macOS 27.2), release builds, fresh sample folders, 2026-11-04 and -05:

- The real window, started as an app bundle (`open -n`, so it comes to the front) with `ZWT_MEASURE=1 ZWT_LOG=warn`:
  drawn completely by FemtoVG on Metal (`screencapture -l <window id>` of the window, 2720 x 1864:
  `.stack/out/orchestrator/20261104-2050-m1-resume/nodes/REN/window-femtovg-wgpu.png`, gitignored), and laid out the
  same as the software renderer draws it (`.../nodes/REN/final/window-software.png`). stderr: the renderer line
  above and the first-frame line, nothing else. Without a hook or `ZWT_LOG`: only
  `zwt-app renderer=femtovg-wgpu adapter=unknown backend=Metal`.
- With wgpu validation and debug flags and Metal API validation on (`WGPU_VALIDATION=1 WGPU_DEBUG=1
  MTL_DEBUG_LAYER=1 ZWT_LOG=warn ZWT_MEASURE=1 ZWT_MEASURE_EXIT=1`): the app ended by itself after the first frame;
  stderr held "Metal API Validation Enabled", the renderer line and the first-frame line, and no warning or error
  (wgpu panics on a validation error; Metal aborts). The debug build (wgpu's debugging flags on) under `cargo test`,
  with `ZWT_LOG=warn`, printed no `zwt-log` line.
- Quick numbers, OpenGL build (before) against wgpu build (after), alternating, started as bundles, with a task
  running (n = 3 each; the full re-measure is a later node):

  | | OpenGL (before) | wgpu (after) |
  |---|---|---|
  | `main` to the first frame | 674-702 ms (median 683) | 698-734 ms (median 717) |
  | RSS at 10 s | 137-138 MB | 226-229 MB |
  | `phys_footprint` at 10 s | 660-663 MB | 1,211-1,213 MB |

  The wgpu footprint is flat from 5 s to 60 s (1,210-1,212 MB): no growth. Of it, 907 MB is "owned physical
  footprint (unmapped) (graphics)", 171 MB IOAccelerator (graphics) dirty plus 280 MB reclaimable, 57 MB IOSurface.
  It is the same with and without a rendering notifier set (1,212 MB; 1,210 and 1,216 MB without) and with Slint's
  own wgpu defaults (`SLINT_BACKEND=winit-femtovg-wgpu`: 1,212 MB), so it is FemtoVG on wgpu itself. The software
  renderer, for scale: 59 MB footprint, RSS 142 MB. (These runs set the notifier in every wgpu run, as an earlier
  version of this change did; the first-frame runs set it through `ZWT_MEASURE=1` anyway.)
- CPU, one quick look (% of one core over 60 s, window in front, a task running): OpenGL 2.7 and 3.0 %; wgpu 2.9
  and 4.1 % without a rendering notifier (as runs without hooks are now), 2.6, 5.4 and 6.9 % with one. The spread
  within one build is as large as any difference, so none is claimed; the re-measure decides. A window fully covered
  by another (a second instance at the same place) used no more than one in front (wgpu 6.2 % against 7.0 %, OpenGL
  2.7 % against 3.3 %, n = 1), and a hidden app (Cmd+H) about 0.1 %: no redrawing at the display's rate while
  covered.
- **A window that is covered draws nothing:** on macOS wgpu hands out no drawable while the window is occluded
  (`wgpu-hal-30.0.1/src/metal/surface.rs`, the `occlusionState` check), and Slint keeps a window transparent until
  its first frame (`i-slint-backend-winit-1.18.1/macos.rs`, `RevealOnFirstFrame`). So the first frame waits until
  the window is visible. Started straight from a script (no app bundle, so nothing brings it to the front), two of
  five wgpu runs had their window up but drew nothing within 10 s, while the three OpenGL runs between them drew.
  The next six such runs drew, as did every run started as a bundle and the 20 wgpu windows of five rounds of
  `tests/lifecycle.rs` and `tests/renderer.rs`. Anything that waits for a first frame (the measurement scripts,
  those tests) needs the window in front.

### wgpu in the build

Packages built into the app (`cargo tree -e normal,build`, before against after):

- **macOS, added (23):** `wgpu`, `wgpu-core`, `wgpu-hal`, `wgpu-types`, `wgpu-naga-bridge`, `wgpu-core-deps-apple`,
  `naga`, `naga-types` 30.0.1; `objc2-metal` 0.3.2, `raw-window-metal` 1.1.0, `codespan-reporting` 0.13.1,
  `half` 2.7.1, `bit-set` 0.10.0, `bit-vec` 0.9.1, `parking_lot` 0.12.5, `parking_lot_core` 0.9.12, `lock_api`
  0.4.14, `profiling` 1.0.18, `static_assertions` 1.1.0, `zerocopy` 0.8.63; build-time only: `document-features`
  0.2.12, `litrs` 1.0.1, `zerocopy-derive` 0.8.63. **Removed (4):** `glutin` 0.32.3, `glutin-winit` 0.5.0, `cgl`
  0.3.2, `dispatch2` 0.3.1.
- **Windows, added (29):** the same wgpu and naga crates with `wgpu-core-deps-windows-linux-android` instead of the
  Apple one; `ash` 0.38.0+1.3.281 (Vulkan), `gpu-allocator` 0.28.0 and `presser` 0.3.1 (Direct3D 12 memory),
  `range-alloc` 0.1.5, `renderdoc-sys` 1.1.0, `spirv` 0.4.0+sdk-1.4.341.0, `ordered-float` 5.5.1, `glow` 0.17.0 and
  `khronos-egl` 6.0.0 (wgpu's OpenGL backend), and the shared helpers above (`codespan-reporting`, `half`, `bit-set`,
  `bit-vec`, `parking_lot`, `parking_lot_core`, `lock_api`, `profiling`, `zerocopy`, and the three build-time ones).
  **Removed (3):** `glutin` 0.32.3, `glutin-winit` 0.5.0, `glutin_egl_sys` 0.7.1.
- **Licences:** all MIT, Apache-2.0, Zlib or BSD-2-Clause, most with a choice; `spirv` and `codespan-reporting` are
  Apache-2.0 only, `ordered-float` MIT only. Details and the duties of a binary release: THIRD_PARTY_NOTICES.md.
- **Lock:** `wgpu` and `naga` 30.0.1 were in `Cargo.lock` already (Slint's Android and Linux targets), so the lock
  went from 592 to 598 packages: added `android-build` 0.1.4, `drm` 0.15.0, `glow` 0.17.0,
  `i-slint-backend-android-activity` 1.18.1, `khronos-egl` 6.0.0, `termcolor` 1.4.1, `wgpu-core-deps-emscripten` and
  `wgpu-core-deps-wasm` 30.0.1; removed `gbm` 0.18.0 and `gbm-sys` 0.4.0 (Linux, with glutin). Only `glow` and
  `khronos-egl` are built for our targets (Windows); the rest are lock entries for other targets. No version of an
  existing package changed. `cargo metadata --offline` could not resolve (the index cache lacked
  `i-slint-backend-android-activity`, named by `unstable-wgpu-30`), so `cargo metadata` ran online once; no
  `cargo update`.
- **Sizes** (release, stripped): macOS `zwt-app` 20,182,352 B, was 16,838,208 B (+3,344,144 B, +19.9 %); Windows
  `zwt-app.exe` 23,995,904 B, was 18,710,016 B (+5,285,888 B, +28.3 %; 18,537,984 B in docs/MEASUREMENTS.md, before
  this milestone's lifecycle fixes).

## Windows cross-compilation (MinGW)

- Target `x86_64-pc-windows-gnu` (in `rust-toolchain.toml`), linker and C compiler
  `/opt/homebrew/bin/x86_64-w64-mingw32-gcc` (GCC 16.2.0) found on `PATH` by rustc and by `cc` (which builds the
  core's bundled SQLite). No `.cargo/config.toml` entry is needed for it.
- `cargo build --release --target x86_64-pc-windows-gnu` gives
  `target/x86_64-pc-windows-gnu/release/zwt-app.exe`, `PE32+ executable (GUI) x86-64`, subsystem 2 (Windows GUI)
  from `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]` in `src/main.rs` (debug builds keep a
  console for logs). It imports only system DLLs (`kernel32`, `user32`, `gdi32`, `opengl32`, `dwrite`,
  `uiautomationcore`, `comctl32`, UCRT `api-ms-win-crt-*`, …): no `libgcc_s` or `libwinpthread` to ship. The wgpu
  build adds `dxgi` and `setupapi` to the imports; `d3d12.dll` and `vulkan-1.dll` are loaded at run time, and
  `opengl32` stays imported (wgpu's unused OpenGL backend). Cross-compiled and linked with MinGW without changes;
  not run on Windows yet.
- Skia is in `Cargo.lock` only as an Android dependency of Slint; `cargo tree --target x86_64-pc-windows-gnu` and
  `--target aarch64-apple-darwin` do not contain it, so it is never built.

## Release profile

```toml
[profile.release]
codegen-units = 1
lto = true
strip = true
```

Like the 1.x app, except `opt-level`, which stays at the default 3 where 1.x used `"s"`: here the UI is drawn by
Rust code (the software renderer, FemtoVG's tessellation, Slint's property engine), and the targets in PLAN.md
section 10.2 are about latency and CPU, not binary size. On the Milestone 1 skeleton the macOS release binary is
8,853,680 bytes at opt-level 3 and 7,665,472 bytes at `"s"` (13 % smaller); the Windows .exe is 9,630,720 bytes at
opt-level 3. `docs/MEASUREMENTS.md` has the numbers that decide it for good.

## Dependencies in use (milestone 1)

Every entry of `[workspace.dependencies]` is used by now; the versions are what `Cargo.lock` holds. Wiring the UI
(node U1) added no package to the lock (576 packages before and after), only the edges of `zwt-app`'s new entries.
Vendoring the core (node VC) added the 14 test-only packages listed under [Lock](#lock) (590). The fixes of review
findings R1 and R4 added two (592): `unicode-normalization` 0.1.26 and `signal-hook` 0.4.6. `unicase` 2.10.0 and
`signal-hook-registry` 1.4.8 were in the lock already (through Slint's `pulldown-cmark` and `async-signal`), and
`unicode-normalization`'s one dependency, `tinyvec` 1.13.3, through `fontdb`. The switch to FemtoVG on wgpu took the
lock to 598 ([wgpu in the build](#wgpu-in-the-build)) and made `log` 0.4.35, locked and built already through wgpu
and winit, a direct dependency of `zwt-app`.

| Dependency | Locked | Used by | What for |
|---|---|---|---|
| `slint` | 1.18.1 | zwt-app | the UI (features above) |
| `slint-build` | 1.18.1 | zwt-app (build) | compiles `ui/*.slint` |
| `i-slint-backend-testing` | 1.18.1 | zwt-app (dev) | headless UI tests, mock time, software-rendered screenshots |
| `z-work-core` | 1.3.3, path `crates/z-work-core` | zwt-ui-logic, zwt-bridge, zwt-app | the core, vendored; zwt-app reads `TaskState` and the `Clock` trait (the 1 s clock reads the tests' `ManualClock`) |
| `zwt-ui-logic` | workspace | zwt-bridge, zwt-app | 1.x formats and views; zwt-app uses `color::contrast` (readable chip text) and `format` |
| `zwt-bridge` | workspace | zwt-app | data-folder guard, worker, hub |
| `chrono` | 0.4.45 | zwt-ui-logic, zwt-bridge, zwt-app | dates; zwt-app parses a day chip's date for `SelectDay` |
| `chrono-tz` | 0.10.4 | zwt-ui-logic, zwt-bridge | zones |
| `serde_json` | 1.0.154 | zwt-ui-logic, zwt-bridge | the sample store and fixtures |
| `iana-time-zone` | 0.1.65 | zwt-bridge | the OS zone for the worker |
| `thiserror` | 2.0.21 | zwt-bridge | error types (`DataDirError`, `CommandError`, ...) |
| `png` | 0.18.1 | zwt-app (dev) | writes screenshots (`tests/ui_screens.rs`, `tests/ui_smoke.rs`) |
| `tempfile` | 3.27.0 | zwt-bridge (dev), zwt-app (dev) | sample stores in temp folders for tests |
| `unicode-normalization` | 0.1.26 | zwt-bridge | NFKC of folder names in the data-folder guard, so that spellings APFS or NTFS take for the real folder's name (`ſ`, `ﬆ`) are refused before any disk call (review R4) |
| `unicase` | 2.10.0 | zwt-bridge | full Unicode case folding of the same names (review R4) |
| `signal-hook` | 0.4.6 | zwt-app (Unix only) | SIGTERM, SIGINT and SIGHUP quit the event loop as Quit does, so the worker pauses what runs (review R1) |
| `log` | 0.4.35 | zwt-app | `ZWT_LOG=<level>` prints the log records of wgpu, naga and winit on stderr (`src/renderer.rs`) |

`zwt-app`'s own dependencies: `slint`, `zwt-bridge`, `zwt-ui-logic`, `z-work-core`, `chrono`, `log`, and on Unix
`signal-hook`; build: `slint-build`; dev: `i-slint-backend-testing`, `png`, `tempfile`. No new crate was added to the
workspace for the UI.

The core declares its own dependencies in `crates/z-work-core/Cargo.toml`, as it did in 1.x (not through
`[workspace.dependencies]`): `chrono` 0.4.45, `chrono-tz` 0.10.4, `rusqlite` 0.41.1 (`bundled`: SQLite 3.54.1 from
`libsqlite3-sys` 0.39.1), `rust_xlsxwriter` 0.102.0, `pdf-writer` 0.15.0, `png` 0.18.1, `miniz_oxide` 0.9.1,
`subsetter` 0.2.6, `ttf-parser` 0.25.1, `serde` 1.0.229, `serde_json` 1.0.154, `thiserror` 2.0.21, `uuid` 1.29.0,
and on Windows `winreg` 0.55.0; dev: `proptest` 1.12.0, `tempfile` 3.27.0. Their notices: THIRD_PARTY_NOTICES.md.
