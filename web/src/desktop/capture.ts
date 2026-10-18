// R13 P5a task F (design `docs/v2/design/omarchy-p5a.md` §2.2-§2.5): the board's calls for the
// owner screenshot. `take` sends the one POST of a capture (the header carries the capture ID);
// `status` is the durable lookup; `features` tells whether the deck knows `desktop_capture.v1`.
// Nothing here repeats a POST.
// R13 P5b (design `omarchy-p5b.md` §3.2, §4): the same calls also start a recording, discover the
// active recording and stop it; the status answer carries the optional recording progress.
import type { ActiveRecording, DesktopActResult, RecordingCaptureStatus } from '../gen';
import { DeckError, type DeckHttp } from '../settings/http';

/** The header that carries the capture ID (§2.2). */
export const CAPTURE_ID_HEADER = 'X-FleetDeck-Capture-Id';

/** The answer of the POST: `200` with the result, or a state body (`202`, or a deleted capture). */
export type TakeResult =
  | { readonly kind: 'done'; readonly result: DesktopActResult }
  | { readonly kind: 'pending' | 'unknown' | 'deleted'; readonly captureId: string };

export type CaptureActions = {
  /** `POST /api/stations/{station}/desktop/act`; `station` is the station name. */
  take(station: string, monitor: string, captureId: string): Promise<TakeResult>;
  /** `GET /api/stations/{station}/desktop/captures/{capture_id}`; a recording adds its progress. */
  status(station: string, captureId: string): Promise<RecordingCaptureStatus>;
  /** `POST …/desktop/act` with `screen_record` and the capture ID header (§3.2); one POST only. */
  record(station: string, monitor: string, seconds: number, captureId: string): Promise<TakeResult>;
  /** `GET /api/stations/{station}/desktop/recording` (§4.3). */
  active(station: string): Promise<ActiveRecording>;
  /** `POST …/desktop/captures/{capture_id}/stop` with `{}` (§4.4): the current durable status. */
  stop(station: string, captureId: string): Promise<RecordingCaptureStatus>;
  /** `GET /api/features`; `null` is the 404 of an old deck. */
  features(): Promise<readonly string[] | null>;
};

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

const seg = encodeURIComponent;

const STATES = ['pending', 'unknown', 'committed', 'failed', 'deleted'];
const PHASES = ['preparing', 'recording', 'stopping', 'finalizing'];

/** The POST answer of a capture: the result, or a state body. */
function takeResult(v: unknown): TakeResult {
  if (record(v) && record(v['info'])) return { kind: 'done', result: v as DesktopActResult };
  if (
    record(v) &&
    typeof v['capture_id'] === 'string' &&
    (v['state'] === 'pending' || v['state'] === 'unknown' || v['state'] === 'deleted')
  ) {
    return { kind: v['state'], captureId: v['capture_id'] };
  }
  throw new Error('The deck sent an invalid desktop answer.');
}

/**
 * A capture status. A `recording` field that is not a valid progress is dropped, so that a
 * malformed observation is never shown as the phase of a recording.
 */
function statusOf(v: unknown): RecordingCaptureStatus {
  if (!record(v) || typeof v['state'] !== 'string' || !STATES.includes(v['state'])) {
    throw new Error('The deck sent an invalid capture status.');
  }
  const progress = v['recording'];
  if (
    progress !== undefined &&
    !(
      record(progress) &&
      typeof progress['phase'] === 'string' &&
      PHASES.includes(progress['phase']) &&
      typeof progress['requested_seconds'] === 'number'
    )
  ) {
    const { recording: _dropped, ...rest } = v;
    return rest as RecordingCaptureStatus;
  }
  return v as RecordingCaptureStatus;
}

/** The completion allowance of the recording POST on top of the duration (§12; O12-10 item 5). */
export const RECORD_ALLOWANCE_S = 45;

/**
 * `recordIo(timeoutMs)` gives the transport of the recording POST, whose time limit is
 * `seconds + 45` s. Without it the POST uses `captureIo`.
 */
export function captureActions(
  captureIo: DeckHttp,
  lookupIo: DeckHttp,
  recordIo?: (timeoutMs: number) => DeckHttp,
): CaptureActions {
  return {
    take: async (station, monitor, captureId) =>
      takeResult(
        await captureIo.post(
          `/api/stations/${seg(station)}/desktop/act`,
          { action: 'screen_capture', monitor },
          { [CAPTURE_ID_HEADER]: captureId },
        ),
      ),
    record: async (station, monitor, seconds, captureId) => {
      const io =
        recordIo === undefined ? captureIo : recordIo((seconds + RECORD_ALLOWANCE_S) * 1000);
      return takeResult(
        await io.post(
          `/api/stations/${seg(station)}/desktop/act`,
          { action: 'screen_record', monitor, seconds },
          { [CAPTURE_ID_HEADER]: captureId },
        ),
      );
    },
    status: async (station, captureId) =>
      statusOf(
        await lookupIo.get(`/api/stations/${seg(station)}/desktop/captures/${seg(captureId)}`),
      ),
    active: async (station) => {
      const v = await lookupIo.get(`/api/stations/${seg(station)}/desktop/recording`);
      if (record(v) && v['state'] === 'idle') return { state: 'idle' };
      if (record(v) && v['state'] === 'active' && typeof v['capture_id'] === 'string') {
        return { state: 'active', capture_id: v['capture_id'], status: statusOf(v['status']) };
      }
      throw new Error('The deck sent an invalid recording answer.');
    },
    stop: async (station, captureId) =>
      statusOf(
        await lookupIo.post(
          `/api/stations/${seg(station)}/desktop/captures/${seg(captureId)}/stop`,
          {},
        ),
      ),
    features: async () => {
      try {
        const v = await lookupIo.get('/api/features');
        const list = record(v) ? v['features'] : undefined;
        if (!Array.isArray(list)) throw new Error('The deck sent an invalid feature list.');
        return list.filter((f): f is string => typeof f === 'string');
      } catch (e) {
        if (e instanceof DeckError && e.status === 404) return null;
        throw e;
      }
    },
  };
}
