// FG-6 (design §9.4): the traits line of a station card: "<OS> · <arch>[ · on battery][ · <label>
// · <label>]", with the display name of the OS ("Linux", "macOS", "Omarchy"). An older station sends
// no `traits`: no line.
import type { StationTraits as Traits } from '../gen';
import { osLabel } from '../tailnet/model';

/** The text of the line: one separator for every part, so that each label reads as its own value. */
export function traitsText(t: Traits): string {
  const parts = [osLabel(t.os), t.arch];
  if (t.on_battery === true) parts.push('on battery');
  parts.push(...t.labels);
  return parts.join(' · ');
}

export function StationTraits({ traits }: { readonly traits: Traits | undefined }) {
  if (traits === undefined || !Array.isArray(traits.labels)) return null;
  return <p className="fd-note fd-station-traits">{traitsText(traits)}</p>;
}
