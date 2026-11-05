# 1.x sources read by the core's tests

Two files of the 1.x TypeScript app, which the core's tests compare the Rust side against:

| File | Read by |
|---|---|
| `lib/types.ts` | `src/scenario_hlc_tests.rs`, `ts_interface` (the `ClockBehindView` fields and types) |
| `components/TakeOverDialog.tsx` | `src/scenario_tests.rs`, `OFFER_TAKE_OVER_TO_A_LIVE_HOLDER` |

Copied byte for byte from `src/lib/types.ts` and `src/components/TakeOverDialog.tsx` of
https://github.com/rustcircle/z-work-tracker at a88c1aa82d5e14179f18349172e0b4c6e6715f5f, with `git archive`, on
2026-11-04, together with the core (docs/DEPENDENCIES.md). They pin the core to the 1.x contract: when the Rust side
changes on purpose, change the copy with it and say so in the commit.
