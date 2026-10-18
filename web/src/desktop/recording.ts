// R13 P5b task W1 (design `omarchy-p5b.md` §12, §16): the fixed texts, the duration rule and the
// availability of the owner recording in the Desktop panel. Pure code: the flow is in
// `recordFlow.ts`, the calls are in `capture.ts`, the controls are in `RecordPanel.tsx`.
import type { DesktopInfo, RecordingPhase } from '../gen';

/** The feature that the deck and the station advertise for the owner recording (§3.1). */
export const RECORD_FEATURE = 'desktop_recording.v1';
const CAPTURE_FEATURE = 'desktop_capture.v1';
const VIDEO_FEATURE = 'artifacts_video.v1';

export const MIN_SECONDS = 1;
export const MAX_SECONDS = 120;
export const DEFAULT_SECONDS = '30';
/** The completion allowance on top of the duration, for the POST and for the polling (O12-10). */
export const ALLOWANCE_MS = 45_000;

/** The time that the board follows a recording of `seconds` seconds before "Check result". */
export const windowMs = (seconds: number): number => seconds * 1000 + ALLOWANCE_MS;

export const DURATION_LABEL = 'Duration (seconds)';
export const DURATION_ERROR = 'Recording duration must be a whole number from 1 to 120 seconds.';
export const DISCLOSURE =
  'Records the whole selected monitor, including other applications, notifications and private information visible there. No audio is recorded. The recording is stored as an ordinary artifact until deleted.';

export const PHASE_TEXT: Readonly<Record<RecordingPhase, string>> = {
  preparing: 'Preparing recording',
  recording: 'Recording',
  stopping: 'Stopping recording',
  finalizing: 'Saving recording',
};

export const UNKNOWN_TEXT =
  'The recording result is unknown. Check the artifact list before starting another recording.';
export const SAVED_TEXT =
  'The recording was saved on the station. The artifact list is still updating.';
export const DELETED_TEXT = 'The recording was deleted.';
export const OFFLINE_TEXT = 'The station is not connected.';

export const REASONS = {
  deck: 'This deck does not support monitor recordings.',
  station: 'This station does not support monitor recordings.',
  recorder: 'The recording tool wf-recorder is not installed.',
  off: 'Desktop recordings are off on this station. Turn them on in Settings.',
  monitor: 'This monitor is not allowed for recordings.',
} as const;

/** The duration that the owner typed: a whole number from 1 to 120, else `null`. */
export function parseSeconds(text: string): number | null {
  if (!/^\d{1,4}$/.test(text)) return null;
  const n = Number(text);
  return n >= MIN_SECONDS && n <= MAX_SECONDS ? n : null;
}

/** The key of the capture ID of the recording of one station in `localStorage`. */
export const recordingKey = (stationId: string): string => `fd.recording.${stationId}`;

/** The deck (`/api/features`: `undefined` while it loads, `null` for an old deck) knows recordings. */
export const deckRecords = (deck: readonly string[] | null | undefined): boolean =>
  deck?.includes(RECORD_FEATURE) === true;

/**
 * Whether the station and the deck can record at all. A capability failure is shown on its own,
 * apart from a missing tool or a permission (§12).
 */
export function capability(
  deck: readonly string[] | null | undefined,
  info: DesktopInfo,
): string | null {
  if (deck === undefined) return null;
  if (!deckRecords(deck)) return REASONS.deck;
  const need = [CAPTURE_FEATURE, RECORD_FEATURE, VIDEO_FEATURE];
  if (!need.every((f) => info.features.includes(f))) return REASONS.station;
  return null;
}

export type Availability = { readonly enabled: boolean; readonly reason: string | null };

/** Whether Record can start on one monitor, and the first reason when it cannot. */
export function availability(
  deck: readonly string[] | null | undefined,
  info: DesktopInfo,
  monitor: string,
): Availability {
  if (deck === undefined) return { enabled: false, reason: null };
  const no = (reason: string): Availability => ({ enabled: false, reason });
  const cap = capability(deck, info);
  if (cap !== null) return no(cap);
  if (!info.tools.recorder) return no(REASONS.recorder);
  if (!info.allowed.capture) return no(REASONS.off);
  const allowed = info.allowed.capture_monitors;
  if (allowed.length > 0 && !allowed.includes(monitor)) return no(REASONS.monitor);
  return { enabled: true, reason: null };
}

/**
 * The shared busy state of the station (decision 1): Screenshot and Record both stay off while
 * another action runs (`busy`) or while a screenshot or a recording runs on the station
 * (`slotActive`). "Stop recording" never uses this rule.
 */
export const startOff = (a: {
  readonly busy: boolean;
  readonly slotActive: boolean;
  readonly enabled: boolean;
}): boolean => a.busy || a.slotActive || !a.enabled;
