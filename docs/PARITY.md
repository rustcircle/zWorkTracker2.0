# zWorkTracker 2.0 parity checklist

Every feature and behaviour rule of the 1.x Tauri app, one line each, with its source. 2.0 follows **every** one
(the owner: "I want the app to follow every feature we implemented so far"). **M** = the PLAN.md milestone that
delivers it. **Checked** = ticked only at the parity gate (PLAN.md milestone 8), after the line was compared with
the Tauri app. Rows marked `1.4.0, follow as merged` are still landing in 1.x: implement them as 1.4.0 is finally
merged, not as written here, and re-read the 1.x repo at that point.

Inventory built read-only on 2026-11-04 from the sources below; re-run it against the 1.4.0 release before the gate.

Read-only inventory, 2026-11-04. W = `/Users/p/Z/ZCode/z-work-tracker/.claude/worktrees/task-deletion-timeline-ad2298` (branch `claude/task-deletion-timeline-ad2298`, HEAD `fd51d79`; `main` = `0154df5` "Merge release 1.3.3 into main").

Source keys: **R** = W/README.md (working tree; line numbers are of that file), **H** = W/site/howto.html, **S** = W/STORE_LISTING_NOTICE.md, **LU** = W/.stack/out/delegator/live-updates-spec.md, **P** = W/.stack/out/planner/20261104-0638-140-subtask-tags/plan.md, **P/OA** = P "Owner answers", commit shas from `git -C W log`.

Status rules:
- `shipped 1.3.3`: in docs and code reachable from `main` (0154df5).
- `1.4.0, follow as merged`: only in `main..HEAD` (fd51d79), in uncommitted working-tree changes of W (the 30 s refresh text in R/H and edits to store.tsx, stepClock.ts, analysisClock.ts, TimelineTab.tsx and others), or only in LU / P. Follow the final merged form, not the draft.
- `1.x-only, needs a 2.0 equivalent`: tied to Tauri/WebView2/web stack; keep the idea, not the mechanism.

## Setup & first run

| # | Feature / rule | Source | Status | M | Checked |
|---|---|---|---|---|---|
| 1 | Fresh store opens a full-page setup wizard, ten steps | R L155; 4b9f00f | shipped 1.3.3 | 6 | [ ] |
| 2 | Steps: Welcome, theme with live preview, My zone + Client zone, time rules (15 min Up, plain table), first project, shifts from presets, sync, Start with Windows, summary | R L155; H L77 | shipped 1.3.3 | 6 | [ ] |
| 3 | Every step has Back and Skip; "Finish with defaults" ends it in one click | R L155 | shipped 1.3.3 | 6 | [ ] |
| 4 | Each step saves through normal settings; interrupted setup resumes at the saved step | R L155; 4b9f00f | shipped 1.3.3 | 6 | [ ] |
| 5 | Setup logs no time | R L155; H L77 | shipped 1.3.3 | 6 | [ ] |
| 6 | Sync step lists detected OneDrive/Google Drive folders with checkboxes; nothing on without a tick | R L155 | shipped 1.3.3 | 6 | [ ] |
| 7 | Start with Windows step, off by default | R L155 | shipped 1.3.3 | 6 | [ ] |
| 8 | Second PC: a detected folder with data offers "Connect your other computer" with counts and "Connect and bring it in" first | R L112, L157 | shipped 1.3.3 | 6 | [ ] |
| 9 | After connecting, setup skips what merged data answers, still asks device-local items | R L157 | shipped 1.3.3 | 6 | [ ] |
| 10 | Users with existing data never see the wizard; connecting a folder midway doesn't reopen it | R L157; 310d911, f578995 | shipped 1.3.3 | 6 | [ ] |
| 11 | "Run setup again" in Settings: confirm first, changes settings only, never data; leaves out data steps | R L157; 310d911 | shipped 1.3.3 | 6 | [ ] |
| 12 | Old one-time sync and client-zone prompts only for updaters who still owe them | R L157 | shipped 1.3.3 | 6 | [ ] |
| 13 | Store build: sync step opens "Choose a cloud folder." and explains uninstall deletes data | R L56 | shipped 1.3.3 | 6 | [ ] |
| 14 | Store build: Skip or Finish with defaults before sync asks "Continue without a folder?" | R L56 | shipped 1.3.3 | 6 | [ ] |

## Projects & billing setup

| # | Feature / rule | Source | Status | M | Checked |
|---|---|---|---|---|---|
| 15 | Project has unique name and a colour; a free colour is suggested | R L163 | shipped 1.3.3 | 2 | [ ] |
| 16 | Project with tasks can be archived, not deleted; one with no tasks can be deleted | R L163 | shipped 1.3.3 | 2 | [ ] |
| 17 | Archived projects can be restored | R L163 | shipped 1.3.3 | 2 | [ ] |
| 18 | Project with any billing summary (voided or not) can only be archived | R L187; 1de8cf1 | shipped 1.3.3 | 2 | [ ] |
| 19 | Price per hour in EUR (€) or USD ($) | R L165 | shipped 1.3.3 | 2 | [ ] |
| 20 | Blank rate = no rate ("No rate set"), distinct from 0 (real 0) | R L165, L245 | shipped 1.3.3 | 2 | [ ] |
| 21 | New price applies from now; backdating needs a date and confirmation | R L165 | shipped 1.3.3 | 2 | [ ] |
| 22 | Segment crossing a price change is split at that moment | R L165 | shipped 1.3.3 | 2 | [ ] |
| 23 | Project "billable" off = earns nothing | R L165 | shipped 1.3.3 | 2 | [ ] |
| 24 | Billing mode hourly or fixed fee (EUR/USD); fixed fee leaves hourly earnings empty | R L349 | shipped 1.3.3 | 2 | [ ] |
| 25 | Fixed fee: Analysis shows effective hourly rate = fee / billable hours | R L349 | shipped 1.3.3 | 2 | [ ] |
| 26 | Budget in hours or of the fixed fee; warns at 80% and 100% on screen and once per threshold as a system notification | R L349 | shipped 1.3.3 | 2 | [ ] |
| 27 | Fee-based budget values time at the project rate as target rate; says so when none set | R L349 | shipped 1.3.3 | 2 | [ ] |
| 28 | On-screen budget counts the running task up to now and moves with live refresh | R L349 (uncommitted); LU r2 | 1.4.0, follow as merged | 2 | [ ] |
| 29 | Project may have its own client zone (used for its entries in Show both) | R L270 | shipped 1.3.3 | 2 | [ ] |
| 30 | Project may override the rounding increment | R L305; 56a041e | shipped 1.3.3 | 2 | [ ] |
| 31 | Rate is tied to the task's current project; moving a task re-derives earnings | R L1097 | shipped 1.3.3 | 2 | [ ] |
| 32 | Two projects created offline with the same name on two PCs stay two projects | R L446 | shipped 1.3.3 | 2 | [ ] |

## Tasks & subtasks

| # | Feature / rule | Source | Status | M | Checked |
|---|---|---|---|---|---|
| 33 | Task has name, project, rich-text description (bold, italic, headings, numbered/bulleted lists, task lists) | R L169 | shipped 1.3.3 | 2 | [ ] |
| 34 | In description, Enter = new paragraph/next list item, Shift+Enter = line break | R L169; e471d83, b3a903f | shipped 1.3.3 | 2 | [ ] |
| 35 | Cold launch (boot, Quit+reopen, crash): new-task form empty; name, project and description with real text required (spaces don't count) | R L175-179 | shipped 1.3.3 | 2 | [ ] |
| 36 | Cold stays cold until the first task of this launch has started | R L177 | shipped 1.3.3 | 2 | [ ] |
| 37 | Warm (app stayed running, incl. tray; show from tray or 2nd launch stays warm): form prefilled with copy of most recently worked task, with "Copied from" note | R L177-178 | shipped 1.3.3 | 2 | [ ] |
| 38 | Warm: name and project required, description may be blank | R L179 | shipped 1.3.3 | 2 | [ ] |
| 39 | Cold/warm rules enforced by backend, not only the form | R L181; dd6e69d | shipped 1.3.3 | 2 | [ ] |
| 40 | Warm copy is a separate task; editing it never changes the original | R L181 | shipped 1.3.3 | 2 | [ ] |
| 41 | If the original's project is archived, choose another project first; form says why | R L181 | shipped 1.3.3 | 2 | [ ] |
| 42 | Cold start requires at least one tag while any tag exists; warm copies the tags | R L333 | shipped 1.3.3 | 2 | [ ] |
| 43 | Name and description editable in one click from Timeline entries and task details, in every state incl. running and stopped | R L185 | shipped 1.3.3 | 2 | [ ] |
| 44 | Edits autosave, on leaving the field, hiding the window or quitting; never touch timer or logged time | R L185 | shipped 1.3.3 | 2 | [ ] |
| 45 | Blank name not saved: last valid name stays, field says why | R L185 | shipped 1.3.3 | 2 | [ ] |
| 46 | Description can be cleared after the task has started | R L185 | shipped 1.3.3 | 2 | [ ] |
| 47 | Timeline, Analysis and exports always show current name/description | R L185 | shipped 1.3.3 | 2 | [ ] |
| 48 | Draft deleted from the bin on its card after one question | R L187 | shipped 1.3.3 | 2 | [ ] |
| 49 | Started task deleted only from Details: red "Delete task" under the tabs; no delete on cards or Timeline rows | R L187, L246; 0c77fdd | shipped 1.3.3 | 2 | [ ] |
| 50 | Review dialog "Delete this task?" names task, logged time and days, earnings (masked by Hide earnings), unanswered time away, continuing tasks (which stay, unlinked); says it can't be undone | R L187 | shipped 1.3.3 | 2 | [ ] |
| 51 | "Continue" leads to "Delete “name” for good?"; its "Delete task" deletes; "Keep it" and Esc back out; next try restarts at review | R L187 | shipped 1.3.3 | 2 | [ ] |
| 52 | Delete is mouse/tap only: Enter does nothing; Space / screen-reader activation on Continue or Delete task does nothing | R L187, L1086 | shipped 1.3.3 | 2 | [ ] |
| 53 | Continue and Delete task wait 1 s after the dialog appears and ignore the 2nd click of a double click | R L187 | shipped 1.3.3 | 2 | [ ] |
| 54 | Only Esc and Space on "Keep it" act from the keyboard; both dialogs say so | R L187 | shipped 1.3.3 | 2 | [ ] |
| 55 | Delete removes the whole task and all its time from Timeline, Analysis, billing, budgets, exports, tray, on all PCs at next sync | R L187; 1de8cf1 | shipped 1.3.3 | 2 | [ ] |
| 56 | Task running on this PC can't be deleted: button dimmed with reason | R L187 | shipped 1.3.3 | 2 | [ ] |
| 57 | Task running on another PC can be deleted; review warns its logged time goes and that PC's timer stops at its next sync | R L187 | shipped 1.3.3 | 2 | [ ] |
| 58 | Task with time on a live billing summary can't be deleted until voided; reason names the summary and links to Billing | R L187 | shipped 1.3.3 | 2 | [ ] |
| 59 | Voided summary keeps rows and the task name, says that time is gone (not unbilled) | R L187 | shipped 1.3.3 | 2 | [ ] |
| 60 | Delete crossing a new summary from another PC: summary keeps its numbers | R L187 | shipped 1.3.3 | 2 | [ ] |
| 61 | After delete: selection, open editors (unsaved words dropped silently) and Analysis filter forget the task | R L187; 0c77fdd | shipped 1.3.3 | 2 | [ ] |
| 62 | Delete goes in the audit trail; records stay in the data file (a pre-delete backup restores until sync re-merges the delete) | R L187 | shipped 1.3.3 | 2 | [ ] |
| 63 | Deletion marker beats every edit from every PC; time arriving later is hidden too | R L441 | shipped 1.3.3 | 2 | [ ] |
| 64 | Task-list item in description can be made active ("Make active", saved items only); exactly one active | R L327; H L147 | shipped 1.3.3 | 2 | [ ] |
| 65 | Each segment belongs to the subtask active when it started; switching while running closes and opens a segment | R L327 | shipped 1.3.3 | 2 | [ ] |
| 66 | Each subtask shows its own time on the task card | H L147 | shipped 1.3.3 | 2 | [ ] |
| 67 | Removing the active item from the description clears the active subtask | R L327 | shipped 1.3.3 | 2 | [ ] |
| 68 | Copying a task keeps subtask text, clears ticks, carries no time | R L327 | shipped 1.3.3 | 2 | [ ] |
| 69 | Subtask ids for version-1 task lists derived from task id + position (same on every device) | R L664 | shipped 1.3.3 | 2 | [ ] |
| 70 | Subtask tags: a subtask can carry a subset of its task's tags; default "Same as task" | P Goal, Semantics | 1.4.0, follow as merged | 2 | [ ] |
| 71 | Tags button on each subtask row of the task card (beside time and Make active) showing effective chips or "Same as task" | P UI baseline; P/OA Q3 | 1.4.0, follow as merged | 2 | [ ] |
| 72 | Popover: "Same as task" at top, then task's tags as checkboxes, plus one Select all / Deselect all toggle | P/OA Q3 | 1.4.0, follow as merged | 2 | [ ] |
| 73 | Popover keyboard: Tab to button, Enter/Space opens, arrows move, Space toggles, Esc closes and refocuses button | P UI baseline | 1.4.0, follow as merged | 2 | [ ] |
| 74 | Subtask tag control disabled in read-only and held states; not in the description editor or new-task form | P UI baseline; P/OA Q3 | 1.4.0, follow as merged | 2 | [ ] |
| 75 | Empty subset (all unticked, or last kept tag removed from task) counts as "No tag" | P Semantics; P/OA Q1 | 1.4.0, follow as merged | 2 | [ ] |
| 76 | Effective tags = override ∩ task's live tags in task order; no override / no subtask / unknown subtask id = task tags | P Semantics | 1.4.0, follow as merged | 2 | [ ] |
| 77 | Adding a task tag reaches only "Same as task" subtasks; removing one prunes it from all overrides in the same write | P Semantics | 1.4.0, follow as merged | 2 | [ ] |
| 78 | Overrides retroactive to all the subtask's logged time | P Semantics | 1.4.0, follow as merged | 2 | [ ] |
| 79 | Stored as overrides only, per entry stamped; reset = entry with tags null + stamp; skipped when empty (file byte-identical to 1.3.x) | P Semantics | 1.4.0, follow as merged | 2 | [ ] |
| 80 | Copies and templates start at "Same as task" (overrides not carried) | P Semantics; P/OA Q5 | 1.4.0, follow as merged | 2 | [ ] |

## Timer

| # | Feature / rule | Source | Status | M | Checked |
|---|---|---|---|---|---|
| 81 | States: Draft→Start; Running→Pause, Stop; Paused→Resume, Stop; Stopped→none (final) | R L191-196; H L104 | shipped 1.3.3 | 1 | [ ] |
| 82 | Each Start/Resume opens a segment, each Pause/Stop closes it | R L198 | shipped 1.3.3 | 1 | [ ] |
| 83 | Times only from the backend clock, stored UTC, whole seconds | R L198, L309 | shipped 1.3.3 | 1 | [ ] |
| 84 | Paused time excluded from total | R L198 | shipped 1.3.3 | 1 | [ ] |
| 85 | Space never touches a timer (Start, Pause, Resume, Stop, form Start, Pause it from here, reminder Pause, Confirm close), pressed or held; Enter on a focused button works | R L200; 68da0cd | shipped 1.3.3 | 1 | [ ] |
| 86 | Cold-launch Resume is refused by backend; opens the empty form, all fields required | R L202 | shipped 1.3.3 | 1 | [ ] |
| 87 | Submitting creates a new task linked to the paused one and starts it; paused one stays paused, can be stopped, shows link to continuation | R L202 | shipped 1.3.3 | 1 | [ ] |
| 88 | Cancel on that form changes nothing, logs no time | R L202 | shipped 1.3.3 | 1 | [ ] |
| 89 | Once a task has started in this launch, Resume works directly | R L202 | shipped 1.3.3 | 1 | [ ] |
| 90 | Warm copies and continuations are their own tasks, linked in details, counted and rounded separately | R L206 | shipped 1.3.3 | 1 | [ ] |
| 91 | One task runs at a time per device; starting/resuming another pauses the running one and says so on screen and on the paused task | R L208 | shipped 1.3.3 | 1 | [ ] |
| 92 | Running task's elapsed clock ticks every second (the only per-second figure) | R L242; LU r1 | shipped 1.3.3 | 1 | [ ] |
| 93 | Timing never stops while hidden in tray; only Quit pauses | R L212, L244; LU r3 | shipped 1.3.3 | 1 | [ ] |

## Tray & lifecycle

| # | Feature / rule | Source | Status | M | Checked |
|---|---|---|---|---|---|
| 94 | Closing with X hides to tray; app and running task keep going | R L212 | shipped 1.3.3 | 3 | [ ] |
| 95 | First hide shows a one-time notice (icon may be in overflow) | R L212 | shipped 1.3.3 | 3 | [ ] |
| 96 | Left-click tray icon shows the window | R L214 | shipped 1.3.3 | 3 | [ ] |
| 97 | Tray menu: Show, current task with elapsed time, Pause or Resume, Quit | R L215 | shipped 1.3.3 | 3 | [ ] |
| 98 | Cold-launch tray Resume opens window with the form, starts nothing | R L215 | shipped 1.3.3 | 3 | [ ] |
| 99 | Tray elapsed is h:mm, no seconds; text rewritten only when minute or data changes; amount recomputed only at billing step, day or rate change | R L216; 2e8b1ed | shipped 1.3.3 | 3 | [ ] |
| 100 | Tray may lag up to a minute after a self-initiated change | R L216 | shipped 1.3.3 | 3 | [ ] |
| 101 | Tooltip shows running task or "No task running" | R L217 | shipped 1.3.3 | 3 | [ ] |
| 102 | Tray icon differs by state: idle clock (hands 10:10), running = same outline, green dial, play triangle | 47a7aff; R L217 (older wording) | shipped 1.3.3 | 3 | [ ] |
| 103 | Hide earnings masks the tray amount | R L394 | shipped 1.3.3 | 3 | [ ] |
| 104 | Only tray Quit exits; running segment closes at that moment, task marked paused | R L219 | shipped 1.3.3 | 3 | [ ] |
| 105 | Single instance: second launch brings up the existing window (stays warm) | R L219, L177 | shipped 1.3.3 | 3 | [ ] |
| 106 | Start with Windows optional, off by default; opens the window at sign-in (not hidden in tray) | R L219 | shipped 1.3.3 | 3 | [ ] |
| 107 | Heartbeat about every 30 s; after crash the open segment closes at last heartbeat, task paused, notice says next start asks every field | R L221 | shipped 1.3.3 | 3 | [ ] |
| 108 | OS shutdown relies on exit event; else heartbeat recovery within ~30 s | R L1079 | shipped 1.3.3 | 3 | [ ] |
| 109 | Quit syncs once and releases the lease | R L90, L111 | shipped 1.3.3 | 3 | [ ] |
| 110 | Read-only PC: tray Resume shows toast "A is using zWorkTracker, so this computer is read-only. Take over from the banner to work here.", raises window, starts nothing | R L1010 | shipped 1.3.3 | 3 | [ ] |
| 111 | Tray elapsed stays current while window hidden | LU r3 | 1.4.0, follow as merged | 3 | [ ] |

## Time away & reminders

| # | Feature / rule | Source | Status | M | Checked |
|---|---|---|---|---|---|
| 112 | While running, detect gaps: sleep, wall-clock jump > 2 min between checks, session lock, idle beyond limit | R L223 | shipped 1.3.3 | 3 | [ ] |
| 113 | Idle limit default 10 min; 0 turns idle off (sleep, jump, lock always noticed) | R L223 | shipped 1.3.3 | 3 | [ ] |
| 114 | Windows lock polled every few seconds; lock ≤ 10 s stays work; if Windows stops answering, gap closes after a minute at last confirmed lock | R L223; cb9d338 | shipped 1.3.3 | 3 | [ ] |
| 115 | Unlock opens a time-away gap whatever idle says | aa31e0d | shipped 1.3.3 | 3 | [ ] |
| 116 | Idle after wake clamped so it can't span the sleep | R L1083 | shipped 1.3.3 | 3 | [ ] |
| 117 | On return, dialog asks: Keep, Drop, Thinking | R L223-230 | shipped 1.3.3 | 3 | [ ] |
| 118 | Unanswered: counts nowhere; Keep: counts; Drop: doesn't; Thinking: counts, reported separately | R L225-230 | shipped 1.3.3 | 3 | [ ] |
| 119 | Raw UTC timestamps never change whatever the answer | R L232 | shipped 1.3.3 | 3 | [ ] |
| 120 | Absence in progress at Pause/Stop/Quit/crash is recorded the same way | R L232 | shipped 1.3.3 | 3 | [ ] |
| 121 | Answering time away on a billed segment requires voiding the summary | R L361, L235 | shipped 1.3.3 | 3 | [ ] |
| 122 | Long-running reminder (default 4 h) and still-running-after-shift reminder; both can be off; snooze doesn't pause | R L236 | shipped 1.3.3 | 3 | [ ] |
| 123 | macOS: no lock signal; locks only via idle | R L1081-1082 | shipped 1.3.3 | 3 | [ ] |

## Timeline

| # | Feature / rule | Source | Status | M | Checked |
|---|---|---|---|---|---|
| 124 | Main screen: period total, then days newest first | R L240 | shipped 1.3.3 | 1, 4 | [ ] |
| 125 | Day axis of the day's real length with blocks in project colours | R L240 | shipped 1.3.3 | 1, 4 | [ ] |
| 126 | Entries grouped into Night 00-06, Morning 06-12, Afternoon 12-18, Evening 18-24 | R L240 | shipped 1.3.3 | 1, 4 | [ ] |
| 127 | Segment crossing midnight split across days and bands | R L240 | shipped 1.3.3 | 1, 4 | [ ] |
| 128 | Day header: Billable and Exact totals plus per-project totals with earnings | R L240 | shipped 1.3.3 | 1, 4 | [ ] |
| 129 | Earnings everywhere show Billable amount, stepping at the increment; no countdown; Exact amount only in Excel Segment details | R L242; 76c33f1 | shipped 1.3.3 | 1, 4 | [ ] |
| 130 | With rounding Up the first increment shows when work starts that task-day; increment Off follows exact time | R L242 | shipped 1.3.3 | 1, 4 | [ ] |
| 131 | A billing step shows at the second it falls due on Timeline, Analysis and the running card | R L244 (uncommitted) | 1.4.0, follow as merged | 1, 4 | [ ] |
| 132 | Click selects an entry (opens nothing); Ctrl/Cmd+click toggles; Shift+click selects a run across days | R L246 | shipped 1.3.3 | 1, 4 | [ ] |
| 133 | Keyboard: Space selects, Ctrl+Space toggles, Shift+Arrow extends, Ctrl+A selects visible, Esc clears | R L246; 626c40e | shipped 1.3.3 | 1, 4 | [ ] |
| 134 | Row keeps separate buttons: task name (shows card) and pencil (edit) | R L246 | shipped 1.3.3 | 1, 4 | [ ] |
| 135 | Entry = one task on one day; selecting selects all rows of that task-day | R L246 | shipped 1.3.3 | 1, 4 | [ ] |
| 136 | Selection bar: count, total Billable (masked by Hide earnings), Export selected, Clear | R L246 | shipped 1.3.3 | 1, 4 | [ ] |
| 137 | Selection survives scrolling and filter changes; bar says when a filter hides some | R L246 | shipped 1.3.3 | 1, 4 | [ ] |
| 138 | Shared task filter for task list and Timeline: text on name+description incl. list items, ignoring case and accents, plus project and tags | R L250 | shipped 1.3.3 | 1, 4 | [ ] |
| 139 | Filter shows match count and clear button; forgotten on quit | R L250 | shipped 1.3.3 | 1, 4 | [ ] |
| 140 | Shift bands as backgrounds; DST markers on axis | R L295, L284 | shipped 1.3.3 | 1, 4 | [ ] |
| 141 | Show both: second axis in client zone | R L276 | shipped 1.3.3 | 1, 4 | [ ] |

## Corrections & manual entries

| # | Feature / rule | Source | Status | M | Checked |
|---|---|---|---|---|---|
| 142 | Add a manual entry for unlogged time | R L319 | shipped 1.3.3 | 2 | [ ] |
| 143 | Correct a segment's start and end | R L319 | shipped 1.3.3 | 2 | [ ] |
| 144 | Void a segment and bring it back | R L319 | shipped 1.3.3 | 2 | [ ] |
| 145 | Each asks a reason where it helps and warns about overlaps | R L319 | shipped 1.3.3 | 2 | [ ] |
| 146 | Non-destructive: raw UTC stays; correction is an overlay; audit trail lists when, what, why | R L319, L666 | shipped 1.3.3 | 2 | [ ] |
| 147 | Running segment can't be corrected; billed time can't until summary voided | R L319 | shipped 1.3.3 | 2 | [ ] |
| 148 | Pause can be marked billable: counts in Billable/earnings, rounded with the task-day, in rounding adjustment, never in Exact, reported alone | R L321 | shipped 1.3.3 | 2 | [ ] |
| 149 | Time covered twice bills once (pause never bills a span already covered) | R L321 | shipped 1.3.3 | 2 | [ ] |
| 150 | "Close at last known time" in task details for a task running on an unreachable PC (after 24 h silence or after Pause it from here) | R L488 | shipped 1.3.3 | 2 | [ ] |

## Tags & templates

| # | Feature / rule | Source | Status | M | Checked |
|---|---|---|---|---|---|
| 151 | Tags have name and colour; four starter tags ship | R L333 | shipped 1.3.3 | 2 | [ ] |
| 152 | "New tag" beside the tag picker opens Settings at Add tag, focused; tags aren't created in the task form | R L335 | shipped 1.3.3 | 2 | [ ] |
| 153 | "Back to task" stays until taken; typed form content preserved | R L335; f09af00 | shipped 1.3.3 | 2 | [ ] |
| 154 | Template stores name, description, project, tags; creates an independent draft with unticked subtasks | R L333 | shipped 1.3.3 | 2 | [ ] |
| 155 | Template preview shows whole first line | 07cf720 | shipped 1.3.3 | 2 | [ ] |
| 156 | Tags analytics count a task's time under each tag (totals may exceed total) | R L145 | shipped 1.3.3 | 2 | [ ] |

## Colours

| # | Feature / rule | Source | Status | M | Checked |
|---|---|---|---|---|---|
| 157 | Shared picker: SV area, hue slider, R/G/B 0-255, hex, kept in step, live preview | R L339 | shipped 1.3.3 | 2 | [ ] |
| 158 | Picker works by keyboard in every theme; curated swatches and recent colours; Home/End on swatches | R L339; c293acc | shipped 1.3.3 | 2 | [ ] |
| 159 | Any colour allowed; chip text picks black/white; gentle warning if close to another project or low contrast | R L339 | shipped 1.3.3 | 2 | [ ] |
| 160 | Backend stores six-digit lowercase hex `#1a2b3c`, refuses anything else | R L339 | shipped 1.3.3 | 2 | [ ] |
| 161 | Exact clash with another active project: replaced by a random distinct colour readable in all 4 themes, notice with "Choose again"; never silent | R L341 | shipped 1.3.3 | 2 | [ ] |
| 162 | Same rule among tags; archived projects don't count; unchanged colour never replaced | R L341 | shipped 1.3.3 | 2 | [ ] |
| 163 | Two PCs picking the same colour offline: both kept, with a warning | R L341 | shipped 1.3.3 | 2 | [ ] |

## Time zones & shifts

| # | Feature / rule | Source | Status | M | Checked |
|---|---|---|---|---|---|
| 164 | Stored times always UTC; zones affect display, grouping, exports only | R L270 | shipped 1.3.3 | 4 | [ ] |
| 165 | Header selector: My time (default), Client time, UTC, Other; always labelled, e.g. "New York, EDT, UTC-4" | R L270 | shipped 1.3.3 | 4 | [ ] |
| 166 | Zone picker: searchable list (keyboard route) and interactive world map; ocean click picks nearest zone; wheel/buttons zoom | R L272 | shipped 1.3.3 | 4 | [ ] |
| 167 | Show both entries read "14:00-16:30 Lisbon / 09:00-11:30 New York"; marker where dates differ | R L276 | shipped 1.3.3 | 4 | [ ] |
| 168 | Client working hours default 09:00-17:00, shaded, overlap reported; count every weekday; end<start runs overnight; 00:00-00:00 = round the clock | R L276, L147 | shipped 1.3.3 | 4 | [ ] |
| 169 | Each segment uses the offset at that instant; app states gap for date viewed ("New York is 4h behind you on this date") | R L282 | shipped 1.3.3 | 4 | [ ] |
| 170 | Clock-change markers on Timeline and Analysis time charts, e.g. "New York clocks went forward 1h, 8 Mar"; none for non-DST zones | R L284 | shipped 1.3.3 | 4 | [ ] |
| 171 | Picker shows next change per zone and warns about weeks when US and EU dates differ | R L285 | shipped 1.3.3 | 4 | [ ] |
| 172 | 23 h / 25 h days drawn at true length; repeated hour labelled "01:00 again"; durations from UTC | R L286 | shipped 1.3.3 | 4 | [ ] |
| 173 | Hour charts bucket by wall-clock hour (repeated hour one bucket, skipped hour empty) | R L287 | shipped 1.3.3 | 4 | [ ] |
| 174 | One tz database for backend and UI, generated at build, shown in Settings > About; rules 2000-2045 | R L289, L1096 | shipped 1.3.3 | 4 | [ ] |
| 175 | Shifts: name, colour, start, end, weekdays, zone, optional unpaid break | R L293 | shipped 1.3.3 | 4 | [ ] |
| 176 | Morning, Afternoon, Night presets, editable | R L293 | shipped 1.3.3 | 4 | [ ] |
| 177 | Shifts crossing midnight belong to their start date; follow own zone through DST (7 h or 9 h) | R L293 | shipped 1.3.3 | 4 | [ ] |
| 178 | Shift changes saved as dated versions with effective date; "Revert" removes one version after confirmation; only version can't be reverted | R L293, L297; 4f913ff | shipped 1.3.3 | 4 | [ ] |
| 179 | Attribution: work cut at shift boundaries; overlap goes to earliest-starting shift, ties by id | R L299 | shipped 1.3.3 | 4 | [ ] |
| 180 | Overtime precedence: shift hours if shifts exist, else daily target | R L299 | shipped 1.3.3 | 4 | [ ] |
| 181 | Shift analysis: scheduled vs worked, outside any shift, late start, early finish, client-hours coverage | R L295 | shipped 1.3.3 | 4 | [ ] |
| 182 | Map boundaries approximate; highlight clipped to country outlines; attribution shown in About | R L1095, L1103 | shipped 1.3.3 | 4 | [ ] |

## Rounding

| # | Feature / rule | Source | Status | M | Checked |
|---|---|---|---|---|---|
| 183 | Default 15 minutes, rounding Up | R L305 | shipped 1.3.3 | 4 | [ ] |
| 184 | Increments Off, 1, 5, 6, 10, 15, 30, 60 or custom minutes; modes Up, Nearest, Down | R L305 | shipped 1.3.3 | 4 | [ ] |
| 185 | Raw UTC never rounded; rounding is a reversible calculation | R L307 | shipped 1.3.3 | 4 | [ ] |
| 186 | Unit = one task on one day in the viewed zone, rounded once; totals are sums of rounded rows | R L308 | shipped 1.3.3 | 4 | [ ] |
| 187 | Up: 1 min → 15; 15:00 → 15:00; 15:01 → 30:00 | R L309 | shipped 1.3.3 | 4 | [ ] |
| 188 | Row with no exact time stays 0 and flagged; Nearest/Down row rounding to 0 kept and flagged | R L310 | shipped 1.3.3 | 4 | [ ] |
| 189 | Nearest rounds exact halves up | R L311 | shipped 1.3.3 | 4 | [ ] |
| 190 | Settings change applies from its effective date; backdating needs confirmation | R L312 | shipped 1.3.3 | 4 | [ ] |
| 191 | Totals show "Billable (rounded)" and "Exact"; Exact same in every zone, Billable may differ | R L315 | shipped 1.3.3 | 4 | [ ] |
| 192 | Rounding adjustment metric = Billable − Exact, billable pauses shown separately | R L144 | shipped 1.3.3 | 4 | [ ] |
| 193 | Band or shift filter hides Billable figures | R L146 | shipped 1.3.3 | 4 | [ ] |

## Analysis & Share of time

| # | Feature / rule | Source | Status | M | Checked |
|---|---|---|---|---|---|
| 194 | Ranges: day, week, month, quarter, year, custom | R L258 | shipped 1.3.3 | 5 | [ ] |
| 195 | Filters: project, task, tag (incl. No tag), band, shift, text search | R L258; H L205 | shipped 1.3.3 | 5 | [ ] |
| 196 | Comparison with previous equal period | R L258 | shipped 1.3.3 | 5 | [ ] |
| 197 | Sections: volume, time of day, projects, tasks, focus and sessions, consistency, goals and money, forecast, shifts, zones | R L258 | shipped 1.3.3 | 5 | [ ] |
| 198 | Every chart has a "View data" table and exact-value tooltips; clicking drills down to tasks/segments | R L258 | shipped 1.3.3 | 5 | [ ] |
| 199 | Heatmaps (weekday×hour, calendar) are one tab stop: arrows move, Enter/Space drill down, focus shows tooltip | c293acc | shipped 1.3.3 | 5 | [ ] |
| 200 | Range and Time basis radio groups respond to arrow, Home, End | c293acc | shipped 1.3.3 | 5 | [ ] |
| 201 | Each metric has an info popover and an entry in the searchable Metrics glossary | R L264 | shipped 1.3.3 | 5 | [ ] |
| 202 | Written Insights generated from numbers, no AI; no productivity score | R L264 | shipped 1.3.3 | 5 | [ ] |
| 203 | Exact-based metrics: time of day, hour histogram, weekday×hour heatmap, year calendar, sessions, longest stretch, pauses, focus ratio, context switches, fragmentation, typical start/finish, working span, shift attainment/coverage | R L138 | shipped 1.3.3 | 5 | [ ] |
| 204 | Billable-based metrics: project totals/shares, hours over time, billable hours/share, budget burn, earnings, effective hourly rate, rounding adjustment, projection | R L139 | shipped 1.3.3 | 5 | [ ] |
| 205 | Pause = same-day gap between consecutive segments of a task (or last segment to Stop); cross-midnight gaps excluded; still-paused open gap not counted | R L143 | shipped 1.3.3 | 5 | [ ] |
| 206 | Forecast: straight line, needs ≥ 7 days, 20% elapsed, 2 active days; budget pace uses last 28 days | R L148 | shipped 1.3.3 | 5 | [ ] |
| 207 | Too little data shows "Not enough data" | R L149 | shipped 1.3.3 | 5 | [ ] |
| 208 | Share of time: donut with legend beside (below when narrow), each row like `16h 50m (47%)` | R L262 | shipped 1.3.3 | 5 | [ ] |
| 209 | Legend hours follow Billable/Exact basis and sum to centre total | R L262 | shipped 1.3.3 | 5 | [ ] |
| 210 | Level switch: Projects / Tasks / Subtasks / Tags | R L262 | shipped 1.3.3 | 5 | [ ] |
| 211 | Default level follows filter: Projects (no/several projects), Tasks (one project), Subtasks (one task); Tags only by hand | R L262 | shipped 1.3.3 | 5 | [ ] |
| 212 | Once picked, a level stays however the filter changes; only the user switches it | R L262; fd51d79 | 1.4.0, follow as merged | 5 | [ ] |
| 213 | Picked Subtasks with no subtask time: shows another level meanwhile, returns when subtask time returns | R L262; fd51d79 | 1.4.0, follow as merged | 5 | [ ] |
| 214 | Row names only what the filter leaves open; tooltip and accessible name give the full path | R L262 | shipped 1.3.3 | 5 | [ ] |
| 215 | Time on no subtask is its own row `<Task> (no subtask)`; Subtasks level off while nothing logged on one | R L262 | shipped 1.3.3 | 5 | [ ] |
| 216 | Tags level: each exact tag combination is a row (`Design + Client`), untagged `No tag`, each task counted once | R L262 | shipped 1.3.3 | 5 | [ ] |
| 217 | Tag filter: none ticked = all; ticked = tasks with any; "No tag" alone = untagged only, with tags = adds untagged | R L262; H L211 | shipped 1.3.3 | 5 | [ ] |
| 218 | > 9 rows: top 8 plus `Other (N tasks)` / `Other (N subtasks)` / `Other (N combinations)`; View data lists all | R L262 | shipped 1.3.3 | 5 | [ ] |
| 219 | Percentages whole numbers by largest remainder summing to 100; share rounding to 0 reads `<1%` | R L262; 68da0cd | shipped 1.3.3 | 5 | [ ] |
| 220 | Legend shows hours, so Hide earnings doesn't change it | R L262 | shipped 1.3.3 | 5 | [ ] |
| 221 | Share of time Tags, tag filter, tag metrics and Excel Analytics count subtask time under its effective tags (per piece of time) | P Done-when 3, Semantics | 1.4.0, follow as merged | 5 | [ ] |
| 222 | Excel tags sheet stays overlapping, one row per tag, counting segments | P Semantics | 1.4.0, follow as merged | 5 | [ ] |
| 223 | Rust and TS analysis held equal by parity fixtures (analytics-parity.json; subtask-tags-parity.json) | R L407; P | shipped 1.3.3 (subtask part 1.4.0) | 5 | [ ] |

## Billing, earnings, exports

| # | Feature / rule | Source | Status | M | Checked |
|---|---|---|---|---|---|
| 224 | Billing summaries labelled "not an invoice and not a tax document" on page and in exports | R L353 | shipped 1.3.3 | 5 | [ ] |
| 225 | Pick project + period → unbilled closed time per task and day | R L355 | shipped 1.3.3 | 5 | [ ] |
| 226 | "Create summary" freezes it with reference like `A-001` and marks time billed; tasks show billed/unbilled | R L355 | shipped 1.3.3 | 5 | [ ] |
| 227 | Copy or export summary as CSV; with Hide earnings neither holds an amount | R L355; aa31e0d | shipped 1.3.3 | 5 | [ ] |
| 228 | Voiding returns time to unbilled; its number is never reused; repeated void writes one audit entry | R L355; aa31e0d | shipped 1.3.3 | 5 | [ ] |
| 229 | Per-device editable reference prefix; numbers count up per prefix across devices; warns on shared prefix or repeated reference | R L359 | shipped 1.3.3 | 5 | [ ] |
| 230 | Preview flags unanswered time away (left out); time belongs to its segment's start day | R L361 | shipped 1.3.3 | 5 | [ ] |
| 231 | Task-day billed in parts rounds once: later summary bills rounded day total minus live summaries (20+20 at 15 Up → 30 then 15); preview flags such rows | R L363 | shipped 1.3.3 | 5 | [ ] |
| 232 | Voiding takes its share off the day; day rounds under the rule in force now; older summaries keep issued amounts | R L365-367 | shipped 1.3.3 | 5 | [ ] |
| 233 | Summary from another zone shared out over this zone's days; rate change in a day bills each row at its own rates | R L368-370 | shipped 1.3.3 | 5 | [ ] |
| 234 | Summary amounts stored at creation, read as stored, never change | R L372 | shipped 1.3.3 | 5 | [ ] |
| 235 | Unbilled time per project counts a running task up to now and moves with live refresh | R L355 (uncommitted); LU r2 | 1.4.0, follow as merged | 5 | [ ] |
| 236 | Amounts exact (rate cents × seconds); row rounds half up to cents; total rounds sum of exact once; PDF, summaries, Excel, Analysis agree | R L390 | shipped 1.3.3 | 5 | [ ] |
| 237 | Where visible rows differ from total by a cent, PDF, summary CSV/text, Billing tab and Excel Earnings title say so | R L390 | shipped 1.3.3 | 5 | [ ] |
| 238 | Rate change within a task-day: rounding difference goes to last rate | R L390 | shipped 1.3.3 | 5 | [ ] |
| 239 | EUR and USD never added; totals side by side; no exchange rates | R L392 | shipped 1.3.3 | 5 | [ ] |
| 240 | Hide earnings in header masks amounts everywhere incl. tray, % of fixed fee, saved fee/price in project editor ("Change" reveals to edit); remembered per device | R L394 | shipped 1.3.3 | 5 | [ ] |
| 241 | Timesheet PDF: period, with or without prices, client zone, optional PNG logo, built locally by Rust | R L376 | shipped 1.3.3 | 5 | [ ] |
| 242 | PDF font Noto Sans (bundled), per-document subset, selectable text; Latin/Greek/Cyrillic print | R L376; aad5c4b, de4d234 | shipped 1.3.3 | 5 | [ ] |
| 243 | CJK, Hebrew, Arabic, emoji print as empty boxes (never `?`); export warns on screen and on timesheet; copy still gives text | R L376, L1087 | shipped 1.3.3 | 5 | [ ] |
| 244 | Quote helper: name + tags → range and count from comparable stopped tasks; labelled estimate; low sample shown | R L380 | shipped 1.3.3 | 5 | [ ] |
| 245 | Export dialog: format, day zone, your zone, client zone, include earnings | R L398 | shipped 1.3.3 | 5 | [ ] |
| 246 | Earnings off = no price data anywhere in the file | R L398 | shipped 1.3.3 | 5 | [ ] |
| 247 | Excel sheet 1 "Labor by project": exactly `Project / Day / Time / Task name / Subtasks`, sorted project, day, start; Time real `[h]:mm:ss`; Day real date; subtotals and grand total formulas with cached values; title states zone and increment; filter keeps subtotal row | R L402 | shipped 1.3.3 | 5 | [ ] |
| 248 | Sheet 2 "Segment details": raw UTC start/end, your time, client time, Exact, Shift, and (if included) rate, currency, exact amount | R L403 | shipped 1.3.3 | 5 | [ ] |
| 249 | Sheet 3 "Earnings" (if included): per project and currency with per-currency totals | R L404 | shipped 1.3.3 | 5 | [ ] |
| 250 | Sheet 4 "Analytics": KPI summary, Bands, Projects, Weekdays, Shifts, earnings; follows Analysis period/filters while that tab is open, else all history up to 1830 days; titles say when filtered | R L405 | shipped 1.3.3 | 5 | [ ] |
| 251 | Export scope defaults to "Selected" when entries are selected, else "All in period" | R L407 | shipped 1.3.3 | 5 | [ ] |
| 252 | Selected export: backend gets only task ids + days, validates, refuses empty or no-time entries, recomputes per task-day; "About this selection" sheet | R L407; a81c1b5, 7ead150 | shipped 1.3.3 | 5 | [ ] |
| 253 | Selected export differences: no row for left-out days, shift figures for selected days only, no projected total, etc.; CSV ends with `selection` row; PDF prints a line | R L407 | shipped 1.3.3 | 5 | [ ] |
| 254 | Selection clears after export only if asked | R L407 | shipped 1.3.3 | 5 | [ ] |
| 255 | CSV `record_type` rows: `segment` (raw UTC, zone times, exact) and `task_day` (billable) | R L413 | shipped 1.3.3 | 5 | [ ] |
| 256 | CSV user text protected from formula injection; Excel writes plain strings | R L413 | shipped 1.3.3 | 5 | [ ] |
| 257 | CSV and PDF print no subtasks (and no tags) | R L407; P Repo facts | shipped 1.3.3 | 5 | [ ] |
| 258 | Save dialogs run from the backend | R L671 | shipped 1.3.3 | 5 | [ ] |

## Settings & themes

| # | Feature / rule | Source | Status | M | Checked |
|---|---|---|---|---|---|
| 259 | Themes Light (default), Solarized Light, Solarized Dark, Dark; per device; OS theme not followed | R L417 | shipped 1.3.3 | 6 | [ ] |
| 260 | Project colours unchanged across themes | R L417 | shipped 1.3.3 | 6 | [ ] |
| 261 | Theme choice is a radio group with arrow/Home/End | c293acc | shipped 1.3.3 | 6 | [ ] |
| 262 | Window opaque (Mica switch removed in 1.3.3) | R L1075; 68da0cd | shipped 1.3.3 | 6 | [ ] |
| 263 | Settings > About shows app version, tz database version, map data attribution | R L82, L289, L1108 | shipped 1.3.3 | 6 | [ ] |
| 264 | Settings sections named in docs: Sync between devices, Versions, Backups, About, Appearance, tags, shifts, zones, idle limit, reminders, rounding, Start with Windows | R passim | shipped 1.3.3 | 6 | [ ] |
| 265 | Device-local prefs (theme, Hide earnings, last currency) don't sync; shared settings do | R L96, L83 | shipped 1.3.3 | 6 | [ ] |

## Data store, backups, versions

| # | Feature / rule | Source | Status | M | Checked |
|---|---|---|---|---|---|
| 266 | Data folder: Windows `%APPDATA%\com.rustcircle.zworktracker\`; macOS `~/Library/Application Support/com.rustcircle.zworktracker/` | R L554-556 | shipped 1.3.3 | 6 | [ ] |
| 267 | Main store `z-work-data.db` SQLite (bundled 3.54.1), WAL, `synchronous=FULL`, exclusive lock, no `-shm`, `quick_check` and application id checked at open | R L557, L662, L675 | shipped 1.3.3 | 6 | [ ] |
| 268 | Tables `schema_version`, `meta` (`local`, heartbeat `beat`), `records(kind,id,json)` holding the same JSON as sync files | R L662 | shipped 1.3.3 | 6 | [ ] |
| 269 | Every change one transaction, on disk before continuing; save writes only changed records | R L566, L662 | shipped 1.3.3 | 6 | [ ] |
| 270 | Save validates before writing and reads back each record; failure writes nothing and next save rewrites all | R L674 | shipped 1.3.3 | 6 | [ ] |
| 271 | Damaged DB set aside as `z-work-data.corrupt-<time>.db` (+ `-wal`); data back from sync folder, else newest readable backup, notice names it | R L563, L566, L1041 | shipped 1.3.3 | 6 | [ ] |
| 272 | DB from a newer app version refused and untouched | R L566 | shipped 1.3.3 | 6 | [ ] |
| 273 | Schema migrations forward only, each a transaction, pre-migration copy `z-work-data.v<n>.db`; DB_SCHEMA_VERSION 1, data SCHEMA_VERSION 2 | R L561, L664 | shipped 1.3.3 | 6 | [ ] |
| 274 | Old v1 data (e.g. old backup) still migrates | R L664 | shipped 1.3.3 | 6 | [ ] |
| 275 | Drafts file `z-work-drafts.json`: description edits stashed ~300 ms after change, restored once at next launch with notice; damaged kept as `.corrupt-<time>.json`; unreadable left for next launch | R L558, L1091; ac40a7e | shipped 1.3.3 | 6 | [ ] |
| 276 | Kept words `z-work-kept-words.json`: words the store wouldn't take, shown in a notice each launch until dismissed | R L559, L1091 | shipped 1.3.3 | 6 | [ ] |
| 277 | Daily dated JSON backup in `backups/`, keep newest 30 by default (1-365) | R L384 | shipped 1.3.3 | 6 | [ ] |
| 278 | Restore a backup: safety copy first (newest 5 kept), running task paused, next start cold | R L384 | shipped 1.3.3 | 6 | [ ] |
| 279 | Restore keeps device-local: prefs, sync folders, reference prefix, used numbers, audit trail (records the restore) | R L384; aa31e0d | shipped 1.3.3 | 6 | [ ] |
| 280 | Backup's running task closes at that backup's last known moment, no crash notice | R L384; aa31e0d | shipped 1.3.3 | 6 | [ ] |
| 281 | With sync on, newer folder records merge back after restore; app warns | R L384, L1085 | shipped 1.3.3 | 6 | [ ] |
| 282 | Backup from before a restored version refused while folders connected | R L384 | shipped 1.3.3 | 6 | [ ] |
| 283 | 1.3.1 backups and `z-work-data-legacy-1.3.1.json` listed, restorable, never removed | R L386 | shipped 1.3.3 | 6 | [ ] |
| 284 | No daily backup and no cloud write until data has arrived from the sync folder after a first 1.3.2 start | R L91, L386 | shipped 1.3.3 | 6 | [ ] |
| 285 | First 1.3.2 start reads only device id/name, sync folders, setup progress, backup count from old JSON; never changes old files | R L87, L564 | shipped 1.3.3 | 6 | [ ] |
| 286 | Versions in `zWorkTracker/versions/<YYYY-MM>/<UTC>--<8 id chars>--<reason>.json.gz` with `<name>.changes.txt` readable UTC record | R L514 | shipped 1.3.3 | 6 | [ ] |
| 287 | Version saved by the lease holder after changes at most hourly; on close after changes (unless one in last 15 min); "Save a version now"; after take over; around a restore | R L516 | shipped 1.3.3 | 6 | [ ] |
| 288 | Retention: all 7 days, newest per UTC day to 60 days, newest per UTC month forever; restore/take-over versions kept a year ("Kept for a year"); only holder thins by file name | R L518 | shipped 1.3.3 | 6 | [ ] |
| 289 | Over 1 GB of versions: page says so, nothing deleted | R L518 | shipped 1.3.3 | 6 | [ ] |
| 290 | `versions-base.json.gz` local base for the next change record | R L562 | shipped 1.3.3 | 6 | [ ] |
| 291 | Restore a version: "What changed", "Restore…", two-step (counts compare, then consequences), copy of today saved first locally and as version, running task paused | R L123, L144-145 | shipped 1.3.3 | 6 | [ ] |
| 292 | Restored data replaces data on every syncing PC at next sync, each keeps a copy as a version; epoch fence prevents merge-away | R L522; R L633 epoch.rs | shipped 1.3.3 | 6 | [ ] |
| 293 | Old-version PCs after restore skipped: "Skipped *name*: it holds data from before a restored version. Update and sync that device." | R L524 | shipped 1.3.3 | 6 | [ ] |
| 294 | PC with own data connecting to a restored folder asked: "Follow the restored data" or "Keep this computer's data too" | R L525 | shipped 1.3.3 | 6 | [ ] |
| 295 | "Look in another folder…" lists versions in a disconnected folder; versions not copied on folder change | R L526 | shipped 1.3.3 | 6 | [ ] |
| 296 | Store build backups live in its private folder and go on uninstall; Backups page says so | R L56, L384 | shipped 1.3.3 | 6 | [ ] |
| 297 | Uninstaller asks "Also delete my time data" (unticked default); never touches cloud folders | R L49 | shipped 1.3.3 | 6 | [ ] |

## Sync & lease (one computer at a time, two folders, HLC)

| # | Feature / rule | Source | Status | M | Checked |
|---|---|---|---|---|---|
| 298 | Local first, works offline; sync via OneDrive/Google Drive desktop folders; app makes no network calls | R L421 | shipped 1.3.3 | 6 | [ ] |
| 299 | Detected folders listed with checkboxes plus "Choose another folder"; none on silently; explains desktop client needed if none detected | R L423 | shipped 1.3.3 | 6 | [ ] |
| 300 | Google Drive detection: DriveFS registry first, then drive letters and common folder names; only common "My Drive" language variants | R L1080, L1037 | shipped 1.3.3 | 6 | [ ] |
| 301 | Sync times: launch, ~2 s after major actions, ~15 s after text edits, every 30 min (incl. tray, running), real quit; backoff 30 s, 1, 2 min ... up to 15 min | R L427 | shipped 1.3.3 | 6 | [ ] |
| 302 | Per-folder status e.g. "Written to OneDrive folder, 2 min ago" (claims the write only); failures isolated, never block timing | R L429 | shipped 1.3.3 | 6 | [ ] |
| 303 | Each PC writes only `<folder>/zWorkTracker/devices/<device-uuid>.json`, reads others | R L431 | shipped 1.3.3 | 6 | [ ] |
| 304 | Merge: projects per field group (identity, billing, budget, archive), latest stamp wins, ties by device id | R L435 | shipped 1.3.3 | 6 | [ ] |
| 305 | Merge: settings, shifts, rates, increments latest per record; tasks per field; Stopped final and wins | R L436-439 | shipped 1.3.3 | 6 | [ ] |
| 306 | Merge: segments by id, closed beats open; tombstones; draft deleted on one PC but started on another stays | R L440-441 | shipped 1.3.3 | 6 | [ ] |
| 307 | Merge is idempotent, commutative, associative (property tests) | R L666 | shipped 1.3.3 | 6 | [ ] |
| 308 | Conflict copies and unknown files ignored, never deleted; damaged/newer-version files skipped with warning, never overwritten; bad record skipped alone and named | R L442 | shipped 1.3.3 | 6 | [ ] |
| 309 | OneDrive Files On-Demand placeholder retried, not treated as damage | R L443 | shipped 1.3.3 | 6 | [ ] |
| 310 | Sync files untrusted: size/count limits, schema and plausibility checks, per-folder worker with time limit | R L460, L673 | shipped 1.3.3 | 6 | [ ] |
| 311 | Two PCs running at once: both kept, warning stays, neither alters the other's segments; remote open segment counts to its last heartbeat | R L444 | shipped 1.3.3 | 6 | [ ] |
| 312 | "Pause it from here": synced audited request; time counts to the request; owner closes its own segment at next sync, marked paused by request, shows notice + system notification if hidden | R L444; 7d117bb, 2678d49 | shipped 1.3.3 | 6 | [ ] |
| 313 | Editor CAS: won't save over a newer synced value; notice in editor or banner; "Done" (ignored in first 1 s, double-click guarded) saves over; "Discard my words" keeps newer | R L438, L1089-1090; c734828, 7210b9c | shipped 1.3.3 | 6 | [ ] |
| 314 | Failing save retried 5× over ~1 min, then on next change, hide, quit or reopen; banner until it lands; refused saves not auto-retried, banner gives reason | R L1093 | shipped 1.3.3 | 6 | [ ] |
| 315 | Unsaved edits to a task deleted elsewhere shown as plain text in a notice | R L1094 | shipped 1.3.3 | 6 | [ ] |
| 316 | Change folder: preview source/destination counts; merge old and new, copy others, write own, read back and compare counts + checksum before switching; old folder never touched; "Retry copy" | R L448-454 | shipped 1.3.3 | 6 | [ ] |
| 317 | Recovers what only its own sync file holds after restoring an older local file | R L462 | shipped 1.3.3 | 6 | [ ] |
| 318 | Warning in Settings: "Only one computer at a time can make changes. This is a best effort: ..." | R L466 | shipped 1.3.3 | 6 | [ ] |
| 319 | Lease records `zWorkTracker/lease/<device-uuid>.json` in every folder; highest claim number wins | R L468 | shipped 1.3.3 | 6 | [ ] |
| 320 | Claimed at first change (not at launch); kept while a task runs; released after 15 min idle, on quit, or "Release now" (refused while a task runs) | R L468 | shipped 1.3.3 | 6 | [ ] |
| 321 | Holder rewrites its record ~every 2 min; all PCs read ~every minute and on "Sync now"/"Check again" | R L468 | shipped 1.3.3 | 6 | [ ] |
| 322 | Holder sees "This computer is the one making changes." with Release now | R L75 | shipped 1.3.3 | 6 | [ ] |
| 323 | Read-only banner "Read-only. *name* is using zWorkTracker (last seen *time* ago)..." with Check again; Take over… only after 15 min silence | R L76 | shipped 1.3.3 | 6 | [ ] |
| 324 | Inactive banner "... looks inactive: nothing from it for *time*..." with Check again, Take over… | R L77 | shipped 1.3.3 | 6 | [ ] |
| 325 | No folder readable, holder seen within 24 h: banner with Check again, "Work here anyway…" | R L78 | shipped 1.3.3 | 6 | [ ] |
| 326 | Banner notes when the other computer's clock seems to differ | R L81 | shipped 1.3.3 | 6 | [ ] |
| 327 | Read-only allows: view all, filter, export, device prefs, zone switches, sync, Pause it from here; forbids every edit, timer, correction, summary, shared setting, restore, version save/restore | R L83 | shipped 1.3.3 | 6 | [ ] |
| 328 | Editor while read-only: "Not saved: *name* is using zWorkTracker. Take over to save." | R L83 | shipped 1.3.3 | 6 | [ ] |
| 329 | Take over asks twice ("Take over from *name*?" → "Are you sure?"); only its Take over acts; re-checks holder before acting | R L85 | shipped 1.3.3 | 6 | [ ] |
| 330 | After take over: sync at once, save a version, close other PC's running time at last known moment; other PC shows "LAPTOP took over, so this computer is now read-only..." | R L87 | shipped 1.3.3 | 6 | [ ] |
| 331 | Other PC's real recorded end replaces the take-over end unless already billed | R L87 | shipped 1.3.3 | 6 | [ ] |
| 332 | Two folders (Main and Second copy): each a full copy, read both, merge once, write both; lease to both; removing Main promotes the other | R L492 | shipped 1.3.3 | 6 | [ ] |
| 333 | Folder not written for 30 min: top warning names it, error, and other folder's state; folder > 1 h behind the other says so | R L494 | shipped 1.3.3 | 6 | [ ] |
| 334 | "Remove" stops using a folder, leaves files; removing the last folder (Store) warns | R L494, L56 | shipped 1.3.3 | 6 | [ ] |
| 335 | HLC stamps: edit stamped ≥ 1 s after newest seen change, capped at 1 day past own clock; peers refuse records > 2 days ahead | R L99; 3777188 | shipped 1.3.3 | 6 | [ ] |
| 336 | Segment times, heartbeats and version dates use the wall clock, not stamps | R L99 | shipped 1.3.3 | 6 | [ ] |
| 337 | Clock-behind warning when ≥ 2 min behind newest seen change; not dismissible; two wordings (other PC / this PC); gap "5 min", "1 h 30 min", "3 days"; ≥ 1 day variant; clears by itself | R L101-109 | shipped 1.3.3 | 6 | [ ] |
| 338 | Sync file format unchanged since 1.3.1 (byte-for-byte round-trip tests); keep SCHEMA_VERSION 2 | 3777188; P Repo facts | shipped 1.3.3 | 6 | [ ] |
| 339 | Catch-all for unknown fields on Task and Segment, so a newer version's fields survive | P/OA Q2 | 1.4.0, follow as merged | 6 | [ ] |
| 340 | Subtask-tag overrides survive a round trip through a 1.3.3 device (stamp rule) | P Done-when 2, C1 | 1.4.0, follow as merged | 6 | [ ] |

## Live updates/reactivity

| # | Feature / rule | Source | Status | M | Checked |
|---|---|---|---|---|---|
| 341 | Views refresh immediately after every change the user makes | 7107403 | shipped 1.3.3 | 1 | [ ] |
| 342 | Shipped 1.3.3: only components showing time follow the clock; clock stops while hidden; Analysis redraws when a figure can change | 0920de8, d6d6825, 6ce45ef | shipped 1.3.3 | 1 | [ ] |
| 343 | Every view recomputes when opened (tab switch, window shown from tray/minimised) | LU r2; R L244 (uncommitted) | 1.4.0, follow as merged | 1 | [ ] |
| 344 | Final rule: no polling; all views derive reactively from one store plus a shared 1 s running-time signal (active only while a task runs) | LU Change 2 | 1.4.0, follow as merged | 1 | [ ] |
| 345 | Views re-render only when their output changes (minute-level figures once a minute) | LU Change 2 | 1.4.0, follow as merged | 1 | [ ] |
| 346 | Covered views: Analysis (all charts incl. Share of time centre, cards, insights, comparison, View data), Timeline totals/axes/headers/earnings, Billing unbilled, budget 80/100% warnings, task cards and subtask times | LU r2 | 1.4.0, follow as merged | 1 | [ ] |
| 347 | Hidden/minimised: no recomputation; on show everything catches up at once; timing never stops | LU r3; R L244 | 1.4.0, follow as merged | 1 | [ ] |
| 348 | User events (start/pause/resume/stop, corrections, added time, tag/subtask changes) and sync changes show immediately everywhere; event recorded first, then shown | LU r4, r7, r8 | 1.4.0, follow as merged | 1 | [ ] |
| 349 | A refresh never resets chart level, filters, selection, scroll, open View data tables or form input; created summaries stay frozen | LU r5; R L244 | 1.4.0, follow as merged | 1 | [ ] |
| 350 | No view holds a mount-time snapshot | LU r9 | 1.4.0, follow as merged | 1 | [ ] |
| 351 | Granular subscriptions: each component subscribes to its slice (entity id, range, tags, settings, running clock); backend emits typed change events after every write and sync merge; unsubscribe on unmount | LU Change 3 | 1.4.0, follow as merged | 1 | [ ] |
| 352 | Tests: per-view hooks, fake clock (views advance, 1 s timer, hide/show catch-up, no recompute hidden), render-count isolation | LU r6, Change 3 | 1.4.0, follow as merged | 1 | [ ] |

## Windows integration & Store build

| # | Feature / rule | Source | Status | M | Checked |
|---|---|---|---|---|---|
| 353 | Targets Windows 10 2004+ (`MinVersion 10.0.19041.0`) and 11, x64 | R L805; scripts/msix-manifest.mjs L451 | shipped 1.3.3 | 7 | [ ] |
| 354 | Installer per-user in `%LOCALAPPDATA%\zWorkTracker`, no admin; Start Menu shortcut, optional desktop shortcut, launch on finish | R L35 | shipped 1.3.3 | 7 | [ ] |
| 355 | Installer upgrades in place, asks a running copy to quit cleanly (`--quit`), keeps data | R L81 | shipped 1.3.3 | 7 | [ ] |
| 356 | Uninstall asks running copy to quit; removes files, shortcuts, registry, Start with Windows entry | R L47 | shipped 1.3.3 | 7 | [ ] |
| 357 | Installer build Start with Windows via `HKCU\...\CurrentVersion\Run` | R L813, L1003 | shipped 1.3.3 | 7 | [ ] |
| 358 | Store build detected by package identity at launch | R L53 | shipped 1.3.3 | 7 | [ ] |
| 359 | Store build Start with Windows via StartupTask `zWorkTrackerStartup` (declared Enabled=false) | R L57; msix-manifest.mjs L461 | shipped 1.3.3 | 7 | [ ] |
| 360 | Disabled in Windows: switch greyed, "You turned this off in Windows, so only Windows can turn it back on: Settings > Apps > Startup." + "Open Startup settings" | R L57 | shipped 1.3.3 | 7 | [ ] |
| 361 | Policy-blocked: "A policy on this computer doesn't allow this, or the device doesn't support it." greyed | R L997 | shipped 1.3.3 | 7 | [ ] |
| 362 | Store notifications as toasts under package AUMID; fallback to installer route | R L58, L815 | shipped 1.3.3 | 7 | [ ] |
| 363 | Store build persistent warning until a folder is connected: "This copy of zWorkTracker keeps its data in a private Windows folder..." | R L56 | shipped 1.3.3 | 7 | [ ] |
| 364 | Store data likely `%LOCALAPPDATA%\Packages\PedroMiguelRodriguesJorge.zWorkTracker_54k2cbaqgvhqp\LocalCache\Roaming\com.rustcircle.zworktracker\` (unconfirmed) | R L125 | shipped 1.3.3 | 7 | [ ] |
| 365 | Only restricted capability `runFullTrust`; no `unvirtualizedResources` | R L812 | shipped 1.3.3 | 7 | [ ] |
| 366 | MSIX version = app version + `.0` (1.3.3 → 1.3.3.0) | R L805 | shipped 1.3.3 | 7 | [ ] |
| 367 | Store and test identities in one script; test package family `rustcircle.zWorkTracker.Test_ym7m1j5c19s9r` has its own data folder | R L769 | shipped 1.3.3 | 7 | [ ] |
| 368 | 60 MSIX images: StoreLogo, Square44x44Logo, Square150x150Logo at 5 scales + 14 target sizes with `_altform-unplated` and `_altform-lightunplated`; needs `resources.pri` | R L777 | shipped 1.3.3 | 7 | [ ] |
| 369 | MSIX built 1.4.0.0 for 1.4.0 | P Done-when 4 | 1.4.0, follow as merged | 7 | [ ] |
| 370 | Windows session/package queries isolated in one crate (only unsafe code) | R L243, L676 | shipped 1.3.3 | 7 | [ ] |
| 371 | Self-signed code signing route (SHA-256 + RFC 3161 timestamp) for own PCs | R L817 | shipped 1.3.3 | 7 | [ ] |

## Privacy & notices

| # | Feature / rule | Source | Status | M | Checked |
|---|---|---|---|---|---|
| 372 | No network calls, no telemetry; build contains no HTTP/TLS library, enforced by a test | R L7, L568; S | shipped 1.3.3 | 7 | [ ] |
| 373 | Data leaves only through the user's own sync folder: `devices/`, `lease/`, `versions/`, unencrypted, incl. prices | R L568 | shipped 1.3.3 | 7 | [ ] |
| 374 | No account, no sign-in (MSA app ID unused) | S Store identity, Description | shipped 1.3.3 | 7 | [ ] |
| 375 | Notice: uninstalling (Store) removes local data; connect a cloud folder | S Notices 1; site/index.html | shipped 1.3.3 | 7 | [ ] |
| 376 | Notice: one device at a time | S Notices 2; R L1064 | shipped 1.3.3 | 7 | [ ] |
| 377 | Notice: sync files not encrypted; don't use a folder shared with others | S Notices 3; R L460 | shipped 1.3.3 | 7 | [ ] |
| 378 | Notice: not an invoice or tax tool | S Notices 4; R L353 | shipped 1.3.3 | 7 | [ ] |
| 379 | Notice: cloud sync needs the provider's desktop app | S Notices 5 | shipped 1.3.3 | 7 | [ ] |
| 380 | Privacy policy text (site/privacy.html), support pmrj@rustcircle.com | S; site/privacy.html | shipped 1.3.3 | 7 | [ ] |
| 381 | Third-party data licences shown (tz-boundary ODbL, Natural Earth, IANA) in About and notices file | R L1103 | shipped 1.3.3 | 7 | [ ] |

## Accessibility/keyboard

| # | Feature / rule | Source | Status | M | Checked |
|---|---|---|---|---|---|
| 382 | Space never acts on timer buttons; Enter does | R L200 | shipped 1.3.3 | 1 (every screen) | [ ] |
| 383 | Timeline selection keys (Space, Ctrl+Space, Shift+Arrow, Ctrl+A, Esc) | R L246 | shipped 1.3.3 | 1 (every screen) | [ ] |
| 384 | Radio groups (theme, shift colours, Range, Time basis) arrow/Home/End, one tab stop | c293acc | shipped 1.3.3 | 1 (every screen) | [ ] |
| 385 | Heatmaps one tab stop with arrows, Enter/Space drill-down, focus tooltip | c293acc | shipped 1.3.3 | 1 (every screen) | [ ] |
| 386 | Zone list is the keyboard route for zones | R L272 | shipped 1.3.3 | 1 (every screen) | [ ] |
| 387 | Colour picker fully keyboard-operable | R L339 | shipped 1.3.3 | 1 (every screen) | [ ] |
| 388 | Share of time slices have full-path accessible names | R L262 | shipped 1.3.3 | 1 (every screen) | [ ] |
| 389 | Task delete deliberately not keyboard-operable (by design) | R L1086 | shipped 1.3.3 | 1 (every screen) | [ ] |
| 390 | Subtask tag popover keyboard model | P UI baseline | 1.4.0, follow as merged | 1 (every screen) | [ ] |
| 391 | Dialog headings take focus (wizard, voided summary) | b4739c3, 2697419 | shipped 1.3.3 | 1 (every screen) | [ ] |

## Error/runtime checks

| # | Feature / rule | Source | Status | M | Checked |
|---|---|---|---|---|---|
| 392 | App checks WebView2 at every launch; if missing/off: message titled "zWorkTracker", "zWorkTracker needs the Microsoft Edge WebView2 Runtime, and it wasn't found or is turned off on this computer", link, "Your data is safe.", buttons Open download page / Close; exits with no window or tray | R L44 | 1.x-only, needs a 2.0 equivalent | 3 | [ ] |
| 393 | Window creation failure shows the same message with "Details: ..." | R L45, L990 | 1.x-only, needs a 2.0 equivalent | 3 | [ ] |
| 394 | Installer pre-checks WebView2, offers "Open the download page now?"; silent `/S` asks nothing | R L43 | 1.x-only, needs a 2.0 equivalent | 3 | [ ] |
| 395 | 2.0 equivalent: a friendly native startup check (e.g. GPU/renderer or data-folder failure) with plain message, "Your data is safe.", no blank window | task brief; R L41 | 1.x-only, needs a 2.0 equivalent | 3 | [ ] |
| 396 | Newer-version device file: "Update this device" warning, never overwritten | R L664 | shipped 1.3.3 | 3 | [ ] |
| 397 | Folder that stops responding: reported, no further work started on it until the stuck call returns | R L462 | shipped 1.3.3 | 3 | [ ] |
| 398 | Sync folder unreadable on first 1.3.2 start: warning to check the folder is synced | R L91 | shipped 1.3.3 | 3 | [ ] |
| 399 | Helper-thread panic doesn't take the app down (no `panic=abort`) | R L723 | shipped 1.3.3 | 3 | [ ] |
| 400 | Strict CSP, no remote content, minimal capabilities, all command inputs validated in Rust, descriptions allow-listed | R L670-672 | 1.x-only, needs a 2.0 equivalent | 3 | [ ] |
| 401 | UI receives a full snapshot after every change (to be replaced by typed events) | R L221; LU Change 3 | 1.x-only, needs a 2.0 equivalent | 3 | [ ] |
| 402 | Window appears after first paint | 310d911 | 1.x-only, needs a 2.0 equivalent | 3 | [ ] |

## Store identity

Verified verbatim:

| Value | Text | Defined in |
|---|---|---|
| Identity Name | `PedroMiguelRodriguesJorge.zWorkTracker` | W/scripts/msix-manifest.mjs L55 (`PACKAGES.store.identity.name`); also W/STORE_LISTING_NOTICE.md L22 |
| Publisher | `CN=1DEF8D7B-1D68-4604-8047-048DC92CF3A9` | W/scripts/msix-manifest.mjs L56; W/STORE_LISTING_NOTICE.md L23 |
| PublisherDisplayName | `Pedro Miguel Rodrigues Jorge` | W/scripts/msix-manifest.mjs L57; STORE_LISTING_NOTICE.md L24 |
| Package Family Name | `PedroMiguelRodriguesJorge.zWorkTracker_54k2cbaqgvhqp` | W/scripts/msix-vm.mjs L89 (`EXPECTED_FAMILY.store`); derived and tested in W/scripts/msix-manifest.test.ts L201, L218; STORE_LISTING_NOTICE.md L25 |
| Store ID | `9P9RM9G0QBRC` | W/STORE_LISTING_NOTICE.md L19-21 only as data; used in links W/site/index.html L45 and W/site/howto.html L74 (no code/manifest holds it) |
| Store page | `https://apps.microsoft.com/detail/9P9RM9G0QBRC` | STORE_LISTING_NOTICE.md L20 |
| Startup task | `zWorkTrackerStartup` | W/scripts/msix-manifest.mjs L461 |
| Application Id | `zWorkTracker`, EntryPoint `Windows.FullTrustApplication` | W/scripts/msix-manifest.mjs L457 |
| Capability | `rescap:Capability Name="runFullTrust"` | W/scripts/msix-manifest.mjs L467 |
| Min / tested OS | `MinVersion="10.0.19041.0" MaxVersionTested="10.0.26100.0"` | W/scripts/msix-manifest.mjs L451 |
| Test identity | `rustcircle.zWorkTracker.Test`, `CN=zWorkTracker Test Signing`, "zWorkTracker (test)", family `rustcircle.zWorkTracker.Test_ym7m1j5c19s9r` | W/scripts/msix-manifest.mjs L60-66; msix-vm.mjs L89 |
| MSA app ID (unused) | `561b212b-0a7a-4a47-a49d-94c7b6d51893` | STORE_LISTING_NOTICE.md L26 |
| App data id | `com.rustcircle.zworktracker` | R L554-556 |

No `packaging/msix/*/resources.pri` exists in W yet (README L1075: comes with 1.3.4 via `pnpm msix:vm`).

## Icons

- Master: `W/assets/logo/logo.svg` (round wall clock, rainbow dial, hands 10:10, transparent; only hand-made file). Generator `W/assets/logo/build-icons.py` (`uv run assets/logo/build-icons.py`), notes `W/assets/logo/README.md`.
- Tray sources (hand-made, not generated): `W/assets/logo/tray-idle.svg`, `W/assets/logo/tray-running.svg`.
- `W/src-tauri/icons/`: `32x32.png` (32), `128x128.png` (128), `128x128@2x.png` (256), `icon.png` (512), `icon.ico`, `icon.icns`, `tray-idle.png` (32), `tray-running.png` (32). Tray PNGs embedded via `include_bytes!` in W/src-tauri/src/lib.rs L1625-1626.
- MSIX images: 60 PNGs cut from `src-tauri/icons/icon.png` by the packaging script (R L777).
- Site: `W/site/assets/logo.svg`, `favicon.svg`, `icon-64.png`, `apple-touch-icon.png` (generated).
- Stale root files: `W/app-icon.svg`, `W/app-icon-small.svg`, `W/tray-idle.svg`, `W/tray-running.svg` (last changed f578995, 2026-11-01, purple-gradient era); README L656 still names app-icon*.svg as sources.

## Not features (deliberately absent)

- No network calls, no HTTP/TLS library, no telemetry (R L7, L568).
- No account or sign-in; MSA app ID unused (S).
- No exchange-rate lookups; EUR and USD never summed (R L392).
- No productivity score; Insights use no AI (R L264).
- No invoices, accounting or tax documents (R L353).
- No encryption of sync files (R L568; S Notices 3).
- No shared-file sync: never a file with two writers; conflict copies never deleted (R L431, L442).
- No lock on cloud folders; one-computer-at-a-time is best effort (R L466).
- Mica / translucent backdrop removed in 1.3.3; window opaque (R L1075; 68da0cd).
- OS theme not followed (R L417).
- No in-app purchases; free (S Partner Center fields).
- No WebView2 runtime shipped or downloaded by the app (R L39).
- No keyboard-only path to delete a started task (R L1086).
- No CJK/RTL/emoji glyphs in the PDF (empty boxes) (R L1087).
- No subtasks or tags in CSV/PDF exports (R L407; P Repo facts).
- No macOS lock signal (R L1081).
- Start with Windows never starts hidden in the tray (R L219).
- Subtask tags outside the task's tags; tags in billing summaries or quotes: non-goals for 1.4.0 (P Non-goals).
- No polling for view refresh in the final 1.4.0 design (LU Change 2).
