# Third-party notices

zWorkTracker's own source code is licensed under the MIT License (see [LICENSE](LICENSE)),
Copyright (c) 2026 Pedro Miguel Rodrigues Jorge.

## Slint

zWorkTracker 2.x uses [Slint](https://slint.dev) for its user interface, under the
**Slint Royalty-free Desktop, Mobile, and Web Applications License 2.0**.

- Terms: https://slint.dev/terms-and-conditions#royalty-free
- License text: https://github.com/slint-ui/slint/blob/master/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md
- The owner accepted this license on 2026-11-04.

The app meets the license's attribution requirement both ways:

1. The **About** screen, reachable from the app's top-level menu, shows Slint's `AboutSlint` widget.
2. The public download page (`site/index.html`, footer) shows the "Made with Slint" badge (snippet: [docs/site-slint-badge.html](docs/site-slint-badge.html)).

## z-work-core

The core (`crates/z-work-core`) is the owner's own code, MIT licensed under [LICENSE](LICENSE) like the rest of
this repository. It was copied here on 2026-11-04 from the 1.x app, https://github.com/rustcircle/z-work-tracker at
`a88c1aa` (docs/DEPENDENCIES.md). It compiles these into the app, under their own licences:

| Component | Licence | Used for |
|---|---|---|
| [Noto Sans](https://github.com/notofonts/latin-greek-cyrillic) 2.015, Regular and Bold, © 2022 The Noto Project Authors | SIL Open Font License 1.1, no Reserved Font Name. Text in `crates/z-work-core/fonts/OFL.txt`; source and checksums in `fonts/SOURCE.md` beside it | Text of the PDF timesheet. A subset goes into each PDF |
| [SQLite](https://sqlite.org/) 3.54.1, the amalgamation in `libsqlite3-sys` 0.39.1 | Public domain ([sqlite.org/copyright.html](https://sqlite.org/copyright.html)) | The local store, `z-work-data.db`, compiled into the app |
| `rusqlite` 0.41.1 and `libsqlite3-sys` 0.39.1, © 2014 The rusqlite developers | MIT, text below | Rust's access to SQLite |
| IANA Time Zone Database in `chrono-tz` | Data: public domain. `chrono-tz`: MIT OR Apache-2.0 | Time zone rules |

### rusqlite and libsqlite3-sys licence

Both crates carry this text:

```
Copyright (c) 2014 The rusqlite developers

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
```

## zwt-bridge and zwt-app

Besides Slint and the core, the app compiles these into itself, under their own licences:

| Component | Licence | Used for |
|---|---|---|
| `unicode-normalization` 0.1.26, with tables from the Unicode Character Database 18.0.0 | Code: MIT OR Apache-2.0. Tables: Unicode License v3 (its text: the crate's `LICENSE-UNICODE`, or https://www.unicode.org/license.txt) | NFKC of folder names in the data-folder guard |
| `unicase` 2.10.0 | MIT OR Apache-2.0 | Unicode case folding of folder names in the data-folder guard |
| `signal-hook` 0.4.6 and `signal-hook-registry` 1.4.8, macOS and Linux builds only | MIT OR Apache-2.0 | SIGTERM, SIGINT and SIGHUP end the app as Quit does |

The Unicode License v3 asks for its copyright and permission notice to appear with copies of the data, or in their
documentation: the release's notices must carry its full text (`cargo about`, below, collects it).

## Graphics: wgpu, under Slint's FemtoVG renderer

Since 2026-11-04 the window is drawn by Slint's FemtoVG renderer on [wgpu](https://wgpu.rs) (Metal on macOS, Direct3D
12 or Vulkan on Windows; docs/DEPENDENCIES.md). That compiles these crates into the app, all from crates.io, under their
own licences (as `cargo metadata` gives them; versions as locked):

| Crate | Version | Licence | In the build for |
|---|---|---|---|
| `wgpu`, `wgpu-core`, `wgpu-hal`, `wgpu-types`, `wgpu-naga-bridge`, `naga`, `naga-types` | 30.0.1 | MIT OR Apache-2.0 | macOS, Windows |
| `wgpu-core-deps-apple` | 30.0.1 | MIT OR Apache-2.0 | macOS |
| `wgpu-core-deps-windows-linux-android` | 30.0.1 | MIT OR Apache-2.0 | Windows |
| `objc2-metal` | 0.3.2 | Zlib OR Apache-2.0 OR MIT | macOS |
| `raw-window-metal` | 1.1.0 | MIT OR Apache-2.0 | macOS |
| `static_assertions` | 1.1.0 | MIT OR Apache-2.0 | macOS |
| `ash` (Vulkan) | 0.38.0+1.3.281 | MIT OR Apache-2.0 | Windows |
| `gpu-allocator` (Direct3D 12 memory) | 0.28.0 | MIT OR Apache-2.0 | Windows |
| `glow`, `khronos-egl` (wgpu's OpenGL backend: compiled in, never used) | 0.17.0, 6.0.0 | MIT OR Apache-2.0 OR Zlib; MIT/Apache-2.0 | Windows |
| `range-alloc` | 0.1.5 | MIT OR Apache-2.0 | Windows |
| `renderdoc-sys` | 1.1.0 | MIT OR Apache-2.0 | Windows |
| `presser` | 0.3.1 | MIT OR Apache-2.0 | Windows |
| `spirv` | 0.4.0+sdk-1.4.341.0 | **Apache-2.0** | Windows |
| `ordered-float` | 5.5.1 | **MIT** | Windows |
| `codespan-reporting` | 0.13.1 | **Apache-2.0** | macOS, Windows |
| `half` | 2.7.1 | MIT OR Apache-2.0 | macOS, Windows |
| `bit-set`, `bit-vec` | 0.10.0, 0.9.1 | Apache-2.0 OR MIT | macOS, Windows |
| `parking_lot`, `parking_lot_core`, `lock_api` | 0.12.5, 0.9.12, 0.4.14 | MIT OR Apache-2.0 | macOS, Windows |
| `profiling` | 1.0.18 | MIT OR Apache-2.0 | macOS, Windows |
| `zerocopy` | 0.8.63 | BSD-2-Clause OR Apache-2.0 OR MIT | macOS, Windows |
| `log` (also a direct dependency of zwt-app now; compiled in before, through winit) | 0.4.35 | MIT OR Apache-2.0 | macOS, Windows |

Build-time only, not in the binary: `document-features` 0.2.12, `litrs` 1.0.1 and `zerocopy-derive` 0.8.63 (MIT OR
Apache-2.0; zerocopy-derive also BSD-2-Clause). The OpenGL path's crates went out with the OpenGL renderer: `glutin`,
`glutin-winit`, `cgl` and `dispatch2` (macOS), `glutin`, `glutin-winit` and `glutin_egl_sys` (Windows).

All are permissive; none is copyleft, and none ships a `NOTICE` file. What they ask of a binary release:

- **Apache-2.0 only** (no MIT option): `spirv` and `codespan-reporting`. Give recipients a copy of the Apache License
  2.0 (section 4a) and keep their copyright notices; there is no `NOTICE` file to carry.
- **MIT only**: `ordered-float`; its copyright and permission notice goes with the app, as for the MIT option of
  every other crate here.
- **No licence text in the crate package**: `objc2-metal`, `profiling` and `spirv` declare their licence in
  `Cargo.toml` only; their texts are in their repositories (github.com/madsmtm/objc2, github.com/aclysma/profiling,
  github.com/gfx-rs/rspirv). `cargo about` must be pointed at those (or use the SPDX texts) when the release notices
  are generated.
- Nothing from Microsoft's DirectX Shader Compiler is shipped: wgpu's `static-dxc` feature is off, so Direct3D 12
  compiles shaders with the system's FXC, or with a `dxcompiler.dll` only if one is already on the machine.

Each other Rust crate dependency keeps its own license. Before a release, generate a full list with `cargo about`
or `cargo deny list` (planned in PLAN.md, milestone 7).
