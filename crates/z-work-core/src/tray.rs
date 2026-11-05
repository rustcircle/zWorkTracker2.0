//! Close-versus-quit decision and tray content, free of any windowing code.

use std::collections::BTreeMap;

use chrono_tz::Tz;

use crate::billing::{cents_from_numerator, money, next_rate_change, secs_to_next_step, task_days};
use crate::model::{LocalFile, Segment, TaskState};
use crate::report::{ExportSource, hm};
use crate::time::Ts;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseDecision {
    /// Hide the window. The app, its timers and any running task keep going.
    Hide,
    /// Hide after telling the user, once, that the app stays in the tray.
    HideWithNotice,
    /// The tray's Quit or an OS shutdown: really exit.
    Exit,
}

#[must_use]
pub fn close_decision(quit_requested: bool, notice_already_shown: bool) -> CloseDecision {
    if quit_requested {
        CloseDecision::Exit
    } else if notice_already_shown {
        CloseDecision::Hide
    } else {
        CloseDecision::HideWithNotice
    }
}

/// What the tray's toggle does. A cold launch never resumes directly: it opens the form instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToggleRoute {
    Pause,
    Resume,
    OpenColdForm,
    Nothing,
}

#[must_use]
pub fn toggle_route(action: Option<TrayAction>, warm: bool) -> ToggleRoute {
    match action {
        Some(TrayAction::Pause) => ToggleRoute::Pause,
        Some(TrayAction::Resume) if warm => ToggleRoute::Resume,
        Some(TrayAction::Resume) => ToggleRoute::OpenColdForm,
        None => ToggleRoute::Nothing,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    Pause,
    Resume,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayState {
    pub running: bool,
    pub tooltip: String,
    /// The disabled menu line that names the current task.
    pub status_line: String,
    pub action: Option<TrayAction>,
    pub task_id: Option<String>,
}

fn own_running(file: &LocalFile) -> Option<&Segment> {
    file.data.segments.values().find(|s| {
        s.end.is_none()
            && s.device_id == file.device_id
            && file
                .data
                .tasks
                .get(&s.task_id)
                .is_some_and(|t| t.is_live() && t.state == TaskState::Running)
    })
}

/// Today's money for a task in the reporting zone, unless earnings are hidden, and the moment that
/// text can next change. `None` for the moment means only a data change can change it.
fn earnings_text(file: &LocalFile, task_id: &str, now: Ts, tz: Tz) -> (Option<String>, Option<Ts>) {
    if file.prefs.hide_earnings {
        return (None, None);
    }
    // Only this task's records: a store can hold years of data.
    let Some(task) = file.data.tasks.get(task_id).cloned() else {
        return (None, None);
    };
    let mut data = crate::model::Shared {
        projects: file
            .data
            .projects
            .iter()
            .filter(|(id, _)| **id == task.project_id)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        rates: file.data.rates.clone(),
        rounding: file.data.rounding.clone(),
        segments: file
            .data
            .segments
            .iter()
            .filter(|(_, s)| s.task_id == task_id)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        gaps: file
            .data
            .gaps
            .iter()
            .filter(|(_, g)| g.task_id == task_id)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        pauses: file
            .data
            .pauses
            .iter()
            .filter(|(_, p)| p.task_id == task_id)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        pause_requests: file.data.pause_requests.clone(),
        ..Default::default()
    };
    let project_id = task.project_id.clone();
    data.tasks.insert(task.id.clone(), task);
    let source = ExportSource {
        only: None,
        data,
        device_id: file.device_id.clone(),
        remote_devices: file.remote_devices.clone(),
        now,
    };
    let end = |s: &Segment| source.end_of(s);
    let today = crate::billing::day_of(now, tz);
    let row = task_days(&source.data, tz, &end)
        .into_iter()
        .find(|r| r.task_id == task_id && r.day == today);
    let text = row.as_ref().and_then(|r| {
        let parts: Vec<String> = r
            .numerators()
            .iter()
            .map(|(cur, n)| money(cur, cents_from_numerator(*n)))
            .collect();
        (!parts.is_empty()).then(|| parts.join(" + "))
    });
    // A new day starts a new row. A new rate moves the rounded part onto it one second in.
    let tomorrow = today
        .succ_opt()
        .map(|d| crate::billing::start_of_day(d, tz));
    // Asked from a second back: a rate starting in this very second still has its second one to come.
    let new_rate =
        next_rate_change(&source.data, &project_id, Ts(now.secs() - 1)).map(|t| Ts(t.secs() + 1));
    // Only this device's open segment grows, one second per second: another device's stops at what
    // its last sync said, and a newer sync is a data change. With rounding off, the tray's minute is
    // as often as the text needs to follow exact time.
    // No row yet means the very start of a day for a running task: it has one a second from now.
    let wait = row.as_ref().map_or(1, |r| {
        let counted = r.exact_secs + r.pause_secs;
        secs_to_next_step(counted, r.rule.increment_minutes, r.rule.method).unwrap_or(60)
    });
    let step = Ts(now.secs() + wait);
    let due = [tomorrow, new_rate, Some(step)].into_iter().flatten().min();
    (text, due)
}

fn running_state(run: &RunningTask, now: Ts) -> TrayState {
    let elapsed = hm((now.secs() - run.start.secs()).max(0));
    let money_part = run
        .earnings
        .as_ref()
        .map(|m| format!(" - {m} today"))
        .unwrap_or_default();
    TrayState {
        running: true,
        tooltip: format!("Running: {} ({elapsed}){money_part}", run.name),
        status_line: format!("{} - {elapsed}{money_part}", run.name),
        action: Some(TrayAction::Pause),
        task_id: Some(run.task_id.clone()),
    }
}

/// Which task the tray offers while nothing runs here. One pass over the segments, not one per paused
/// task: a store can hold years of segments.
fn idle_state(file: &LocalFile) -> TrayState {
    let mut last_end: BTreeMap<&str, Ts> = BTreeMap::new();
    for s in file.data.segments.values() {
        if let Some(end) = s.end {
            let last = last_end.entry(s.task_id.as_str()).or_insert(end);
            *last = (*last).max(end);
        }
    }
    let latest_paused = file
        .data
        .tasks
        .values()
        .filter(|t| t.is_live() && t.state == TaskState::Paused)
        .filter(|t| {
            file.data
                .projects
                .get(&t.project_id)
                .is_some_and(|p| !p.deleted && !p.archived)
        })
        .max_by_key(|t| last_end.get(t.id.as_str()).copied());
    match latest_paused {
        Some(t) => TrayState {
            running: false,
            tooltip: "No task running".to_owned(),
            status_line: format!("Paused: {}", t.name),
            action: Some(TrayAction::Resume),
            task_id: Some(t.id.clone()),
        },
        None => TrayState {
            running: false,
            tooltip: "No task running".to_owned(),
            status_line: "No task running".to_owned(),
            action: None,
            task_id: None,
        },
    }
}

/// The current tray content, worked out from scratch. Use [`TrayCache`] for anything that repeats.
/// `zone` is the zone the user is viewing, used for "today".
#[must_use]
pub fn tray_state(file: &LocalFile, now: Ts, zone: Tz) -> TrayState {
    match Basis::of(file, now, zone) {
        Basis::Idle(state) => state,
        Basis::Running(run) => running_state(&run, now),
    }
}

struct RunningTask {
    task_id: String,
    name: String,
    start: Ts,
    earnings: Option<String>,
    earnings_at: Ts,
    earnings_due: Option<Ts>,
}

impl RunningTask {
    fn amount_is_current(&self, now: Ts) -> bool {
        // A clock set back could otherwise leave a later day's figure showing.
        now >= self.earnings_at && self.earnings_due.is_none_or(|due| now < due)
    }
}

enum Basis {
    Idle(TrayState),
    Running(RunningTask),
}

impl Basis {
    fn of(file: &LocalFile, now: Ts, zone: Tz) -> Self {
        let Some(seg) = own_running(file) else {
            return Self::Idle(idle_state(file));
        };
        let (earnings, earnings_due) = earnings_text(file, &seg.task_id, now, zone);
        Self::Running(RunningTask {
            task_id: seg.task_id.clone(),
            name: file
                .data
                .tasks
                .get(&seg.task_id)
                .map_or_else(|| "Task".to_owned(), |t| t.name.clone()),
            start: seg.start,
            earnings,
            earnings_at: now,
            earnings_due,
        })
    }
}

/// The longest the tray goes without looking, for a change that didn't announce itself.
const MAX_WAKE_SECS: i64 = 60;

/// Keeps the tray's work to what its text needs: the store is read only after a data change, the amount
/// is worked out again only when a billing step is due, and the text is handed over only when it differs.
#[derive(Default)]
pub struct TrayCache {
    key: Option<(u64, Tz)>,
    basis: Option<Basis>,
    shown: Option<TrayState>,
    amounts_computed: u64,
}

impl TrayCache {
    /// `revision` is the service's count of committed changes. Returns the content to show, or `None`
    /// when what is on the tray already says this.
    pub fn refresh(
        &mut self,
        file: &LocalFile,
        revision: u64,
        now: Ts,
        zone: Tz,
    ) -> Option<TrayState> {
        let key = Some((revision, zone));
        let stale = self.key != key
            || matches!(&self.basis, Some(Basis::Running(run)) if !run.amount_is_current(now))
            || self.basis.is_none();
        if stale {
            let basis = Basis::of(file, now, zone);
            if matches!(basis, Basis::Running(_)) && !file.prefs.hide_earnings {
                self.amounts_computed += 1;
            }
            self.key = key;
            self.basis = Some(basis);
        }
        let state = match self.basis.as_ref()? {
            Basis::Idle(state) => state.clone(),
            Basis::Running(run) => running_state(run, now),
        };
        if self.shown.as_ref() == Some(&state) {
            return None;
        }
        self.shown = Some(state.clone());
        Some(state)
    }

    /// Seconds until the text can next change by itself: the next whole minute of the running task or
    /// its next amount step, whichever comes first. Data changes wake the tray earlier.
    #[must_use]
    pub fn next_wake_secs(&self, now: Ts) -> u64 {
        let wait = match &self.basis {
            Some(Basis::Running(run)) => {
                let minute = 60 - (now.secs() - run.start.secs()).rem_euclid(60);
                run.earnings_due
                    .map_or(minute, |due| minute.min(due.secs() - now.secs()))
            }
            _ => MAX_WAKE_SECS,
        };
        wait.clamp(1, MAX_WAKE_SECS).unsigned_abs()
    }

    /// How many times the amount has been worked out. It is what the tray must not do between steps.
    #[must_use]
    pub const fn amounts_computed(&self) -> u64 {
        self.amounts_computed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::RateInput;
    use crate::machine::tests::{DEV, ctx, project_input, task_input};
    use crate::persist::new_local_file;
    use crate::validate::parse_zone;

    fn file() -> (LocalFile, String) {
        let mut f = new_local_file("pc");
        f.device_id = DEV.into();
        let c = ctx("2026-01-01T08:00:00Z");
        let p = f.data.create_project(&c, &project_input("Alpha")).unwrap();
        f.data
            .set_rate(
                &c,
                &RateInput {
                    project_id: p.clone(),
                    rate_cents: Some(3600),
                    currency: "USD".into(),
                    effective_from: None,
                    confirm_retroactive: false,
                },
            )
            .unwrap();
        let t = f.data.create_task(&c, &task_input("Write", &p)).unwrap();
        (f, t)
    }

    #[test]
    fn cold_tray_resume_opens_the_form_and_starts_nothing() {
        assert_eq!(
            toggle_route(Some(TrayAction::Resume), false),
            ToggleRoute::OpenColdForm
        );
        assert_eq!(
            toggle_route(Some(TrayAction::Resume), true),
            ToggleRoute::Resume
        );
        assert_eq!(
            toggle_route(Some(TrayAction::Pause), false),
            ToggleRoute::Pause
        );
        assert_eq!(toggle_route(None, true), ToggleRoute::Nothing);
    }

    #[test]
    fn close_versus_quit() {
        assert_eq!(close_decision(false, false), CloseDecision::HideWithNotice);
        assert_eq!(close_decision(false, true), CloseDecision::Hide);
        assert_eq!(close_decision(true, false), CloseDecision::Exit);
        assert_eq!(close_decision(true, true), CloseDecision::Exit);
    }

    #[test]
    fn idle_running_paused_and_hidden_earnings() {
        let utc = parse_zone("UTC").unwrap();
        let (mut f, t) = file();
        let now = Ts::parse("2026-01-01T09:30:00Z").unwrap();
        let st = tray_state(&f, now, utc);
        assert!(!st.running);
        assert_eq!(st.tooltip, "No task running");
        assert_eq!(st.action, None);

        f.data.start(&ctx("2026-01-01T09:00:00Z"), &t).unwrap();
        let st = tray_state(&f, now, utc);
        assert!(st.running);
        assert_eq!(st.action, Some(TrayAction::Pause));
        assert!(
            st.status_line.contains("Write - 0:30 "),
            "{}",
            st.status_line
        );
        assert!(st.tooltip.contains("(0:30)"), "{}", st.tooltip);
        // Default rounding: 30 min at $36/h.
        assert!(st.tooltip.contains("$18.00"), "{}", st.tooltip);

        f.prefs.hide_earnings = true;
        let st = tray_state(&f, now, utc);
        assert!(!st.tooltip.contains('$') && !st.status_line.contains('$'));
        f.prefs.hide_earnings = false;

        f.data.pause(&ctx("2026-01-01T09:30:00Z"), &t).unwrap();
        let st = tray_state(&f, now, utc);
        assert!(!st.running);
        assert_eq!(st.action, Some(TrayAction::Resume));
        assert_eq!(st.task_id.as_deref(), Some(t.as_str()));
        assert_eq!(st.tooltip, "No task running");
    }

    #[test]
    fn a_task_running_on_another_device_is_not_this_trays_task() {
        let utc = parse_zone("UTC").unwrap();
        let (mut f, t) = file();
        f.data
            .start(
                &crate::machine::Ctx::at(Ts::parse("2026-01-01T09:00:00Z").unwrap(), "other"),
                &t,
            )
            .unwrap();
        let st = tray_state(&f, Ts::parse("2026-01-01T09:30:00Z").unwrap(), utc);
        assert!(!st.running);
        assert_eq!(st.action, None);
    }

    const START: i64 = 1_767_258_000; // 2026-01-01T09:00:00Z

    fn at(offset: i64) -> Ts {
        Ts(START + offset)
    }

    /// `t` running since 09:00 with the default rounding: 15 minutes, up.
    fn running_file() -> (LocalFile, String) {
        let (mut f, t) = file();
        assert_eq!(at(0), Ts::parse("2026-01-01T09:00:00Z").unwrap());
        f.data.start(&ctx("2026-01-01T09:00:00Z"), &t).unwrap();
        (f, t)
    }

    #[test]
    fn the_amount_is_not_worked_out_again_between_steps() {
        let utc = parse_zone("UTC").unwrap();
        let (f, _) = running_file();
        let mut cache = TrayCache::default();
        let first = cache.refresh(&f, 1, at(30), utc).unwrap();
        assert!(first.tooltip.contains("$9.00"), "{}", first.tooltip);
        assert_eq!(cache.amounts_computed(), 1);
        // Every 5 s up to the end of the first 15 minutes: the old tray's pace, with nothing to work out.
        for secs in (35..=900).step_by(5) {
            let shown = cache.refresh(&f, 1, at(secs), utc);
            assert_eq!(cache.amounts_computed(), 1, "at {secs} s");
            assert!(
                shown.is_none_or(|s| s.tooltip.contains("$9.00")),
                "at {secs} s"
            );
        }
        // One second into the next increment.
        let next = cache.refresh(&f, 1, at(901), utc).unwrap();
        assert_eq!(cache.amounts_computed(), 2);
        assert!(next.tooltip.contains("$18.00"), "{}", next.tooltip);
    }

    #[test]
    fn the_text_is_handed_over_only_when_its_minute_or_data_changes() {
        let utc = parse_zone("UTC").unwrap();
        let (mut f, t) = running_file();
        let mut cache = TrayCache::default();
        // From one second in: at nought seconds nothing has been worked yet, so no increment has begun.
        let updates: Vec<i64> = (1..=180)
            .filter(|secs| cache.refresh(&f, 1, at(*secs), utc).is_some())
            .collect();
        assert_eq!(updates, [1, 60, 120, 180]);
        assert_eq!(cache.amounts_computed(), 1);

        // A data change is seen at once and works the amount out again, even inside a minute.
        f.data.tasks.get_mut(&t).unwrap().name = "Rewrite".into();
        let renamed = cache.refresh(&f, 2, at(190), utc).unwrap();
        assert!(
            renamed.status_line.starts_with("Rewrite - 0:03"),
            "{}",
            renamed.status_line
        );
        assert_eq!(cache.amounts_computed(), 2);
        assert!(cache.refresh(&f, 2, at(191), utc).is_none());

        // A change that leaves the text as it was hands nothing over.
        assert!(cache.refresh(&f, 3, at(192), utc).is_none());
        assert_eq!(cache.amounts_computed(), 3);
    }

    #[test]
    fn hidden_earnings_and_idle_trays_do_no_amount_work() {
        let utc = parse_zone("UTC").unwrap();
        let (mut f, _) = running_file();
        f.prefs.hide_earnings = true;
        let mut cache = TrayCache::default();
        for secs in (0..7200).step_by(5) {
            let _ = cache.refresh(&f, 1, at(secs), utc);
        }
        assert_eq!(cache.amounts_computed(), 0);

        let (idle, _) = file();
        let mut cache = TrayCache::default();
        assert!(cache.refresh(&idle, 1, at(0), utc).is_some());
        assert!(cache.refresh(&idle, 1, at(5), utc).is_none());
        assert_eq!(cache.amounts_computed(), 0);
        assert_eq!(cache.next_wake_secs(at(5)), 60);
    }

    #[test]
    fn it_wakes_for_the_next_minute_or_the_next_step_whichever_is_first() {
        let utc = parse_zone("UTC").unwrap();
        let (f, _) = running_file();
        let mut cache = TrayCache::default();
        let _ = cache.refresh(&f, 1, at(30), utc);
        assert_eq!(cache.next_wake_secs(at(30)), 30);
        // At 14:50 the minute is 10 s away and the step 11 s.
        let _ = cache.refresh(&f, 1, at(890), utc);
        assert_eq!(cache.next_wake_secs(at(890)), 10);
        // At 15:00 the step is 1 s away.
        let _ = cache.refresh(&f, 1, at(900), utc);
        assert_eq!(cache.next_wake_secs(at(900)), 1);
    }

    #[test]
    fn a_new_day_is_due_even_with_no_step_in_between() {
        let utc = parse_zone("UTC").unwrap();
        let (mut f, t) = file();
        f.data.start(&ctx("2026-01-01T23:50:00Z"), &t).unwrap();
        let mut cache = TrayCache::default();
        let night = Ts::parse("2026-01-01T23:59:59Z").unwrap();
        let before = cache.refresh(&f, 1, night, utc).unwrap();
        assert!(before.tooltip.contains("$9.00"), "{}", before.tooltip);
        let computed = cache.amounts_computed();
        let after = cache.refresh(&f, 1, Ts(night.secs() + 1), utc);
        assert_eq!(cache.amounts_computed(), computed + 1);
        // Nothing has counted on the new day yet.
        assert!(after.unwrap().tooltip.contains("0:10"));
    }

    /// Refreshing as the tray thread does, waking only when `next_wake_secs` says, the text must match
    /// one worked out from scratch at every wake.
    fn assert_wakes_match(f: &LocalFile, from: Ts, secs: i64) {
        let utc = parse_zone("UTC").unwrap();
        let mut cache = TrayCache::default();
        let mut now = from;
        let mut shown = None;
        while now.secs() < from.secs() + secs {
            if let Some(state) = cache.refresh(f, 1, now, utc) {
                shown = Some(state);
            }
            assert_eq!(
                shown.as_ref(),
                Some(&tray_state(f, now, utc)),
                "at {}",
                now.iso()
            );
            now = Ts(now.secs() + i64::try_from(cache.next_wake_secs(now)).unwrap());
        }
    }

    #[test]
    fn the_amount_comes_back_one_second_into_a_new_day() {
        let (mut f, t) = file();
        f.data.start(&ctx("2026-01-01T23:50:00Z"), &t).unwrap();
        assert_wakes_match(&f, Ts::parse("2026-01-01T23:50:00Z").unwrap(), 1200);
    }

    #[test]
    fn a_rate_starting_in_the_second_of_a_step_is_still_picked_up() {
        let (mut f, _) = running_file();
        let mut later = f.data.rates.values().next().unwrap().clone();
        later.id = "later".into();
        later.rate_cents = Some(7200);
        // 15 minutes and one second in: the second the Up step falls due.
        later.effective_from = at(901);
        f.data.rates.insert(later.id.clone(), later);
        assert_wakes_match(&f, at(1), 1800);
    }

    /// The cached text must never lag the real figure, whatever the rule, past pauses and a new rate.
    #[test]
    fn the_cached_amount_always_equals_one_worked_out_from_scratch() {
        use crate::model::{RoundMethod, RoundingVersion, Stamp};
        let utc = parse_zone("UTC").unwrap();
        for (increment, method) in [
            (15, RoundMethod::Up),
            (15, RoundMethod::Down),
            (15, RoundMethod::Nearest),
            (7, RoundMethod::Nearest),
            (1, RoundMethod::Up),
        ] {
            let (mut f, t) = file();
            f.data.rounding.insert(
                "rule".into(),
                RoundingVersion {
                    id: "rule".into(),
                    project_id: None,
                    increment_minutes: increment,
                    method,
                    effective_from: "2026-01-01".into(),
                    deleted: false,
                    stamp: Stamp::default(),
                },
            );
            // Earlier time today, then a rate that starts while the task runs.
            f.data.start(&ctx("2026-01-01T08:00:00Z"), &t).unwrap();
            f.data.pause(&ctx("2026-01-01T08:04:10Z"), &t).unwrap();
            f.data.resume(&ctx("2026-01-01T09:00:00Z"), &t).unwrap();
            let mut later = f.data.rates.values().next().unwrap().clone();
            later.id = "later".into();
            later.rate_cents = Some(7200);
            later.effective_from = at(1000);
            f.data.rates.insert(later.id.clone(), later);
            let money_of = |s: &TrayState| s.tooltip.rsplit(") - ").next().map(str::to_owned);
            let mut cache = TrayCache::default();
            let mut shown = None;
            for secs in 0..2400 {
                if let Some(state) = cache.refresh(&f, 1, at(secs), utc) {
                    shown = Some(state);
                }
                let fresh = tray_state(&f, at(secs), utc);
                assert_eq!(
                    shown.as_ref().and_then(money_of),
                    money_of(&fresh),
                    "{increment} min {method:?} at {secs} s"
                );
            }
            assert!(
                cache.amounts_computed() < 2400 / 30,
                "{increment} min {method:?}: {} amounts",
                cache.amounts_computed()
            );
        }
    }
}
