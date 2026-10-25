// DL-172: what happens after a save, a secret or a restart: the deck's answer becomes one outcome
// that the page shows. No function here keeps a secret: a value is read, sent and dropped.
import type { RestartItem, SettingsSaved, SettingsView } from '../gen';
import type { Credentials, SettingsActions, Target } from './actions';
import { DeckError, describe } from './http';

export type SaveOutcome =
  | { readonly kind: 'saved'; readonly saved: SettingsSaved }
  | { readonly kind: 'elevate'; readonly keys: readonly string[] }
  | { readonly kind: 'changed' }
  | { readonly kind: 'invalid'; readonly keys: readonly string[]; readonly text: string }
  | { readonly kind: 'failed'; readonly text: string };

const strings = (v: unknown): string[] =>
  Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : [];

/**
 * The fields that an `invalid` answer names: the `keys` of the body, else the key that the text
 * starts with (`` `allowed_roots` has an invalid value ``), mapped to a key of the field table.
 */
export function keysOfError(e: DeckError, known: readonly string[]): string[] {
  const listed = strings(e.details['keys']);
  const quoted = /^`([^`]+)`/.exec(e.message)?.[1];
  const names = listed.length > 0 ? listed : quoted === undefined ? [] : [quoted];
  const found = names.flatMap((n) => known.filter((k) => n === k || n.startsWith(`${k}.`)));
  return [...new Set(found)];
}

/** The save codes that mean "the file is not the one that the board read". */
const CHANGED = new Set(['settings_changed', 'settings_changed_on_disk']);

function saveFailure(
  e: unknown,
  actions: SettingsActions,
  known: readonly string[],
  sentElevation: boolean,
): SaveOutcome {
  if (!(e instanceof DeckError)) return { kind: 'failed', text: describe(e) };
  if (e.code === 'elevation_required') {
    if (sentElevation) actions.forgetElevation();
    return { kind: 'elevate', keys: strings(e.details['keys']) };
  }
  if (e.code !== null && CHANGED.has(e.code)) return { kind: 'changed' };
  if (e.code === 'invalid' || e.code === 'cannot_apply' || e.code === 'secret_via_relay') {
    const keys = keysOfError(e, known);
    if (keys.length > 0) return { kind: 'invalid', keys, text: e.message };
  }
  return { kind: 'failed', text: e.message };
}

/** Saves the changes with the elevation that is still valid, if there is one. */
export async function trySave(
  actions: SettingsActions,
  target: Target,
  changes: Record<string, unknown>,
  revision: string,
  known: readonly string[],
): Promise<SaveOutcome> {
  const elevation = actions.elevation();
  try {
    const saved = await actions.save(target, { changes, revision, elevation });
    return { kind: 'saved', saved };
  } catch (e) {
    return saveFailure(e, actions, known, elevation !== null);
  }
}

export type SecretOutcome =
  | { readonly kind: 'saved'; readonly saved: SettingsSaved }
  | { readonly kind: 'elevate' }
  | { readonly kind: 'changed' }
  | { readonly kind: 'empty' }
  | { readonly kind: 'failed'; readonly text: string };

/**
 * The secret relay. `read` gives the typed value and `clear` empties the input: the input is
 * cleared before the call starts, so the value lives only in this request.
 */
export async function relaySecret(
  actions: SettingsActions,
  p: {
    readonly station: string;
    readonly key: string;
    readonly revision: string;
    readonly read: () => string;
    readonly clear: () => void;
  },
): Promise<SecretOutcome> {
  const elevation = actions.elevation();
  if (elevation === null) return { kind: 'elevate' };
  const value = p.read();
  p.clear();
  if (value === '') return { kind: 'empty' };
  try {
    const saved = await actions.setSecret(p.station, {
      key: p.key,
      value,
      revision: p.revision,
      elevation,
    });
    return { kind: 'saved', saved };
  } catch (e) {
    if (!(e instanceof DeckError)) return { kind: 'failed', text: describe(e) };
    if (e.code === 'elevation_required') {
      actions.forgetElevation();
      return { kind: 'elevate' };
    }
    if (e.code !== null && CHANGED.has(e.code)) return { kind: 'changed' };
    return { kind: 'failed', text: e.message };
  }
}

export type ElevateOutcome =
  | { readonly kind: 'ok' }
  | { readonly kind: 'failed'; readonly text: string };

/** Asks for an elevation. The password and the code are sent and dropped. */
export async function elevate(
  actions: SettingsActions,
  credentials: Credentials,
): Promise<ElevateOutcome> {
  try {
    await actions.elevate(credentials);
    return { kind: 'ok' };
  } catch (e) {
    if (e instanceof DeckError && e.status === 401) {
      return { kind: 'failed', text: 'Wrong password or code.' };
    }
    if (e instanceof DeckError && e.status === 429) {
      return { kind: 'failed', text: 'Too many tries. Wait a minute.' };
    }
    return { kind: 'failed', text: describe(e) };
  }
}

export type RestartOutcome =
  | { readonly kind: 'restarting' }
  | { readonly kind: 'elevate' }
  | { readonly kind: 'would_stop'; readonly items: readonly RestartItem[] }
  | { readonly kind: 'unsupported' }
  | { readonly kind: 'failed'; readonly text: string };

const isItem = (v: unknown): v is RestartItem =>
  typeof v === 'object' && v !== null && 'kind' in v && 'title' in v && typeof v.title === 'string';

/** Asks the deck to restart a process. It needs an elevation; the refusal lists what would stop. */
export async function tryRestart(
  actions: SettingsActions,
  target: Target,
  stopDirectSessions: boolean,
): Promise<RestartOutcome> {
  const elevation = actions.elevation();
  if (elevation === null) return { kind: 'elevate' };
  try {
    const answer = await actions.restart(target, stopDirectSessions, elevation);
    if (answer.restarting) return { kind: 'restarting' };
    return { kind: 'would_stop', items: answer.would_stop };
  } catch (e) {
    if (!(e instanceof DeckError)) return { kind: 'failed', text: describe(e) };
    if (e.code === 'elevation_required') {
      actions.forgetElevation();
      return { kind: 'elevate' };
    }
    if (e.code === 'restart_would_stop_sessions') {
      const list = e.details['would_stop'];
      return { kind: 'would_stop', items: Array.isArray(list) ? list.filter(isItem) : [] };
    }
    if (e.status === 501 || e.code === 'restart_unsupported') return { kind: 'unsupported' };
    return { kind: 'failed', text: e.message };
  }
}

/** What the page says when the restart cannot run. */
export const UNSUPPORTED_TEXT = 'This station cannot restart itself. Restart it from its machine.';

/** The posture part of a view (B27 §3.6): `relaxed` is absent for an older station. */
export type Posture = Pick<SettingsView, 'relaxed'>;

/**
 * B27 §3.6: a save answer has no posture, so the tab reads the view again after a save. `null`:
 * the read failed (the tab keeps the posture that it shows).
 */
export async function readPosture(
  actions: SettingsActions,
  target: Target,
): Promise<Posture | null> {
  try {
    const fresh = await actions.view(target);
    return fresh.relaxed === undefined ? {} : { relaxed: fresh.relaxed };
  } catch {
    return null;
  }
}

/** `view` with the posture of `posture`; `null` keeps the posture of `view`. */
export function withPosture(view: SettingsView, posture: Posture | null): SettingsView {
  if (posture === null) return view;
  const { relaxed: _old, ...rest } = view;
  return { ...rest, ...posture };
}
