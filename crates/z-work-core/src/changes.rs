//! The plain-text record of what changed since the previous version.
//!
//! Written beside each version as `<name>.changes.txt`, in UTC and plain words, so it reads the same on every computer
//! and in any text editor. It compares the data with the version before it, map by map, and lists what the audit trail
//! gained in between. The version itself holds the structured facts.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde_json::Value;

use crate::model::{AuditEntry, Segment, Shared};
use crate::service::span_text;
use crate::sync::{Counts, counts};
use crate::time::Ts;

/// The audit lines a record lists before it only counts the rest.
pub const AUDIT_LINES: usize = 2_000;
/// No line runs on past this, whatever a name or a reason holds.
const LINE_CHARS: usize = 300;
const UNKNOWN_DEVICE: &str = "another computer";

/// Device names by id, for the audit lines.
#[derive(Clone, Debug, Default)]
pub struct DeviceNames(BTreeMap<String, String>);

impl DeviceNames {
    pub fn new(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        Self(pairs.into_iter().collect())
    }

    #[must_use]
    pub fn of(&self, id: &str) -> &str {
        self.0
            .get(id)
            .map(String::as_str)
            .filter(|n| !n.trim().is_empty())
            .unwrap_or(UNKNOWN_DEVICE)
    }
}

/// Why a version was made, in the words the Versions page uses for the same code.
#[must_use]
pub fn reason_words(code: &str) -> &'static str {
    match code {
        "auto" => "automatic hourly copy",
        "quit" => "copy when the app was closed",
        "manual" => "saved by hand",
        "takeover" => "copy when a computer took over",
        "pre-restore" => "copy of the data before a restore",
        "pre-adopt" => "copy of the data before following a restore",
        "restored" => "copy of the restored data",
        _ => "saved copy",
    }
}

/// One line from the audit trail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditLine {
    pub at: Ts,
    /// Who did it, by name.
    pub by: String,
    pub what: String,
}

/// What changed between two versions of the data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeRecord {
    /// What the data holds now, counted as the app shows it.
    pub totals: Counts,
    /// What changed, in words, or `None` with nothing to compare with.
    pub changes: Option<Vec<String>>,
    /// The audit trail's new entries, oldest first, up to `AUDIT_LINES`.
    pub audit: Vec<AuditLine>,
    /// How many more there were.
    pub more: usize,
}

/// What the record says about the version it sits beside.
#[derive(Clone, Copy, Debug)]
pub struct Header<'a> {
    pub taken_at: Ts,
    pub device_name: &'a str,
    pub app_version: &'a str,
    /// The reason code from the version's name.
    pub reason: &'a str,
    /// When the version compared with was taken.
    pub compared_with: Option<Ts>,
    pub epoch: u64,
    pub schema: u64,
    pub checksum: &'a str,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Tally {
    added: usize,
    changed: usize,
    /// Segments whose correction changed. Other records have no such thing.
    corrected: usize,
    /// Newly deleted, voided or discarded, or gone altogether, as after a restore.
    removed: usize,
    /// The time the added segments hold.
    added_secs: i64,
}

/// Counts what is new, changed and removed in one map. A record that arrives already removed, or changes while it
/// stays removed, is nothing to report.
fn tally<V: PartialEq>(
    prev: &BTreeMap<String, V>,
    cur: &BTreeMap<String, V>,
    gone: impl Fn(&V) -> bool,
) -> Tally {
    let mut t = Tally::default();
    for (id, now) in cur {
        match prev.get(id) {
            None if !gone(now) => t.added += 1,
            Some(before) if before != now && !gone(before) => {
                if gone(now) {
                    t.removed += 1;
                } else {
                    t.changed += 1;
                }
            }
            Some(before) if before != now && !gone(now) => t.changed += 1,
            _ => {}
        }
    }
    t.removed += prev
        .iter()
        .filter(|(id, before)| !cur.contains_key(*id) && !gone(before))
        .count();
    t
}

/// For maps whose records are never removed.
fn never<V>(_: &V) -> bool {
    false
}

fn voided(s: &Segment) -> bool {
    s.edit.as_ref().is_some_and(|e| e.voided)
}

/// Segments say more: how much time was added, and which changes are corrections.
fn tally_segments(prev: &BTreeMap<String, Segment>, cur: &BTreeMap<String, Segment>) -> Tally {
    let mut t = tally(prev, cur, voided);
    t.added_secs = cur
        .iter()
        .filter(|(id, s)| !prev.contains_key(*id) && !voided(s))
        .filter_map(|(_, s)| match s.effective() {
            Some((start, Some(end))) => Some((end.secs() - start.secs()).max(0)),
            _ => None,
        })
        .sum();
    t.corrected = cur
        .iter()
        .filter(|(id, now)| {
            prev.get(*id)
                .is_some_and(|before| before.edit != now.edit && !voided(before) && !voided(now))
        })
        .count();
    t.changed -= t.corrected.min(t.changed);
    t
}

/// "2,391".
fn count_text(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn counted(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", count_text(n), if n == 1 { one } else { many })
}

fn describe(t: Tally, one: &str, many: &str, out: &mut Vec<String>) {
    if t.added > 0 {
        let mut words = format!("{} added", counted(t.added, one, many));
        if t.added_secs > 0 {
            let _ = write!(words, " ({})", span_text(t.added_secs));
        }
        out.push(words);
    }
    if t.changed > 0 {
        out.push(format!("{} changed", counted(t.changed, one, many)));
    }
    if t.corrected > 0 {
        out.push(format!("{} corrected", counted(t.corrected, one, many)));
    }
    if t.removed > 0 {
        out.push(format!("{} removed", counted(t.removed, one, many)));
    }
}

/// Every map of `Shared` but the audit trail, which the record lists line by line instead.
fn changes(prev: &Shared, cur: &Shared) -> Vec<String> {
    let mut out = Vec::new();
    describe(
        tally(&prev.projects, &cur.projects, |p| p.deleted),
        "project",
        "projects",
        &mut out,
    );
    describe(
        tally(&prev.tasks, &cur.tasks, |t| {
            t.deleted || t.discarded.is_some()
        }),
        "task",
        "tasks",
        &mut out,
    );
    describe(
        tally_segments(&prev.segments, &cur.segments),
        "time segment",
        "time segments",
        &mut out,
    );
    describe(
        tally(&prev.settings, &cur.settings, never),
        "setting",
        "settings",
        &mut out,
    );
    describe(
        tally(&prev.shifts, &cur.shifts, |s| s.deleted),
        "shift",
        "shifts",
        &mut out,
    );
    describe(
        tally(&prev.rates, &cur.rates, |r| r.deleted),
        "price",
        "prices",
        &mut out,
    );
    describe(
        tally(&prev.rounding, &cur.rounding, |r| r.deleted),
        "rounding rule",
        "rounding rules",
        &mut out,
    );
    describe(
        tally(&prev.gaps, &cur.gaps, never),
        "period of time away",
        "periods of time away",
        &mut out,
    );
    describe(
        tally(&prev.pauses, &cur.pauses, |p| p.deleted),
        "billable pause",
        "billable pauses",
        &mut out,
    );
    describe(
        tally(&prev.tags, &cur.tags, |t| t.deleted),
        "tag",
        "tags",
        &mut out,
    );
    describe(
        tally(&prev.templates, &cur.templates, |t| t.deleted),
        "template",
        "templates",
        &mut out,
    );
    describe(
        tally(&prev.summaries, &cur.summaries, |s| s.voided),
        "billing summary",
        "billing summaries",
        &mut out,
    );
    describe(
        tally(&prev.prefixes, &cur.prefixes, never),
        "reference prefix",
        "reference prefixes",
        &mut out,
    );
    describe(
        tally(&prev.pause_requests, &cur.pause_requests, never),
        "pause request",
        "pause requests",
        &mut out,
    );
    out
}

/// One line of text, however the name or reason it comes from was typed.
fn one_line(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = flat.trim();
    if flat.chars().count() <= LINE_CHARS {
        return flat.to_owned();
    }
    let mut cut: String = flat.chars().take(LINE_CHARS - 3).collect();
    cut.push_str("...");
    cut
}

fn same_day(a: Ts, b: Ts) -> bool {
    a.secs().div_euclid(86_400) == b.secs().div_euclid(86_400)
}

fn hm(at: Ts) -> String {
    at.datetime().format("%H:%M").to_string()
}

/// "13:40" on the day of `day`, else "1 Nov 13:40", with the year when that differs too.
fn moment(at: Ts, day: Ts) -> String {
    if same_day(at, day) {
        hm(at)
    } else if at.datetime().format("%Y").to_string() == day.datetime().format("%Y").to_string() {
        at.datetime().format("%-d %b %H:%M").to_string()
    } else {
        at.datetime().format("%-d %b %Y %H:%M").to_string()
    }
}

/// The times a segment's audit value holds, as they count: its correction, else what was timed.
fn span(value: &Value, day: Ts) -> String {
    let ts = |v: Option<&Value>| v.and_then(Value::as_str).and_then(Ts::parse);
    let edit = value
        .get("edit")
        .filter(|e| e.is_object() && e.get("voided") != Some(&Value::Bool(true)));
    let (start, end) = match edit {
        Some(e) => (ts(e.get("start")), ts(e.get("end"))),
        None => (ts(value.get("rawStart")), ts(value.get("rawEnd"))),
    };
    let Some(start) = start else {
        return "times unknown".to_owned();
    };
    let from = moment(start, day);
    match end {
        Some(end) if same_day(start, end) => format!("{from}-{}", hm(end)),
        Some(end) => format!("{from}-{}", moment(end, start)),
        None => format!("{from}, still running"),
    }
}

/// The task an entry is about, by name.
fn task_name<'a>(e: &AuditEntry, data: &'a Shared) -> Option<&'a str> {
    let task = match e.entity.as_str() {
        "segment" => data.segments.get(&e.entity_id).map(|s| &s.task_id),
        "gap" => data.gaps.get(&e.entity_id).map(|g| &g.task_id),
        "pause" => data.pauses.get(&e.entity_id).map(|p| &p.task_id),
        "task" => Some(&e.entity_id),
        _ => None,
    }?;
    data.tasks.get(task).map(|t| t.name.as_str())
}

/// "markBillable" as "mark billable".
fn words(action: &str) -> String {
    let mut out = String::new();
    for c in action.chars() {
        if c.is_ascii_uppercase() {
            out.push(' ');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn phrase(e: &AuditEntry, cur: &Shared, prev: &Shared) -> String {
    let task = task_name(e, cur).or_else(|| task_name(e, prev));
    let on = task.map_or_else(String::new, |t| format!(" on \"{t}\""));
    let reference = || {
        e.after
            .get("reference")
            .or_else(|| e.before.get("reference"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| cur.summaries.get(&e.entity_id).map(|s| s.reference.clone()))
            .unwrap_or_default()
    };
    let at = e.at;
    let what = match (e.entity.as_str(), e.action.as_str()) {
        ("segment", "correct") => format!(
            "corrected a time segment{on}: {} became {}",
            span(&e.before, at),
            span(&e.after, at)
        ),
        ("segment", "void") => format!("voided a time segment{on} ({})", span(&e.before, at)),
        ("segment", "restore") => format!(
            "brought back a voided time segment{on} ({})",
            span(&e.after, at)
        ),
        ("segment", "manualEntry") => {
            format!("added a time segment by hand{on} ({})", span(&e.after, at))
        }
        ("segment", "clockPutBack") => {
            format!("closed a time segment{on} because the clock was put back")
        }
        ("segment", "settleLost") => format!(
            "closed a time segment{on} for a computer that stopped responding ({})",
            span(&e.after, at)
        ),
        ("segment", "reclaimAfterReturn") => {
            format!("set a time segment{on} back to the end its own computer recorded")
        }
        ("segment", "reclaimOnSummary") => format!(
            "kept a time segment{on} as billed, though its own computer recorded another end"
        ),
        ("gap", "detected") => format!("found time away{on}"),
        ("gap", "decide") => format!("answered about time away{on}"),
        ("pause", "markBillable") => format!("marked a billable pause{on}"),
        ("pause", "unmarkBillable") => format!("took back a billable pause{on}"),
        ("summary", "create") => format!("created the billing summary {}", reference()),
        ("summary", "void") => format!("voided the billing summary {}", reference()),
        ("task", "delete") => {
            let name = e.before.get("name").and_then(Value::as_str).or(task);
            let named = name.map_or_else(String::new, |n| format!(" \"{n}\""));
            format!("deleted the task{named} with all its time")
        }
        ("task", "requestPause") => format!("asked for a pause{on}"),
        ("task", "pausedByRequest") => format!("paused{on} because another computer asked"),
        ("price", "removed") => "removed a price".to_owned(),
        ("backup", "restore") => format!("restored the backup {}", e.entity_id),
        ("version", "restore") => format!("restored the version {}", e.entity_id),
        ("version", "keepWithRestore") => format!(
            "kept this computer's data along with the restored version {}",
            e.entity_id
        ),
        ("lease", "leaseLost") => "stopped changing data, as another computer took over".to_owned(),
        (entity, action) => format!("{} {entity}{on}", words(action)),
    };
    let reason = e.reason.trim();
    one_line(&if reason.is_empty() {
        what
    } else {
        format!("{what}. Reason: {reason}")
    })
}

fn audit_lines(prev: &Shared, cur: &Shared, names: &DeviceNames) -> (Vec<AuditLine>, usize) {
    let mut new: Vec<&AuditEntry> = cur
        .audit_log
        .iter()
        .filter(|(id, _)| !prev.audit_log.contains_key(*id))
        .map(|(_, e)| e)
        .collect();
    new.sort_by(|a, b| (a.at, &a.id).cmp(&(b.at, &b.id)));
    let more = new.len().saturating_sub(AUDIT_LINES);
    let lines = new
        .into_iter()
        .take(AUDIT_LINES)
        .map(|e| AuditLine {
            at: e.at,
            by: one_line(names.of(&e.by)),
            what: phrase(e, cur, prev),
        })
        .collect();
    (lines, more)
}

/// Compares the data with the version before it. With none, the record gives totals only.
#[must_use]
pub fn diff(prev: Option<&Shared>, cur: &Shared, names: &DeviceNames) -> ChangeRecord {
    let (audit, more) = prev.map_or_else(|| (Vec::new(), 0), |p| audit_lines(p, cur, names));
    ChangeRecord {
        totals: counts(cur),
        changes: prev.map(|p| changes(p, cur)),
        audit,
        more,
    }
}

fn utc(at: Ts, day: Ts) -> String {
    if same_day(at, day) {
        format!("{} UTC", hm(at))
    } else {
        at.datetime().format("%-d %b %Y, %H:%M UTC").to_string()
    }
}

impl ChangeRecord {
    /// The text of `<name>.changes.txt`.
    #[must_use]
    pub fn text(&self, h: &Header<'_>) -> String {
        let mut out = String::new();
        let saved = h.taken_at.datetime().format("%-d %b %Y, %H:%M UTC");
        let _ = writeln!(out, "zWorkTracker version saved {saved}");
        let _ = writeln!(
            out,
            "Saved by {} (app {}). Reason: {}.",
            one_line(h.device_name),
            h.app_version,
            reason_words(h.reason)
        );
        match (h.compared_with, &self.changes) {
            (Some(at), Some(_)) => {
                let _ = writeln!(
                    out,
                    "Compared with the version from {}.",
                    utc(at, h.taken_at)
                );
            }
            _ => {
                let _ = writeln!(
                    out,
                    "There is no earlier version to compare with, so this gives totals only."
                );
            }
        }
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "Epoch {}. App schema {}. Data checksum {}.",
            h.epoch, h.schema, h.checksum
        );
        let _ = writeln!(out);
        let t = self.totals;
        let _ = writeln!(
            out,
            "Now: {}, {}, {}.",
            counted(t.projects, "project", "projects"),
            counted(t.tasks, "task", "tasks"),
            counted(t.segments, "time segment", "time segments")
        );
        let Some(changes) = &self.changes else {
            return out;
        };
        if changes.is_empty() {
            let _ = writeln!(out, "Changed since then: nothing.");
        } else {
            let _ = writeln!(out, "Changed since then: {}.", changes.join(", "));
        }
        let _ = writeln!(out);
        if self.audit.is_empty() {
            let _ = writeln!(out, "Nothing new in the audit trail.");
            return out;
        }
        let _ = writeln!(out, "What was done, from the audit trail:");
        for line in &self.audit {
            let _ = writeln!(
                out,
                "{}  {}  {}",
                moment(line.at, h.taken_at),
                line.by,
                line.what
            );
        }
        if self.more > 0 {
            let _ = writeln!(out, "and {} more.", count_text(self.more));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::Ctx;
    use crate::model::{Project, SegmentEdit, Stamp, Task, TaskState};
    use serde_json::json;

    const ME: &str = "11111111-1111-4111-8111-111111111111";
    const LAPTOP: &str = "22222222-2222-4222-8222-222222222222";

    fn at(s: &str) -> Ts {
        Ts::parse(&format!("2026-11-02T{s}:00Z")).unwrap()
    }

    fn stamp() -> Stamp {
        Stamp {
            at: at("08:00"),
            by: ME.to_owned(),
        }
    }

    fn project(id: &str, name: &str) -> Project {
        Project {
            id: id.to_owned(),
            name: name.to_owned(),
            color: "#336699".to_owned(),
            archived: false,
            deleted: false,
            billable: true,
            budget_hours: None,
            client_zone: None,
            billing_type: Default::default(),
            fixed_fee_cents: None,
            fee_currency: None,
            created_at: at("08:00"),
            stamp: stamp(),
            identity_stamp: None,
            billing_stamp: None,
            budget_stamp: None,
            archive_stamp: None,
        }
    }

    fn task(id: &str, name: &str) -> Task {
        Task {
            id: id.to_owned(),
            name: name.to_owned(),
            name_stamp: stamp(),
            project_id: "p1".to_owned(),
            project_stamp: stamp(),
            description: Value::Null,
            description_stamp: stamp(),
            deleted: false,
            deleted_stamp: stamp(),
            discarded: None,
            continued_from: None,
            created_at: at("08:00"),
            state: TaskState::Paused,
            pause_reason: None,
            state_stamp: stamp(),
            stopped_at: None,
            tags: Vec::new(),
            tags_stamp: stamp(),
            active_subtask: None,
            active_subtask_stamp: stamp(),
        }
    }

    fn segment(id: &str, task: &str, from: &str, to: &str) -> Segment {
        Segment {
            id: id.to_owned(),
            task_id: task.to_owned(),
            device_id: ME.to_owned(),
            start: at(from),
            end: Some(at(to)),
            ..Segment::default()
        }
    }

    fn names() -> DeviceNames {
        DeviceNames::new([
            (ME.to_owned(), "DESKTOP".to_owned()),
            (LAPTOP.to_owned(), "LAPTOP".to_owned()),
        ])
    }

    fn header<'a>(reason: &'a str, compared_with: Option<Ts>) -> Header<'a> {
        Header {
            taken_at: at("14:25"),
            device_name: "LAPTOP",
            app_version: "1.3.1",
            reason,
            compared_with,
            epoch: 0,
            schema: 2,
            checksum: "9f3a01c2d4e5b678",
        }
    }

    /// The data of the version before: two projects, two tasks, one segment.
    fn before() -> Shared {
        let mut d = Shared::default();
        d.projects.insert("p1".into(), project("p1", "Client"));
        d.projects.insert("p2".into(), project("p2", "Internal"));
        d.tasks.insert("t1".into(), task("t1", "Client review"));
        d.tasks.insert("t2".into(), task("t2", "Planning"));
        d.segments
            .insert("s1".into(), segment("s1", "t1", "09:00", "10:00"));
        d
    }

    /// Test 46.
    #[test]
    fn change_record_counts_and_audit_lines() {
        let prev = before();
        let mut cur = prev.clone();
        // One task renamed, one project deleted, three segments added (1 h 12 min), one corrected.
        cur.tasks.get_mut("t2").unwrap().name = "Planning Q4".into();
        cur.projects.get_mut("p2").unwrap().deleted = true;
        for (id, from, to) in [
            ("s2", "10:30", "11:00"),
            ("s3", "11:00", "11:30"),
            ("s4", "12:00", "12:12"),
        ] {
            cur.segments.insert(id.into(), segment(id, "t1", from, to));
        }
        let before_edit = crate::adjust::seg_json(&cur.segments["s1"]);
        cur.segments.get_mut("s1").unwrap().edit = Some(SegmentEdit {
            start: at("09:10"),
            end: at("10:00"),
            voided: false,
            reason: "started late".into(),
            settled: false,
        });
        let after_edit = crate::adjust::seg_json(&cur.segments["s1"]);
        let laptop = Ctx::at(at("13:40"), LAPTOP);
        cur.audit(
            &laptop,
            "segment",
            "s1",
            "correct",
            "started late",
            before_edit,
            after_edit,
        );
        let unknown = Ctx::at(at("13:45"), "33333333-3333-4333-8333-333333333333");
        cur.audit(
            &unknown,
            "summary",
            "b1",
            "create",
            "",
            Value::Null,
            json!({"reference": "A-001"}),
        );
        let record = diff(Some(&prev), &cur, &names());
        let text = record.text(&header("auto", Some(at("13:25"))));
        assert_eq!(
            text,
            "zWorkTracker version saved 2 Nov 2026, 14:25 UTC\n\
             Saved by LAPTOP (app 1.3.1). Reason: automatic hourly copy.\n\
             Compared with the version from 13:25 UTC.\n\
             \n\
             Epoch 0. App schema 2. Data checksum 9f3a01c2d4e5b678.\n\
             \n\
             Now: 1 project, 2 tasks, 4 time segments.\n\
             Changed since then: 1 project removed, 1 task changed, 3 time segments added (1 h 12 min), 1 time segment corrected.\n\
             \n\
             What was done, from the audit trail:\n\
             13:40  LAPTOP  corrected a time segment on \"Client review\": 09:00-10:00 became 09:10-10:00. Reason: started late\n\
             13:45  another computer  created the billing summary A-001\n"
        );

        // Past the cap the rest is only counted, and an earlier day says its date.
        let mut many = cur.clone();
        let yesterday = Ctx::at(at("13:40").later(-86_400), ME);
        for i in 0..AUDIT_LINES + 3 {
            many.audit(
                &yesterday,
                "segment",
                "s2",
                "void",
                &format!("line {i}\nwith a break"),
                crate::adjust::seg_json(&cur.segments["s2"]),
                Value::Null,
            );
        }
        let record = diff(Some(&cur), &many, &names());
        assert_eq!(record.audit.len(), AUDIT_LINES);
        assert_eq!(record.more, 3);
        let text = record.text(&header("manual", Some(at("14:00"))));
        assert!(
            text.ends_with("and 3 more.\n"),
            "{}",
            &text[text.len() - 200..]
        );
        assert!(text.contains("Changed since then: nothing.\n"));
        assert!(text.contains(
            "1 Nov 13:40  DESKTOP  voided a time segment on \"Client review\" (2 Nov 10:30-11:00). Reason: line "
        ));
        assert!(
            !text.contains("line 0\n"),
            "a break in a reason stays on one line"
        );
        assert_eq!(text.lines().count(), 11 + AUDIT_LINES);
    }

    /// Test 47.
    #[test]
    fn change_record_first_version_has_totals_only() {
        let cur = before();
        let record = diff(None, &cur, &names());
        assert_eq!(record.changes, None);
        assert!(record.audit.is_empty());
        assert_eq!(
            record.text(&header("takeover", None)),
            "zWorkTracker version saved 2 Nov 2026, 14:25 UTC\n\
             Saved by LAPTOP (app 1.3.1). Reason: copy when a computer took over.\n\
             There is no earlier version to compare with, so this gives totals only.\n\
             \n\
             Epoch 0. App schema 2. Data checksum 9f3a01c2d4e5b678.\n\
             \n\
             Now: 2 projects, 2 tasks, 1 time segment.\n"
        );
    }

    #[test]
    fn records_gone_after_a_restore_count_as_removed_and_a_voided_one_too() {
        let prev = before();
        let mut cur = Shared::default();
        cur.projects.insert("p1".into(), project("p1", "Client"));
        let mut voided = prev.clone();
        voided.segments.get_mut("s1").unwrap().edit = Some(SegmentEdit {
            start: at("09:00"),
            end: at("10:00"),
            voided: true,
            reason: String::new(),
            settled: false,
        });
        assert_eq!(
            diff(Some(&prev), &cur, &names()).changes.unwrap(),
            [
                "1 project removed",
                "2 tasks removed",
                "1 time segment removed"
            ]
        );
        assert_eq!(
            diff(Some(&prev), &voided, &names()).changes.unwrap(),
            ["1 time segment removed"]
        );
        assert_eq!(count_text(2_391), "2,391");
        assert_eq!(count_text(1_000_000), "1,000,000");
        assert_eq!(count_text(999), "999");
    }
}
