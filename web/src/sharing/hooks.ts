// R11.5 to R11.7 (DL-166): the hooks that the session view uses for the people of a session.
import { useCallback, useEffect, useMemo, useState } from 'react';
import type { ChatMessage } from '../gen';
import { sessionKey } from '../runtime/state';
import { isMob, isShared, usePeople } from './context';
import { PRESENCE_HEARTBEAT_S } from './member';
import { type DriveView, driveView, mergeChat, type SessionRef } from './people';

const message = (e: unknown) => (e instanceof Error ? e.message : 'The request failed.');

/** True while the page is visible (a hidden tab is not looking at the session). */
function useVisible(): boolean {
  const [visible, setVisible] = useState(
    () => typeof document === 'undefined' || document.visibilityState !== 'hidden',
  );
  useEffect(() => {
    const sync = () => setVisible(document.visibilityState !== 'hidden');
    document.addEventListener('visibilitychange', sync);
    return () => document.removeEventListener('visibilitychange', sync);
  }, []);
  return visible;
}

type Timers = {
  readonly set: (fn: () => void, ms: number) => unknown;
  readonly clear: (id: unknown) => void;
};

/** One heartbeat now and one every `seconds`; the result stops them. */
export function startHeartbeat(
  send: (key: SessionRef) => void,
  key: SessionRef,
  seconds: number = PRESENCE_HEARTBEAT_S,
  timers: Timers = { set: setInterval, clear: (id) => clearInterval(id as number) },
): () => void {
  send(key);
  const id = timers.set(() => send(key), seconds * 1000);
  return () => timers.clear(id);
}

/** While the session view is open and visible: one heartbeat now and one every 20 seconds. */
export function usePresenceHeartbeat(key: SessionRef): void {
  const { heartbeat } = usePeople();
  const visible = useVisible();
  const { station_id, session_id } = key;
  useEffect(() => {
    if (heartbeat === undefined || !visible) return undefined;
    return startHeartbeat(heartbeat, { station_id, session_id });
  }, [heartbeat, visible, station_id, session_id]);
}

export type SessionPeople = {
  readonly who: readonly string[];
  readonly messages: readonly ChatMessage[];
  readonly loading: boolean;
  readonly error: string | null;
  readonly drive: DriveView | null;
  /** The People tab is offered: 2 or more people, any chat message, or a shared session of the owner. */
  readonly offered: boolean;
  readonly say: (text: string) => Promise<string | null>;
  readonly requestDrive: () => Promise<{ ok: boolean; text: string }>;
  readonly handOver: (to: string) => Promise<{ ok: boolean; text: string }>;
  readonly release: () => Promise<{ ok: boolean; text: string }>;
};

/** The people of one session: presence, side chat, and the mob driver. */
export function useSessionPeople(key: SessionRef): SessionPeople {
  const env = usePeople();
  const id = sessionKey(key);
  const { station_id, session_id } = key;
  const calls = env.calls;
  const shared = isShared(env, id) || env.member !== null;
  const [loaded, setLoaded] = useState<readonly ChatMessage[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // The chat history loads once for a session that is shared (a member sees the history at once).
  useEffect(() => {
    if (calls === undefined || !shared) return undefined;
    let live = true;
    setLoading(true);
    calls.chat({ station_id, session_id }, 0).then(
      (list) => {
        if (!live) return;
        setLoaded(list);
        setLoading(false);
        setError(null);
      },
      (e: unknown) => {
        if (!live) return;
        setLoading(false);
        setError(message(e));
      },
    );
    return () => {
      live = false;
    };
  }, [calls, shared, station_id, session_id]);
  const live = env.chat[id];
  const messages = useMemo(() => mergeChat(loaded, live ?? []), [loaded, live]);
  const who = useMemo(() => [...new Set(env.presence[id] ?? [])], [env.presence, id]);

  const driver = env.driver[id] ?? null;
  const requests = env.driveRequests[id] ?? [];
  const member = env.member;
  const drive = driveView({
    self: env.self,
    isMember: member !== null,
    collaborator: member !== null && member.role !== 'viewer',
    shareMob: member?.scope?.steering === 'mob' || isMob(env, id),
    known: id in env.driver || id in env.driveRequests,
    driver,
    requests,
  });

  const say = useCallback(
    async (text: string): Promise<string | null> => {
      if (calls === undefined) return 'The board has no deck.';
      try {
        const sent = await calls.say({ station_id, session_id }, text);
        // The socket brings the message too; a message with the same `seq` is kept once.
        setLoaded((now) => mergeChat(now, [sent]));
        return null;
      } catch (e) {
        return message(e);
      }
    },
    [calls, station_id, session_id],
  );
  const none = { ok: false, text: 'The board has no deck.' };
  const requestDrive = useCallback(
    async () => (calls === undefined ? none : calls.requestDrive({ station_id, session_id })),
    [calls, station_id, session_id],
  );
  const handOver = useCallback(
    async (to: string) =>
      calls === undefined ? none : calls.handOver({ station_id, session_id }, to),
    [calls, station_id, session_id],
  );
  const release = useCallback(
    async () => (calls === undefined ? none : calls.releaseDrive({ station_id, session_id })),
    [calls, station_id, session_id],
  );
  const offered =
    (env.member === null && isShared(env, id)) || who.length >= 2 || messages.length > 0;
  return { who, messages, loading, error, drive, offered, say, requestDrive, handOver, release };
}
