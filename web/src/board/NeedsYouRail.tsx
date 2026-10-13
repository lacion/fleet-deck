import { useEffect, useRef, useState } from 'react';
import { shouldIgnore } from '../app/keys';
import { formatHash } from '../app/routes';
import { relativeAge } from '../app/sessions';
import type { DeckSession, OpenRequest, SessionKey, StationView } from '../gen';
import type { SessionActions } from '../session/commands';
import { questionForm } from '../session/question';
import { Bell, BellOff, Check, ChevronRight, Icon } from '../ui/icons';
import { useNow } from '../ui/useNow';
import { stationName } from './fleet';
import {
  freshRequests,
  notifyEnabled,
  type Permission,
  permission,
  show,
  storeNotify,
  waitingRequests,
} from './notify';

type Props = {
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly onOpen: (key: SessionKey) => void;
  readonly sessionActions?: ((key: SessionKey) => SessionActions) | undefined;
  readonly variant: 'side' | 'strip';
  readonly now?: number | undefined;
};

/** Open approvals of every session, the oldest first (PLAN §12 needs-you rail). */
export function NeedsYouRail({
  sessions,
  stations,
  onOpen,
  sessionActions,
  variant,
  now: given,
}: Props) {
  const now = useNow(given, 30000);
  const waiting = sessions
    .flatMap((s) =>
      s.summary.open_requests.map((r) => ({
        key: { station_id: s.station_id, session_id: s.summary.session_id },
        card: s,
        title: s.summary.title ?? s.summary.session_id,
        request: r,
      })),
    )
    .sort((a, b) => a.request.requested_at - b.request.requested_at);
  const [notify, setNotify] = useState(notifyEnabled);
  const [pending, setPending] = useState<ReadonlySet<string>>(new Set());
  const [errors, setErrors] = useState<Readonly<Record<string, string>>>({});
  const [picked, setPicked] = useState(0);
  useNotifications(sessions, notify, onOpen);
  // The selection stays inside the list when the list shrinks.
  const selected = Math.min(picked, Math.max(waiting.length - 1, 0));
  const answerableOf = (w: (typeof waiting)[number]) => {
    const actions = sessionActions?.(w.key);
    const r = w.request;
    const ok =
      actions?.canApprove === true &&
      w.card.summary.capabilities?.approvals === true &&
      questionForm(r) === null &&
      r.options.length > 0;
    return ok && actions !== undefined ? actions : null;
  };
  const settle = (requestId: string) =>
    setPending((p) => {
      const next = new Set(p);
      next.delete(requestId);
      return next;
    });
  const answer = (actions: SessionActions, request: OpenRequest, optionId: string) => {
    const id = request.request_id;
    setPending((p) => new Set(p).add(id));
    actions.answer(id, optionId, null).then(
      (outcome) => {
        settle(id);
        if (!outcome.ok) setErrors((e) => ({ ...e, [id]: outcome.text }));
      },
      (e: unknown) => {
        settle(id);
        setErrors((all) => ({ ...all, [id]: e instanceof Error ? e.message : 'Request failed' }));
      },
    );
  };
  const anyAnswerable = waiting.some((w) => answerableOf(w) !== null);
  const keys = useRef<(e: KeyboardEvent) => void>(() => {});
  keys.current = (e) => {
    // Help, the palette and the drawer are modal: the rail keys do nothing behind them.
    if (document.querySelector('[role="dialog"][aria-modal="true"]') !== null) return;
    if (e.target instanceof Element && e.target.closest('[role="dialog"]') !== null) return;
    if (!anyAnswerable || shouldIgnore(e) || e.shiftKey) return;
    if (e.key === '[' || e.key === ']') {
      e.preventDefault();
      const step = e.key === ']' ? 1 : -1;
      setPicked(Math.min(Math.max(selected + step, 0), waiting.length - 1));
      return;
    }
    const item = waiting[selected];
    if (item === undefined) return;
    const actions = answerableOf(item);
    const options = item.request.options;
    if (actions === null || pending.has(item.request.request_id)) return;
    const index =
      e.key === 'a'
        ? 0
        : e.key === 'd'
          ? options.length >= 2
            ? options.length - 1
            : -1
          : /^[1-9]$/.test(e.key)
            ? Number(e.key) - 1
            : -1;
    const option = options[index];
    if (option === undefined) return;
    e.preventDefault();
    answer(actions, item.request, option.option_id);
  };
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => keys.current(e);
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, []);
  if (variant === 'strip' && waiting.length === 0) return null;
  return (
    <section className="fd-rail" data-variant={variant} aria-label="Needs you rail">
      <div className="fd-rail-top">
        <h2 className="fd-rail-head">
          <span className="fd-rail-title">NEEDS YOU</span>{' '}
          <span className="fd-rail-count">{waiting.length}</span>
          {anyAnswerable ? <span className="fd-rail-keys">[ ] · a/d</span> : null}
        </h2>
        <NotifyToggle on={notify} onChange={setNotify} />
      </div>
      {waiting.length === 0 ? (
        <div className="fd-allclear">
          <span className="fd-allclear-ring">
            <Icon of={Check} />
          </span>
          <span className="fd-allclear-t1">ALL CLEAR</span>
          <span className="fd-allclear-t2">Nothing waits for you.</span>
        </div>
      ) : (
        <div className="fd-rail-list">
          {waiting.map((w, index) => {
            const r = w.request;
            const actions = answerableOf(w);
            const busy = pending.has(r.request_id);
            const error = errors[r.request_id];
            return (
              <article
                key={`${w.key.station_id}/${w.key.session_id}/${r.request_id}`}
                className="fd-q"
                data-request={r.request_id}
                data-selected={anyAnswerable && index === selected ? 'true' : undefined}
              >
                <div className="fd-q-row">
                  <a
                    className="fd-q-session"
                    href={formatHash({
                      name: 'session',
                      station: w.key.station_id,
                      session: w.key.session_id,
                      tab: 'chat',
                    })}
                  >
                    {w.title}
                  </a>
                  <span
                    className="fd-badge fd-caps"
                    data-tone={r.request_kind === 'question' ? 'info' : 'act'}
                  >
                    {r.request_kind === 'question' ? 'Question' : 'Permission'}
                  </span>
                  <span className="fd-badge">{stationName(w.key.station_id, stations)}</span>
                  <span className="fd-q-age">{relativeAge(r.requested_at, now)}</span>
                </div>
                <p className="fd-q-title">{r.title}</p>
                {r.details === '' ? null : <pre className="fd-code fd-q-cmd">{r.details}</pre>}
                {actions !== null ? (
                  <div className="fd-q-answer">
                    {r.options.map((o, i) => (
                      <button
                        key={o.option_id}
                        type="button"
                        className={i === 0 ? 'fd-primary' : undefined}
                        aria-label={`${o.label}: ${r.title} in ${w.title}`}
                        disabled={busy}
                        onClick={() => answer(actions, r, o.option_id)}
                      >
                        {o.label}
                        {hint(i, r.options.length) === null ? null : (
                          <span className="fd-kbd">{hint(i, r.options.length)}</span>
                        )}
                      </button>
                    ))}
                  </div>
                ) : (
                  <button
                    type="button"
                    className="fd-sm"
                    aria-label={`Answer ${r.title} in ${w.title}`}
                    onClick={() => onOpen(w.key)}
                  >
                    Open session <Icon of={ChevronRight} size={14} />
                  </button>
                )}
                {error === undefined ? null : (
                  <p className="fd-q-error" role="alert">
                    {error}
                  </p>
                )}
              </article>
            );
          })}
        </div>
      )}
    </section>
  );
}

/** The key hint of option `i` of `n`: first `a`, last `d` (with two or more), the others their number. */
function hint(i: number, n: number): string | null {
  if (i === 0) return 'a';
  if (i === n - 1) return 'd';
  return i < 9 ? String(i + 1) : null;
}

/** One notification for each request that appears while notifications are on (DL-305). */
function useNotifications(
  sessions: readonly DeckSession[],
  on: boolean,
  onOpen: (key: SessionKey) => void,
) {
  const seen = useRef<Set<string> | null>(null);
  useEffect(() => {
    const now = waitingRequests(sessions);
    // The first render and the turn-on only record what is open already.
    if (on && seen.current) {
      for (const w of freshRequests(seen.current, now)) show(w, onOpen);
    }
    seen.current = on ? new Set(now.map((w) => w.id)) : null;
  }, [sessions, on, onOpen]);
}

function NotifyToggle(props: { on: boolean; onChange: (on: boolean) => void }) {
  const [state, setState] = useState(permission);
  if (state === 'unsupported' || state === 'denied') {
    const text =
      state === 'unsupported'
        ? 'This browser cannot show notifications'
        : "Notifications are blocked in this browser's settings";
    return (
      <button type="button" className="fd-iconbtn" disabled title={text} aria-label={text}>
        <Icon of={BellOff} />
      </button>
    );
  }
  const turn = async (on: boolean) => {
    let now: Permission = state;
    if (on && now !== 'granted') {
      now = await Notification.requestPermission();
      setState(now);
    }
    const enabled = on && now === 'granted';
    storeNotify(enabled);
    props.onChange(enabled);
  };
  return (
    <button
      type="button"
      className="fd-iconbtn"
      aria-pressed={props.on}
      aria-label="Notifications"
      title={props.on ? 'Notifications are on' : 'Notifications are off'}
      onClick={() => void turn(!props.on)}
    >
      <Icon of={props.on ? Bell : BellOff} />
    </button>
  );
}
