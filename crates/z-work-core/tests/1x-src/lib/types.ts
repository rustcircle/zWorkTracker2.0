// The backend's read model. Field names match `z-work-core`'s `view.rs` exactly.
export type TaskState = 'draft' | 'running' | 'paused' | 'stopped';
export type RoundMethod = 'nearest' | 'up' | 'down';
export type Currency = 'EUR' | 'USD';
export type Theme = 'light' | 'solarizedLight' | 'solarizedDark' | 'dark';
export type SyncProvider = 'oneDrive' | 'googleDrive' | 'custom';

export type PauseReason =
  | { kind: 'user' }
  | { kind: 'autoPaused'; byTaskId: string }
  | { kind: 'appQuit' }
  | { kind: 'crashRecovery' }
  | { kind: 'syncReconciled' }
  | { kind: 'backupRestored' }
  /** Another device asked this one to pause the task. */
  | { kind: 'remoteRequest' };

export type BillingType = 'hourly' | 'fixedFee';

/** Where a project stands against its budget. Whole percent, rounded down; level is 0, 80 or 100. */
export interface BudgetStatus {
  /** `hours`, or `fee` when time value is measured against a fixed fee. */
  basis: 'hours' | 'fee';
  percent: number;
  level: 0 | 80 | 100;
  /** A fixed fee can't be tracked without a price per hour to value the time. */
  needsTargetRate: boolean;
}

export interface ProjectFigures {
  exactSecs: number;
  pauseSecs: number;
  billableSecs: number;
  thinkingSecs: number;
  /** Fixed-fee projects: fee divided by billable hours, in cents per hour. */
  effectiveHourlyCents: number | null;
  /** Time at the project's rate versions, even on a fixed-fee project, per currency. */
  timeValueCents: Partial<Record<Currency, number>>;
  /** The same before rounding, so a figure carried forward rounds once too. Missing when there is none. */
  timeValueNumerators?: Partial<Record<Currency, number>>;
  budget: BudgetStatus | null;
}

export interface Project {
  id: string;
  name: string;
  color: string;
  archived: boolean;
  billable: boolean;
  budgetHours: number | null;
  clientZone: string | null;
  createdAt: string;
  hasTasks: boolean;
  billingType: BillingType;
  /** The agreed price of a fixed-fee project, in cents of `feeCurrency`. */
  fixedFeeCents: number | null;
  feeCurrency: Currency | null;
  figures: ProjectFigures;
}

/** One task-list item of a task description, the unit that can carry its own tracked time. */
export interface Subtask {
  id: string;
  text: string;
  checked: boolean;
  secs: number;
}

/** TipTap JSON. Only ever rendered through the editor. */
export type RichDoc = Record<string, unknown> | null;

export interface Task {
  id: string;
  name: string;
  projectId: string;
  description: RichDoc;
  state: TaskState;
  pauseReason: PauseReason | null;
  createdAt: string;
  stoppedAt: string | null;
  runningElsewhere: string | null;
  /** The earlier task this one continues. Each copy is a separate task. */
  continuedFrom: string | null;
  tags: string[];
  /** The subtask new time is attributed to. */
  activeSubtask: string | null;
  subtasks: Subtask[];
  /** Seconds on a live billing summary, and seconds not yet on one. */
  billedSecs: number;
  unbilledSecs: number;
  thinkingSecs: number;
  pauseSecs: number;
  /** Away with no answer yet: counted nowhere until confirmed. */
  unconfirmedSecs: number;
}

/** Fields that must be filled before the first start of a launch. Data-driven: tags join this later. */
export type RequiredField = 'name' | 'project' | 'description' | 'tags';

/** What a warm start copies: the task most recently worked in this launch. */
export interface CopySource {
  taskId: string;
  name: string;
  projectId: string;
  projectArchived: boolean;
  description: RichDoc;
  lastWorkedAt: string | null;
  tags: string[];
}

/** Cold until the first task of this launch has started. Hiding to the tray stays warm. */
export interface LaunchState {
  warm: boolean;
  coldRequired: RequiredField[];
  copySource: CopySource | null;
}

export type PieceKind = 'work' | 'thinking' | 'billablePause';

/**
 * One piece of counted time, as the backend reports it. A recorded segment can become several pieces
 * (away time cut out, or marked as thinking), and a billable pause is a piece of its own. `kind` and
 * the fields after it may be absent in old fixtures: a missing kind means work.
 */
export interface Segment {
  id: string;
  taskId: string;
  deviceId: string;
  start: string;
  end: string | null;
  provisionalEnd: string | null;
  /** The recorded segment this piece came from, or the pause record for a billable pause. */
  segmentId?: string;
  kind?: PieceKind;
  subtaskId?: string | null;
}

/** One recorded segment with its correction, for the corrections screen. */
export interface RawSegment {
  id: string;
  taskId: string;
  deviceId: string;
  rawStart: string;
  rawEnd: string | null;
  start: string;
  end: string | null;
  voided: boolean;
  corrected: boolean;
  manual: boolean;
  reason: string;
  billed: boolean;
}

export type GapKind = 'sleep' | 'idle' | 'lock';
export type GapDecision = 'pending' | 'keep' | 'drop' | 'thinking';

/** Time away detected inside a running segment. The segment's recorded times don't change. */
export interface IdleGap {
  id: string;
  segmentId: string;
  taskId: string;
  deviceId: string;
  start: string;
  end: string;
  kind: GapKind;
  decision: GapDecision;
  decisionStamp: Stamp;
}

/** A pause marked as still working. It counts in Billable and never in Exact. */
export interface BillablePause {
  id: string;
  taskId: string;
  start: string;
  end: string;
  note: string;
  deleted: boolean;
  stamp: Stamp;
}

/** A synced request to pause a task that seems to have been left running on another device. */
export interface PauseRequest {
  id: string;
  taskId: string;
  /** The open segment the request is about. That device's record is never changed from here. */
  segmentId: string;
  requestedBy: string;
  at: string;
  stamp: Stamp;
}

export interface Tag {
  id: string;
  name: string;
  color: string;
  deleted: boolean;
  stamp: Stamp;
}

export interface Template {
  id: string;
  name: string;
  projectId: string | null;
  description: RichDoc;
  tags: string[];
  deleted: boolean;
  stamp: Stamp;
}

export interface AuditEntry {
  id: string;
  at: string;
  by: string;
  entity: string;
  entityId: string;
  action: string;
  reason: string;
  before: unknown;
  after: unknown;
}

export interface SummaryRow {
  taskId: string;
  taskName: string;
  day: string;
  exactSecs: number;
  pauseSecs: number;
  billableSecs: number;
  cents: Partial<Record<Currency, number>>;
}

/** For the user's own guidance. Not an invoice, and not a tax document. */
export interface BillingSummary {
  id: string;
  /** `<prefix>-<number>`, such as `A-001`. Unique per device. */
  reference: string;
  prefix: string;
  number: number;
  deviceId: string;
  projectId: string;
  from: string;
  to: string;
  zone: string;
  createdAt: string;
  segmentIds: string[];
  pauseIds: string[];
  rows: SummaryRow[];
  totals: Partial<Record<Currency, number>>;
  voided: boolean;
  stamp: Stamp;
}

export interface BillingView {
  prefix: string;
  /** Names of other devices using the same prefix. */
  clashes: string[];
  summaries: BillingSummary[];
}

export interface SummaryRequest {
  projectId: string;
  /** Inclusive days, `YYYY-MM-DD`, in `zone`. */
  from: string;
  to: string;
  zone: string;
}

export interface SummaryPreview {
  rows: SummaryRow[];
  totals: Partial<Record<Currency, number>>;
  segmentIds: string[];
  pauseIds: string[];
  exactSecs: number;
  pauseSecs: number;
  billableSecs: number;
  /** Time in the period that is still running, so it isn't listed yet. */
  runningExcluded: boolean;
  /** Time away inside these segments with no decision yet. It is left out, and a summary freezes that. */
  unconfirmedSecs: number;
  /** Rows that bill a difference: their task-day is partly on a summary already, so they don't bill what their own time rounds to. */
  partlyBilledRows: number;
}

export type Reminder = { kind: 'longRun'; taskId: string; since: string } | { kind: 'pastShift'; taskId: string; shift: string; ended: string };

export interface ReminderSettings {
  idleMinutes: number;
  longRunMinutes: number;
  remindPastShift: boolean;
}

export interface BackupInfo {
  name: string;
  bytes: number;
  /** A copy made automatically just before a restore. */
  safety: boolean;
}

/** One Timeline entry: one task on one day, the same unit as an Excel row. */
export interface SelectedEntry {
  taskId: string;
  /** `YYYY-MM-DD`, in the zone of the request. */
  day: string;
}

export interface PdfRequest {
  projectId: string | null;
  from: string;
  to: string;
  zone: string;
  title: string;
  includePrices: boolean;
  useLogo: boolean;
  /** Only these task-days, or null for everything in the period. Client totals are never sent. */
  selection: SelectedEntry[] | null;
}

/** What a PDF export reports: where it went (null when cancelled), and what prints as an empty box. */
export interface PdfSaved {
  path: string | null;
  unprintable: string[];
}

export interface ManualEntryInput {
  taskId: string;
  start: string;
  end: string;
  reason: string;
  subtaskId?: string | null;
  /** The user saw the overlap warning and wants the entry anyway. */
  allowOverlap?: boolean;
}

export interface CorrectionInput {
  segmentId: string;
  start: string;
  end: string;
  reason: string;
  allowOverlap?: boolean;
}

export interface PauseInput {
  taskId: string;
  start: string;
  end: string;
  note: string;
}

export interface TagInput {
  id?: string | null;
  name: string;
  /** `#rrggbb`; the backend stores it lowercase. */
  color: string;
}

export interface TemplateInput {
  id?: string | null;
  name: string;
  projectId: string | null;
  description: RichDoc;
  tags: string[];
}

export interface Stamp {
  at: string;
  by: string;
}

export interface ShiftVersion {
  id: string;
  shiftId: string;
  effectiveFrom: string;
  name: string;
  color: string;
  start: string;
  end: string;
  /** ISO weekdays of the date the shift starts on: 1 = Monday .. 7 = Sunday. */
  weekdays: number[];
  zone: string;
  breakMinutes: number;
  retired: boolean;
  deleted: boolean;
  stamp: Stamp;
}

export interface RateVersion {
  id: string;
  projectId: string;
  effectiveFrom: string;
  /** null is "no rate", which is different from 0. */
  rateCents: number | null;
  currency: string;
  deleted: boolean;
  stamp: Stamp;
}

export interface RoundingVersion {
  id: string;
  projectId: string | null;
  effectiveFrom: string;
  /** 0 means exact. */
  incrementMinutes: number;
  method: RoundMethod;
  deleted: boolean;
  stamp: Stamp;
}

export interface Notice {
  id: string;
  level: 'info' | 'warning';
  message: string;
}

export interface TargetStatus {
  lastWrittenAt: string | null;
  lastReadAt: string | null;
  lastError: string | null;
  lastErrorAt: string | null;
  warnings: string[];
  ignoredFiles: number;
  devicesSeen: number;
  // The fields below are optional only so that fixtures built by hand can leave them out, and a missing one means null.
  // The backend always sends all six.
  /** When this computer last wrote its lease record in this folder (ISO). */
  lastLeaseAt?: string | null;
  /** Why the lease record could not be written or read here. */
  leaseError?: string | null;
  /** When a saved version was last written in this folder (ISO). */
  lastVersionAt?: string | null;
  lastVersionError?: string | null;
  /** The size of the saved versions in this folder, in bytes. */
  versionsBytes?: number | null;
  /** How far this folder's copy of another computer's file is behind the other folder's, in seconds. Null when it isn't. */
  lagSecs?: number | null;
}

export interface SyncTargetView {
  provider: SyncProvider;
  root: string;
  folder: string;
  status: TargetStatus;
}

export interface Detected {
  provider: SyncProvider;
  root: string;
  source: string;
}

export interface FolderPreview {
  hasData: boolean;
  devices: string[];
  projects: number;
  tasks: number;
  segments: number;
  warnings: string[];
}

/** Counts of what a sync folder (or this computer) holds. */
export interface DataCounts {
  projects: number;
  tasks: number;
  segments: number;
}

/** What moving sync to another folder would do, read without changing anything. */
export interface MovePreview {
  sourceAvailable: boolean;
  sourceDevices: string[];
  sourceCounts: DataCounts;
  destinationHasData: boolean;
  destinationDevices: string[];
  destinationCounts: DataCounts;
  localCounts: DataCounts;
  warnings: string[];
}

export interface MoveResult {
  filesCopied: number;
  filesAlreadyThere: number;
  filesIgnored: number;
  sourceMissing: boolean;
  counts: DataCounts;
  checksum: string;
  warnings: string[];
}

/** Payload of the `sync-move-progress` event. */
export interface MoveProgress {
  /** The backend's own words for the step, such as "Copying device files". Shown as it is. */
  step: string;
  done: number;
  total: number;
}

/** The previous folder could not be read, so a copy into `to` is still owed. */
export interface PendingCopy {
  from: string;
  to: string;
}

/**
 * A folder that holds data restored from a saved version, while this computer has data from before it. Nothing is read
 * from or written to that folder until the user chooses (`resolve_join`). `takenFrom` is the name of the version that was
 * restored, `at` when that version was taken (ISO), and `deviceName` the computer that restored it.
 */
export interface JoinPending {
  root: string;
  takenFrom: string;
  at: string;
  deviceName: string;
}

export type JoinChoice = 'follow' | 'keep';

// ---- Who may change the data. Only one computer at a time holds the lease, on a best effort basis: cloud folders update
// late and can't lock anything, so two computers may both change data for a few minutes, and the changes merge.

/**
 * `off`: no sync folder, so no lease. `checking`: just opened and the first look at the folders isn't back; writable.
 * `free`: nobody holds it. `heldByMe`: this computer does. `heldByOtherLive` and `heldByOtherStale`: another computer
 * does and this one is read-only; stale means nothing has been heard from it for a long while. `unknown`: no folder can
 * be read; read-only only if another computer held it recently.
 */
export type LeaseStateName = 'off' | 'checking' | 'free' | 'heldByMe' | 'heldByOtherLive' | 'heldByOtherStale' | 'unknown';

export interface LeaseHolderView {
  deviceId: string;
  deviceName: string;
  generation: number;
  /** Counts up with each beat of the holder. A take over names the one it was shown, so a holder that came back to life is noticed. */
  beatSeq: number;
  /** The last sign of the holder as this computer knows it (ISO), or null when there was none. */
  lastSignAt: string | null;
  /** Seconds from `lastSignAt` to the backend's clock, or null with no sign. */
  ageSecs: number | null;
  /** The holder's own clock seems to differ from this computer's. */
  clockSuspect: boolean;
}

export interface LeaseView {
  state: LeaseStateName;
  /** True when the backend refuses every change to shared data. The page shows it early (`useReadOnly`); the refusal is the guarantee. */
  readOnly: boolean;
  /** The other computer that holds the lease, or the last one known while `unknown` (then with `beatSeq` 0, since only its name, generation and time are remembered). Null otherwise. */
  holder: LeaseHolderView | null;
  /** The generation this computer holds, or the newest seen. Null with no folder. */
  generation: number | null;
  /** One more line for the banner, such as a folder that can't be read. Null when there is nothing to add. */
  note: string | null;
  /** The sync folders (by root) the last look read, and those it couldn't. */
  reachableRoots: string[];
  unreachableRoots: string[];
}

/** The holder a take over was shown. The backend reads again and answers `changed` when this is no longer the top. */
export interface TakeOverExpected {
  generation: number;
  beatSeq: number;
}

/** `stale` and `live` take over from a holder that looks inactive or in use; `offline` makes this computer writable while no folder can be read. */
export type TakeOverMode = 'stale' | 'live' | 'offline';

/** A task the holder shows as running: a take over closes it at `end`, the last moment that computer is known to have been running. */
export interface TakeOverSegment {
  segmentId: string;
  taskName: string;
  end: string;
  secs: number;
}

export interface TakeOverPreview {
  holder: LeaseHolderView | null;
  /** The holder looks like it is still in use. */
  live: boolean;
  /** No folder could be read just now. */
  unreachable: boolean;
  segments: TakeOverSegment[];
}

export interface TakeOverResult {
  /** `done`: this computer holds the lease. `changed`: the holder moved on while the dialog was open, and `preview` is the fresh view. `unreachable`: no folder could be written, and nothing changed. */
  outcome: 'done' | 'changed' | 'unreachable';
  preview: TakeOverPreview | null;
  snapshot: Snapshot;
}

// ---- Saved versions of the data, in the sync folders.

export interface VersionLocation {
  root: string;
  provider: SyncProvider;
  bytes: number;
}

export interface VersionInfo {
  /** The file name without its extension, such as `20261102T142530Z--a1b2c3d4--auto`. The same name in two folders is one version. */
  name: string;
  /** When it was taken (ISO). */
  takenAt: string;
  deviceName: string;
  /** The reason code from the name: `auto`, `quit`, `manual`, `takeover`, `pre-restore`, `pre-adopt` or `restored`. The page puts it into words. */
  reason: string;
  /** Kept for a year whatever the retention rule says. */
  pinned: boolean;
  /** Every folder that holds a copy. */
  locations: VersionLocation[];
}

export interface VersionList {
  /** Newest first. */
  versions: VersionInfo[];
  /** Folders that couldn't be listed. The versions of the others are still here. */
  errors: { root: string; message: string }[];
  totalBytes: number;
  /** The versions take more than 1 GiB. Nothing is deleted for that. */
  overBudget: boolean;
}

export interface RestorePreview {
  name: string;
  takenAt: string;
  reason: string;
  /** What the version holds, against what this computer holds now. */
  version: DataCounts;
  current: DataCounts;
  /** Records that couldn't be read and are left out of the restore. */
  warnings: string[];
  /** Why a restore can't go ahead, or null when it can. */
  blockedReason: string | null;
}

// ---- Start with Windows. A Microsoft Store (packaged) build asks Windows, which can refuse and has the last word.

export type AutostartState = 'on' | 'off' | 'offByUser' | 'offByPolicy' | 'onByPolicy' | 'unavailable';

export interface AutostartInfo {
  packaged: boolean;
  state: AutostartState;
  /** The switch can be changed from here. */
  canChange: boolean;
  /** What to tell the user when it can't. */
  note: string | null;
}

/** What kind of build this is. */
export interface PlatformView {
  /** Running from the Microsoft Store package, which keeps its files in a private folder that uninstalling removes. */
  packaged: boolean;
}

/** Replicated settings; every key is optional. */
export interface Settings {
  myZone?: string | null;
  clientZone?: string | null;
  activeZone?: string | null;
  showBoth?: boolean;
  clientWorkStart?: string | null;
  clientWorkEnd?: string | null;
  dailyTargetHours?: number | null;
  weeklyTargetHours?: number | null;
  /** Whole minutes; 0 or null turns the feature off. */
  idleMinutes?: number | null;
  longRunMinutes?: number | null;
  remindPastShift?: boolean;
}

/** This computer's clock is behind the newest change it has seen (design D1 §6). */
export interface ClockBehindView {
  /** Seconds this clock was behind `newestAt` when the snapshot was built: at least 120. */
  behindSecs: number;
  /** When the newest change seen says it was saved, `YYYY-MM-DDTHH:MM:SSZ`. */
  newestAt: string;
  /** The other computer whose change is ahead of this clock, by name; null when the changes ahead were saved here. Always sent, never left out. */
  causeDevice: string | null;
  /**
   * The gap is a day or more (`behindSecs >= 86400`), the furthest a new change may be dated past this clock. New changes can't be
   * dated after `newestAt` until the clock is fixed, so they may lose to it. Always sent.
   */
  capped: boolean;
}

export interface Snapshot {
  /** The backend's count of committed changes when this was built. A lower count than one already adopted is an older reply. */
  revision: number;
  now: string;
  deviceId: string;
  deviceName: string;
  systemZone: string;
  tzdbVersion: string;
  projects: Project[];
  tasks: Task[];
  segments: Segment[];
  shifts: ShiftVersion[];
  rates: RateVersion[];
  rounding: RoundingVersion[];
  settings: Settings;
  prefs: {
    theme: Theme;
    hideEarnings: boolean;
    lastCurrency: Currency | null;
    clientZonePromptDone: boolean;
  };
  notices: Notice[];
  warnings: string[];
  /** Optional only so that fixtures built by hand can leave it out (missing means null). The backend always sends it. */
  clockBehind?: ClockBehindView | null;
  sync: {
    targets: SyncTargetView[];
    detected: Detected[];
    promptPending: boolean;
    pendingCopy: PendingCopy | null;
    /** Optional only so that fixtures built by hand can leave it out (missing means null). The backend always sends it. */
    joinPending?: JoinPending | null;
  };
  /** Who may change the data. With no sync folder it is `off`, which never refuses anything. */
  lease: LeaseView;
  platform: PlatformView;
  launch: LaunchState;
  rawSegments: RawSegment[];
  /** Time away. Pending gaps count nowhere until the user decides. */
  gaps: IdleGap[];
  pauses: BillablePause[];
  tags: Tag[];
  templates: Template[];
  billing: BillingView;
  /** First-run setup: pending only on a fresh store, and the step to resume at. */
  setup: { pending: boolean; step: number };
  /** Asks made from this or other devices to pause a task left running. */
  pauseRequests: PauseRequest[];
  reminder: Reminder | null;
  reminderSettings: ReminderSettings;
  backupKeep: number;
  hasPdfLogo: boolean;
}

export interface CommandError {
  code: string;
  message: string;
}

export interface ProjectInput {
  name: string;
  color: string;
  billable: boolean;
  budgetHours: number | null;
  clientZone: string | null;
  billingType?: BillingType;
  fixedFeeCents?: number | null;
  feeCurrency?: Currency | null;
}

export interface TaskInput {
  name: string;
  projectId: string;
  description: RichDoc;
  /** Only read when creating a task. */
  continuedFrom?: string | null;
  tags?: string[];
}

/** An edit to some fields of a task. A key left out keeps the stored value; description null clears it. */
export interface TaskPatch {
  name?: string;
  projectId?: string;
  description?: RichDoc;
  tags?: string[];
}

/**
 * For each field of a patch, what the editor sending it last showed or saved. The backend refuses the field if it
 * holds something else by then, unless that is what is being saved. A field left out here is written regardless.
 */
export type TaskExpected = TaskPatch;

/** What the store holds in the fields an editor can change. */
export interface StoredFields {
  name: string;
  projectId: string;
  description: RichDoc;
  tags: string[];
}

/** The answer to a patch: all from one hold of the store's lock. */
export interface PatchReply {
  snapshot: Snapshot;
  /** The fields refused because the store has moved on. The rest of the patch went. */
  conflicts: ('name' | 'project' | 'description' | 'tags')[];
  stored: StoredFields;
}

export interface ShiftInput {
  shiftId?: string | null;
  effectiveFrom: string;
  name: string;
  color: string;
  start: string;
  end: string;
  weekdays: number[];
  zone: string;
  breakMinutes: number;
  retired?: boolean;
}

export interface RateInput {
  projectId: string;
  rateCents: number | null;
  currency: Currency;
  effectiveFrom?: string | null;
  confirmRetroactive?: boolean;
}

export interface RoundingInput {
  projectId?: string | null;
  effectiveFrom: string;
  incrementMinutes: number;
  method: RoundMethod;
  confirmRetroactive?: boolean;
}

export type CellKind = 'text' | 'int' | 'number' | 'percent' | 'duration' | 'eur' | 'usd';

export interface AnalyticsSheet {
  name: string;
  title: string;
  columns: { header: string; kind: CellKind }[];
  rows: (string | number | null)[][];
  containsPrices: boolean;
}

export interface ExportRequest {
  dayZone: string;
  myZone: string;
  clientZone: string | null;
  includeEarnings: boolean;
  segmentShifts: Record<string, string>;
  analytics: AnalyticsSheet[];
  /** Only these task-days, or null for everything. The backend checks every id and leaves analytics out. */
  selection: SelectedEntry[] | null;
}

export interface TransitionResult {
  autoPausedTaskId: string | null;
  autoPausedTaskName: string | null;
  snapshot: Snapshot;
}

export interface WithId {
  id: string;
  snapshot: Snapshot;
}

/** A saved project or tag: the colour it got, and what was asked for when that clashed and was replaced. */
export interface SavedColor extends WithId {
  color: string;
  replacedFrom: string | null;
}
