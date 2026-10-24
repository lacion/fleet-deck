// R11 (DL-166): who is on the board and what the deck says about the people of a session.
// One context carries it to the layout, the session view, the header and the pages, so that no
// prop crosses the session route.
import { createContext, useContext } from 'react';
import type { ChatMessage } from '../gen';
import type { Member } from './member';
import type { PeopleCalls, SessionRef, ShareCalls } from './people';

export type PeopleEnv = {
  /** The actor of this login: `owner`, `member:<name>` or `token:<name>`. */
  readonly self: string;
  /** Set for a member (a guest of the owner's deck): the board hides what a member never sees. */
  readonly member: Member | null;
  /** The share, space and invite calls; set for the owner's board login only. */
  readonly shares: ShareCalls | undefined;
  /** The chat and mob calls; absent without a deck (tests). */
  readonly calls: PeopleCalls | undefined;
  /** Session keys (`sessionKey`) that have a live share: the owner's People tab is always on. */
  readonly shared: ReadonlySet<string>;
  /** Session keys that a live share with `mob` steering covers (the owner's strip). */
  readonly mob: ReadonlySet<string>;
  readonly presence: Readonly<Record<string, readonly string[]>>;
  readonly chat: Readonly<Record<string, readonly ChatMessage[]>>;
  readonly driver: Readonly<Record<string, string | null>>;
  readonly driveRequests: Readonly<Record<string, readonly string[]>>;
  /** The presence heartbeat of one open session. */
  readonly heartbeat?: ((key: SessionRef) => void) | undefined;
  /** "Leave": ends the member's login. */
  readonly leave?: (() => void) | undefined;
};

/** An owner board without people: the default of the context (the tests without a deck). */
export const ownerEnv: PeopleEnv = {
  self: 'owner',
  member: null,
  shares: undefined,
  calls: undefined,
  shared: new Set(),
  mob: new Set(),
  presence: {},
  chat: {},
  driver: {},
  driveRequests: {},
};

const Context = createContext<PeopleEnv>(ownerEnv);

export const PeopleProvider = Context.Provider;

export const usePeople = (): PeopleEnv => useContext(Context);

/** The marker in `shared` and `mob` for a share of the whole board. */
export const ALL_SESSIONS = '*';

/** True when a live share of the owner covers the session. */
export const isShared = (env: PeopleEnv, id: string): boolean =>
  env.shared.has(id) || env.shared.has(ALL_SESSIONS);

/** True when a live share of the owner with `mob` steering covers the session. */
export const isMob = (env: PeopleEnv, id: string): boolean =>
  env.mob.has(id) || env.mob.has(ALL_SESSIONS);

/** True when the owner's login may make shares: the deck gives the share calls to it only. */
export const canShare = (env: PeopleEnv): boolean =>
  env.member === null && env.shares !== undefined;
