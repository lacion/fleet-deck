// DL-172: the settings calls of the board. The shapes are the deck's
// (`crates/fleetdeck-deck/src/http/{settings_routes,secrets_routes,auth_routes}.rs`).
import type { RestartAnswer, SettingsChange, SettingsSaved, SettingsView } from '../gen';
import type { DeckIo } from './http';

/** The deck's own settings, or the settings of one station (by id). */
export type Target = { readonly kind: 'deck' } | { readonly kind: 'station'; readonly id: string };

export const DECK: Target = { kind: 'deck' };

/** The elevation is dropped this long before the deck would refuse it (clock skew, a slow save). */
const ELEVATION_SKEW_MS = 5_000;

export type Credentials = { readonly password: string } | { readonly code: string };

/** What the deck gives for a secret: the key, the value, and the file that they belong to. */
export type SecretInput = {
  readonly key: string;
  readonly value: string;
  readonly revision: string;
  readonly elevation: string;
};

export type SettingsActions = {
  view(target: Target): Promise<SettingsView>;
  save(target: Target, change: SettingsChange): Promise<SettingsSaved>;
  /** The secret relay: the value crosses the deck once and is not kept anywhere. */
  setSecret(station: string, input: SecretInput): Promise<SettingsSaved>;
  /** Asks the deck for an elevation and keeps it in memory until it expires. */
  elevate(credentials: Credentials): Promise<void>;
  /** The elevation that is still valid, or `null`. */
  elevation(): string | null;
  forgetElevation(): void;
  restart(target: Target, stopDirectSessions: boolean, elevation: string): Promise<RestartAnswer>;
};

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

function shaped<T>(v: unknown, fields: readonly string[], what: string): T {
  if (!record(v) || fields.some((f) => !(f in v))) {
    throw new Error(`The deck sent an invalid ${what}.`);
  }
  return v as T;
}

const seg = encodeURIComponent;

/** `/api/settings/deck` or `/api/stations/<id>/settings`. */
export const settingsPath = (target: Target): string =>
  target.kind === 'deck' ? '/api/settings/deck' : `/api/stations/${seg(target.id)}/settings`;

/** The restart target of the deck API: `deck` or a station id. */
export const restartTarget = (target: Target): string =>
  target.kind === 'deck' ? 'deck' : target.id;

/** The settings calls over a transport. The elevation lives in this closure and nowhere else. */
export function settingsActions(io: DeckIo, now: () => number = Date.now): SettingsActions {
  let held: { token: string; expiresAt: number } | null = null;
  return {
    view: async (target) =>
      shaped<SettingsView>(
        await io.get(settingsPath(target)),
        ['values', 'fields', 'revision', 'overrides'],
        'settings view',
      ),
    save: async (target, change) =>
      shaped<SettingsSaved>(
        await io.post(settingsPath(target), change),
        ['values', 'restart_needed', 'revision', 'durability'],
        'save answer',
      ),
    setSecret: async (station, input) =>
      shaped<SettingsSaved>(
        await io.post(`/api/secrets/stations/${seg(station)}/settings`, {
          key: input.key,
          value: input.value,
          revision: input.revision,
          elevation: input.elevation,
        }),
        ['values', 'restart_needed', 'revision', 'durability'],
        'save answer',
      ),
    elevate: async (credentials) => {
      const v = shaped<{ elevation: unknown; expires_at: unknown }>(
        await io.post('/api/auth/elevate', credentials),
        ['elevation', 'expires_at'],
        'elevation',
      );
      if (typeof v.elevation !== 'string' || typeof v.expires_at !== 'number') {
        throw new Error('The deck sent an invalid elevation.');
      }
      held = { token: v.elevation, expiresAt: v.expires_at };
    },
    elevation: () =>
      held !== null && held.expiresAt - ELEVATION_SKEW_MS > now() ? held.token : null,
    forgetElevation: () => {
      held = null;
    },
    restart: async (target, stopDirectSessions, elevation) =>
      shaped<RestartAnswer>(
        await io.post('/api/settings/restart', {
          target: restartTarget(target),
          stop_direct_sessions: stopDirectSessions,
          elevation,
        }),
        ['restarting'],
        'restart answer',
      ),
  };
}
