// Taking over from the computer that holds the lease, or working here while no folder can be read. Two questions. The first says who
// holds it, when it was last heard from and which running tasks would be closed. The second says what happens next. Nothing is sent
// to take over before the button of the second question is pressed, and backing out of either changes nothing.
//
// Cloud folders sync late and can't lock anything, so every wording says what is known and no more: "inactive" is a guess made from
// the time since the last sign, and the other computer may simply not have synced yet. The backend reads the folders again before it
// acts and answers `changed` when the holder is no longer the one the first question showed. That sends the page back to the first
// question with the fresh view, and nothing has been taken over.
import { useEffect, useRef, useState } from 'react';

import { useApp } from '../app/store';
import { api } from '../lib/api';
import { errorMessage } from '../lib/errorMessage';
import { dayOf, parseIso } from '../lib/tz';
import type { LeaseHolderView, TakeOverMode, TakeOverPreview } from '../lib/types';
import { clock, shortDay } from './format';
import { Button, Modal } from './ui';

/**
 * D2 (design section 15). Off, as the owner asked: take over only "when the holder looks dead". When on, take over is also offered while the holder looks like it is still in
 * use, behind stronger words. A computer left on at the office with a timer running would otherwise keep this one read-only with no
 * way out. This is the one place that says so, and `LeaseBanner` and `TakeOverDialog` read it in the defaults of their `offerToLive`
 * props. Set it to false to offer take over only to a holder that looks inactive, as the request first put it: the banner then has
 * no Take over button for a holder that looks live, and the dialog won't go past its first question for one. Nothing else on the page
 * needs to change. The backend has a matching condition of its own (core `lease_io.rs`), which has to be flipped in step.
 */
export const OFFER_TAKE_OVER_TO_A_LIVE_HOLDER = false;

/** Whether take over is offered for a holder that looks live (`looksLive`) or inactive. `offerToLive` is D2. */
export function takeOverOffered(looksLive: boolean, offerToLive: boolean): boolean {
  return !looksLive || offerToLive;
}

/** What the page calls a computer whose name it doesn't have. It is the same word the backend's refusal uses. */
export const UNNAMED_COMPUTER = 'Another computer';

export function holderName(holder: LeaseHolderView | null): string {
  const name = holder?.deviceName.trim();
  return name ? name : UNNAMED_COMPUTER;
}

/**
 * A span of time in the backend's own words (`span_text`), so a notice from the backend and this page agree: "less than a minute",
 * "5 min", "2 h", "2 h 5 min", "1 day", "3 days".
 */
export function ageText(secs: number): string {
  const minutes = Math.floor(Math.max(0, secs) / 60);
  const days = Math.floor(minutes / (24 * 60));
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  if (days >= 2) return `${days} days`;
  if (days === 1) return '1 day';
  if (hours === 0) return rest === 0 ? 'less than a minute' : `${rest} min`;
  return rest === 0 ? `${hours} h` : `${hours} h ${rest} min`;
}

/**
 * How long ago the holder was last heard from, on the backend's clock. Worked out from the time of the last sign, not taken from the
 * snapshot's `ageSecs`, so a banner that has been up for an hour doesn't still say "2 min".
 */
export function ageOf(holder: LeaseHolderView, nowSec: number): number | null {
  return holder.lastSignAt === null ? holder.ageSecs : Math.max(0, nowSec - parseIso(holder.lastSignAt));
}

/** "14:12" for today, "11 Mar 14:12" for another day, on the wall clock of `zone`. */
export function whenText(sec: number, nowSec: number, zone: string): string {
  const day = dayOf(sec, zone);
  return day === dayOf(nowSec, zone) ? clock(sec, zone) : `${shortDay(day)} ${clock(sec, zone)}`;
}

/** The mode the backend is told. With no folder to read it can only be `offline`, and it is the one mode that is allowed then. */
const modeOf = (p: TakeOverPreview): TakeOverMode => (p.unreachable ? 'offline' : p.live ? 'live' : 'stale');

type Step = 'review' | 'confirm';

/** What the folders said when they were last read for this dialog. `changed` is true when the backend sent the page back here. */
type Look = { kind: 'loading' } | { kind: 'failed'; message: string } | { kind: 'ready'; preview: TakeOverPreview; changed: boolean };

const NOT_TAKEN_OVER = 'Nothing was taken over. Things changed while this was open, so here is how it stands now.';
const FOLDER_NOT_WRITABLE = "Nothing was changed. The sync folder couldn't be written to just now, so this computer did not take over. Check that the folder is available, then try again.";

export interface TakeOverDialogProps {
  open: boolean;
  /** Called when the dialog is left, whether by taking over or by backing out. */
  onClose: () => void;
  /** D2: see `OFFER_TAKE_OVER_TO_A_LIVE_HOLDER`. Only a test passes it. */
  offerToLive?: boolean;
}

/** Mounted only while open, so every opening starts at the first question with a fresh look at the folders. */
export function TakeOverDialog({ open, onClose, offerToLive = OFFER_TAKE_OVER_TO_A_LIVE_HOLDER }: TakeOverDialogProps) {
  return open ? <TakeOver onClose={onClose} offerToLive={offerToLive} /> : null;
}

function TakeOver({ onClose, offerToLive }: { onClose: () => void; offerToLive: boolean }) {
  const { snapshot, zones, nowSec, adopt } = useApp();
  const lease = snapshot.lease;
  const [look, setLook] = useState<Look>({ kind: 'loading' });
  const [looks, setLooks] = useState(0);
  const [step, setStep] = useState<Step>('review');
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const sending = useRef(false);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  // A fresh read of the folders, now and whenever the user asks to look again. A read that comes back after the dialog has gone is ignored.
  useEffect(() => {
    let alive = true;
    void api.leasePreviewTakeOver().then(
      (preview) => {
        if (alive) setLook({ kind: 'ready', preview, changed: false });
      },
      (e: unknown) => {
        if (alive) setLook({ kind: 'failed', message: errorMessage(e) });
      },
    );
    return () => {
      alive = false;
    };
  }, [looks]);

  const lookAgain = (): void => {
    setLook({ kind: 'loading' });
    setLooks((n) => n + 1);
  };

  const preview = look.kind === 'ready' ? look.preview : null;
  const holder = preview?.holder ?? lease.holder;
  const name = holderName(holder);
  // What the folders say wins over what the banner said when it was opened, once they have been read.
  const offline = preview ? preview.unreachable : lease.state === 'unknown';
  // Nobody holds it any more, whether the folders say so or the page was told while the dialog was open. Not while a take over is on its way.
  const nobodyHolds = !busy && (!lease.readOnly || (preview !== null && preview.holder === null));

  const back = (): void => {
    if (busy) return;
    setProblem(null);
    setStep('review');
  };

  async function send(p: TakeOverPreview): Promise<void> {
    const h = p.holder;
    if (h === null || sending.current) return;
    sending.current = true;
    setBusy(true);
    setProblem(null);
    try {
      const result = await api.leaseTakeOver({ generation: h.generation, beatSeq: h.beatSeq }, modeOf(p));
      adopt(result.snapshot);
      if (result.outcome === 'done') {
        onClose();
        return;
      }
      if (result.outcome === 'changed') {
        setStep('review');
        if (result.preview) setLook({ kind: 'ready', preview: result.preview, changed: true });
        else lookAgain();
        return;
      }
      setProblem(FOLDER_NOT_WRITABLE);
    } catch (e) {
      setProblem(errorMessage(e));
    } finally {
      sending.current = false;
      if (mounted.current) setBusy(false);
    }
  }

  if (nobodyHolds) {
    return (
      <Modal
        key="nobody"
        open
        title="Nothing to take over"
        onClose={onClose}
        footer={
          <Button variant="primary" data-autofocus="" onClick={onClose}>
            Close
          </Button>
        }
      >
        <p>No other computer is using zWorkTracker now, so this computer can make changes.</p>
      </Modal>
    );
  }

  if (step === 'confirm' && preview && preview.holder) {
    const sure = preview;
    return (
      <Modal
        key="confirm"
        open
        title="Are you sure?"
        onClose={onClose}
        dismissible={!busy}
        footer={
          <>
            <Button data-autofocus="" disabled={busy} onClick={back}>
              Back
            </Button>
            <Button
              variant="primary"
              disabled={busy}
              onClick={(e) => {
                // The second click of a double click on Continue lands here, and must not be what decides. A key press counts 0 and a click 1.
                if (e.detail > 1) return;
                void send(sure);
              }}
            >
              {busy ? 'Taking over…' : offline ? 'Work here anyway' : 'Take over'}
            </Button>
          </>
        }
      >
        {offline ? (
          <p>
            Until the folder can be reached, {name} may be making changes too. They merge; nothing is deleted. Once the folder is back, this computer takes over from {name}, and anything {name} left running is closed at its last known time. If
            another computer took over in the meantime, this one goes back to read-only. What you did here is kept and will merge.
          </p>
        ) : (
          <p>
            After this, {name} becomes read-only as soon as it hears about it. That can take a few minutes, and if it&apos;s offline, until it reconnects. Until then both computers could make changes. They merge; nothing is deleted.
          </p>
        )}
        {problem && (
          <p className="field-error" role="alert">
            {problem}
          </p>
        )}
      </Modal>
    );
  }

  const title = offline ? 'Work here anyway?' : `Take over from ${name}?`;
  const offered = preview === null || preview.holder === null ? false : preview.unreachable || takeOverOffered(preview.live, offerToLive);
  return (
    <Modal
      key="review"
      open
      title={title}
      onClose={onClose}
      footer={
        <>
          <Button data-autofocus="" onClick={onClose}>
            Cancel
          </Button>
          {look.kind === 'failed' && <Button onClick={lookAgain}>Try again</Button>}
          {(offered || look.kind === 'loading') && (
            <Button variant="primary" disabled={!offered} onClick={() => setStep('confirm')}>
              Continue
            </Button>
          )}
        </>
      }
    >
      {look.kind === 'loading' && <p role="status">Reading the sync folders…</p>}
      {look.kind === 'failed' && (
        <p className="field-error" role="alert">
          Couldn&apos;t look at the sync folders: {look.message}
        </p>
      )}
      {look.kind === 'ready' && look.preview.holder && (
        <Review preview={look.preview} holder={look.preview.holder} changed={look.changed} offered={offered} name={name} nowSec={nowSec} zone={zones.my} />
      )}
    </Modal>
  );
}

/** "Last sign of LAPTOP: 3 min ago (14:12), and its clock seems to differ from this one." */
function lastSign(holder: LeaseHolderView, name: string, nowSec: number, zone: string): string {
  const age = ageOf(holder, nowSec);
  if (age === null || holder.lastSignAt === null) return `No sign of ${name} has been seen yet.`;
  const skew = holder.clockSuspect ? ', and its clock seems to differ from this one' : '';
  return `Last sign of ${name}: ${ageText(age)} ago (${whenText(parseIso(holder.lastSignAt), nowSec, zone)})${skew}.`;
}

function Review({ preview, holder, changed, offered, name, nowSec, zone }: { preview: TakeOverPreview; holder: LeaseHolderView; changed: boolean; offered: boolean; name: string; nowSec: number; zone: string }) {
  const sign = lastSign(holder, name, nowSec, zone);
  const note = changed ? (
    <p role="status" data-testid="takeover-changed">
      {NOT_TAKEN_OVER}
    </p>
  ) : null;

  if (preview.unreachable) {
    return (
      <>
        {note}
        <p>The sync folder can&apos;t be reached, so this computer can&apos;t tell whether {name} is still using zWorkTracker.</p>
        <p>{sign}</p>
        <p>If you go on, this computer can make changes now, without waiting for the folder.</p>
      </>
    );
  }

  const segments = preview.segments;
  const ends = (end: string): string => whenText(parseIso(end), nowSec, zone);
  return (
    <>
      {note}
      <p>{sign}</p>
      {preview.live ? (
        <p>
          <strong>It looks like it&apos;s still in use.</strong> {offered ? 'It will stop within a few minutes of hearing about this.' : 'Take over is offered once it looks inactive.'}
        </p>
      ) : (
        <p>It looks inactive, but this can simply mean its cloud app hasn&apos;t synced yet.</p>
      )}
      {offered && segments.length === 1 && segments[0] && (
        <p>
          &quot;{segments[0].taskName || 'A task'}&quot; is shown as running there. It will close at {ends(segments[0].end)}, the last moment that computer is known to have been running. That is written in the audit trail. If {name} really kept working,
          its time comes back when it next syncs.
        </p>
      )}
      {offered && segments.length > 1 && (
        <>
          <p>{segments.length} tasks are shown as running there. Each will close at the last moment that computer is known to have been running:</p>
          <ul style={{ margin: 0, paddingInlineStart: 20 }}>
            {segments.map((s) => (
              <li key={s.segmentId}>
                &quot;{s.taskName || 'A task'}&quot; at {ends(s.end)}
              </li>
            ))}
          </ul>
          <p>That is written in the audit trail. If {name} really kept working, that time comes back when it next syncs.</p>
        </>
      )}
    </>
  );
}
