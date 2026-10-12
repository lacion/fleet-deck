import { useEffect, useRef, useState } from 'react';
import type { DeckSession, SessionKey, StationView } from '../gen';
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
};

/** Open approvals of every session, the oldest first (PLAN §12 needs-you rail). */
export function NeedsYouRail({ sessions, stations, onOpen }: Props) {
  const waiting = sessions
    .flatMap((s) =>
      s.summary.open_requests.map((r) => ({
        key: { station_id: s.station_id, session_id: s.summary.session_id },
        title: s.summary.title ?? s.summary.session_id,
        request: r,
      })),
    )
    .sort((a, b) => a.request.requested_at - b.request.requested_at);
  const [notify, setNotify] = useState(notifyEnabled);
  useNotifications(sessions, notify, onOpen);
  return (
    <section className="fd-rail" aria-label="Needs you rail">
      <h2>
        Needs you <span className="fd-count">{waiting.length}</span>
      </h2>
      <NotifyToggle on={notify} onChange={setNotify} />
      {waiting.length === 0 ? (
        <p className="fd-empty">Nothing waits for you.</p>
      ) : (
        <ul>
          {waiting.map((w) => (
            <li key={`${w.key.station_id}/${w.key.session_id}/${w.request.request_id}`}>
              <span>
                {w.title} · {stationName(w.key.station_id, stations)}: {w.request.title}
              </span>
              <button
                type="button"
                aria-label={`Answer ${w.request.title} in ${w.title}`}
                onClick={() => onOpen(w.key)}
              >
                Answer
              </button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
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
  if (state === 'unsupported') {
    return <p className="fd-note">This browser cannot show notifications.</p>;
  }
  if (state === 'denied') {
    return <p className="fd-note">Notifications are blocked in this browser's settings.</p>;
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
    <button type="button" aria-pressed={props.on} onClick={() => void turn(!props.on)}>
      {props.on ? 'Notifications on' : 'Notify me'}
    </button>
  );
}
