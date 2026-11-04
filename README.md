# zWorkTracker 2.0 (Slint)

A native Rust desktop version of [zWorkTracker](https://github.com/rustcircle/z-work-tracker), the local-first time
tracker for freelancers, with a [Slint](https://slint.dev) UI over the same Rust core (`z-work-core`) and the same
data as the 1.x Tauri app.

**Status: planning.** No app code yet. Start with **[PLAN.md](PLAN.md)**: it holds the milestones, the
z-work-core dependency strategy, the parity checklist against the Tauri app, the reactive architecture, the
Windows x86_64 cross-compile, the measurement plan, the license obligations and the gates.
The exhaustive feature checklist against the 1.x app is [docs/PARITY.md](docs/PARITY.md).

The public website (z.rustcircle.com) lives in [site/](site/) and is deployed by
[.github/workflows/pages.yml](.github/workflows/pages.yml) through GitHub Pages; the custom domain is set in the
repo's Pages settings.

## Licensing

- zWorkTracker's own code is licensed under the **MIT License**: see [LICENSE](LICENSE)
  (Copyright (c) 2026 Pedro Miguel Rodrigues Jorge).
- It uses **Slint** under the **Slint Royalty-free Desktop, Mobile, and Web Applications License 2.0**
  ([terms](https://slint.dev/terms-and-conditions#royalty-free),
  [license text](https://github.com/slint-ui/slint/blob/master/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md)),
  accepted by the owner on 2026-11-04. As that license requires, the app shows Slint's `AboutSlint` widget on its
  About screen, and the download page shows the "Made with Slint" badge
  (in the footer of [site/index.html](site/index.html); [snippet](docs/site-slint-badge.html)).
- Other third-party components: [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
