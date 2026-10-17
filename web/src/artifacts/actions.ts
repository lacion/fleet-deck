// R12 (DL-165): the artifact calls of the board. The shapes are the deck's
// (`crates/fleetdeck-deck/src/http/artifact_routes.rs`).
import { ApiError, type Fetch } from '../runtime/api';
import type {
  ArtifactPublished,
  ArtifactPublishRequest,
  DeckArtifact,
  DeckArtifactDetail,
  ShareLink,
  ShareRequest,
  ViewLink,
} from '../gen';

type Get = (path: string) => Promise<unknown>;
type Post = (path: string, body: unknown) => Promise<unknown>;
type Del = (path: string) => Promise<unknown>;
type Text = (path: string) => Promise<string>;

/** The transport of the calls; `del` and `text` are not on `Connection`. */
export type ArtifactTransport = {
  readonly get: Get;
  readonly post: Post;
  readonly del: Del;
  readonly text: Text;
};

/** The filters of `GET /api/artifacts`; an empty one is left out of the query. */
export type ArtifactFilter = {
  readonly station?: string;
  readonly session?: string;
  readonly q?: string;
};

export type ArtifactActions = {
  /** The drive scope: "Publish as artifact" and "Keep as artifact". */
  readonly canPublish: boolean;
  /** The admin scope: "Share…" (the deck also needs the owner's login). */
  readonly canShare: boolean;
  list(filter?: ArtifactFilter): Promise<DeckArtifact[]>;
  detail(station: string, id: string): Promise<DeckArtifactDetail>;
  /** A one-hour link for the sandboxed frame. `null`: the latest version. */
  view(station: string, id: string, version: number | null): Promise<ViewLink>;
  /** The text of one file of a version, from the raw route. */
  text(station: string, id: string, version: number, path: string): Promise<string>;
  publish(request: ArtifactPublishRequest): Promise<ArtifactPublished>;
  share(station: string, id: string, request: ShareRequest): Promise<ShareLink>;
  revoke(station: string, id: string, shareId: string): Promise<void>;
};

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

function shaped<T>(v: unknown, fields: readonly string[], what: string): T {
  if (!record(v) || fields.some((f) => !(f in v))) {
    throw new Error(`The deck sent an invalid ${what}.`);
  }
  return v as T;
}

const seg = encodeURIComponent;

/** `/api/artifacts/<station>/<id>`: the base of one artifact's routes. */
const base = (station: string, id: string): string => `/api/artifacts/${seg(station)}/${seg(id)}`;

/** The raw route of one file of a version; every path segment is escaped. */
export const rawPath = (station: string, id: string, version: number, path: string): string =>
  `${base(station, id)}/${version}/raw/${path.split('/').map(seg).join('/')}`;

/** `?session=&station=&q=` of the list call, only the filters that are set. */
export function listPath(filter: ArtifactFilter = {}): string {
  const query = new URLSearchParams();
  if (filter.station) query.set('station', filter.station);
  if (filter.session) query.set('session', filter.session);
  if (filter.q?.trim()) query.set('q', filter.q.trim());
  const text = query.toString();
  return text === '' ? '/api/artifacts' : `/api/artifacts?${text}`;
}

const LIST_FIELDS = ['station_id', 'artifact_id', 'title', 'kind', 'latest_version', 'updated_at'];

/** The artifact calls (`/api/artifacts/...`) over a transport and the scopes of the login. */
export function artifactActions(io: ArtifactTransport, scopes: readonly string[]): ArtifactActions {
  return {
    canPublish: scopes.includes('drive'),
    canShare: scopes.includes('admin'),
    list: async (filter) => {
      const v = await io.get(listPath(filter));
      if (!Array.isArray(v)) throw new Error('The deck sent an invalid artifact list.');
      return v.map((row) => shaped<DeckArtifact>(row, LIST_FIELDS, 'artifact'));
    },
    detail: async (station, id) =>
      shaped<DeckArtifactDetail>(
        await io.get(base(station, id)),
        ['artifact', 'versions', 'shares'],
        'artifact',
      ),
    view: async (station, id, version) =>
      shaped<ViewLink>(
        await io.post(`${base(station, id)}/view`, version === null ? {} : { version }),
        ['url', 'expires_at'],
        'view link',
      ),
    text: (station, id, version, path) => io.text(rawPath(station, id, version, path)),
    publish: async (request) =>
      shaped<ArtifactPublished>(
        await io.post('/api/artifacts/publish', request),
        ['artifact', 'version', 'created'],
        'publish answer',
      ),
    share: async (station, id, request) =>
      shaped<ShareLink>(
        await io.post(`${base(station, id)}/shares`, request),
        ['url', 'share_id', 'expires_at'],
        'share link',
      ),
    revoke: async (station, id, shareId) => {
      await io.del(`${base(station, id)}/shares/${seg(shareId)}`);
    },
  };
}

async function failure(response: Response): Promise<ApiError> {
  let message = `Request failed (${response.status})`;
  try {
    const value: unknown = await response.json();
    if (record(value) && typeof value['message'] === 'string') message = value['message'];
  } catch {
    // The body is not JSON: the status text stays.
  }
  return new ApiError(response.status, message);
}

/**
 * `DELETE` and a text `GET` for the artifact routes: `Connection` has `get` (JSON) and `post` only.
 * A mutating call carries the CSRF token of the login, read when the call is made.
 */
export function artifactHttp(
  fetcher: Fetch,
  csrf: () => string | null,
  timeoutMs: number,
): Pick<ArtifactTransport, 'del' | 'text'> {
  const call = async (path: string, init: RequestInit): Promise<Response> => {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), timeoutMs);
    try {
      return await fetcher(path, {
        credentials: 'same-origin',
        cache: 'no-store',
        redirect: 'error',
        signal: controller.signal,
        ...init,
      });
    } finally {
      clearTimeout(timer);
    }
  };
  return {
    del: async (path) => {
      const token = csrf();
      const response = await call(path, {
        method: 'DELETE',
        headers: token === null ? {} : { 'X-CSRF-Token': token },
      });
      if (!response.ok) throw await failure(response);
      return response.status === 204 ? null : response.json();
    },
    text: async (path) => {
      const response = await call(path, { method: 'GET' });
      if (!response.ok) throw await failure(response);
      return response.text();
    },
  };
}
