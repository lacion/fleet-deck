// R13 P2c: the calls and the pure parts of a wall display page (`/display`): the display header,
// the one-time display link, the pairing calls, the directive and the change summary.
import { type WallRoute, wallDefaults } from '../app/routes';
import type {
  DirectiveBody,
  DirectiveView,
  DisplayChanges,
  PairingPoll,
  PairingStarted,
} from '../gen';
import { FEATURES_HEADER } from '../artifacts/video';
import { type Environment, browserEnvironment } from '../runtime/connection';
import { type Fetch, request } from '../runtime/api';

/** The features that a wall display declares (B17): in `features=` of the socket and in the header
 * of every request. The declaration grants no authority; the deck checks every use. */
export const DISPLAY_FEATURES = 'artifacts_video.v1 wall_control.v1 wall_pages.v1';

/** The header that makes the deck answer the display login when the browser holds both cookies. */
export const DISPLAY_HEADER = 'X-Fleetdeck-Display';

/** The path of the display document. */
export const DISPLAY_PATH = '/display';

/** `fetcher` with the display header and the display features on every request. */
export const withDisplayHeader =
  (fetcher: Fetch): Fetch =>
  (path, init) =>
    fetcher(path, {
      ...init,
      headers: {
        ...(init.headers as Record<string, string> | undefined),
        [DISPLAY_HEADER]: '1',
        [FEATURES_HEADER]: DISPLAY_FEATURES,
      },
    });

/** The board environment, with the display header and the display features on every request, and
 * the display features in the query of the socket. */
export function displayEnvironment(): Environment {
  const base = browserEnvironment(DISPLAY_FEATURES);
  return { ...base, fetch: withDisplayHeader(base.fetch) };
}

/** True for the display document (`/display`). */
export const isDisplayPath = (pathname: string): boolean => pathname === DISPLAY_PATH;

/** Strips a one-time display link (`#display=<token>`) before a request or a render. */
export function takeDisplayLink(
  location: Pick<Location, 'hash' | 'pathname' | 'search'>,
  history: Pick<History, 'replaceState'>,
): string | null {
  const hash = location.hash.slice(1);
  if (hash.startsWith('/')) return null;
  const token = new URLSearchParams(hash).get('display');
  if (token !== null) history.replaceState(null, '', `${location.pathname}${location.search}`);
  return token === null || token === '' ? null : token;
}

const TIMEOUT_MS = 10_000;

async function call(fetcher: Fetch, path: string, body?: unknown): Promise<unknown> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), TIMEOUT_MS);
  try {
    return await request(fetcher, path, controller.signal, body);
  } finally {
    clearTimeout(timer);
  }
}

/** Redeems a one-time link; the deck answers 204 and sets `fd_display`. `true`: it worked. */
export async function redeemDisplayLink(fetcher: Fetch, token: string): Promise<boolean> {
  try {
    await call(fetcher, '/api/auth/display-link', { token });
    return true;
  } catch {
    return false;
  }
}

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

/** The non-secret launch values of the pair page URL, or `null` unless both are valid. */
export function expectedLaunch(
  query: string,
): { readonly display_id: string; readonly operation_revision: number } | null {
  const q = new URLSearchParams(query);
  const id = q.get('expect');
  const op = q.get('op');
  if (id === null || op === null || !/^[0-9A-Za-z]{26}$/.test(id) || !/^\d{1,15}$/.test(op)) {
    return null;
  }
  return { display_id: id, operation_revision: Number(op) };
}

/** `POST /api/displays/pair`: the code that this screen shows. */
export async function startPairing(
  fetcher: Fetch,
  launch: ReturnType<typeof expectedLaunch>,
): Promise<PairingStarted> {
  const v = await call(
    fetcher,
    '/api/displays/pair',
    launch === null
      ? {}
      : { expected_display_id: launch.display_id, operation_revision: launch.operation_revision },
  );
  if (
    !record(v) ||
    typeof v['pairing_id'] !== 'string' ||
    typeof v['code'] !== 'string' ||
    typeof v['expires_at'] !== 'number'
  ) {
    throw new Error('The deck sent an invalid pairing.');
  }
  return v as PairingStarted;
}

/** `GET /api/displays/pair/{id}`. */
export async function pollPairing(fetcher: Fetch, id: string): Promise<PairingPoll['state']> {
  const v = await call(fetcher, `/api/displays/pair/${encodeURIComponent(id)}`);
  if (!record(v) || (v['state'] !== 'pending' && v['state'] !== 'approved_claimed')) {
    throw new Error('The deck sent an invalid pairing state.');
  }
  return v['state'];
}

/** Validates the answer of `GET /api/displays/self/directive`. */
export function directiveView(v: unknown): DirectiveView {
  if (
    !record(v) ||
    typeof v['generation'] !== 'number' ||
    !record(v['directive']) ||
    typeof v['directive']['mode'] !== 'string'
  ) {
    throw new Error('The deck sent an invalid directive.');
  }
  return v as DirectiveView;
}

/**
 * The wall route that a directive asks for. `scale` and `shape` of `base` stay; the filter of the
 * directive is not used here, because the deck enforces it.
 */
export function directiveRoute(body: DirectiveBody, base: WallRoute = wallDefaults): WallRoute {
  const { session: _s, batch: _b, artifact: _a, ...keep } = base;
  const common = { ...keep, name: 'wall' as const, pin: body.pin, stations: [], repo: '' };
  switch (body.mode) {
    case 'mission':
    case 'ambient':
      return { ...common, mode: body.mode };
    case 'focus':
    case 'review':
      return {
        ...common,
        mode: body.mode,
        session: { station: body.session.station_id, id: body.session.session_id },
      };
    case 'race':
      return { ...common, mode: 'race', batch: body.batch_id };
    case 'artifact':
      return {
        ...common,
        mode: 'artifact',
        artifact: {
          station: body.artifact.station_id,
          id: body.artifact.artifact_id,
          version: body.artifact.version,
        },
      };
  }
}

/** The path of the change summary of a session (`patch=1` asks for the patches too). */
export const changesPath = (station: string, session: string, patch: boolean): string =>
  `/api/displays/self/sessions/${encodeURIComponent(station)}/${encodeURIComponent(session)}/changes?patch=${patch ? 1 : 0}`;

/** Validates the answer of the change route. */
export function displayChanges(v: unknown): DisplayChanges {
  if (
    !record(v) ||
    typeof v['session_id'] !== 'string' ||
    typeof v['truncated'] !== 'boolean' ||
    !record(v['summary']) ||
    !Array.isArray(v['files'])
  ) {
    throw new Error('The deck sent an invalid change summary.');
  }
  return v as DisplayChanges;
}

/** The path of the one file of an artifact version (`_` is the one file, R15). */
export const artifactPath = (station: string, id: string, version: number): string =>
  `/api/displays/self/artifact/${encodeURIComponent(station)}/${encodeURIComponent(id)}/${version}/_`;
