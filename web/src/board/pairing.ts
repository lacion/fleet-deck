import type { AddStationRequest, PairingInvitation, StationView } from '../gen';

/** The pairing calls of the stations screen (admin scope). */
export type StationActions = {
  readonly canAdmin: boolean;
  invite(): Promise<PairingInvitation>;
  add(request: AddStationRequest): Promise<StationView>;
  remove(name: string): Promise<StationView>;
};

type Post = (path: string, body: unknown) => Promise<unknown>;

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

export function invitationView(v: unknown): PairingInvitation {
  if (
    !record(v) ||
    typeof v['invitation'] !== 'string' ||
    v['invitation'].length === 0 ||
    typeof v['join_command'] !== 'string' ||
    typeof v['expires_at'] !== 'number' ||
    !Number.isSafeInteger(v['expires_at'])
  ) {
    throw new Error('Invalid invitation response');
  }
  return v as PairingInvitation;
}

export function stationResult(v: unknown): StationView {
  if (!record(v) || typeof v['station_id'] !== 'string' || typeof v['name'] !== 'string') {
    throw new Error('Invalid station response');
  }
  return v as StationView;
}

/** The board-API pairing calls over one authenticated connection. */
export function stationActions(post: Post, canAdmin: boolean): StationActions {
  return {
    canAdmin,
    invite: async () => invitationView(await post('/api/stations/invite', {})),
    add: async (request) => stationResult(await post('/api/stations', request)),
    remove: async (name) =>
      stationResult(await post(`/api/stations/${encodeURIComponent(name)}/remove`, {})),
  };
}

/** A name for the deck to use: trimmed, or `null` for the station's own name. */
export function requestedName(value: string): string | null {
  const name = value.trim();
  return name === '' ? null : name;
}

export function authLabel(auth: string): string {
  switch (auth) {
    case 'signed_in':
      return 'signed in';
    case 'signed_out':
      return 'signed out';
    default:
      return 'sign-in unknown';
  }
}

export function expiresText(at: number): string {
  return `${new Date(at).toISOString().slice(0, 16).replace('T', ' ')} UTC`;
}
