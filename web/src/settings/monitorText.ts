// The words of a monitor of a station (MonitorInfo): the settings rows of `capture_monitors` and the
// bound monitor of a wall display show the same text. The EDID description of the monitor carries
// hex model codes ("Samsung Display Corp. 0x41A1"): the option shows the name without them, and its
// title keeps the full text.
import type { MonitorInfo } from '../gen';

/** The connectors of a built-in panel (a laptop screen). */
const BUILT_IN = /^(eDP|LVDS|DSI)-/i;

/** The full text of a monitor: `<name> — <description>`, or only the name. */
export const monitorTitle = (m: MonitorInfo): string =>
  m.description === '' ? m.name : `${m.name} — ${m.description}`;

/**
 * The text of a monitor option: "Built-in screen (eDP-1)" for a laptop panel, else the name and the
 * description without hex model codes (`DP-1 — Dell U2720Q`), or only the name.
 */
export function monitorText(m: MonitorInfo): string {
  if (BUILT_IN.test(m.name)) return `Built-in screen (${m.name})`;
  const description = m.description.replace(/\s*\b0x[0-9a-f]+\b/gi, '').trim();
  return description === '' ? m.name : `${m.name} — ${description}`;
}

/** The `title` of an option: the full text when the option shows less of it. */
export function monitorTitleOf(m: MonitorInfo): string | undefined {
  const full = monitorTitle(m);
  return full === monitorText(m) ? undefined : full;
}
