// R13 P2c: the owner's calls for wall displays and the desktop relay. The shapes are the deck's
// (`crates/fleetdeck-deck/src/http/{display_routes,display_directive_routes,desktop_routes}.rs`).
// The elevation of a change lives in `SettingsActions`; these calls only carry it in the body.
import type {
  DesktopActBody,
  DesktopActResult,
  DesktopInfo,
  DirectiveBody,
  DirectiveView,
  DisplayLinkMade,
  DisplayList,
  DisplayView,
  StationId,
} from '../gen';
import { DeckError, type DeckIo } from '../settings/http';

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

const seg = encodeURIComponent;

/** The changes of `POST /api/displays/{id}/settings`; an absent key stays as it is. */
export type DisplayChange = {
  readonly name?: string;
  readonly ceiling?: readonly StationId[];
  readonly show_diffs?: boolean;
  readonly wake_on_needs_you?: boolean;
  /** `null` clears the quiet hours. */
  readonly quiet_hours?: string | null;
  /** `null` clears the binding. */
  readonly binding?: { readonly station_id: StationId; readonly monitor: string } | null;
};

/** What the approval of a pairing needs: the new display, or an existing one. */
export type Approve =
  | { readonly display_id: string }
  | { readonly name: string; readonly ceiling: readonly StationId[] };

export type DisplayActions = {
  list(): Promise<DisplayList>;
  approve(
    pairingId: string,
    code: string,
    what: Approve,
    elevation: string | null,
  ): Promise<DisplayView>;
  makeLink(
    name: string,
    ceiling: readonly StationId[],
    elevation: string | null,
  ): Promise<DisplayLinkMade>;
  change(id: string, change: DisplayChange, elevation: string | null): Promise<DisplayView>;
  revoke(id: string): Promise<void>;
  directive(id: string, body: DirectiveBody): Promise<DirectiveView>;
};

const withElevation = (elevation: string | null) => (elevation === null ? {} : { elevation });

function view(v: unknown): DisplayView {
  if (!record(v) || typeof v['display_id'] !== 'string' || typeof v['name'] !== 'string') {
    throw new Error('The deck sent an invalid display.');
  }
  return v as DisplayView;
}

export function displayActions(io: DeckIo): DisplayActions {
  return {
    list: async () => {
      const v = await io.get('/api/displays');
      if (!record(v) || !Array.isArray(v['displays']) || !Array.isArray(v['pending'])) {
        throw new Error('The deck sent an invalid display list.');
      }
      return { displays: v['displays'].map(view), pending: v['pending'] } as DisplayList;
    },
    approve: async (pairingId, code, what, elevation) =>
      view(
        await io.post(`/api/displays/pair/${seg(pairingId)}/approve`, {
          code,
          ...what,
          ...withElevation(elevation),
        }),
      ),
    makeLink: async (name, ceiling, elevation) => {
      const v = await io.post('/api/displays/link', {
        name,
        ceiling,
        ...withElevation(elevation),
      });
      if (!record(v) || typeof v['url'] !== 'string' || typeof v['expires_at'] !== 'number') {
        throw new Error('The deck sent an invalid display link.');
      }
      return v as DisplayLinkMade;
    },
    change: async (id, change, elevation) =>
      view(
        await io.post(`/api/displays/${seg(id)}/settings`, {
          ...change,
          ...withElevation(elevation),
        }),
      ),
    revoke: async (id) => {
      await io.post(`/api/displays/${seg(id)}/revoke`, {});
    },
    directive: async (id, body) => {
      const v = await io.post(`/api/displays/${seg(id)}/directive`, body);
      if (!record(v) || typeof v['generation'] !== 'number') {
        throw new Error('The deck sent an invalid directive.');
      }
      return v as DirectiveView;
    },
  };
}

export type DesktopActions = {
  /** `GET /api/stations/{station}/desktop`; `station` is the station name. */
  info(station: string): Promise<DesktopInfo>;
  /** `POST /api/stations/{station}/desktop/act`. */
  act(station: string, body: DesktopActBody): Promise<DesktopActResult>;
};

export function desktopActions(io: DeckIo): DesktopActions {
  return {
    info: async (station) => {
      const v = await io.get(`/api/stations/${seg(station)}/desktop`);
      if (!record(v) || !Array.isArray(v['monitors'])) {
        throw new Error('The deck sent an invalid desktop answer.');
      }
      return v as DesktopInfo;
    },
    act: async (station, body) => {
      const v = await io.post(`/api/stations/${seg(station)}/desktop/act`, body);
      if (!record(v) || !record(v['info'])) {
        throw new Error('The deck sent an invalid desktop answer.');
      }
      return v as DesktopActResult;
    },
  };
}

/** True for the answer of a deck without wall displays (an older deck). */
export const noDisplays = (e: unknown): boolean =>
  e instanceof DeckError && (e.status === 404 || e.code === 'not_found');

export const NO_DISPLAYS_TEXT = 'This deck has no wall displays';
