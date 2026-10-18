// R13 P5a task F (design `docs/v2/design/omarchy-p5a.md` §2.2-§2.5): the board's calls for the
// owner screenshot. `take` sends the one POST of a capture (the header carries the capture ID);
// `status` is the durable lookup; `features` tells whether the deck knows `desktop_capture.v1`.
// Nothing here repeats a POST.
import type { CaptureStatus, DesktopActResult } from '../gen';
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
  /** `GET /api/stations/{station}/desktop/captures/{capture_id}`. */
  status(station: string, captureId: string): Promise<CaptureStatus>;
  /** `GET /api/features`; `null` is the 404 of an old deck. */
  features(): Promise<readonly string[] | null>;
};

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

const seg = encodeURIComponent;

const STATES = ['pending', 'unknown', 'committed', 'failed', 'deleted'];

export function captureActions(captureIo: DeckHttp, lookupIo: DeckHttp): CaptureActions {
  return {
    take: async (station, monitor, captureId) => {
      const v = await captureIo.post(
        `/api/stations/${seg(station)}/desktop/act`,
        { action: 'screen_capture', monitor },
        { [CAPTURE_ID_HEADER]: captureId },
      );
      if (record(v) && record(v['info'])) return { kind: 'done', result: v as DesktopActResult };
      if (
        record(v) &&
        typeof v['capture_id'] === 'string' &&
        (v['state'] === 'pending' || v['state'] === 'unknown' || v['state'] === 'deleted')
      ) {
        return { kind: v['state'], captureId: v['capture_id'] };
      }
      throw new Error('The deck sent an invalid desktop answer.');
    },
    status: async (station, captureId) => {
      const v = await lookupIo.get(
        `/api/stations/${seg(station)}/desktop/captures/${seg(captureId)}`,
      );
      if (!record(v) || typeof v['state'] !== 'string' || !STATES.includes(v['state'])) {
        throw new Error('The deck sent an invalid capture status.');
      }
      return v as CaptureStatus;
    },
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
