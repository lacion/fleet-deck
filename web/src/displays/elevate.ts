// R13 P2c: a call that may need the owner's elevation (DL-172). It tries with the elevation that is
// still valid; on `elevation_required` it asks once ("Confirm it is you") and sends the call again.
import type { SettingsActions } from '../settings/actions';
import type { ElevationAsk } from '../settings/elevation';
import { DeckError } from '../settings/http';

/** The label that the dialog shows for a change of the displays. */
export const ELEVATION_LABELS: readonly string[] = ['displays'];

/** The answer of `run`, or `null` when the owner cancels the dialog. */
export async function withElevation<T>(
  settings: SettingsActions,
  ask: ElevationAsk,
  run: (elevation: string | null) => Promise<T>,
): Promise<T | null> {
  try {
    return await run(settings.elevation());
  } catch (e) {
    if (!(e instanceof DeckError) || e.code !== 'elevation_required') throw e;
    settings.forgetElevation();
    const token = await ask(ELEVATION_LABELS);
    return token === null ? null : run(token);
  }
}
