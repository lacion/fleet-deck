import { useEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { shouldIgnore } from '../app/keys';
import { formatHash } from '../app/routes';
import { basename, relativeAge } from '../app/sessions';
import type { DeckSession, OpenRequest, SessionKey, StationView } from '../gen';
import type { Freshness } from '../runtime/state';
import { optionClass as approvalClass } from '../session/ApprovalDock';
import type { SessionActions } from '../session/commands';
import { sessionFlags } from '../session/controller';
import { questionForm } from '../session/question';
import { shortModel } from '../ui/format';
import {
  Bell,
  BellOff,
  ChevronRight,
  Folder,
  GitBranch,
  Icon,
  MessageSquare,
  Shield,
} from '../ui/icons';
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
  readonly freshness: Freshness;
  readonly now?: number | undefined;
  /**
   * The header slot of the fleet home: with no request, the rail puts its notification switch
   * there (the last item of the page header, on the desktop and the phone). `null`: the slot is
   * not mounted yet. Absent: the side variant renders the switch in place, the strip variant
   * renders nothing.
   */
  readonly bellSlot?: HTMLElement | null | undefined;
  /** The slot is a row of a menu (the phone header): the switch is a labelled switch row. */
  readonly bellRow?: boolean | undefined;
};

/** Where a request's session works: its branch, else its folder. */
function placeOf(card: DeckSession): { readonly text: string; readonly branch: boolean } | null {
  const s = card.summary;
  if (s.branch !== null && s.branch !== '') return { text: s.branch, branch: true };
  if (s.cwd !== null && s.cwd !== '') return { text: basename(s.cwd), branch: false };
  return null;
}

/** Open approvals of every session, the oldest first (PLAN §12 needs-you rail). */
export function NeedsYouRail({
  sessions,
  stations,
  onOpen,
  sessionActions,
  variant,
  freshness,
  now: given,
  bellSlot,
  bellRow = false,
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
  const answerableOf = (w: (typeof waiting)[number]) => {
    const actions = sessionActions?.(w.key);
    const r = w.request;
    const ok =
      sessionFlags(w.card, freshness, actions ?? null).approve &&
      w.card.summary.capabilities?.approvals === true &&
      questionForm(r) === null &&
      r.options.length > 0;
    return ok && actions !== undefined ? actions : null;
  };
  const answerable = waiting.map((w) => answerableOf(w) !== null);
  // The keyboard cursor is always on a request that the keys can answer: it starts on the first
  // one, stays inside the list when the list shrinks, and leaves a request that became
  // unanswerable.
  const selected = railCursor(answerable, picked);
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
  const anyAnswerable = selected >= 0;
  // The number keys of the middle options (`hint`): the legend names them while an answerable
  // request has more than two options.
  const numberTop = Math.min(
    9,
    Math.max(
      0,
      ...waiting.filter((_, i) => answerable[i]).map((w) => w.request.options.length - 1),
    ),
  );
  const keys = useRef<(e: KeyboardEvent) => void>(() => {});
  keys.current = (e) => {
    // Help, the palette and the drawer are modal: the rail keys do nothing behind them.
    if (document.querySelector('[role="dialog"][aria-modal="true"]') !== null) return;
    if (e.target instanceof Element && e.target.closest('[role="dialog"]') !== null) return;
    if (!anyAnswerable || shouldIgnore(e) || e.shiftKey) return;
    if (e.key === '[' || e.key === ']') {
      e.preventDefault();
      setPicked(railStep(answerable, selected, e.key === ']' ? 1 : -1));
      return;
    }
    const item = waiting[selected];
    if (item === undefined) return;
    const actions = answerableOf(item);
    const options = item.request.options;
    if (actions === null || pending.has(item.request.request_id)) return;
    const option = options[railKeyOption(e.key, options.length)];
    if (option === undefined) return;
    e.preventDefault();
    answer(actions, item.request, option.option_id);
  };
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => keys.current(e);
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, []);
  // No request, no rail: the fleet header says "All clear". The hooks above stay mounted, and
  // the rail keeps only the notification switch, as the last item of the page header.
  if (waiting.length === 0) {
    if (variant !== 'side' && bellSlot === undefined) return null;
    const mini = (
      <div className="fd-rail-mini">
        <NotifyToggle on={notify} onChange={setNotify} row={bellRow} />
      </div>
    );
    if (bellSlot === undefined) return mini;
    return bellSlot === null ? null : createPortal(mini, bellSlot);
  }
  return (
    <section className="fd-rail" data-variant={variant} aria-label="Needs you rail">
      <div className="fd-rail-top">
        <h2 className="fd-rail-head">
          <span className="fd-rail-title">Needs you</span>{' '}
          <span className="fd-count fd-rail-count" data-tone="act">
            {waiting.length}
          </span>
        </h2>
        {anyAnswerable ? (
          <span className="fd-rail-keys">
            <span className="fd-kbd">[</span>
            <span className="fd-kbd">]</span> Move <span className="fd-kbd">a</span> First{' '}
            <span className="fd-kbd">d</span> Last
          </span>
        ) : null}
        {anyAnswerable && numberTop >= 2 ? (
          <span className="fd-rail-keys" data-keys="number">
            <span className="fd-kbd">2</span>
            {numberTop > 2 ? (
              <>
                –<span className="fd-kbd">{numberTop}</span>
              </>
            ) : null}{' '}
            Option
          </span>
        ) : null}
        <NotifyToggle on={notify} onChange={setNotify} />
      </div>
      <div className="fd-rail-list">
        {waiting.map((w, index) => {
          const r = w.request;
          const actions = answerableOf(w);
          const busy = pending.has(r.request_id);
          const error = errors[r.request_id];
          const question = r.request_kind === 'question';
          const place = placeOf(w.card);
          return (
            <article
              key={`${w.key.station_id}/${w.key.session_id}/${r.request_id}`}
              className="fd-q fd-glow"
              data-request={r.request_id}
              data-selected={anyAnswerable && index === selected ? 'true' : undefined}
              data-kind={question ? 'question' : 'permission'}
            >
              <span className="fd-holder" data-status="needs_you" aria-hidden="true" />
              <div className="fd-q-row">
                <span className="fd-q-id">
                  <span className="fd-q-call fd-callsign">
                    {w.card.summary.callsign ?? w.key.session_id.slice(0, 8)}
                  </span>
                  <span className="fd-q-model">{shortModel(w.card.summary.model)}</span>
                </span>
                <span className="fd-q-what">
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
                  <span className="fd-q-kind" title={r.title}>
                    <Icon of={question ? MessageSquare : Shield} size={12} />
                    {question ? 'Question' : `Permission · ${r.title}`}
                  </span>
                </span>
                <span className="fd-q-where">
                  {place === null ? null : (
                    <span
                      className="fd-q-place"
                      title={`${place.branch ? 'Branch' : 'Folder'}: ${place.text}`}
                    >
                      <Icon of={place.branch ? GitBranch : Folder} size={12} />
                      <span className="fd-q-path">{place.text}</span>
                    </span>
                  )}
                  <span className="fd-q-station">{stationName(w.key.station_id, stations)}</span>
                </span>
                <span className="fd-q-age">{relativeAge(r.requested_at, now)}</span>
              </div>
              <div className="fd-q-body">
                {question ? <p className="fd-q-title">{r.title}</p> : null}
                {r.details === '' ? null : <pre className="fd-code fd-q-cmd">{r.details}</pre>}
                {actions !== null ? (
                  <div className="fd-q-answer">
                    {r.options.map((o, i) => (
                      <button
                        key={o.option_id}
                        type="button"
                        className={question ? undefined : approvalClass(o, i)}
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
                    className="fd-sm fd-q-open"
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
              </div>
            </article>
          );
        })}
      </div>
    </section>
  );
}

/**
 * The keyboard cursor: the request at `picked` when the keys can answer it, else the next
 * answerable one after it, else the last answerable one before it. `-1`: none is answerable.
 */
export function railCursor(answerable: readonly boolean[], picked: number): number {
  const from = Math.min(Math.max(picked, 0), answerable.length - 1);
  for (let i = from; i < answerable.length; i += 1) if (answerable[i]) return i;
  for (let i = from - 1; i >= 0; i -= 1) if (answerable[i]) return i;
  return -1;
}

/** `[` and `]`: the next answerable request in the direction `step`; at the end the cursor stays. */
export function railStep(answerable: readonly boolean[], selected: number, step: 1 | -1): number {
  for (let i = selected + step; i >= 0 && i < answerable.length; i += step)
    if (answerable[i]) return i;
  return selected;
}

/** The option index that a key answers among `n` options: `a` the first, `d` the last (with two
 * or more), `1`–`9` by number. `-1`: the key answers nothing. */
export function railKeyOption(key: string, n: number): number {
  if (key === 'a') return n >= 1 ? 0 : -1;
  if (key === 'd') return n >= 2 ? n - 1 : -1;
  if (/^[1-9]$/.test(key)) return Number(key) <= n ? Number(key) - 1 : -1;
  return -1;
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

/** The name of the switch: what it turns on (its state is `aria-pressed`, or the switch's own). */
const NOTIFY_NAME = 'Needs-you notifications';

/**
 * The needs-you notification switch: an icon toggle whose tooltip says its state, or (`row`, a row
 * of the phone header menu) a labelled `.fd-switch` row.
 */
function NotifyToggle(props: { on: boolean; onChange: (on: boolean) => void; row?: boolean }) {
  const [state, setState] = useState(permission);
  if (state === 'unsupported' || state === 'denied') {
    const text =
      state === 'unsupported'
        ? 'This browser cannot show notifications'
        : "Notifications are blocked in this browser's settings";
    if (props.row === true) {
      return (
        <label className="fd-menu-switch" data-disabled="true">
          <Icon of={BellOff} />
          <span className="fd-menu-switch-text">{text}</span>
          <input
            type="checkbox"
            role="switch"
            className="fd-switch"
            aria-checked={false}
            disabled
            checked={false}
          />
        </label>
      );
    }
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
  if (props.row === true) {
    return (
      <label className="fd-menu-switch">
        <Icon of={props.on ? Bell : BellOff} />
        <span className="fd-menu-switch-text">{NOTIFY_NAME}</span>
        <input
          type="checkbox"
          role="switch"
          className="fd-switch"
          aria-checked={props.on}
          checked={props.on}
          onChange={(event) => void turn(event.target.checked)}
        />
      </label>
    );
  }
  return (
    <button
      type="button"
      className="fd-iconbtn"
      aria-pressed={props.on}
      aria-label={NOTIFY_NAME}
      title={props.on ? `${NOTIFY_NAME}: on` : `${NOTIFY_NAME}: off`}
      onClick={() => void turn(!props.on)}
    >
      <Icon of={props.on ? Bell : BellOff} />
    </button>
  );
}
