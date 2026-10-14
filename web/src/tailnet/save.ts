// DL-178, tailnet design 14 and 15: one deck key saved from the Tailnet card (the toggle saves
// `tailnet_auto`) through the normal settings save, with the elevation of DL-172.
import { DECK, type SettingsActions } from '../settings/actions';
import type { ElevationAsk } from '../settings/elevation';
import { trySave } from '../settings/flow';
import { describe } from '../settings/http';

export type KeyOutcome =
  | { readonly kind: 'saved' }
  | { readonly kind: 'cancelled' }
  | { readonly kind: 'failed'; readonly text: string };

/**
 * Saves `{ [key]: value }` in the deck settings. With `askFirst` the owner confirms before the
 * save. Without it, the save goes with the elevation that the board holds, and a deck that
 * answers `elevation_required` makes the owner confirm and the save runs once more.
 */
export async function saveDeckKey(
  settings: SettingsActions,
  ask: ElevationAsk,
  key: string,
  value: unknown,
  labels: readonly string[],
  askFirst: boolean,
): Promise<KeyOutcome> {
  if (askFirst && (await ask(labels)) === null) return { kind: 'cancelled' };
  let asked = askFirst;
  try {
    for (;;) {
      const view = await settings.view(DECK);
      const out = await trySave(settings, DECK, { [key]: value }, view.revision, [key]);
      switch (out.kind) {
        case 'saved':
          return { kind: 'saved' };
        case 'elevate':
          if (asked) return { kind: 'failed', text: 'The deck did not accept the confirmation.' };
          asked = true;
          if ((await ask(labels)) === null) return { kind: 'cancelled' };
          break;
        case 'changed':
          return { kind: 'failed', text: 'These settings changed since you opened them.' };
        default:
          return { kind: 'failed', text: out.text };
      }
    }
  } catch (e) {
    return { kind: 'failed', text: describe(e) };
  }
}
