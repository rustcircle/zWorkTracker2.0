//! Validation of everything entering the core from the UI or from another device's file.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use chrono_tz::Tz;
use serde_json::Value;

use crate::error::CoreError;
use crate::model::{
    AuditEntry, BillablePause, BillingSummary, DeviceFile, DevicePrefix, EpochInfo, IdleGap,
    PauseRequest, Project, RateVersion, RoundingVersion, Segment, SettingEntry, Shared,
    ShiftVersion, Stamp, Tag, Task, Template,
};
use crate::time::Ts;

pub const MAX_NAME: usize = 200;
pub const MAX_PROJECT_NAME: usize = 80;
pub const MAX_DESCRIPTION_BYTES: usize = 512 * 1024;
const MAX_DOC_DEPTH: usize = 24;

const NODE_TYPES: &[&str] = &[
    "doc",
    "paragraph",
    "text",
    "heading",
    "bulletList",
    "orderedList",
    "listItem",
    "taskList",
    "taskItem",
    "hardBreak",
];
const MARK_TYPES: &[&str] = &["bold", "italic"];

fn bad(msg: impl Into<String>) -> CoreError {
    CoreError::Validation(msg.into())
}

pub fn clean_text(raw: &str, field: &str, max: usize) -> Result<String, CoreError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(bad(format!("{field} is required")));
    }
    if trimmed.chars().count() > max {
        return Err(bad(format!("{field} is longer than {max} characters")));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(bad(format!("{field} contains control characters")));
    }
    Ok(trimmed.to_owned())
}

pub fn clean_color(raw: &str) -> Result<String, CoreError> {
    let ok = raw.len() == 7
        && raw.starts_with('#')
        && raw.chars().skip(1).all(|c| c.is_ascii_hexdigit());
    if ok {
        Ok(raw.to_ascii_lowercase())
    } else {
        Err(bad("a color must look like #1a2b3c"))
    }
}

pub fn parse_zone(raw: &str) -> Result<Tz, CoreError> {
    Tz::from_str(raw).map_err(|_| bad(format!("unknown time zone: {raw}")))
}

pub const MAX_RATE_CENTS: i64 = 1_000_000;
pub const MAX_INCREMENT_MINUTES: u32 = 24 * 60;

#[must_use]
pub fn is_currency(raw: &str) -> bool {
    matches!(raw, "EUR" | "USD")
}

pub fn check_rate(v: &RateVersion) -> Result<(), CoreError> {
    if !is_currency(&v.currency) {
        return Err(bad("currency must be EUR or USD"));
    }
    match v.rate_cents {
        Some(c) if !(0..=MAX_RATE_CENTS).contains(&c) => {
            Err(bad("price per hour must be between 0 and 10000.00"))
        }
        _ => Ok(()),
    }
}

pub fn check_rounding(v: &RoundingVersion) -> Result<(), CoreError> {
    check_date(&v.effective_from, "Effective date")?;
    if v.increment_minutes > MAX_INCREMENT_MINUTES {
        return Err(bad("increment must be at most 1440 minutes"));
    }
    Ok(())
}

pub fn clean_non_negative(v: f64, field: &str) -> Result<f64, CoreError> {
    if v.is_finite() && (0.0..=1e9).contains(&v) {
        Ok(v)
    } else {
        Err(bad(format!(
            "{field} must be a number between 0 and 1000000000"
        )))
    }
}

/// Allow-lists the TipTap document so nothing except known nodes and marks is ever stored.
pub fn check_description(doc: &Value) -> Result<(), CoreError> {
    if doc.is_null() {
        return Ok(());
    }
    if serde_json::to_string(doc)?.len() > MAX_DESCRIPTION_BYTES {
        return Err(bad("description is too large"));
    }
    if doc.get("type").and_then(Value::as_str) != Some("doc") {
        return Err(bad("description must be a document"));
    }
    check_node(doc, 0)
}

fn check_node(node: &Value, depth: usize) -> Result<(), CoreError> {
    if depth > MAX_DOC_DEPTH {
        return Err(bad("description is nested too deeply"));
    }
    let kind = node.get("type").and_then(Value::as_str).unwrap_or_default();
    if !NODE_TYPES.contains(&kind) {
        return Err(bad(format!(
            "description contains an unsupported node: {kind}"
        )));
    }
    if let Some(id) = node.get("attrs").and_then(|a| a.get("subtaskId"))
        && !id.is_null()
        && !id.as_str().is_some_and(|i| i.len() <= 64)
    {
        return Err(bad("description contains an invalid subtask id"));
    }
    if let Some(marks) = node.get("marks") {
        let marks = marks
            .as_array()
            .ok_or_else(|| bad("description marks are invalid"))?;
        for m in marks {
            let name = m.get("type").and_then(Value::as_str).unwrap_or_default();
            if !MARK_TYPES.contains(&name) {
                return Err(bad(format!(
                    "description contains an unsupported mark: {name}"
                )));
            }
        }
    }
    if let Some(content) = node.get("content") {
        let content = content
            .as_array()
            .ok_or_else(|| bad("description content is invalid"))?;
        for child in content {
            check_node(child, depth + 1)?;
        }
    }
    Ok(())
}

/// `HH:MM`, 24-hour.
pub fn check_hhmm(raw: &str, field: &str) -> Result<(), CoreError> {
    let parsed = raw.split_once(':').and_then(|(h, m)| {
        if h.len() == 2 && m.len() == 2 {
            Some((h.parse::<u8>().ok()?, m.parse::<u8>().ok()?))
        } else {
            None
        }
    });
    match parsed {
        Some((h, m)) if h < 24 && m < 60 => Ok(()),
        _ => Err(bad(format!("{field} must look like 09:30"))),
    }
}

pub fn check_date(raw: &str, field: &str) -> Result<(), CoreError> {
    chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .map(|_| ())
        .map_err(|_| bad(format!("{field} must look like 2026-03-08")))
}

pub fn check_shift(s: &ShiftVersion) -> Result<(), CoreError> {
    clean_text(&s.name, "Shift name", MAX_PROJECT_NAME)?;
    clean_color(&s.color)?;
    check_hhmm(&s.start, "Shift start")?;
    check_hhmm(&s.end, "Shift end")?;
    check_date(&s.effective_from, "Effective date")?;
    parse_zone(&s.zone)?;
    if s.weekdays.is_empty() && !s.retired {
        return Err(bad("choose at least one weekday"));
    }
    if s.weekdays.iter().any(|d| !(1..=7).contains(d)) {
        return Err(bad("weekdays must be 1 (Monday) to 7 (Sunday)"));
    }
    if s.break_minutes > 24 * 60 {
        return Err(bad("break allowance is longer than a day"));
    }
    Ok(())
}

fn check_project(p: &Project) -> Result<(), CoreError> {
    clean_color(&p.color)?;
    if p.name.chars().count() > MAX_PROJECT_NAME * 4 {
        return Err(bad("project name too long"));
    }
    if let Some(b) = p.budget_hours {
        clean_non_negative(b, "budget")?;
    }
    if let Some(z) = &p.client_zone {
        parse_zone(z)?;
    }
    if p.fixed_fee_cents
        .is_some_and(|c| !(0..=MAX_CENTS).contains(&c))
    {
        return Err(bad("a fixed fee is out of range"));
    }
    if p.fee_currency.as_deref().is_some_and(|c| !is_currency(c)) {
        return Err(bad("a fee currency is not supported"));
    }
    Ok(())
}

/// A hundred billion in either currency: far past real use and far from overflowing in a sum.
const MAX_CENTS: i64 = 10_000_000_000_000;
/// A summary row is one task on one day; a few years' worth bounds any total.
const MAX_ROW_SECS: i64 = 10 * 366 * 86_400;
const MAX_ROWS: usize = 100_000;

fn check_cents(cents: &BTreeMap<String, i64>) -> Result<(), CoreError> {
    for (currency, c) in cents {
        if !is_currency(currency) || !(-MAX_CENTS..=MAX_CENTS).contains(c) {
            return Err(bad("a billing summary holds an invalid amount"));
        }
    }
    Ok(())
}

fn check_summary(b: &BillingSummary) -> Result<(), CoreError> {
    let prefix_ok = !b.prefix.is_empty()
        && b.prefix.len() <= 16
        && b.prefix.bytes().all(|c| c.is_ascii_alphanumeric());
    if !prefix_ok || b.reference != format!("{}-{:03}", b.prefix, b.number) {
        return Err(bad("a billing summary has an invalid reference"));
    }
    if b.number > 1_000_000_000 {
        return Err(bad("a billing summary number is out of range"));
    }
    check_date(&b.from, "a summary date")?;
    check_date(&b.to, "a summary date")?;
    parse_zone(&b.zone)?;
    if b.rows.len() > MAX_ROWS {
        return Err(bad("a billing summary has too many rows"));
    }
    for r in &b.rows {
        check_date(&r.day, "a summary row date")?;
        let secs = [r.exact_secs, r.pause_secs, r.billable_secs];
        if secs.iter().any(|s| !(0..=MAX_ROW_SECS).contains(s)) {
            return Err(bad("a billing summary row is out of range"));
        }
        check_cents(&r.cents)?;
    }
    check_cents(&b.totals)
}

fn check_task(t: &Task) -> Result<(), CoreError> {
    if t.name.chars().count() > MAX_NAME * 4 {
        return Err(bad("task name too long"));
    }
    check_description(&t.description)
}

/// The oldest date a synced record may carry, and how far ahead of this clock it may be.
const EARLIEST: Ts = Ts::EARLIEST;
pub(crate) const MAX_SKEW_SECS: i64 = 2 * 86_400;
const FAR_FUTURE: Ts = Ts(4_102_444_800); // 2100-01-01, for chosen effective dates
/// The latest any clock can be believed to read, so what fails even then is bad whatever the date.
const LAST_CLOCK: Ts = FAR_FUTURE;
pub const MAX_SEGMENT_SECS: i64 = 366 * 86_400;
const MAX_RECORDS: usize = 1_000_000;
const MAX_ID_LEN: usize = 64;
pub(crate) const MAX_EPOCH: u64 = 1 << 40;

/// `YYYY-MM-DD` in UTC, for naming a date in a message.
fn day_text(t: Ts) -> String {
    t.datetime().format("%Y-%m-%d").to_string()
}

/// A date a person chose. This is the one bound for those dates, on entry and in the reader of a sync
/// folder: what passes here is never refused on another device for its date.
pub fn check_chosen_date(t: Ts, what: &str) -> Result<(), CoreError> {
    if t < EARLIEST || t > FAR_FUTURE {
        return Err(bad(format!(
            "{what} must be between {} and {}",
            day_text(EARLIEST),
            day_text(FAR_FUTURE)
        )));
    }
    Ok(())
}

fn check_time(t: Ts, now: Ts, what: &str) -> Result<(), CoreError> {
    if t < EARLIEST || t > now.later(MAX_SKEW_SECS) {
        return Err(bad(format!("{what} is outside the plausible time range")));
    }
    Ok(())
}

fn check_stamp(s: &Stamp, now: Ts) -> Result<(), CoreError> {
    check_time(s.at, now, "a change stamp")?;
    if s.by.len() > MAX_ID_LEN {
        Err(bad("a device id is too long"))
    } else {
        Ok(())
    }
}

fn check_id(id: &str) -> Result<(), CoreError> {
    if id.is_empty() || id.len() > MAX_ID_LEN {
        Err(bad("an id has an invalid length"))
    } else {
        Ok(())
    }
}

/// A record's map key and id must agree, or it could never be found to change or void.
fn check_key(key: &str, id: &str) -> Result<(), CoreError> {
    check_id(id)?;
    if key == id {
        Ok(())
    } else {
        Err(bad("a record's id does not match its key"))
    }
}

pub(crate) fn check_plain(text: &str, max: usize, what: &str) -> Result<(), CoreError> {
    if text.chars().count() > max || text.chars().any(char::is_control) {
        return Err(bad(format!("{what} is too long or has control characters")));
    }
    Ok(())
}

/// Device-level checks on a file from a sync folder, on top of `check_shared`.
pub fn check_device_file(f: &DeviceFile, now: Ts) -> Result<(), CoreError> {
    check_file_as_a_whole(f, now)?;
    if let Some(h) = f.last_heartbeat {
        check_time(h, now, "the heartbeat")?;
    }
    check_shared(&f.data, now)
}

/// What no reader can do without, so a file that fails this is no use to anyone.
fn check_file_as_a_whole(f: &DeviceFile, now: Ts) -> Result<(), CoreError> {
    check_plain(&f.device_name, MAX_NAME, "the device name")?;
    check_time(f.written_at, now, "the file's write time")?;
    if let Some(e) = &f.epoch_info {
        check_epoch(e, now)?;
    }
    check_record_count(&f.data)
}

/// It decides which files every device goes on reading, so one that can't be right is no use to anyone.
pub(crate) fn check_epoch(e: &EpochInfo, now: Ts) -> Result<(), CoreError> {
    if e.epoch > MAX_EPOCH {
        return Err(bad("a restored version's number is out of range"));
    }
    check_id(&e.by)?;
    check_plain(&e.restored_from, MAX_NAME, "a restored version's name")?;
    check_time(e.at, now, "a restored version's date")?;
    check_time(e.base_taken_at, now, "a restored version's date")
}

fn check_record_count(s: &Shared) -> Result<(), CoreError> {
    let total = s.projects.len()
        + s.tasks.len()
        + s.segments.len()
        + s.settings.len()
        + s.shifts.len()
        + s.rates.len()
        + s.rounding.len()
        + s.gaps.len()
        + s.pauses.len()
        + s.tags.len()
        + s.templates.len()
        + s.summaries.len()
        + s.prefixes.len()
        + s.audit_log.len();
    if total > MAX_RECORDS {
        return Err(bad("too many records"));
    }
    Ok(())
}

/// True only when every record passes. Timestamps must stay in a sane window: a far-future stamp would
/// otherwise win every merge and could not be saved. A reader takes what it can with `keep_valid_records`.
pub fn check_shared(s: &Shared, now: Ts) -> Result<(), CoreError> {
    check_record_count(s)?;
    match bad_records(s, now).into_iter().next() {
        Some(record) => Err(bad(record.why)),
        None => Ok(()),
    }
}

/// Takes out of a file from a sync folder whatever can't be trusted, and says what that was, so one bad
/// record never costs a device the rest of the file. An error means the file as a whole is no use.
pub fn keep_valid_records(f: &mut DeviceFile, now: Ts) -> Result<Vec<BadRecord>, CoreError> {
    check_file_as_a_whole(f, now)?;
    let mut records = bad_records(&f.data, now);
    leave_out(&mut f.data, &records);
    records.extend(take_bad_heartbeat(f, now));
    Ok(records)
}

/// A heartbeat is one figure about a device, so a wrong one costs that figure.
fn take_bad_heartbeat(f: &mut DeviceFile, now: Ts) -> Option<BadRecord> {
    let at = f.last_heartbeat?;
    let e = check_time(at, now, "the heartbeat").err()?;
    f.last_heartbeat = None;
    Some(BadRecord {
        kind: RecordKind::Heartbeat,
        id: f.device_id.clone(),
        name: "the last heartbeat".to_owned(),
        why: e.to_string(),
        gone: true,
        ahead: check_time(at, LAST_CLOCK, "the heartbeat").is_ok(),
    })
}

/// True when all that stops a file being read is a date ahead of this clock. Whether that clock or the
/// one that wrote the file is wrong, or the file is damaged, only a person can tell.
#[must_use]
pub fn only_dated_ahead(f: &DeviceFile, now: Ts) -> bool {
    check_file_as_a_whole(f, now).is_err() && check_file_as_a_whole(f, LAST_CLOCK).is_ok()
}

/// What `prepare_own_file` found.
#[derive(Debug, Default)]
pub struct OwnFileCheck {
    /// Bad whatever the date. No reader takes these, so they stay out of the file.
    pub left_out: Vec<BadRecord>,
    /// Dated ahead of this clock. They stay in the file, as this clock may be the wrong one, and each
    /// reader judges them by its own.
    pub ahead: Vec<BadRecord>,
    /// Why no reader will take the file at all.
    pub refused: Option<String>,
}

/// Gets this device's own file ready to write, checked as a reader will check it. What goes out is what
/// even a reader on an older version takes, which refuses a whole file for one bad record.
pub fn prepare_own_file(f: &mut DeviceFile, now: Ts) -> OwnFileCheck {
    let left_out = bad_records(&f.data, LAST_CLOCK);
    leave_out(&mut f.data, &left_out);
    let ahead = bad_records(&f.data, now);
    take_bad_heartbeat(f, now);
    let refused = check_file_as_a_whole(f, now).err().map(|e| e.to_string());
    OwnFileCheck {
        left_out,
        ahead,
        refused,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordKind {
    Project,
    Task,
    Segment,
    Setting,
    Shift,
    Rate,
    Rounding,
    Gap,
    Pause,
    Tag,
    Template,
    Summary,
    PauseRequest,
    Prefix,
    Audit,
    Heartbeat,
}

/// A record no device should take from a sync folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BadRecord {
    pub kind: RecordKind,
    /// Its key in the store.
    pub id: String,
    /// Which one it is, in words a person can find it by.
    pub name: String,
    pub why: String,
    /// Nothing counts it, as it was deleted or voided or is a line in the record of changes, so a device
    /// that holds it needn't ask for anything to be done about it.
    pub gone: bool,
    /// Bad only for a date ahead of the clock that judged it, so it can be read when that date comes.
    pub ahead: bool,
}

/// A name as it goes in a message. It may come from a file that is damaged, so it is kept short and plain.
fn plain(name: &str) -> String {
    name.chars().filter(|c| !c.is_control()).take(80).collect()
}

fn quoted(name: &str) -> String {
    format!("\"{}\"", plain(name))
}

/// Every record that fails its own checks. Each record is judged by itself, so taking some out never
/// makes another one bad.
#[must_use]
pub fn bad_records(s: &Shared, now: Ts) -> Vec<BadRecord> {
    let mut out = Vec::new();
    let mut note = |kind,
                    id: &str,
                    check: &dyn Fn(Ts) -> Result<(), CoreError>,
                    gone,
                    name: &dyn Fn() -> String| {
        if let Err(e) = check(now) {
            out.push(BadRecord {
                kind,
                id: id.to_owned(),
                name: name(),
                why: plain(&e.to_string()),
                gone,
                ahead: check(LAST_CLOCK).is_ok(),
            });
        }
    };
    let task = |id: &str| s.tasks.get(id);
    let task_name = |id: &str| task(id).map_or_else(|| "a task".to_owned(), |t| quoted(&t.name));
    let task_gone = |id: &str| task(id).is_some_and(|t| !t.is_live());
    let voided = |seg: &Segment| seg.edit.as_ref().is_some_and(|e| e.voided);
    for (id, p) in &s.projects {
        let name = || format!("project {}", quoted(&p.name));
        note(
            RecordKind::Project,
            id,
            &|at| check_project_record(id, p, at),
            p.deleted,
            &name,
        );
    }
    for (id, t) in &s.tasks {
        let name = || format!("task {} and the time on it", quoted(&t.name));
        note(
            RecordKind::Task,
            id,
            &|at| check_task_record(id, t, at),
            !t.is_live(),
            &name,
        );
    }
    for (id, seg) in &s.segments {
        let name = || {
            format!(
                "the time on {} from {}",
                task_name(&seg.task_id),
                day_text(seg.start)
            )
        };
        let gone = voided(seg) || task_gone(&seg.task_id);
        note(
            RecordKind::Segment,
            id,
            &|at| check_segment_record(id, seg, at),
            gone,
            &name,
        );
    }
    for (key, entry) in &s.settings {
        let name = || format!("the setting {}", plain(key));
        note(
            RecordKind::Setting,
            key,
            &|at| check_setting_record(key, entry, at),
            false,
            &name,
        );
    }
    for (id, v) in &s.shifts {
        let name = || format!("shift {}", quoted(&v.name));
        note(
            RecordKind::Shift,
            id,
            &|at| check_shift_record(id, v, at),
            v.deleted,
            &name,
        );
    }
    for (id, v) in &s.rates {
        let project = || {
            s.projects
                .get(&v.project_id)
                .map_or_else(|| "a project".to_owned(), |p| quoted(&p.name))
        };
        let name = || {
            format!(
                "the price of {} from {}",
                project(),
                day_text(v.effective_from)
            )
        };
        note(
            RecordKind::Rate,
            id,
            &|at| check_rate_record(id, v, at),
            v.deleted,
            &name,
        );
    }
    for (id, v) in &s.rounding {
        let name = || format!("the rounding from {}", quoted(&v.effective_from));
        note(
            RecordKind::Rounding,
            id,
            &|at| check_rounding_record(id, v, at),
            v.deleted,
            &name,
        );
    }
    for (id, g) in &s.gaps {
        let name = || {
            format!(
                "time away on {} from {}",
                task_name(&g.task_id),
                day_text(g.start)
            )
        };
        let gone = task_gone(&g.task_id) || s.segments.get(&g.segment_id).is_some_and(voided);
        note(
            RecordKind::Gap,
            id,
            &|at| check_gap_record(id, g, at),
            gone,
            &name,
        );
    }
    for (id, p) in &s.pauses {
        let name = || {
            format!(
                "the billable pause on {} from {}",
                task_name(&p.task_id),
                day_text(p.start)
            )
        };
        let gone = p.deleted || task_gone(&p.task_id);
        note(
            RecordKind::Pause,
            id,
            &|at| check_pause_record(id, p, at),
            gone,
            &name,
        );
    }
    for (id, t) in &s.tags {
        let name = || format!("tag {}", quoted(&t.name));
        note(
            RecordKind::Tag,
            id,
            &|at| check_tag_record(id, t, at),
            t.deleted,
            &name,
        );
    }
    for (id, t) in &s.templates {
        let name = || format!("template {}", quoted(&t.name));
        note(
            RecordKind::Template,
            id,
            &|at| check_template_record(id, t, at),
            t.deleted,
            &name,
        );
    }
    for (id, b) in &s.summaries {
        let name = || format!("summary {}", quoted(&b.reference));
        note(
            RecordKind::Summary,
            id,
            &|at| check_summary_record(id, b, at),
            b.voided,
            &name,
        );
    }
    for (id, q) in &s.pause_requests {
        let name = || format!("a pause asked for on {}", task_name(&q.task_id));
        let gone = task_gone(&q.task_id);
        note(
            RecordKind::PauseRequest,
            id,
            &|at| check_pause_request_record(id, q, at),
            gone,
            &name,
        );
    }
    for (device, p) in &s.prefixes {
        let name = || "a device's reference prefix".to_owned();
        note(
            RecordKind::Prefix,
            device,
            &|at| check_prefix_record(device, p, at),
            false,
            &name,
        );
    }
    for (id, a) in &s.audit_log {
        let name = || "a line in the record of changes".to_owned();
        note(
            RecordKind::Audit,
            id,
            &|at| check_audit_record(id, a, at),
            true,
            &name,
        );
    }
    out
}

/// A task and its time are read together. With only one of them a device can see a running task with no
/// timer, and putting that right would stop the timer on the device that has it.
fn leave_out(s: &mut Shared, records: &[BadRecord]) {
    let tasks: BTreeSet<&str> = records
        .iter()
        .filter(|r| r.kind == RecordKind::Task)
        .map(|r| r.id.as_str())
        .collect();
    if !tasks.is_empty() {
        s.segments
            .retain(|_, seg| !tasks.contains(seg.task_id.as_str()));
    }
    for r in records {
        let id = r.id.as_str();
        match r.kind {
            RecordKind::Project => drop(s.projects.remove(id)),
            RecordKind::Task => drop(s.tasks.remove(id)),
            RecordKind::Segment => drop(s.segments.remove(id)),
            RecordKind::Setting => drop(s.settings.remove(id)),
            RecordKind::Shift => drop(s.shifts.remove(id)),
            RecordKind::Rate => drop(s.rates.remove(id)),
            RecordKind::Rounding => drop(s.rounding.remove(id)),
            RecordKind::Gap => drop(s.gaps.remove(id)),
            RecordKind::Pause => drop(s.pauses.remove(id)),
            RecordKind::Tag => drop(s.tags.remove(id)),
            RecordKind::Template => drop(s.templates.remove(id)),
            RecordKind::Summary => drop(s.summaries.remove(id)),
            RecordKind::PauseRequest => drop(s.pause_requests.remove(id)),
            RecordKind::Prefix => drop(s.prefixes.remove(id)),
            RecordKind::Audit => drop(s.audit_log.remove(id)),
            RecordKind::Heartbeat => {}
        }
    }
}

fn check_project_record(key: &str, p: &Project, now: Ts) -> Result<(), CoreError> {
    check_key(key, &p.id)?;
    check_project(p)?;
    check_plain(&p.name, MAX_PROJECT_NAME * 4, "a project name")?;
    check_time(p.created_at, now, "a project date")?;
    for clock in [
        &p.identity_stamp,
        &p.billing_stamp,
        &p.budget_stamp,
        &p.archive_stamp,
    ]
    .into_iter()
    .flatten()
    {
        check_stamp(clock, now)?;
    }
    check_stamp(&p.stamp, now)
}

fn check_task_record(key: &str, t: &Task, now: Ts) -> Result<(), CoreError> {
    check_key(key, &t.id)?;
    check_task(t)?;
    check_plain(&t.name, MAX_NAME * 4, "a task name")?;
    check_time(t.created_at, now, "a task date")?;
    for stamp in [
        &t.name_stamp,
        &t.project_stamp,
        &t.description_stamp,
        &t.deleted_stamp,
        &t.state_stamp,
        &t.tags_stamp,
        &t.active_subtask_stamp,
    ] {
        check_stamp(stamp, now)?;
    }
    if let Some(stamp) = &t.discarded {
        check_stamp(stamp, now)?;
    }
    if let Some(at) = t.stopped_at {
        check_time(at, now, "a stop time")?;
    }
    if t.tags.len() > 64 {
        return Err(bad("a task has too many tags"));
    }
    Ok(())
}

fn check_segment_record(key: &str, seg: &Segment, now: Ts) -> Result<(), CoreError> {
    check_key(key, &seg.id)?;
    check_time(seg.start, now, "a segment start")?;
    if let Some(end) = seg.end {
        check_time(end, now, "a segment end")?;
        if end < seg.start || end.secs() - seg.start.secs() > MAX_SEGMENT_SECS {
            return Err(bad("a segment has an impossible duration"));
        }
    }
    check_stamp(&seg.edit_stamp, now)?;
    if let Some(e) = &seg.edit {
        check_span(e.start, e.end, now, "a correction")?;
        check_plain(&e.reason, MAX_NAME * 8, "a reason")?;
    }
    Ok(())
}

fn check_setting_record(key: &str, entry: &SettingEntry, now: Ts) -> Result<(), CoreError> {
    crate::machine::check_setting(key, &entry.value)?;
    check_stamp(&entry.stamp, now)
}

fn check_shift_record(key: &str, v: &ShiftVersion, now: Ts) -> Result<(), CoreError> {
    check_key(key, &v.id)?;
    check_shift(v)?;
    check_stamp(&v.stamp, now)
}

fn check_rate_record(key: &str, v: &RateVersion, now: Ts) -> Result<(), CoreError> {
    check_key(key, &v.id)?;
    check_stamp(&v.stamp, now)?;
    check_chosen_date(v.effective_from, "a price's start date")?;
    check_rate(v)
}

fn check_rounding_record(key: &str, v: &RoundingVersion, now: Ts) -> Result<(), CoreError> {
    check_key(key, &v.id)?;
    check_stamp(&v.stamp, now)?;
    check_rounding(v)
}

fn check_span(start: Ts, end: Ts, now: Ts, what: &str) -> Result<(), CoreError> {
    check_time(start, now, what)?;
    check_time(end, now, what)?;
    if end < start || end.secs() - start.secs() > MAX_SEGMENT_SECS {
        return Err(bad(format!("{what} has an impossible duration")));
    }
    Ok(())
}

fn check_gap_record(key: &str, g: &IdleGap, now: Ts) -> Result<(), CoreError> {
    check_key(key, &g.id)?;
    check_span(g.start, g.end, now, "time away")?;
    check_stamp(&g.decision_stamp, now)
}

fn check_pause_record(key: &str, p: &BillablePause, now: Ts) -> Result<(), CoreError> {
    check_key(key, &p.id)?;
    check_span(p.start, p.end, now, "a billable pause")?;
    check_plain(&p.note, MAX_NAME * 8, "a note")?;
    check_stamp(&p.stamp, now)
}

fn check_tag_record(key: &str, t: &Tag, now: Ts) -> Result<(), CoreError> {
    check_key(key, &t.id)?;
    check_plain(&t.name, MAX_NAME, "a tag name")?;
    clean_color(&t.color)?;
    check_stamp(&t.stamp, now)
}

fn check_template_record(key: &str, t: &Template, now: Ts) -> Result<(), CoreError> {
    check_key(key, &t.id)?;
    check_plain(&t.name, MAX_NAME * 4, "a template name")?;
    check_description(&t.description)?;
    check_stamp(&t.stamp, now)
}

fn check_summary_record(key: &str, b: &BillingSummary, now: Ts) -> Result<(), CoreError> {
    check_key(key, &b.id)?;
    check_summary(b)?;
    check_plain(&b.reference, MAX_NAME, "a reference")?;
    check_time(b.created_at, now, "a summary date")?;
    check_stamp(&b.stamp, now)
}

fn check_pause_request_record(key: &str, q: &PauseRequest, now: Ts) -> Result<(), CoreError> {
    check_key(key, &q.id)?;
    check_id(&q.task_id)?;
    check_id(&q.segment_id)?;
    check_id(&q.requested_by)?;
    check_time(q.at, now, "a pause request")?;
    check_stamp(&q.stamp, now)
}

fn check_prefix_record(device: &str, p: &DevicePrefix, now: Ts) -> Result<(), CoreError> {
    check_id(device)?;
    check_plain(&p.prefix, 16, "a reference prefix")?;
    check_stamp(&p.stamp, now)
}

fn check_audit_record(key: &str, a: &AuditEntry, now: Ts) -> Result<(), CoreError> {
    check_key(key, &a.id)?;
    check_time(a.at, now, "an audit entry")?;
    check_plain(&a.reason, MAX_NAME * 8, "an audit reason")
}

/// Used for the local file, where an unusable rate or rounding record is ignored when computing
/// rather than treated as file corruption, so a single bad record can never cost the user their data.
pub fn check_core(s: &Shared) -> Result<(), CoreError> {
    for (id, p) in &s.projects {
        if *id != p.id {
            return Err(bad("project id mismatch"));
        }
        check_project(p)?;
    }
    for (id, t) in &s.tasks {
        if *id != t.id {
            return Err(bad("task id mismatch"));
        }
        check_task(t)?;
    }
    for (id, v) in &s.shifts {
        if *id != v.id {
            return Err(bad("shift id mismatch"));
        }
        check_shift(v)?;
    }
    for (id, seg) in &s.segments {
        if *id != seg.id {
            return Err(bad("segment id mismatch"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn r10_a_group_clock_ahead_is_judged_as_the_record_is_and_a_copy_without_any_passes() {
        let now = Ts::parse("2026-03-01T09:00:00Z").unwrap();
        let stamp = |at: &str| Stamp {
            at: Ts::parse(at).unwrap(),
            by: "a".into(),
        };
        let mut s = Shared::default();
        let project: Project = serde_json::from_value(json!({
            "id": "p", "name": "P", "color": "#112233", "createdAt": "2026-01-01T00:00:00Z",
            "stamp": {"at": "2026-01-02T00:00:00Z", "by": "old"},
        }))
        .unwrap();
        s.projects.insert("p".into(), project.clone());
        check_shared(&s, now).unwrap();
        for bad in 0..4 {
            let mut p = project.clone();
            let clock = [
                &mut p.identity_stamp,
                &mut p.billing_stamp,
                &mut p.budget_stamp,
                &mut p.archive_stamp,
            ]
            .into_iter()
            .nth(bad)
            .unwrap();
            *clock = Some(stamp("2026-03-01T09:00:00Z"));
            s.projects.insert("p".into(), p.clone());
            check_shared(&s, now).unwrap();
            // A clock this far ahead would win every merge, so the record stays out as a whole.
            let clock = [
                &mut p.identity_stamp,
                &mut p.billing_stamp,
                &mut p.budget_stamp,
                &mut p.archive_stamp,
            ]
            .into_iter()
            .nth(bad)
            .unwrap();
            *clock = Some(stamp("2030-01-01T00:00:00Z"));
            s.projects.insert("p".into(), p);
            let records = bad_records(&s, now);
            assert_eq!(records.len(), 1, "clock {bad}");
            assert_eq!(records[0].kind, RecordKind::Project);
            assert!(records[0].why.contains("change stamp"), "{:?}", records[0]);
        }
    }

    #[test]
    fn text_rules() {
        assert!(clean_text("   ", "Name", 10).is_err());
        assert_eq!(clean_text("  ok ", "Name", 10).unwrap(), "ok");
        assert!(clean_text("abcdefghijk", "Name", 10).is_err());
        assert!(clean_text("a\u{0007}b", "Name", 10).is_err());
    }

    #[test]
    fn colors_and_zones() {
        assert_eq!(clean_color("#AABBCC").unwrap(), "#aabbcc");
        assert!(clean_color("red").is_err());
        assert!(clean_color("#12345g").is_err());
        assert!(parse_zone("America/New_York").is_ok());
        assert!(parse_zone("Asia/Kolkata").is_ok());
        assert!(parse_zone("Mars/Olympus").is_err());
    }

    #[test]
    fn description_allow_list() {
        let ok = json!({"type":"doc","content":[{"type":"orderedList","content":[{"type":"listItem",
            "content":[{"type":"paragraph","content":[{"type":"text","text":"a","marks":[{"type":"bold"}]}]}]}]}]});
        assert!(check_description(&ok).is_ok());
        assert!(check_description(&Value::Null).is_ok());
        let img =
            json!({"type":"doc","content":[{"type":"image","attrs":{"src":"http://x/y.png"}}]});
        assert!(check_description(&img).is_err());
        let link = json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"a",
            "marks":[{"type":"link","attrs":{"href":"http://x"}}]}]}]});
        assert!(check_description(&link).is_err());
        assert!(check_description(&json!({"type":"paragraph"})).is_err());
        assert!(check_description(&json!("<b>html</b>")).is_err());
    }

    /// A store with one good record of every kind, and a file holding it.
    fn file_of_every_kind() -> (DeviceFile, Ts) {
        use crate::machine::tests::{ctx, project_input, task_input};
        let c = ctx("2026-03-02T09:00:00Z");
        let mut s = Shared::default();
        let p = s.create_project(&c, &project_input("Acme")).unwrap();
        let t = s.create_task(&c, &task_input("Write", &p)).unwrap();
        s.set_rate(
            &c,
            &crate::machine::RateInput {
                project_id: p.clone(),
                rate_cents: Some(5000),
                currency: "EUR".into(),
                effective_from: None,
                confirm_retroactive: false,
            },
        )
        .unwrap();
        let seg = s
            .add_manual_entry(
                &c,
                &crate::adjust::ManualEntryInput {
                    task_id: t.clone(),
                    start: Ts::parse("2026-03-01T09:00:00Z").unwrap(),
                    end: Ts::parse("2026-03-01T10:00:00Z").unwrap(),
                    reason: String::new(),
                    subtask_id: None,
                    allow_overlap: true,
                },
            )
            .unwrap();
        s.add_gap(
            &c,
            &seg,
            Ts::parse("2026-03-01T09:10:00Z").unwrap(),
            Ts::parse("2026-03-01T09:20:00Z").unwrap(),
            crate::model::GapKind::Idle,
        )
        .unwrap();
        s.mark_billable_pause(
            &c,
            &crate::adjust::PauseInput {
                task_id: t.clone(),
                start: Ts::parse("2026-03-01T11:00:00Z").unwrap(),
                end: Ts::parse("2026-03-01T12:00:00Z").unwrap(),
                note: String::new(),
            },
        )
        .unwrap();
        s.set_setting(&c, "idleMinutes", json!(5)).unwrap();
        s.save_shift(
            &c,
            &crate::machine::ShiftInput {
                shift_id: None,
                effective_from: "2026-01-01".into(),
                name: "Day".into(),
                color: "#112233".into(),
                start: "09:00".into(),
                end: "17:00".into(),
                weekdays: vec![1],
                zone: "UTC".into(),
                break_minutes: 0,
                retired: false,
            },
        )
        .unwrap();
        s.set_rounding(
            &c,
            &crate::machine::RoundingInput {
                project_id: None,
                effective_from: "2026-03-02".into(),
                increment_minutes: 15,
                method: crate::model::RoundMethod::Up,
                confirm_retroactive: false,
            },
            "2026-03-02",
        )
        .unwrap();
        s.seed_tags();
        s.save_template(
            &c,
            &crate::catalog::TemplateInput {
                id: None,
                name: "Usual".into(),
                project_id: None,
                description: Value::Null,
                tags: Vec::new(),
            },
        )
        .unwrap();
        s.set_device_prefix(&c, "A").unwrap();
        s.create_summary_above(
            &c,
            &crate::summary::SummaryRequest {
                project_id: p,
                from: "2026-03-01".into(),
                to: "2026-03-01".into(),
                zone: "UTC".into(),
            },
            &BTreeMap::new(),
        )
        .unwrap();
        s.pause_requests.insert(
            "q".into(),
            PauseRequest {
                id: "q".into(),
                task_id: t,
                segment_id: seg,
                requested_by: "other".into(),
                at: c.now,
                stamp: c.stamp(),
            },
        );
        assert!(!s.audit_log.is_empty());
        let file = DeviceFile {
            schema_version: crate::model::SCHEMA_VERSION,
            device_id: "d".into(),
            device_name: "pc".into(),
            written_at: c.now,
            last_heartbeat: Some(c.now),
            epoch_info: None,
            data: s,
        };
        (file, c.now)
    }

    /// One bad record of every kind in a file, each marked by a date no reader takes.
    fn damage_every_kind(s: &mut Shared) {
        let long_ago = Ts::parse("1999-01-01T00:00:00Z").unwrap();
        let stamp = || Stamp {
            at: long_ago,
            by: "x".into(),
        };
        s.projects.values_mut().next().unwrap().stamp = stamp();
        s.tasks.values_mut().next().unwrap().state_stamp = stamp();
        s.segments.values_mut().next().unwrap().start = long_ago;
        s.settings.values_mut().next().unwrap().stamp = stamp();
        s.shifts.values_mut().next().unwrap().stamp = stamp();
        s.rates.values_mut().next().unwrap().effective_from = long_ago;
        s.rounding.values_mut().next().unwrap().stamp = stamp();
        s.gaps.values_mut().next().unwrap().start = long_ago;
        s.pauses.values_mut().next().unwrap().start = long_ago;
        s.tags.values_mut().next().unwrap().stamp = stamp();
        s.templates.values_mut().next().unwrap().stamp = stamp();
        s.summaries.values_mut().next().unwrap().stamp = stamp();
        s.pause_requests.values_mut().next().unwrap().at = long_ago;
        s.prefixes.values_mut().next().unwrap().stamp = stamp();
        s.audit_log.values_mut().next().unwrap().at = long_ago;
    }

    #[test]
    fn r1_the_reader_can_leave_out_any_kind_of_record_and_what_is_left_passes_the_strict_check() {
        let (mut file, now) = file_of_every_kind();
        check_device_file(&file, now).unwrap();
        assert!(
            keep_valid_records(&mut file.clone(), now)
                .unwrap()
                .is_empty()
        );
        assert!(prepare_own_file(&mut file.clone(), now).left_out.is_empty());

        damage_every_kind(&mut file.data);
        file.last_heartbeat = Some(Ts::parse("2031-01-01T00:00:00Z").unwrap());
        assert!(check_device_file(&file, now).is_err());
        let as_written = file.clone();
        let mut own = file.clone();

        let records = keep_valid_records(&mut file, now).unwrap();
        check_device_file(&file, now).unwrap();
        let kinds: Vec<RecordKind> = records.iter().map(|r| r.kind).collect();
        for kind in [
            RecordKind::Project,
            RecordKind::Task,
            RecordKind::Segment,
            RecordKind::Setting,
            RecordKind::Shift,
            RecordKind::Rate,
            RecordKind::Rounding,
            RecordKind::Gap,
            RecordKind::Pause,
            RecordKind::Tag,
            RecordKind::Template,
            RecordKind::Summary,
            RecordKind::PauseRequest,
            RecordKind::Prefix,
            RecordKind::Audit,
            RecordKind::Heartbeat,
        ] {
            assert!(kinds.contains(&kind), "{kind:?}: {records:?}");
        }
        assert!(file.data.segments.is_empty() && file.data.rates.is_empty());
        assert_eq!(file.last_heartbeat, None);
        // A second pass finds nothing, and every record that wasn't bad is still there.
        assert!(keep_valid_records(&mut file, now).unwrap().is_empty());
        assert!(
            file.data.tags.len() + 1 == as_written.data.tags.len(),
            "only the one tag"
        );

        // The writer leaves out the same ones, as these are bad whatever the date.
        let check = prepare_own_file(&mut own, now);
        assert_eq!(
            check.left_out.len(),
            records.len() - 1,
            "all but the heartbeat"
        );
        assert_eq!(check.refused, None);
        check_device_file(&own, now).unwrap();
    }

    #[test]
    fn r1_a_file_the_reader_cannot_use_as_a_whole_is_refused_whole() {
        let (file, now) = file_of_every_kind();
        let mut bad_name = file.clone();
        bad_name.device_name = "a\u{0007}b".into();
        assert!(keep_valid_records(&mut bad_name, now).is_err());
        assert!(prepare_own_file(&mut bad_name, now).refused.is_some());
        let mut far_future = file;
        far_future.written_at = now.later(MAX_SKEW_SECS + 1);
        assert!(keep_valid_records(&mut far_future, now).is_err());
    }

    #[test]
    fn r1_a_name_from_a_damaged_file_is_kept_short_and_plain_in_a_message() {
        let (mut file, now) = file_of_every_kind();
        let p = file.data.projects.values_mut().next().unwrap();
        p.name = format!("a\u{0007}{}", "x".repeat(5000));
        let records = keep_valid_records(&mut file, now).unwrap();
        let name = &records[0].name;
        assert!(name.chars().count() < 100, "{name}");
        assert!(!name.chars().any(char::is_control), "{name}");
    }

    #[test]
    fn r1_a_reason_from_a_damaged_file_is_kept_short_and_plain_in_a_message() {
        let (mut file, now) = file_of_every_kind();
        let key = format!("a\u{0007}{}", "x".repeat(5000));
        file.data.settings.insert(
            key,
            SettingEntry {
                value: json!(1),
                stamp: Stamp {
                    at: now,
                    by: "x".into(),
                },
            },
        );
        let records = keep_valid_records(&mut file, now).unwrap();
        let why = &records[0].why;
        assert!(why.chars().count() < 100, "{why}");
        assert!(!why.chars().any(char::is_control), "{why}");
    }

    #[test]
    fn the_marker_of_a_task_deleted_with_its_time_is_checked_like_any_stamp() {
        use crate::machine::tests::{ctx, project_input, task_input};
        let c = ctx("2026-01-01T08:00:00Z");
        let mut s = Shared::default();
        let p = s.create_project(&c, &project_input("A")).unwrap();
        let t = s.create_task(&c, &task_input("T", &p)).unwrap();
        s.delete_task(&c, &t).unwrap();
        check_shared(&s, c.now).unwrap();
        let task = s.tasks.get_mut(&t).unwrap();
        task.discarded = Some(Stamp {
            at: c.now.later(MAX_SKEW_SECS + 1),
            by: "x".into(),
        });
        assert!(check_shared(&s, c.now).is_err(), "a far future date");
        let task = s.tasks.get_mut(&t).unwrap();
        task.discarded = Some(Stamp {
            at: c.now,
            by: "x".repeat(MAX_ID_LEN + 1),
        });
        assert!(check_shared(&s, c.now).is_err(), "too long a device id");
    }
}
