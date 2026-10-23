// The facts of a station that more than one surface shows, in one notation (DESIGN-SYSTEM §4.4,
// §5). FG-6 (design §9.4): the traits line of a station card: "<OS> · <arch>[ · on battery][ ·
// <label> · <label>]", with the display name of the OS ("Linux", "macOS", "Omarchy"), the arch as
// an owner names it ("arm64") and the labels as words ("GPU", "Desktop"). An older station sends no
// `traits`: no line. Then the slot meter of a station: the fleet lane, the station card and the
// Properties Station row draw the same ticks and words.
import type { StationTraits as Traits } from '../gen';
import { osLabel } from '../tailnet/model';
import { countText } from '../ui/format';

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

/**
 * The arch of a station as its owner names it: the Rust target arch `aarch64` is `arm64` (the
 * word of macOS and of the concept pictures). The raw arch stays in the `title` of the line and in
 * a placement need (`arch:aarch64`).
 */
export const archText = (arch: string): string => (arch === 'aarch64' ? 'arm64' : arch);

/** The text of the line: one separator for every part, so that each label reads as its own value. */
export function traitsText(t: Traits): string {
  const parts = [osLabel(t.os), archText(t.arch)];
  if (t.on_battery === true) parts.push('on battery');
  parts.push(...t.labels.map(labelText));
  return parts.join(' · ');
}

export function StationTraits({ traits }: { readonly traits: Traits | undefined }) {
  if (traits === undefined || !Array.isArray(traits.labels)) return null;
  // The raw arch, where the line shows another word for it.
  const raw = archText(traits.arch) === traits.arch ? undefined : traits.arch;
  return (
    <p className="fd-note fd-station-traits" title={raw}>
      {traitsText(traits)}
    </p>
  );
}

/** The most slots that a slot meter draws as ticks (§3 `.fd-slots`); more slots get a neutral bar. */
export const MAX_TICKS = 24;

/** The fill of the load bar: the share of the session slots in use, 0 to 100 %. */
export const loadWidth = (active: number, max: number): string =>
  `${max > 0 ? Math.min(100, (active / max) * 100) : 0}%`;

/**
 * The session slots of a station (§4.4), one notation on every surface: the ticks (the needs-you
 * slots first, then the used ones) and "3 of 12"; over `maxTicks` slots a neutral bar. A station
 * that runs no session shows its free slots as words ("12 free", as the New session station pills
 * say): a row of empty ticks says nothing more. The caller wraps it in the element of its place;
 * `countClass` is the class of the words.
 */
export function SlotMeter({
  max,
  used,
  needs = 0,
  maxTicks = MAX_TICKS,
  countClass,
}: {
  readonly max: number;
  readonly used: number;
  readonly needs?: number;
  /** The most ticks that the place has room for. */
  readonly maxTicks?: number;
  readonly countClass?: string;
}) {
  if (max > 0 && used === 0) {
    return (
      <span className={countClass} data-free="true">
        {countText(max)} free<span className="fd-vh"> session slots</span>
      </span>
    );
  }
  const shown = Math.min(used, max);
  const urgent = Math.min(needs, shown);
  return (
    <>
      {max <= 0 ? null : max <= maxTicks ? (
        <span className="fd-slots" aria-hidden="true">
          {Array.from({ length: max }, (_, i) => (
            <span key={i} data-slot={i < urgent ? 'needs_you' : i < shown ? 'used' : undefined} />
          ))}
        </span>
      ) : (
        <span
          className="fd-meter"
          data-tone="neutral"
          data-level={used / max >= 0.9 ? 'high' : undefined}
          aria-hidden="true"
        >
          <span style={{ width: loadWidth(used, max) }} />
        </span>
      )}
      {/* Counts read "3 of 12" (§5); "sessions" is for a screen reader only. */}
      <span className={countClass}>
        {`${countText(used)} of ${countText(max)}`}
        <span className="fd-vh"> sessions</span>
      </span>
    </>
  );
}
