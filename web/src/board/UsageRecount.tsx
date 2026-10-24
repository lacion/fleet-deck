// B20 (design `usage-cost.md` §4.4): the recount of old counts, in the Usage tab. One row with a
// button; a confirm dialog; one `session.usage_recount` command per session that ended (at most 4
// at once); a result line that follows the sessions: a command counts as recounted only when the
// summary of its session shows its command id.
import { useRef, useState } from 'react';
import type { DeckSession, SessionKey } from '../gen';
import { Dialog } from '../integrations/Dialog';
import { Loading } from '../ui/Loading';
import { splitUsage } from './usage';
import type { RecountCall, RecountResult } from './usageRecountCall';

/** The most recount commands in flight at once. */
export const MAX_AT_ONCE = 4;

export const CONFIRM_TITLE = 'Recount the old counts?';
export const CONFIRM_TEXT =
  'Fleet Deck reads the provider records on each station (Claude transcripts, Codex session files) and replaces the old count of every Claude and Codex session that ended. A record can miss a killed run or Codex sub-agents, so a recounted total shows "at least". Properties keeps the old count.';

/** One session of a run: sent, or left out because it may still run. */
export type RecountEntry =
  | { readonly key: SessionKey; readonly sent: false }
  | { readonly key: SessionKey; readonly sent: true; readonly result: RecountResult };

export type RecountRun = {
  readonly phase: 'idle' | 'confirm' | 'running' | 'done';
  /** The commands of this run that finished. */
  readonly finished: number;
  /** The commands of this run. */
  readonly total: number;
  readonly entries: readonly RecountEntry[];
};

export const IDLE_RUN: RecountRun = { phase: 'idle', finished: 0, total: 0, entries: [] };

const keyOf = (s: DeckSession): SessionKey => ({
  station_id: s.station_id,
  session_id: s.summary.session_id,
});

/**
 * The sessions whose whole total is an old count (a Claude or Codex session with no new-rule
 * usage), split by state: `ended` are sent, `running` may still run and are not sent.
 */
export function eligibleSessions(sessions: readonly DeckSession[]): {
  readonly ended: readonly DeckSession[];
  readonly running: readonly DeckSession[];
} {
  const old = sessions.filter(
    (s) =>
      (s.summary.provider === 'claude' || s.summary.provider === 'codex') &&
      splitUsage(s.summary).current === null,
  );
  const isEnded = (s: DeckSession) => s.summary.state === 'exited' || s.summary.state === 'lost';
  return { ended: old.filter(isEnded), running: old.filter((s) => !isEnded(s)) };
}

/** Runs `recount` for each key, at most `MAX_AT_ONCE` at once; `onDone` runs after each one. */
export async function runRecounts(
  keys: readonly SessionKey[],
  recount: RecountCall,
  onDone: (finished: number) => void = () => {},
): Promise<RecountResult[]> {
  const results: RecountResult[] = new Array(keys.length);
  let next = 0;
  let finished = 0;
  const worker = async () => {
    while (next < keys.length) {
      const at = next;
      next += 1;
      const key = keys[at] as SessionKey;
      results[at] = await recount(key);
      finished += 1;
      onDone(finished);
    }
  };
  await Promise.all(Array.from({ length: Math.min(MAX_AT_ONCE, keys.length) }, worker));
  return results;
}

/**
 * The result line of a run, from the call results and the sessions as they are now: the parts
 * that are not 0, joined by a space (design §4.4 item 7).
 */
export function resultLine(
  entries: readonly RecountEntry[],
  sessions: readonly DeckSession[],
): string {
  let recounted = 0;
  let noRecord = 0;
  let waiting = 0;
  let running = 0;
  let otherHistory = 0;
  let originUnproven = 0;
  const stations = new Set<string>();
  let failed = 0;
  let firstMessage: string | null = null;
  for (const entry of entries) {
    if (!entry.sent) {
      running += 1;
      continue;
    }
    const result = entry.result;
    switch (result.kind) {
      case 'accepted':
      case 'pending': {
        const session = sessions.find(
          (s) =>
            s.station_id === entry.key.station_id && s.summary.session_id === entry.key.session_id,
        );
        const recount = session?.summary.usage_recount;
        if (recount === undefined || recount.command_id !== result.commandId) waiting += 1;
        else if (recount.source === 'none') noRecord += 1;
        else recounted += 1;
        break;
      }
      case 'not_supported':
        stations.add(entry.key.station_id);
        break;
      case 'refused':
        if (result.code === 'session_running') running += 1;
        else if (result.code === 'other_history') otherHistory += 1;
        else if (result.code === 'origin_unproven') originUnproven += 1;
        else {
          failed += 1;
          firstMessage ??= result.message;
        }
        break;
      case 'failed':
        failed += 1;
        firstMessage ??= result.text;
        break;
    }
  }
  const parts: string[] = [];
  if (recounted > 0) {
    parts.push(recounted === 1 ? 'Recounted 1 session.' : `Recounted ${recounted} sessions.`);
  }
  if (noRecord > 0) {
    parts.push(`${noRecord} have no provider record: their totals are now unknown.`);
  }
  if (waiting > 0) parts.push(`${waiting} wait for their new totals.`);
  if (running > 0) parts.push(`${running} may still run and keep their old counts.`);
  if (otherHistory > 0) {
    parts.push(
      `${otherHistory} hold usage of another session in their record and keep their old counts.`,
    );
  }
  if (originUnproven > 0) {
    parts.push(
      `${originUnproven} have no proof that their conversation started in Fleet Deck and keep their old counts.`,
    );
  }
  if (stations.size > 0) parts.push(`${stations.size} stations need a newer Fleet Deck.`);
  if (failed > 0) {
    parts.push(
      `${failed} could not be recounted${firstMessage === null ? '' : ` (${firstMessage})`}.`,
    );
  }
  return parts.join(' ');
}

/** The state of one run, and the three steps that change it: ask, cancel, confirm. */
export type RecountFlow = {
  get(): RecountRun;
  /** Opens the confirm dialog. */
  ask(): void;
  /** Closes the confirm dialog: no command is sent. */
  cancel(): void;
  /** Sends one command per ended session of `sessions`; the others are left out. */
  confirm(sessions: readonly DeckSession[]): Promise<void>;
};

export function recountFlow(
  recount: RecountCall,
  notify: (run: RecountRun) => void,
  start: RecountRun = IDLE_RUN,
): RecountFlow {
  let run = start;
  const set = (next: RecountRun) => {
    run = next;
    notify(run);
  };
  return {
    get: () => run,
    ask: () => {
      if (run.phase === 'running') return;
      set({ ...run, phase: 'confirm' });
    },
    cancel: () => {
      if (run.phase !== 'confirm') return;
      set({ ...run, phase: run.entries.length > 0 ? 'done' : 'idle' });
    },
    confirm: async (sessions) => {
      if (run.phase !== 'confirm') return;
      const { ended, running } = eligibleSessions(sessions);
      const keys = ended.map(keyOf);
      const left: RecountEntry[] = running.map((s) => ({ key: keyOf(s), sent: false }));
      set({ phase: 'running', finished: 0, total: keys.length, entries: left });
      const results = await runRecounts(keys, recount, (finished) => set({ ...run, finished }));
      const sent: RecountEntry[] = keys.map((key, i) => ({
        key,
        sent: true,
        result: results[i] as RecountResult,
      }));
      set({
        phase: 'done',
        finished: keys.length,
        total: keys.length,
        entries: [...left, ...sent],
      });
    },
  };
}

type Props = {
  readonly sessions: readonly DeckSession[];
  readonly recount: RecountCall;
  /** The state to start from (tests: a static render shows a state). */
  readonly initial?: RecountRun | undefined;
};

/** The "Old counts" row of the Usage tab, its confirm dialog and its result line. */
export function UsageRecount({ sessions, recount, initial }: Props) {
  const [run, setRun] = useState<RecountRun>(initial ?? IDLE_RUN);
  const latest = useRef(recount);
  latest.current = recount;
  const [flow] = useState(() => recountFlow((key) => latest.current(key), setRun, initial));
  const { ended, running } = eligibleSessions(sessions);
  const line = resultLine(run.entries, sessions);
  const eligible = ended.length + running.length;
  if (eligible === 0 && run.entries.length === 0) return null;
  return (
    <>
      {eligible === 0 ? null : (
        <dl className="fd-usage-stats">
          <div className="fd-usage-stat">
            <dt className="fd-label">Old counts</dt>
            <dd>
              {run.phase === 'running' ? (
                <Loading text={`Recounting ${run.finished} of ${run.total}`} />
              ) : (
                <button type="button" onClick={flow.ask}>
                  Recount from provider records
                </button>
              )}
            </dd>
          </div>
        </dl>
      )}
      {line === '' ? null : (
        <p className="fd-note" role="status">
          {line}
        </p>
      )}
      {run.phase === 'confirm' ? (
        <Dialog title={CONFIRM_TITLE} description={CONFIRM_TEXT} onClose={flow.cancel}>
          <div className="fd-access-foot">
            <button type="button" className="fd-ghost" onClick={flow.cancel}>
              Cancel
            </button>
            <button
              type="button"
              className="fd-primary"
              onClick={() => void flow.confirm(sessions)}
            >
              Recount
            </button>
          </div>
        </Dialog>
      ) : null}
    </>
  );
}
