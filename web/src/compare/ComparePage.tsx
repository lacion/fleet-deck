import { useEffect, useRef, useState } from 'react';
import { trapTab, useFocusReturn } from '../app/focus';
import { useOverlay } from '../app/overlay';
import { formatHash } from '../app/routes';
import { sessionTitle } from '../app/sessions';
import { API_PRICES, apiPricesTitle, costText, tokens } from '../board/usage';
import { stationName } from '../board/fleet';
import { Diff } from '../chat/Diff';
import type { DeckSession, SessionDiff, SessionKey, StationView } from '../gen';
import type { Freshness, Timeline } from '../runtime/state';
import { hasChanges } from '../session/changes';
import type { Outcome, SessionActions } from '../session/commands';
import { modelFamily, shortModel } from '../ui/format';
import { Check, GitCompare, Icon, Trash2, X } from '../ui/icons';
import { Loading } from '../ui/Loading';
import { sessionRef } from '../ui/localSet';
import { ErrorNotice } from '../ui/page';
import {
  digestOf,
  kept,
  keepConfirm,
  keepOne,
  type KeepDeps,
  type KeepProgress,
  lastAgentMessage,
  patchFiles,
  type RaceSummary,
  siblings,
} from './compare';
import './compare.css';

const STATUS_LABEL: Record<DeckSession['status'], string> = {
  queued: 'Queued',
  working: 'Working',
  verifying: 'Verifying',
  needs_you: 'Needs you',
  idle: 'Waiting for a prompt',
  ended: 'Ended',
  unreachable: 'Station unreachable',
  lost: 'Lost',
};

/** The most changed files that a column lists before "and n more". */
const MAX_FILES = 12;

type DiffState =
  | { readonly kind: 'loading' }
  | { readonly kind: 'error'; readonly text: string }
  | { readonly kind: 'ready'; readonly diff: SessionDiff };

type Props = {
  readonly batchId: string;
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly timelines: Readonly<Record<string, Timeline>>;
  readonly freshness: Freshness;
  /** Opens the live timeline of a session; returns the cancel function. */
  readonly watch?: ((key: SessionKey) => () => void) | undefined;
  readonly sessionActions?: ((key: SessionKey) => SessionActions) | undefined;
  /** The worktree trash command (owner only); absent: "Keep this one" is off. */
  readonly trash?: ((key: SessionKey) => Promise<Outcome>) | undefined;
  /** The race summary (R10.5); absent: no summary slot. */
  readonly race?: ((keys: readonly SessionKey[]) => Promise<RaceSummary>) | undefined;
  readonly onOpen: (key: SessionKey) => void;
  /** The wait between two looks at a stopping session; tests give a fast one. */
  readonly sleep?: ((ms: number) => Promise<void>) | undefined;
};

const keyOf = (c: DeckSession): SessionKey => ({
  station_id: c.station_id,
  session_id: c.summary.session_id,
});
const timelineKey = (k: SessionKey) => JSON.stringify([k.station_id, k.session_id]);

function DiffDialog({
  title,
  diff,
  onClose,
}: {
  readonly title: string;
  readonly diff: SessionDiff;
  readonly onClose: () => void;
}) {
  const root = useRef<HTMLDivElement>(null);
  const close = useRef<HTMLButtonElement>(null);
  useFocusReturn(root, close);
  useOverlay();
  return (
    // The overlay click is a mouse shortcut; the Close button and Escape serve the keyboard.
    <div
      className="fd-overlay"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        ref={root}
        className="fd-dialog fd-compare-diff"
        role="dialog"
        aria-modal="true"
        aria-label={`Diff of ${title}`}
        onKeyDown={(e) => {
          if (e.key === 'Escape') {
            e.stopPropagation();
            onClose();
          } else trapTab(e, root.current);
        }}
      >
        <h2>{title}</h2>
        <Diff patch={diff.patch} truncated={diff.truncated} id="compare" />
        <button ref={close} type="button" className="fd-ghost" onClick={onClose}>
          <Icon of={X} size={14} /> Close
        </button>
      </div>
    </div>
  );
}

function Changes({ state, onShow }: { readonly state: DiffState; readonly onShow: () => void }) {
  if (state.kind === 'loading') return <Loading text="Loading the diff…" />;
  if (state.kind === 'error') return <ErrorNotice text={state.text} />;
  const { summary, patch } = state.diff;
  if (!hasChanges(summary) && patch.trim() === '') {
    return <p className="fd-note">No file changes since the session started.</p>;
  }
  const files = patchFiles(patch);
  return (
    <>
      <p className="fd-turn-stat" data-testid="compare-stat">
        {summary.files_changed} {summary.files_changed === 1 ? 'file' : 'files'}{' '}
        <span className="fd-add">+{summary.insertions}</span>{' '}
        <span className="fd-del">−{summary.deletions}</span>
      </p>
      {files.length === 0 ? null : (
        <ul className="fd-compare-files">
          {files.slice(0, MAX_FILES).map((path) => (
            <li key={path} title={path}>
              {path}
            </li>
          ))}
          {files.length > MAX_FILES ? <li>and {files.length - MAX_FILES} more</li> : null}
        </ul>
      )}
      <button type="button" className="fd-sm" onClick={onShow}>
        Show diff
      </button>
    </>
  );
}

function Column({
  card,
  stations,
  timeline,
  diff,
  isKept,
  canKeep,
  keepNote,
  progress,
  confirming,
  busy,
  onOpen,
  onShow,
  onAskKeep,
  onCancel,
  onKeep,
  others,
}: {
  readonly card: DeckSession;
  readonly stations: readonly StationView[];
  readonly timeline: Timeline | undefined;
  readonly diff: DiffState;
  readonly isKept: boolean;
  readonly canKeep: boolean;
  /** Why "Keep this one" is off, or `null`. */
  readonly keepNote: string | null;
  readonly progress: KeepProgress | undefined;
  readonly confirming: boolean;
  readonly busy: boolean;
  readonly onOpen: () => void;
  readonly onShow: () => void;
  readonly onAskKeep: () => void;
  readonly onCancel: () => void;
  readonly onKeep: () => void;
  readonly others: number;
}) {
  const s = card.summary;
  const title = sessionTitle(card);
  const message = timeline?.ready === true ? lastAgentMessage(timeline.events) : null;
  const digest = digestOf(s);
  const cost = costText(s.usage);
  // Only what the station reports (principle 7: no "unknown"); the cost is named as on every
  // surface, "at API prices" (§5 Cost).
  const usage = [
    s.usage.input_tokens === null ? null : `${tokens(s.usage.input_tokens)} in`,
    s.usage.output_tokens === null ? null : `${tokens(s.usage.output_tokens)} out`,
  ].filter((part) => part !== null);
  return (
    <li
      className="fd-compare-col"
      data-session={s.session_id}
      data-status={card.status}
      data-kept={isKept ? 'true' : undefined}
    >
      <header>
        <h2>
          <a
            href={formatHash({
              name: 'session',
              station: card.station_id,
              session: s.session_id,
              tab: 'chat',
            })}
            onClick={(e) => {
              if (e.metaKey || e.ctrlKey || e.shiftKey) return;
              e.preventDefault();
              onOpen();
            }}
          >
            {title}
          </a>
        </h2>
        <p className="fd-compare-meta">
          <span className="fd-dot" data-status={card.status} aria-hidden="true" />{' '}
          {STATUS_LABEL[card.status]}
          {' · '}
          <span className="fd-badge" data-family={modelFamily(s.model) ?? undefined}>
            {shortModel(s.model)}
          </span>
          {' · '}
          {stationName(card.station_id, stations)}
          {s.batch === undefined ? null : ` · ${s.batch.index} of ${s.batch.size}`}
        </p>
        {isKept ? (
          <p className="fd-compare-kept">
            <Icon of={Check} size={12} /> Kept from batch
          </p>
        ) : null}
      </header>
      {digest === null ? null : (
        <section aria-label="Digest">
          <h3 className="fd-label">Digest</h3>
          <p className="fd-compare-digest">{digest}</p>
        </section>
      )}
      <section aria-label="Changes">
        <h3 className="fd-label">Changes</h3>
        <Changes state={diff} onShow={onShow} />
      </section>
      <section aria-label="Last agent message">
        <h3 className="fd-label">Last agent message</h3>
        {message === null ? (
          <p className="fd-note">
            {timeline?.ready === true ? 'No agent message yet.' : 'Loading the timeline…'}
          </p>
        ) : (
          <p className="fd-compare-message">{message}</p>
        )}
      </section>
      {usage.length === 0 && cost === null ? null : (
        <section aria-label="Usage">
          <h3 className="fd-label">Usage</h3>
          <p className="fd-compare-usage">
            {usage.join(' · ')}
            {usage.length > 0 && cost !== null ? ' · ' : ''}
            {cost === null ? null : (
              <span title={apiPricesTitle(1)}>{`${cost} ${API_PRICES}`}</span>
            )}
          </p>
        </section>
      )}
      <footer>
        {progress === undefined ? null : (
          <p
            className="fd-compare-progress"
            data-step={progress.step}
            role={progress.step === 'failed' ? 'alert' : 'status'}
          >
            {progress.text}
          </p>
        )}
        {confirming ? (
          <div className="fd-compare-confirm" role="group" aria-label="Keep this one">
            <p>{keepConfirm(title, others)}</p>
            <button type="button" className="fd-primary fd-sm" disabled={busy} onClick={onKeep}>
              Keep {title}
            </button>
            <button type="button" className="fd-ghost fd-sm" disabled={busy} onClick={onCancel}>
              Cancel
            </button>
          </div>
        ) : (
          <button
            type="button"
            className="fd-compare-keep"
            disabled={!canKeep || busy}
            title={keepNote ?? undefined}
            onClick={onAskKeep}
          >
            <Icon of={Trash2} size={14} /> Keep this one
          </button>
        )}
        {keepNote === null ? null : <p className="fd-note">{keepNote}</p>}
      </footer>
    </li>
  );
}

function Summary({
  race,
  keys,
}: {
  readonly race: NonNullable<Props['race']>;
  readonly keys: readonly SessionKey[];
}) {
  const [state, setState] = useState<
    | { readonly kind: 'idle' }
    | { readonly kind: 'loading' }
    | { readonly kind: 'text'; readonly text: string }
    | { readonly kind: 'error'; readonly text: string }
    | { readonly kind: 'absent' }
  >({ kind: 'idle' });
  if (state.kind === 'absent') return null;
  return (
    <section className="fd-compare-summary" aria-label="Summary">
      <h2 className="fd-label">Summary</h2>
      {state.kind === 'text' ? (
        <>
          <p className="fd-compare-summary-text">{state.text}</p>
          <p className="fd-note">
            Generated by a model from the session digests. It picks no winner.
          </p>
        </>
      ) : null}
      {state.kind === 'error' ? (
        <p className="fd-note" role="alert">
          {state.text}
        </p>
      ) : null}
      <button
        type="button"
        className="fd-ghost fd-sm"
        disabled={state.kind === 'loading'}
        onClick={() => {
          setState({ kind: 'loading' });
          void race(keys).then(
            (answer) =>
              setState(
                answer.kind === 'text'
                  ? { kind: 'text', text: answer.text }
                  : answer.kind === 'absent'
                    ? { kind: 'absent' }
                    : { kind: 'error', text: answer.text },
              ),
            (e: unknown) =>
              setState({
                kind: 'error',
                text: e instanceof Error ? e.message : 'The request failed.',
              }),
          );
        }}
      >
        {state.kind === 'loading' ? 'Summarizing…' : 'Summarize the race'}
      </button>
    </section>
  );
}

const realSleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

/** The compare page of one `Nx` batch (R1.4, DL-170): one column for each sibling, and "Keep this one". */
export function ComparePage({
  batchId,
  sessions,
  stations,
  timelines,
  freshness,
  watch,
  sessionActions,
  trash,
  race,
  onOpen,
  sleep = realSleep,
}: Props) {
  const cards = siblings(sessions, batchId);
  const keys = cards.map(keyOf);
  // The effects depend on the ids only, not on a new array of each render.
  const keyText = JSON.stringify(keys.map((k) => [k.station_id, k.session_id]));
  const keptRefs = kept.use();
  const [diffs, setDiffs] = useState<Readonly<Record<string, DiffState>>>({});
  const [shown, setShown] = useState<string | null>(null);
  const [confirming, setConfirming] = useState<string | null>(null);
  const [progress, setProgress] = useState<Readonly<Record<string, KeepProgress>>>({});
  const [running, setRunning] = useState(false);
  const latest = useRef(sessions);
  latest.current = sessions;
  const actionsFor = useRef(sessionActions);
  actionsFor.current = sessionActions;

  // The timelines of all siblings (the last agent message).
  useEffect(() => {
    if (watch === undefined) return undefined;
    const cancels = parseKeys(keyText).map((key) => watch(key));
    return () => {
      for (const cancel of cancels) cancel();
    };
  }, [keyText, watch]);

  // The session diff of each sibling, loaded once when the sibling first shows.
  const asked = useRef(new Set<string>());
  useEffect(() => {
    for (const key of parseKeys(keyText)) {
      const id = timelineKey(key);
      if (asked.current.has(id)) continue;
      const files = actionsFor.current?.(key).files ?? null;
      if (files === null) continue;
      asked.current.add(id);
      setDiffs((d) => ({ ...d, [id]: { kind: 'loading' } }));
      files.sessionDiff().then(
        (diff) => setDiffs((d) => ({ ...d, [id]: { kind: 'ready', diff } })),
        (e: unknown) =>
          setDiffs((d) => ({
            ...d,
            [id]: {
              kind: 'error',
              text: e instanceof Error ? e.message : 'The diff request failed.',
            },
          })),
      );
    }
  }, [keyText]);

  const canKeep =
    freshness === 'live' &&
    trash !== undefined &&
    sessionActions?.(keys[0] ?? { station_id: '', session_id: '' }).canDrive === true &&
    cards.length >= 2;
  const keepNote =
    cards.length < 2
      ? 'Only one session of this batch is left.'
      : trash === undefined
        ? 'Only the owner can move worktrees to the trash.'
        : freshness !== 'live'
          ? 'The board is not live. Wait for it to reconnect.'
          : null;

  const keep = (chosen: DeckSession) => {
    const others = cards.filter((c) => c !== chosen).map(keyOf);
    if (trash === undefined || sessionActions === undefined || running) return;
    setConfirming(null);
    setRunning(true);
    setProgress({});
    kept.add([sessionRef(chosen.station_id, chosen.summary.session_id)]);
    const deps: KeepDeps = {
      summary: (key) =>
        latest.current.find(
          (c) => c.station_id === key.station_id && c.summary.session_id === key.session_id,
        )?.summary ?? null,
      stop: (key) => sessionActions(key).stop('graceful'),
      trash,
      sleep,
    };
    void keepOne(others, deps, (p) =>
      setProgress((all) => ({ ...all, [timelineKey(p.key)]: p })),
    ).finally(() => setRunning(false));
  };

  const shownCard = cards.find((c) => timelineKey(keyOf(c)) === shown);
  const shownDiff = shown === null ? undefined : diffs[shown];
  return (
    <div className="fd-screen fd-compare">
      <h1 className="fd-screen-title">
        <Icon of={GitCompare} size={18} /> Compare {cards.length} sessions
      </h1>
      {cards.length === 0 ? (
        <>
          <p>This batch has no session on the board.</p>
          <a href="#/fleet">Back to the fleet</a>
        </>
      ) : (
        <>
          <p className="fd-note">
            One column for each session of the batch. The choice is yours: nothing here picks a
            winner.
          </p>
          {race === undefined ? null : <Summary race={race} keys={keys} />}
          <ul className="fd-compare-cols" aria-label="Sessions of the batch">
            {cards.map((card) => {
              const key = keyOf(card);
              const id = timelineKey(key);
              return (
                <Column
                  key={id}
                  card={card}
                  stations={stations}
                  timeline={timelines[id]}
                  diff={diffs[id] ?? { kind: 'loading' }}
                  isKept={keptRefs.has(sessionRef(card.station_id, card.summary.session_id))}
                  canKeep={canKeep}
                  keepNote={keepNote}
                  progress={progress[id]}
                  confirming={confirming === id}
                  busy={running}
                  others={cards.length - 1}
                  onOpen={() => onOpen(key)}
                  onShow={() => setShown(id)}
                  onAskKeep={() => setConfirming(id)}
                  onCancel={() => setConfirming(null)}
                  onKeep={() => keep(card)}
                />
              );
            })}
          </ul>
        </>
      )}
      {shownCard !== undefined && shownDiff?.kind === 'ready' ? (
        <DiffDialog
          title={sessionTitle(shownCard)}
          diff={shownDiff.diff}
          onClose={() => setShown(null)}
        />
      ) : null}
    </div>
  );
}

function parseKeys(text: string): SessionKey[] {
  return (JSON.parse(text) as [string, string][]).map(([station_id, session_id]) => ({
    station_id,
    session_id,
  }));
}
