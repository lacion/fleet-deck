// Test fixtures of the sharing code.
import type { ShareInfo, SpaceView } from '../gen';
import { ownerEnv, type PeopleEnv } from './context';
import type { Member } from './member';
import type { PeopleCalls, ShareCalls } from './people';

export const NOW = Date.parse('2026-10-13T12:00:00Z');
export const DAY = 86_400_000;

export const member = (
  role: Member['role'],
  name = 'Ana',
  steering: 'open' | 'mob' = 'open',
): Member => ({
  role,
  name,
  scope: { kind: 'session', target: 'station-a/s1', space_name: null, steering, sessions: [] },
});

/** A board of the owner (with the share calls) or of a member. */
export const env = (extra: Partial<PeopleEnv> = {}): PeopleEnv => ({ ...ownerEnv, ...extra });

export const memberEnv = (role: Member['role'], extra: Partial<PeopleEnv> = {}): PeopleEnv =>
  env({ self: 'member:Ana', member: member(role), leave: () => {}, ...extra });

const unused = async (): Promise<never> => {
  throw new Error('not used');
};

export const fakeShareCalls = (extra: Partial<ShareCalls> = {}): ShareCalls => ({
  shares: async () => [],
  create: unused,
  revoke: unused,
  spaces: async () => [],
  createSpace: unused,
  addToSpace: unused,
  ...extra,
});

export const fakePeopleCalls = (extra: Partial<PeopleCalls> = {}): PeopleCalls => ({
  chat: async () => [],
  say: unused,
  requestDrive: async () => ({ ok: true, text: 'Requested.' }),
  handOver: async () => ({ ok: true, text: 'Handed over.' }),
  releaseDrive: async () => ({ ok: true, text: 'Released.' }),
  ...extra,
});

export const share = (id: string, extra: Partial<ShareInfo> = {}): ShareInfo => ({
  share_id: id,
  kind: 'session',
  target: 'station-a/s1',
  role: 'viewer',
  steering: 'open',
  expires_at: NOW + 5 * DAY,
  revoked_at: null,
  created_at: NOW - DAY,
  uses_left: 1,
  members: [],
  ...extra,
});

export const space = (
  id: string,
  name: string,
  sessions: SpaceView['sessions'] = [],
): SpaceView => ({
  space_id: id,
  name,
  sessions,
});
