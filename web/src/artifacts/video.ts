// R13 P5b (§12, §14): the declaration of the board for video artifacts and the pure pieces of the
// video viewer.
import type { CaptureProvenance } from '../gen';

/** The feature that the board declares: the deck then sends `kind: "video"` and recording data. */
export const VIDEO_FEATURES = 'artifacts_video.v1';

/** The header (and the `?features=` query of the live connection) that carries the declaration. */
export const FEATURES_HEADER = 'X-FleetDeck-Features';

/** The requested and the saved length differ when they are more than this many ms apart. */
const NOTE_THRESHOLD_MS = 1000;

/** Seconds with one decimal and no trailing `.0`. */
const seconds = (ms: number): string => String(Math.round(ms / 100) / 10);

/**
 * `Desktop recording · <station>/<monitor> · <local time>`; the requested and the saved length are
 * added when they differ by more than one second. `null` for a version that is not a recording.
 */
export function provenanceLine(
  station: string,
  capture: CaptureProvenance | undefined,
): string | null {
  if (capture === undefined || capture.kind !== 'recording') return null;
  const at = new Date(capture.at).toLocaleString();
  const line = `Desktop recording · ${station}/${capture.monitor} · ${at}`;
  const meta = capture.recording;
  if (meta === undefined) return line;
  if (Math.abs(meta.requested_seconds * 1000 - meta.duration_ms) <= NOTE_THRESHOLD_MS) return line;
  return `${line} · requested ${meta.requested_seconds} s, saved ${seconds(meta.duration_ms)} s`;
}

/** What the retry needs from a media element. */
export type ReloadableMedia = Pick<HTMLMediaElement, 'removeAttribute' | 'setAttribute' | 'load'>;

/** A playback retry: it sets the `src` again and reloads the bytes. It starts no other call. */
export function reloadSource(media: ReloadableMedia, url: string): void {
  media.removeAttribute('src');
  media.setAttribute('src', url);
  media.load();
}
