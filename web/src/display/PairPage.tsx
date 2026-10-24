// R13 P2c (design §5A item 7, §5B item 2, §5C item 1, §5E item 4): the pair page of a wall
// display, `/display#/display-pair`. It asks the deck for a pairing, shows the 6-digit code on the
// full screen, and polls until the owner has approved it. It has no button and no control.
import { useEffect, useRef, useState } from 'react';
import { ApiError, type Fetch } from '../runtime/api';
import { Mark } from '../ui/Mark';
import { useNow } from '../ui/useNow';
import { type expectedLaunch, pollPairing, startPairing } from './api';

/** The wait between two polls (the deck allows one a second). */
export const PAIR_POLL_MS = 2_000;
/** The wait before a new try after the deck is busy or limits the page. */
export const PAIR_RETRY_MS = 30_000;

export type PairState =
  | { readonly kind: 'starting' }
  | { readonly kind: 'waiting'; readonly code: string; readonly expiresAt: number }
  | { readonly kind: 'superseded'; readonly text: string }
  | { readonly kind: 'wait'; readonly text: string };

/** `123456` as `123 456`. */
export const groupCode = (code: string): string =>
  code.length === 6 ? `${code.slice(0, 3)} ${code.slice(3)}` : code;

/** `M:SS` for a number of milliseconds (0 when it is past). */
export function countdown(ms: number): string {
  const s = Math.max(0, Math.ceil(ms / 1000));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}

type Props = {
  readonly fetcher: Fetch;
  readonly launch: ReturnType<typeof expectedLaunch>;
  /** The kiosk holds its display cookie now. */
  readonly onClaimed: () => void;
  readonly pollMs?: number;
  readonly retryMs?: number;
  /** Unix ms of "now" (tests). */
  readonly now?: number;
  /** A state at the first render (tests, a server render). */
  readonly initial?: PairState;
};

export function PairPage({
  fetcher,
  launch,
  onClaimed,
  pollMs = PAIR_POLL_MS,
  retryMs = PAIR_RETRY_MS,
  now: givenNow,
  initial = { kind: 'starting' },
}: Props) {
  const [state, setState] = useState<PairState>(initial);
  const clock = useNow(givenNow, 1000);
  const latest = useRef({ fetcher, launch, onClaimed });
  latest.current = { fetcher, launch, onClaimed };
  const launchKey = launch === null ? '' : `${launch.display_id}/${launch.operation_revision}`;
  useEffect(() => {
    let live = true;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const later = (fn: () => void, ms: number) => {
      timer = setTimeout(fn, ms);
    };
    const start = async () => {
      try {
        const started = await startPairing(latest.current.fetcher, latest.current.launch);
        if (!live) return;
        setState({ kind: 'waiting', code: started.code, expiresAt: started.expires_at });
        later(() => void poll(started.pairing_id, started.expires_at), pollMs);
      } catch (e) {
        if (!live) return;
        if (e instanceof ApiError && e.status !== 429 && e.status !== 503) {
          setState({ kind: 'wait', text: e.message });
        } else {
          setState({
            kind: 'wait',
            text: e instanceof Error ? e.message : 'The deck did not answer.',
          });
        }
        later(() => void start(), retryMs);
      }
    };
    const poll = async (id: string, expiresAt: number) => {
      try {
        const next = await pollPairing(latest.current.fetcher, id);
        if (!live) return;
        if (next === 'approved_claimed') {
          latest.current.onClaimed();
          return;
        }
        // The code ends: the page asks for a new one.
        if (Date.now() >= expiresAt && givenNow === undefined) {
          setState({ kind: 'starting' });
          void start();
          return;
        }
        later(() => void poll(id, expiresAt), pollMs);
      } catch (e) {
        if (!live) return;
        if (e instanceof ApiError && e.status === 409 && e.code === 'superseded') {
          setState({ kind: 'superseded', text: e.message });
          return;
        }
        if (e instanceof ApiError && (e.status === 404 || e.status === 401)) {
          // An expired or unknown pairing: a new code.
          setState({ kind: 'starting' });
          void start();
          return;
        }
        later(() => void poll(id, expiresAt), pollMs);
      }
    };
    if (initial.kind === 'starting') void start();
    return () => {
      live = false;
      if (timer !== null) clearTimeout(timer);
    };
    // The launch values stand for `launch`.
  }, [launchKey, pollMs, retryMs]);
  return (
    <main className="fd-wall-root fd-pair" data-mode="pair">
      <section className="fd-pair-box" aria-label="Pair this wall display">
        <Mark size={32} />
        <h1 className="fd-pair-title">Pair this wall display</h1>
        {state.kind === 'waiting' ? (
          <>
            <p className="fd-pair-code" aria-label="Pairing code">
              {groupCode(state.code)}
            </p>
            <p className="fd-pair-line">
              On your Fleet Deck board, open Settings, Displays, and enter this code.
            </p>
            <p className="fd-wall-note" role="status">
              The code ends in {countdown(state.expiresAt - clock)}
            </p>
          </>
        ) : null}
        {state.kind === 'starting' ? (
          <p className="fd-pair-line" role="status">
            Asking the deck for a code…
          </p>
        ) : null}
        {state.kind === 'superseded' || state.kind === 'wait' ? (
          <p className="fd-pair-line" role="alert">
            {state.text}
          </p>
        ) : null}
      </section>
    </main>
  );
}
