//! Which level the Share of time donut shows. Port of `features/analysis/shareLevels.ts`.
//!
//! The owner: the level the card starts on, and moves to when the filter changes, is the one that divides what the
//! filter has left open (Tags is never that level: it is chosen by hand). "Once you pick a level, it never changes on
//! its own": after a pick no change of the filter moves the level; only the user does.

/// A level of the donut: what one slice stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShareLevel {
    Projects,
    Tasks,
    Subtasks,
    Tags,
}

impl ShareLevel {
    /// The four segments of the switch, in order (`SHARE_LEVELS`). Their words, the donut's and the legend's names
    /// and the Subtasks segment's tooltip are the UI's strings (`ui/strings.slint`).
    pub const ALL: [ShareLevel; 4] = [Self::Projects, Self::Tasks, Self::Subtasks, Self::Tags];
}

/// `defaultLevel`: the level a filter asks for, from how many projects and tasks it names. One task leaves its
/// subtasks open (Subtasks); one project its tasks (Tasks); no project, or several, the projects (Projects). Never
/// Tags. The other filters (tags, band, shift, search) do not move it.
#[must_use]
pub fn default_level(project_count: usize, task_count: usize) -> ShareLevel {
    if task_count == 1 {
        ShareLevel::Subtasks
    } else if project_count == 1 {
        ShareLevel::Tasks
    } else {
        ShareLevel::Projects
    }
}

/// `Availability`: which levels have something to show. Only Subtasks can be empty while there is time: time on no
/// subtask is just the tasks again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Availability {
    pub projects: bool,
    pub tasks: bool,
    pub subtasks: bool,
    pub tags: bool,
}

impl Availability {
    /// Every level has something to show.
    pub const ALL: Self = Self {
        projects: true,
        tasks: true,
        subtasks: true,
        tags: true,
    };

    /// Whether `level` has something to show.
    #[must_use]
    pub fn has(self, level: ShareLevel) -> bool {
        match level {
            ShareLevel::Projects => self.projects,
            ShareLevel::Tasks => self.tasks,
            ShareLevel::Subtasks => self.subtasks,
            ShareLevel::Tags => self.tags,
        }
    }
}

/// `availabilityOf`: Subtasks has something to show when some row is time on a real subtask (`has_id`) with
/// seconds; the other levels always do. `rows` are `(has_id, secs)` of the subtask rows of the period.
#[must_use]
pub fn availability_of(rows: impl IntoIterator<Item = (bool, i64)>) -> Availability {
    Availability {
        subtasks: rows.into_iter().any(|(has_id, secs)| has_id && secs > 0),
        ..Availability::ALL
    }
}

/// `shownLevel`: the level to draw. The one asked for, or when it has nothing to show the default of the filter,
/// or the nearest level above it that has something. Never an empty chart. A level the user picked stays picked
/// (see [`LevelState`]) and is drawn again as soon as it has time.
#[must_use]
pub fn shown_level(level: ShareLevel, auto: ShareLevel, available: Availability) -> ShareLevel {
    if available.has(level) {
        return level;
    }
    [auto, ShareLevel::Tasks, ShareLevel::Projects]
        .into_iter()
        .find(|l| available.has(*l))
        .unwrap_or(ShareLevel::Projects)
}

/// `LevelState`: what the card remembers. `auto` is the default of the filters it last saw, kept up to date after a
/// pick too, so a change of the filters can be told apart. `picked` says the user chose a level by hand: once true it
/// stays true, and `level` is theirs from then on. Until then `level` is `auto`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LevelState {
    pub level: ShareLevel,
    pub auto: ShareLevel,
    pub picked: bool,
}

impl LevelState {
    /// The user's pick, if there has been one: the `picked` argument of [`crate::share::share_of_time`].
    #[must_use]
    pub fn pick(self) -> Option<ShareLevel> {
        self.picked.then_some(self.level)
    }
}

/// `LevelEvent`: what can happen to the card's level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LevelEvent {
    /// The user chose a level.
    Pick(ShareLevel),
    /// The card saw the filters again; the value is [`default_level`] of them.
    Filters(ShareLevel),
}

/// `startLevel`: the state of a card that has seen one set of filters and no pick.
#[must_use]
pub fn start_level(auto: ShareLevel) -> LevelState {
    LevelState {
        level: auto,
        auto,
        picked: false,
    }
}

/// `reduceLevel`. Until the user picks a level, a filter change that moves the default applies the new default. A
/// pick ends that for good: the level stays what they chose through every later change of the filters, and only
/// another pick changes it. A change that leaves the default where it was does nothing.
#[must_use]
pub fn reduce_level(state: LevelState, event: LevelEvent) -> LevelState {
    match event {
        LevelEvent::Pick(level) => LevelState {
            level,
            auto: state.auto,
            picked: true,
        },
        LevelEvent::Filters(auto) if auto == state.auto => state,
        LevelEvent::Filters(auto) if state.picked => LevelState { auto, ..state },
        LevelEvent::Filters(auto) => start_level(auto),
    }
}
