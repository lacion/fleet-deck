import type { FieldMeta, SettingsSaved } from '../gen';
import type { Values } from './model';

/**
 * The saved keys that wait for a restart of this tab's process, each with the JSON of its value
 * before the first save that changed it (the value that the process runs with, as the board
 * knows it). Board state only: the settings view has no such fact (DL-501).
 */
export type Pending = Readonly<Record<string, string>>;

export const NO_PENDING: Pending = {};

const json = (v: unknown): string => JSON.stringify(v ?? null);

/**
 * The pending keys after a save. `before`: the values of the view before the save. A key of
 * `saved.restart_needed` that is not pending yet gets its value before the save. A pending key
 * whose saved value is again its first value leaves the list (the owner changed it back), except
 * a secret field: its view value (`{ "set": true }`) does not show a change of the secret.
 */
export function nextPending(
  pending: Pending,
  before: Values,
  saved: SettingsSaved,
  fields: readonly FieldMeta[],
): Pending {
  const out: Record<string, string> = { ...pending };
  for (const key of saved.restart_needed) {
    if (!(key in out)) out[key] = json(before[key]);
  }
  for (const key of Object.keys(out)) {
    const secret = fields.find((f) => f.key === key)?.secret === true;
    if (!secret && json(saved.values[key]) === out[key]) delete out[key];
  }
  return out;
}
