//! The UI-thread half: applies an [`Update`] to the screen's models, writing only rows and values that changed.
//!
//! The hub knows nothing of Slint. The UI implements [`RowSink`] over its list models (a `VecModel` wrapper) and
//! [`CellSink`] over its properties (any `FnMut(&T)` is one); [`KeyedList`] turns a new row vector into the fewest
//! `set_row`/`insert`/`remove` calls against what the sink holds, and [`ValueCell`] writes a value only when it
//! differs from the last one written. Each call reports what it wrote, so tests can count writes.

use std::collections::HashMap;
use std::hash::Hash;
use std::ops::Add;

use chrono::NaiveDate;
use zwt_ui_logic::day::{DayChip, DayPiece};
use zwt_ui_logic::share::ShareRow;

use crate::views::{DayHead, DayPage, Running, ShareHead, StoppedPage, TaskRow};
use crate::worker::Update;

/// A list model the hub writes to. Indexes are positions in the model as it is when the call is made.
pub trait RowSink<T> {
    /// The rows the model holds now. A model whose length differs from the hub's copy is rebuilt from scratch.
    /// [`GroupedSink`] answers with the rows written through it, not with its sinks' lengths: a change made to one of
    /// its sinks by anything else goes unnoticed, so nothing else may write them.
    fn len(&self) -> usize;
    /// Replaces the row at `index` (`index < len()`).
    fn set_row(&mut self, index: usize, row: &T);
    /// Inserts a row at `index` (`index <= len()`).
    fn insert(&mut self, index: usize, row: &T);
    /// Removes the row at `index` (`index < len()`).
    fn remove(&mut self, index: usize);

    /// True when the model holds no rows.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A single value the hub writes to: a property or a group of properties.
pub trait CellSink<T> {
    fn write(&mut self, value: &T);
}

impl<T, F: FnMut(&T)> CellSink<T> for F {
    fn write(&mut self, value: &T) {
        self(value);
    }
}

/// A row with a stable identity, so a changed row is rewritten in place and a moved one is moved.
pub trait Keyed {
    type Key: Eq + Hash;
    fn key(&self) -> &Self::Key;
}

impl Keyed for TaskRow {
    type Key = String;
    fn key(&self) -> &String {
        &self.id
    }
}

impl Keyed for DayPiece {
    type Key = String;
    fn key(&self) -> &String {
        &self.id
    }
}

impl Keyed for DayChip {
    type Key = NaiveDate;
    fn key(&self) -> &NaiveDate {
        &self.date
    }
}

impl Keyed for ShareRow {
    type Key = String;
    fn key(&self) -> &String {
        &self.key
    }
}

/// What one list update wrote.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Writes {
    pub set: usize,
    pub inserted: usize,
    pub removed: usize,
}

impl Writes {
    /// Every call made on the sink.
    #[must_use]
    pub const fn total(self) -> usize {
        self.set + self.inserted + self.removed
    }
}

impl Add for Writes {
    type Output = Self;

    /// The writes of two sinks together.
    fn add(self, other: Self) -> Self {
        Self {
            set: self.set + other.set,
            inserted: self.inserted + other.inserted,
            removed: self.removed + other.removed,
        }
    }
}

/// The hub's copy of a list model, and the keyed diff that keeps the model equal to the newest rows.
#[derive(Clone, Debug)]
pub struct KeyedList<T> {
    rows: Vec<T>,
}

impl<T> Default for KeyedList<T> {
    fn default() -> Self {
        Self { rows: Vec::new() }
    }
}

impl<T: Keyed + PartialEq> KeyedList<T> {
    /// The rows the sink was last given.
    #[must_use]
    pub fn rows(&self) -> &[T] {
        &self.rows
    }

    /// Makes `sink` hold `next`, in order, with the fewest calls this finds: rows whose key is gone are removed,
    /// the longest run of rows already in the right order stays put, the other surviving rows are moved (one
    /// remove and one insert each), new rows are inserted, and a kept row is rewritten only if it differs.
    /// Applying the same rows twice writes nothing the second time.
    pub fn apply(&mut self, next: Vec<T>, sink: &mut dyn RowSink<T>) -> Writes {
        let mut w = Writes::default();
        if sink.len() != self.rows.len() {
            for i in (0..sink.len()).rev() {
                sink.remove(i);
                w.removed += 1;
            }
            self.rows.clear();
        }
        // Where each row now in the model belongs in `next` (the first row with its key), if anywhere.
        let target: Vec<Option<usize>> = {
            let mut index: HashMap<&T::Key, usize> = HashMap::with_capacity(next.len());
            for (i, row) in next.iter().enumerate() {
                index.entry(row.key()).or_insert(i);
            }
            self.rows
                .iter()
                .map(|r| index.get(r.key()).copied())
                .collect()
        };
        let keep = longest_increasing(&target);
        for i in (0..self.rows.len()).rev() {
            if !keep.get(i).copied().unwrap_or(false) {
                self.rows.remove(i);
                sink.remove(i);
                w.removed += 1;
            }
        }
        // Now every row left is in `next`, in `next`'s order: walk `next`, rewriting or inserting.
        let len = next.len();
        for (i, row) in next.into_iter().enumerate() {
            match self.rows.get_mut(i) {
                Some(current) if current.key() == row.key() => {
                    if *current != row {
                        sink.set_row(i, &row);
                        *current = row;
                        w.set += 1;
                    }
                }
                _ => {
                    sink.insert(i, &row);
                    self.rows.insert(i, row);
                    w.inserted += 1;
                }
            }
        }
        // Only reachable with repeated keys in `next`: whatever is left past its end goes.
        while self.rows.len() > len {
            self.rows.pop();
            sink.remove(self.rows.len());
            w.removed += 1;
        }
        w
    }
}

/// Marks the rows of the longest strictly increasing run of targets (rows with no target are never in it).
fn longest_increasing(targets: &[Option<usize>]) -> Vec<bool> {
    // tail_value[k]: the smallest last target of a run of length k + 1; tail_row[k]: the row holding it.
    let mut tail_value: Vec<usize> = Vec::new();
    let mut tail_row: Vec<usize> = Vec::new();
    let mut back: Vec<Option<usize>> = vec![None; targets.len()];
    for (row, target) in targets.iter().enumerate() {
        let Some(t) = *target else {
            continue;
        };
        let at = tail_value.partition_point(|&v| v < t);
        if let Some(b) = back.get_mut(row) {
            *b = at.checked_sub(1).and_then(|p| tail_row.get(p).copied());
        }
        match (tail_value.get_mut(at), tail_row.get_mut(at)) {
            (Some(v), Some(r)) => {
                *v = t;
                *r = row;
            }
            _ => {
                tail_value.push(t);
                tail_row.push(row);
            }
        }
    }
    let mut keep = vec![false; targets.len()];
    let mut cursor = tail_row.last().copied();
    while let Some(row) = cursor {
        if let Some(k) = keep.get_mut(row) {
            *k = true;
        }
        cursor = back.get(row).copied().flatten();
    }
    keep
}

/// A [`RowSink`] that keeps one list in several sinks, one per group, each holding the rows of its group in the
/// list's order: the task list, whose Active, Drafts and Stopped groups the UI draws as lists of their own (1.x
/// TaskRail.tsx). The hub writes the whole list to it as to any sink; the call lands in the sink of the row's group,
/// at the row's place among the rows of that group. A row rewritten into another group moves there: one remove and
/// one insert. It holds the group of every row, so its sinks must not be written by anything else.
pub struct GroupedSink<T, S> {
    group_of: fn(&T) -> usize,
    /// The group of each row of the list, in the list's order.
    groups: Vec<usize>,
    /// One sink per group, by the group's index; a row whose group has no sink is kept out of every sink.
    sinks: Vec<S>,
}

impl<T, S> GroupedSink<T, S> {
    /// An empty list over `sinks` (empty too): `group_of` says which sink a row goes to.
    #[must_use]
    pub const fn new(group_of: fn(&T) -> usize, sinks: Vec<S>) -> Self {
        Self {
            group_of,
            groups: Vec::new(),
            sinks,
        }
    }

    /// The sink of each group, by the group's index.
    #[must_use]
    pub fn sinks(&self) -> &[S] {
        &self.sinks
    }

    /// The place of row `index` of the list among the rows of `group`: how many of them come before it.
    fn place(&self, index: usize, group: usize) -> usize {
        self.groups
            .iter()
            .take(index)
            .filter(|&&g| g == group)
            .count()
    }
}

impl<T, S: RowSink<T>> RowSink<T> for GroupedSink<T, S> {
    fn len(&self) -> usize {
        self.groups.len()
    }

    fn set_row(&mut self, index: usize, row: &T) {
        let Some(&old) = self.groups.get(index) else {
            return;
        };
        let new = (self.group_of)(row);
        let at = self.place(index, old);
        if new == old {
            if let Some(sink) = self.sinks.get_mut(old) {
                sink.set_row(at, row);
            }
            return;
        }
        if let Some(sink) = self.sinks.get_mut(old) {
            sink.remove(at);
        }
        if let Some(g) = self.groups.get_mut(index) {
            *g = new;
        }
        let to = self.place(index, new);
        if let Some(sink) = self.sinks.get_mut(new) {
            sink.insert(to, row);
        }
    }

    fn insert(&mut self, index: usize, row: &T) {
        let index = index.min(self.groups.len());
        let group = (self.group_of)(row);
        let at = self.place(index, group);
        self.groups.insert(index, group);
        if let Some(sink) = self.sinks.get_mut(group) {
            sink.insert(at, row);
        }
    }

    fn remove(&mut self, index: usize) {
        if index >= self.groups.len() {
            return;
        }
        let group = self.groups.remove(index);
        // The rows before `index` are the same after the removal, so its place among its group's rows is too.
        let at = self.place(index, group);
        if let Some(sink) = self.sinks.get_mut(group) {
            sink.remove(at);
        }
    }
}

/// The last value written to a [`CellSink`]; a new value is written only when it differs.
#[derive(Clone, Debug)]
pub struct ValueCell<T> {
    value: Option<T>,
}

impl<T> Default for ValueCell<T> {
    fn default() -> Self {
        Self { value: None }
    }
}

impl<T: PartialEq> ValueCell<T> {
    /// The value last written, if any.
    #[must_use]
    pub const fn get(&self) -> Option<&T> {
        self.value.as_ref()
    }

    /// Writes `value` to `sink` unless it equals the last value written. True when it wrote.
    pub fn set(&mut self, value: T, sink: &mut dyn CellSink<T>) -> bool {
        if self.value.as_ref() == Some(&value) {
            return false;
        }
        sink.write(&value);
        self.value = Some(value);
        true
    }
}

/// The models and properties of the Timeline screen, as the hub writes them.
pub struct Sinks<'a> {
    /// The task list, grouped Active, Drafts, Stopped ([`GroupedSink`] splits it into one model per group).
    pub tasks: &'a mut dyn RowSink<TaskRow>,
    /// The Stopped group's paging: the link under it and what it says.
    pub stopped_page: &'a mut dyn CellSink<StoppedPage>,
    pub running: &'a mut dyn CellSink<Running>,
    pub day_head: &'a mut dyn CellSink<DayHead>,
    pub day_pieces: &'a mut dyn RowSink<DayPiece>,
    pub chips: &'a mut dyn RowSink<DayChip>,
    /// The day picker's page: which way its arrows can move it.
    pub page: &'a mut dyn CellSink<DayPage>,
    pub share_head: &'a mut dyn CellSink<ShareHead>,
    pub share_rows: &'a mut dyn RowSink<ShareRow>,
}

/// What one [`Hub::apply`] wrote, per model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    pub tasks: Writes,
    pub stopped_page: usize,
    pub running: usize,
    pub day_head: usize,
    pub day_pieces: Writes,
    pub chips: Writes,
    pub page: usize,
    pub share_head: usize,
    pub share_rows: Writes,
}

impl Applied {
    /// Every row and value written.
    #[must_use]
    pub const fn total(&self) -> usize {
        self.tasks.total()
            + self.stopped_page
            + self.running
            + self.day_head
            + self.day_pieces.total()
            + self.chips.total()
            + self.page
            + self.share_head
            + self.share_rows.total()
    }
}

/// The per-topic view models of the Timeline screen. Lives on the UI thread; the worker never sees it.
#[derive(Debug, Default)]
pub struct Hub {
    tasks: KeyedList<TaskRow>,
    stopped_page: ValueCell<StoppedPage>,
    running: ValueCell<Running>,
    day_head: ValueCell<DayHead>,
    day_pieces: KeyedList<DayPiece>,
    chips: KeyedList<DayChip>,
    page: ValueCell<DayPage>,
    share_head: ValueCell<ShareHead>,
    share_rows: KeyedList<ShareRow>,
}

impl Hub {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies the topics `update` carries; a topic it does not carry is not looked at.
    pub fn apply(&mut self, update: Update, sinks: &mut Sinks<'_>) -> Applied {
        let mut a = Applied::default();
        if let Some(rows) = update.tasks {
            a.tasks = self.tasks.apply(rows, sinks.tasks);
        }
        if let Some(page) = update.stopped_page {
            a.stopped_page = usize::from(self.stopped_page.set(page, sinks.stopped_page));
        }
        if let Some(running) = update.running {
            a.running = usize::from(self.running.set(running, sinks.running));
        }
        if let Some(day) = update.day {
            a.day_head = usize::from(self.day_head.set(day.head, sinks.day_head));
            a.day_pieces = self.day_pieces.apply(day.pieces, sinks.day_pieces);
        }
        if let Some(chips) = update.chips {
            a.chips = self.chips.apply(chips, sinks.chips);
        }
        if let Some(page) = update.page {
            a.page = usize::from(self.page.set(page, sinks.page));
        }
        if let Some(share) = update.share {
            a.share_head = usize::from(self.share_head.set(share.head, sinks.share_head));
            a.share_rows = self.share_rows.apply(share.rows, sinks.share_rows);
        }
        a
    }

    /// The task rows the UI holds.
    #[must_use]
    pub fn tasks(&self) -> &[TaskRow] {
        self.tasks.rows()
    }

    /// The Stopped group's paging the UI shows.
    #[must_use]
    pub const fn stopped_page(&self) -> Option<&StoppedPage> {
        self.stopped_page.get()
    }

    #[must_use]
    pub const fn running(&self) -> Option<&Running> {
        self.running.get()
    }

    #[must_use]
    pub const fn day_head(&self) -> Option<&DayHead> {
        self.day_head.get()
    }

    #[must_use]
    pub fn day_pieces(&self) -> &[DayPiece] {
        self.day_pieces.rows()
    }

    #[must_use]
    pub fn chips(&self) -> &[DayChip] {
        self.chips.rows()
    }

    /// The day picker's page the UI shows.
    #[must_use]
    pub const fn page(&self) -> Option<&DayPage> {
        self.page.get()
    }

    #[must_use]
    pub const fn share_head(&self) -> Option<&ShareHead> {
        self.share_head.get()
    }

    #[must_use]
    pub fn share_rows(&self) -> &[ShareRow] {
        self.share_rows.rows()
    }
}

/// A [`RowSink`] over a plain `Vec` that counts its calls: for tests here and in the UI crate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VecSink<T> {
    pub rows: Vec<T>,
    pub writes: Writes,
}

impl<T> Default for VecSink<T> {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            writes: Writes::default(),
        }
    }
}

impl<T: Clone> RowSink<T> for VecSink<T> {
    fn len(&self) -> usize {
        self.rows.len()
    }

    fn set_row(&mut self, index: usize, row: &T) {
        if let Some(slot) = self.rows.get_mut(index) {
            slot.clone_from(row);
        }
        self.writes.set += 1;
    }

    fn insert(&mut self, index: usize, row: &T) {
        self.rows.insert(index.min(self.rows.len()), row.clone());
        self.writes.inserted += 1;
    }

    fn remove(&mut self, index: usize) {
        if index < self.rows.len() {
            self.rows.remove(index);
        }
        self.writes.removed += 1;
    }
}

/// A [`CellSink`] that keeps every value written: for tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueLog<T> {
    pub values: Vec<T>,
}

impl<T> Default for ValueLog<T> {
    fn default() -> Self {
        Self { values: Vec::new() }
    }
}

impl<T: Clone> CellSink<T> for ValueLog<T> {
    fn write(&mut self, value: &T) {
        self.values.push(value.clone());
    }
}

/// Test doubles for every model of [`Sinks`].
#[derive(Clone, Debug, Default)]
pub struct Recorder {
    pub tasks: VecSink<TaskRow>,
    pub stopped_page: ValueLog<StoppedPage>,
    pub running: ValueLog<Running>,
    pub day_head: ValueLog<DayHead>,
    pub day_pieces: VecSink<DayPiece>,
    pub chips: VecSink<DayChip>,
    pub page: ValueLog<DayPage>,
    pub share_head: ValueLog<ShareHead>,
    pub share_rows: VecSink<ShareRow>,
}

impl Recorder {
    /// The sinks, borrowed for one [`Hub::apply`].
    pub fn sinks(&mut self) -> Sinks<'_> {
        Sinks {
            tasks: &mut self.tasks,
            stopped_page: &mut self.stopped_page,
            running: &mut self.running,
            day_head: &mut self.day_head,
            day_pieces: &mut self.day_pieces,
            chips: &mut self.chips,
            page: &mut self.page,
            share_head: &mut self.share_head,
            share_rows: &mut self.share_rows,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Row {
        id: String,
        v: u32,
    }

    impl Keyed for Row {
        type Key = String;
        fn key(&self) -> &String {
            &self.id
        }
    }

    fn rows(spec: &str) -> Vec<Row> {
        // "a1 b2 c3": id then value.
        spec.split_whitespace()
            .map(|t| {
                let (id, v) = t.split_at(1);
                Row {
                    id: id.to_owned(),
                    v: v.parse().unwrap(),
                }
            })
            .collect()
    }

    fn run(from: &str, to: &str) -> (Writes, VecSink<Row>) {
        let mut list = KeyedList::default();
        let mut sink = VecSink::default();
        list.apply(rows(from), &mut sink);
        sink.writes = Writes::default();
        let w = list.apply(rows(to), &mut sink);
        assert_eq!(sink.rows, rows(to), "{from} -> {to}");
        assert_eq!(list.rows(), rows(to).as_slice());
        assert_eq!(w, sink.writes);
        (w, sink)
    }

    #[test]
    fn the_same_rows_twice_write_nothing() {
        let (w, _) = run("a1 b2 c3", "a1 b2 c3");
        assert_eq!(w.total(), 0);
    }

    #[test]
    fn a_changed_row_is_rewritten_in_place() {
        let (w, _) = run("a1 b2 c3", "a1 b9 c3");
        assert_eq!(
            w,
            Writes {
                set: 1,
                inserted: 0,
                removed: 0
            }
        );
    }

    #[test]
    fn inserts_and_removals_touch_only_their_rows() {
        let (w, _) = run("a1 b2 c3", "a1 x0 c3 d4");
        assert_eq!(
            w,
            Writes {
                set: 0,
                inserted: 2,
                removed: 1
            }
        );
    }

    #[test]
    fn a_move_up_or_down_costs_one_remove_and_one_insert() {
        let (up, _) = run("a1 b2 c3 d4", "d4 a1 b2 c3");
        assert_eq!(
            up,
            Writes {
                set: 0,
                inserted: 1,
                removed: 1
            }
        );
        let (down, _) = run("a1 b2 c3 d4", "b2 c3 d4 a1");
        assert_eq!(
            down,
            Writes {
                set: 0,
                inserted: 1,
                removed: 1
            }
        );
        let (moved_and_changed, _) = run("a1 b2 c3", "c7 a1 b2");
        assert_eq!(
            moved_and_changed,
            Writes {
                set: 0,
                inserted: 1,
                removed: 1
            }
        );
    }

    #[test]
    fn empty_lists_and_full_replacements() {
        assert_eq!(
            run("", "a1 b2").0,
            Writes {
                set: 0,
                inserted: 2,
                removed: 0
            }
        );
        assert_eq!(
            run("a1 b2", "").0,
            Writes {
                set: 0,
                inserted: 0,
                removed: 2
            }
        );
        assert_eq!(
            run("a1 b2", "c3 d4").0,
            Writes {
                set: 0,
                inserted: 2,
                removed: 2
            }
        );
    }

    #[test]
    fn repeated_keys_still_end_equal() {
        run("a1 b2", "a1 a1 b2");
        run("a1 a1 b2", "b2 a1");
    }

    #[test]
    fn a_sink_changed_behind_the_hub_is_rebuilt() {
        let mut list = KeyedList::default();
        let mut sink = VecSink::default();
        list.apply(rows("a1 b2"), &mut sink);
        sink.rows.pop();
        let w = list.apply(rows("a1 b2"), &mut sink);
        assert_eq!(sink.rows, rows("a1 b2"));
        assert_eq!(
            w,
            Writes {
                set: 0,
                inserted: 2,
                removed: 1
            }
        );
    }

    /// Many random lists: the sink always ends equal to the new rows, and a kept row is never written twice.
    #[test]
    fn random_edits_always_converge() {
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for _ in 0..2000 {
            let make = |next: &mut dyn FnMut(u64) -> u64| {
                let mut ids: Vec<char> = ('a'..='l').collect();
                let mut out = Vec::new();
                for _ in 0..next(10) {
                    let i = usize::try_from(next(ids.len() as u64)).unwrap();
                    let id = ids.remove(i);
                    out.push(Row {
                        id: id.to_string(),
                        v: u32::try_from(next(3)).unwrap(),
                    });
                }
                out
            };
            let (a, b) = (make(&mut next), make(&mut next));
            let mut list = KeyedList::default();
            let mut sink = VecSink::default();
            list.apply(a.clone(), &mut sink);
            let before = sink.writes;
            let w = list.apply(b.clone(), &mut sink);
            assert_eq!(sink.rows, b, "{a:?} -> {b:?}");
            let common = b.iter().filter(|r| a.iter().any(|x| x.id == r.id)).count();
            let added = b.len() - common;
            let gone = a.len() - common;
            assert_eq!(
                w.inserted - added,
                w.removed - gone,
                "moves are one remove and one insert"
            );
            assert!(w.set <= common);
            assert_eq!(sink.writes.total() - before.total(), w.total());
        }
    }

    /// A row's group in the grouped-sink tests: its value (0, 1 and 2 have a sink; 3 has none).
    fn group_of(r: &Row) -> usize {
        usize::try_from(r.v).unwrap()
    }

    fn of_group(rows: &[Row], group: usize) -> Vec<Row> {
        rows.iter()
            .filter(|r| group_of(r) == group)
            .cloned()
            .collect()
    }

    fn grouped() -> GroupedSink<Row, VecSink<Row>> {
        GroupedSink::new(group_of, vec![VecSink::default(); 3])
    }

    #[test]
    fn a_grouped_sink_keeps_each_group_in_the_list_order() {
        let mut list = KeyedList::default();
        let mut sink = grouped();
        list.apply(rows("a0 b1 c2 d0"), &mut sink);
        assert_eq!(sink.sinks()[0].rows, rows("a0 d0"));
        assert_eq!(sink.sinks()[1].rows, rows("b1"));
        assert_eq!(sink.sinks()[2].rows, rows("c2"));
        // `d` moves to the top (one remove, one insert); `b` is rewritten in place into group 2, where it goes before
        // `c`: it leaves group 1 for group 2 (one remove, one insert where the hub made one rewrite).
        let hub = list.apply(rows("d0 a0 b2 c2"), &mut sink);
        assert_eq!(
            hub,
            Writes {
                set: 1,
                inserted: 1,
                removed: 1
            }
        );
        assert_eq!(sink.sinks()[0].rows, rows("d0 a0"));
        assert!(sink.sinks()[1].rows.is_empty());
        assert_eq!(sink.sinks()[2].rows, rows("b2 c2"));
        assert_eq!(sink.len(), 4);
        let writes = sink
            .sinks()
            .iter()
            .fold(Writes::default(), |all, s| all + s.writes);
        assert_eq!(
            writes,
            Writes {
                set: 0,
                inserted: 4 + 2,
                removed: 2
            },
            "the first four rows, then two moves"
        );
        // The same rows again: nothing written.
        list.apply(rows("d0 a0 b2 c2"), &mut sink);
        let again = sink
            .sinks()
            .iter()
            .fold(Writes::default(), |all, s| all + s.writes);
        assert_eq!(again, writes);
    }

    /// Many random lists, groups in any order and some rows in a group with no sink: each sink always ends holding
    /// the rows of its group, in the list's order.
    #[test]
    fn a_grouped_sink_converges_on_random_edits() {
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for _ in 0..2000 {
            let make = |next: &mut dyn FnMut(u64) -> u64| {
                let mut ids: Vec<char> = ('a'..='l').collect();
                let mut out = Vec::new();
                for _ in 0..next(10) {
                    let i = usize::try_from(next(ids.len() as u64)).unwrap();
                    let id = ids.remove(i);
                    out.push(Row {
                        id: id.to_string(),
                        v: u32::try_from(next(4)).unwrap(),
                    });
                }
                out
            };
            let (a, b) = (make(&mut next), make(&mut next));
            let mut list = KeyedList::default();
            let mut sink = grouped();
            list.apply(a.clone(), &mut sink);
            for g in 0..3 {
                assert_eq!(sink.sinks()[g].rows, of_group(&a, g), "{a:?}");
            }
            list.apply(b.clone(), &mut sink);
            for g in 0..3 {
                assert_eq!(sink.sinks()[g].rows, of_group(&b, g), "{a:?} -> {b:?}");
            }
            assert_eq!(sink.len(), b.len());
        }
    }

    #[test]
    fn a_value_cell_writes_only_changes() {
        let mut cell = ValueCell::default();
        let mut log = ValueLog::default();
        assert!(cell.set(1, &mut log));
        assert!(!cell.set(1, &mut log));
        assert!(cell.set(2, &mut log));
        assert_eq!(log.values, vec![1, 2]);
        let mut seen = Vec::new();
        let mut closure = |v: &i32| seen.push(*v);
        assert!(cell.set(3, &mut closure));
        assert_eq!(seen, vec![3]);
        assert_eq!(cell.get(), Some(&3));
    }
}
