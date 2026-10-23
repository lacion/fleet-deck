// The words of a monitor (DESIGN-SYSTEM §5): not the raw EDID text of the station. This file is
// the place of the helper until the cross-area request of the wall r2 audit moves it to
// `ui/format.ts` (the station tab's monitor rows use it too).
import type { MonitorInfo } from '../gen';

/** A connector of a laptop panel: the screen of the machine itself. */
const BUILT_IN = /^(?:eDP|LVDS)-/;
/** The hex product code at the end of an EDID description ("Samsung Display Corp. 0x41AA"). */
const PRODUCT_CODE = /\s*0x[0-9A-Fa-f]+$/;

/**
 * One option text of a monitor, the connector last: `Built-in display · eDP-1` for a laptop panel,
 * `Dell Inc. DELL U2720Q · DP-1` (the description without its hex product code), or only the
 * connector when the station knows nothing more. The option value and its title keep the raw name.
 */
export function monitorLabel(m: Pick<MonitorInfo, 'name' | 'description'>): string {
  const described = m.description.replace(PRODUCT_CODE, '').trim();
  const what = BUILT_IN.test(m.name) ? 'Built-in display' : described;
  return what === '' ? m.name : `${what} · ${m.name}`;
}

/** The raw text of a monitor (the title of its option): `<name> — <description>`. */
export const monitorRaw = (m: Pick<MonitorInfo, 'name' | 'description'>): string =>
  m.description === '' ? m.name : `${m.name} — ${m.description}`;
