# Carried over from 1.4.0

The owner decided on 2026-11-04 not to finish 1.4.0 on Tauri. Its requirements move to 2.0 here, and the Slint app is the first Store release.

- `subtask-tags-plan.md`: the 1.4.0 subtask-tags plan, ending with the owner's answers. Its Tauri-specific steps are reference only; the requirements and answers apply to 2.0.
- `live-updates-spec.md`: the owner-approved reactive rules. These are the final rules: per-data subscriptions, events from the core after every write and sync merge, one 1 s running clock, no polling, redraw only when output changes, hidden-window catch-up.
- `live-updates-tauri-plan.md`: the stopped Tauri plan for the same rules, kept for reference.

The unfinished Tauri code is on branch `wip/tauri-live-updates` (a4a902c) in the old repo. It was not gated and must not be merged.
