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

The core crates (`z-work-core`, `z-work-session`) come from https://github.com/rustcircle/z-work-tracker
(MIT, same copyright holder). Their own third-party dependencies are listed in that repository's
THIRD_PARTY_NOTICES.md. Each Rust crate dependency keeps its own license. Before a release, generate a full
list with `cargo about` or `cargo deny list` (planned in PLAN.md, milestone 7).
