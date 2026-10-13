// R11 (DL-166): the calls of the people features and what the board shows about them.
import type {
  ChatMessage,
  DriverState,
  NewShare,
  ShareCreated,
  ShareInfo,
  SpaceView,
} from '../gen';
import type { Outcome } from '../session/commands';
import { memberName } from './member';

type Get = (path: string) => Promise<unknown>;
type Post = (path: string, body?: unknown) => Promise<unknown>;

const message = (e: unknown) => (e instanceof Error ? e.message : 'The request failed.');
const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

const path = (station: string, session: string, tail: string) =>
  `/api/sessions/${encodeURIComponent(station)}/${encodeURIComponent(session)}/${tail}`;

function list<T>(v: unknown, what: string): T[] {
  if (!Array.isArray(v)) throw new Error(`The deck sent an invalid ${what}.`);
  return v as T[];
}

function one<T>(v: unknown, field: string, what: string): T {
  if (!record(v) || !(field in v)) throw new Error(`The deck sent an invalid ${what}.`);
  return v as T;
}

export type SessionRef = { readonly station_id: string; readonly session_id: string };

/** The chat and mob calls: the owner and the members use them. */
export type PeopleCalls = {
  chat(key: SessionRef, after: number): Promise<ChatMessage[]>;
  say(key: SessionRef, text: string): Promise<ChatMessage>;
  requestDrive(key: SessionRef): Promise<Outcome>;
  handOver(key: SessionRef, to: string): Promise<Outcome>;
  releaseDrive(key: SessionRef): Promise<Outcome>;
};

/** The owner's calls: shares and spaces. */
export type ShareCalls = {
  shares(): Promise<ShareInfo[]>;
  create(body: NewShare): Promise<ShareCreated>;
  revoke(shareId: string): Promise<ShareInfo>;
  spaces(): Promise<SpaceView[]>;
  createSpace(name: string): Promise<SpaceView>;
  addToSpace(spaceId: string, station: string, sessionId: string): Promise<SpaceView>;
};

export function peopleCalls(get: Get, post: Post): PeopleCalls {
  const drive = async (call: () => Promise<unknown>, done: string): Promise<Outcome> => {
    try {
      one<DriverState>(await call(), 'requests', 'driver state');
      return { ok: true, text: done };
    } catch (e) {
      return { ok: false, text: message(e) };
    }
  };
  return {
    chat: async (key, after) =>
      list<ChatMessage>(
        await get(`${path(key.station_id, key.session_id, 'chat')}?after=${after}`),
        'chat',
      ),
    say: async (key, text) =>
      one<ChatMessage>(
        await post(path(key.station_id, key.session_id, 'chat'), { text }),
        'seq',
        'chat message',
      ),
    requestDrive: (key) =>
      drive(() => post(path(key.station_id, key.session_id, 'drive/request'), {}), 'Requested.'),
    handOver: (key, to) =>
      drive(
        () => post(path(key.station_id, key.session_id, 'drive/grant'), { to }),
        'Handed over.',
      ),
    releaseDrive: (key) =>
      drive(() => post(path(key.station_id, key.session_id, 'drive/release'), {}), 'Released.'),
  };
}

export function shareCalls(get: Get, post: Post): ShareCalls {
  return {
    shares: async () => list<ShareInfo>(await get('/api/shares'), 'share list'),
    create: async (body) => one<ShareCreated>(await post('/api/shares', body), 'url', 'share'),
    revoke: async (shareId) =>
      one<ShareInfo>(
        await post(`/api/shares/${encodeURIComponent(shareId)}/revoke`, {}),
        'share_id',
        'share',
      ),
    spaces: async () => list<SpaceView>(await get('/api/spaces'), 'space list'),
    createSpace: async (name) =>
      one<SpaceView>(await post('/api/spaces', { name }), 'space_id', 'space'),
    addToSpace: async (spaceId, station, sessionId) =>
      one<SpaceView>(
        await post(`/api/spaces/${encodeURIComponent(spaceId)}/sessions`, {
          station,
          session_id: sessionId,
        }),
        'space_id',
        'space',
      ),
  };
}

/** The chat of a session with new messages added: sorted by `seq`, one for each `seq`. */
export function mergeChat(
  held: readonly ChatMessage[],
  more: readonly ChatMessage[],
): readonly ChatMessage[] {
  const bySeq = new Map<number, ChatMessage>();
  for (const m of [...held, ...more]) bySeq.set(m.seq, m);
  return [...bySeq.values()].sort((a, b) => a.seq - b.seq);
}

/** The drive strip of a session in mob mode. */
export type DriveView = {
  /** The text of the strip: "Ana is driving", "You are driving" or "Nobody is driving". */
  readonly text: string;
  /** The viewer is the driver. */
  readonly driving: boolean;
  /** The composer is off for the viewer until it drives (a member who does not drive). */
  readonly locked: boolean;
  /** The viewer may ask to drive (a collaborator who does not drive and did not ask yet). */
  readonly canRequest: boolean;
  /** The viewer asked and waits. */
  readonly requested: boolean;
  /** The viewer sees the requests with "Hand over" (the driver and the owner). */
  readonly canHandOver: boolean;
  /** The viewer sees "Release" (the driver). */
  readonly canRelease: boolean;
  readonly requests: readonly string[];
};

/**
 * The state of the mob strip for `self` (the actor of this login). `null`: the session is not in
 * mob mode, as far as the board knows (the deck sends the driver once a session has one).
 */
export function driveView(input: {
  readonly self: string;
  readonly isMember: boolean;
  readonly collaborator: boolean;
  /** The share of a member steers `mob`. */
  readonly shareMob: boolean;
  /** The deck sent the driver or the requests of this session. */
  readonly known: boolean;
  readonly driver: string | null;
  readonly requests: readonly string[];
}): DriveView | null {
  if (!input.shareMob && !input.known) return null;
  const { self, driver } = input;
  const driving = driver !== null && driver === self;
  const name = driver === null ? null : driving ? 'You' : actorName(driver);
  const text =
    name === null ? 'Nobody is driving' : driving ? 'You are driving' : `${name} is driving`;
  const requested = input.requests.includes(self);
  return {
    text,
    driving,
    locked: input.isMember && !driving,
    canRequest: input.isMember && input.collaborator && !driving && !requested,
    requested: input.isMember && requested,
    canHandOver: driving || !input.isMember,
    canRelease: driving,
    requests: input.requests,
  };
}

/** The name of an actor in the strip: `owner` is "Owner", `member:Ana` is "Ana". */
export function actorName(actor: string): string {
  if (actor === 'owner') return 'Owner';
  return memberName(actor) ?? actor;
}

/** The initial of a name for a round avatar. */
export function initial(name: string): string {
  return [...name.trim()][0]?.toUpperCase() ?? '?';
}

/** The most avatars the session header shows. */
export const MAX_AVATARS = 5;

/** `HH:MM` of a unix-ms time, in the local time zone. */
export function clock(ms: number): string {
  const d = new Date(ms);
  const two = (n: number) => String(n).padStart(2, '0');
  return `${two(d.getHours())}:${two(d.getMinutes())}`;
}

/** The longest side chat message, in characters. */
export const MAX_CHAT_CHARS = 2000;
