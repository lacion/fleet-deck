// FG-6 (design §9.4): the traits line of a station card: "<OS> · <arch>[ · on battery][ · <label>
// · <label>]", with the display name of the OS ("Linux", "macOS", "Omarchy") and the labels as
// words ("GPU", "Desktop"). An older station sends no `traits`: no line.
import type { StationTraits as Traits } from '../gen';
import { osLabel } from '../tailnet/model';

/** The labels that are short names: they read in capitals. */
const ACRONYMS: readonly string[] = ['gpu', 'cpu', 'tpu', 'npu', 'nas', 'wsl', 'ssd', 'ram', 'vm'];

/**
 * A station label as words: `gpu` → `GPU`, `desktop` → `Desktop`, `always_on` → `Always on`. The
 * id stays in the Station labels field of the settings, where a start names it.
 */
export function labelText(label: string): string {
  if (ACRONYMS.includes(label)) return label.toUpperCase();
  const words = label.replace(/[-_]+/g, ' ');
  return words.charAt(0).toUpperCase() + words.slice(1);
}

/** The text of the line: one separator for every part, so that each label reads as its own value. */
export function traitsText(t: Traits): string {
  const parts = [osLabel(t.os), t.arch];
  if (t.on_battery === true) parts.push('on battery');
  parts.push(...t.labels.map(labelText));
  return parts.join(' · ');
}

export function StationTraits({ traits }: { readonly traits: Traits | undefined }) {
  if (traits === undefined || !Array.isArray(traits.labels)) return null;
  return <p className="fd-note fd-station-traits">{traitsText(traits)}</p>;
}
