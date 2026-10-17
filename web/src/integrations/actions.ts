// R3 (DL-164): the project calls of the board. The shapes are the deck's
// (`crates/fleetdeck-deck/src/http/{projects_routes,project_writes}.rs`).
import type {
  Asset,
  CheckFailure,
  ChecksFailing,
  CommentBody,
  GitSettingsAnswer,
  GitSettingsPut,
  IssueDetail,
  IssueSummary,
  PrCreateBody,
  PrDetail,
  PrGroups,
  Project,
  ProjectInput,
  ProjectPreflight,
  Prefer,
  ProviderKind,
  PushNowAnswer,
  ReviewBody,
  StartedSessions,
  StartSessions,
  TransitionBody,
  WriteResult,
} from '../gen';
import { ApiError } from '../runtime/api';
import type { IntegrationHttp } from './http';

type Get = (path: string) => Promise<unknown>;
type Post = (path: string, body: unknown) => Promise<unknown>;

/** The transport of the calls; `put` and `del` are not on `Connection`. */
export type IntegrationTransport = IntegrationHttp & { readonly get: Get; readonly post: Post };

/** The filters of the issue list. An empty one is left out of the query. */
export type IssueQuery = {
  /** A tracker id of the project, or `forge`; absent: the first tracker, else the forge. */
  readonly source?: string | undefined;
  readonly query?: string | undefined;
  readonly mine?: boolean | undefined;
  readonly status?: string | undefined;
};

/**
 * The answer of the save of the Git settings (design §17.1): the saved settings, or the refusal
 * of the deck. Another failure (network, 403, 404) throws.
 */
export type GitSaveResult =
  | { readonly kind: 'saved'; readonly answer: GitSettingsAnswer }
  /** 400 `invalid_git_settings`: `field` is the struct field that the deck names. */
  | { readonly kind: 'invalid'; readonly field: string | null; readonly message: string }
  /** 409 `stale_revision`: the settings changed since the board loaded them. */
  | { readonly kind: 'stale'; readonly message: string }
  /** 409 `rebind_required`: the remote URL cannot change while sync is on. */
  | { readonly kind: 'rebind'; readonly message: string };

/** 202 of the add and retry routes of a home (design §8.4). */
export type HomeStarted = { readonly generation: number; readonly attempt_id: string };

/** 200 of `PUT /api/projects/{id}/remote` (decision B9). */
export type RemoteChanged = {
  readonly remote_url: string;
  readonly remote_ssh_user: string | null;
  readonly repo: string;
  readonly remote_dest: string;
  readonly git_revision: number;
};

/** The answer of the save of the remote URL (design §8.4 "Change the remote"). */
export type RemoteSaveResult =
  | { readonly kind: 'saved'; readonly answer: RemoteChanged }
  /** 409 `rebind_needed`: the URL names another repository; the owner must confirm. */
  | { readonly kind: 'rebind_needed'; readonly message: string }
  /** 400 `invalid_git_settings` (field `remote_url`). */
  | { readonly kind: 'invalid'; readonly message: string };

/** The body of `POST /api/placement` (decision D3). */
export type PlacementBody = {
  readonly count: number;
  readonly requires: readonly string[];
  readonly prefer: readonly Prefer[];
  readonly spread: boolean;
  readonly provider?: ProviderKind;
  readonly project_id?: string;
};

/** `GET /api/fleet-git/prefs` (decision D6). */
export type FleetGitPrefs = { readonly drift_reference: string | null };

/** The texts of the push now errors that the design names (§5.8). */
const PUSH_NOW_TEXT: Readonly<Record<string, string>> = {
  not_synced: 'This session is not in a synced project.',
  held: 'A land or a move holds this session.',
  unknown_session: 'The station does not know this session.',
};

export type IntegrationActions = {
  /** The drive scope: sessions from tickets and every write to a system. */
  readonly canDrive: boolean;
  /** The admin scope: the projects themselves (the deck also needs the owner's login). */
  readonly canAdmin: boolean;
  projects(): Promise<Project[]>;
  createProject(input: ProjectInput): Promise<Project>;
  updateProject(id: string, input: ProjectInput): Promise<Project>;
  deleteProject(id: string): Promise<void>;
  preflight(id: string): Promise<ProjectPreflight>;
  issues(id: string, query?: IssueQuery): Promise<IssueSummary[]>;
  issue(id: string, key: string, source?: string): Promise<IssueDetail>;
  asset(id: string, source: string | undefined, url: string): Promise<Asset>;
  prs(id: string): Promise<PrGroups>;
  pr(id: string, number: number): Promise<PrDetail>;
  prChecks(id: string, number: number): Promise<CheckFailure[]>;
  startSessions(id: string, body: StartSessions): Promise<StartedSessions>;
  commentIssue(id: string, key: string, body: CommentBody): Promise<WriteResult>;
  moveIssue(id: string, key: string, body: TransitionBody): Promise<WriteResult>;
  commentPr(id: string, number: number, body: CommentBody): Promise<WriteResult>;
  reviewPr(id: string, number: number, body: ReviewBody): Promise<WriteResult>;
  openPr(id: string, body: PrCreateBody): Promise<WriteResult>;
  /** DL-176: the session belongs to the project (a project of the session's station). */
  assignSession(id: string, station: string, session: string): Promise<void>;
  /** DL-176: the session has no project, whatever its folder. */
  clearSessionProject(station: string, session: string): Promise<void>;
  /** DL-176: back to the automatic match by folder. */
  autoSessionProject(station: string, session: string): Promise<void>;
  /** FG-2 (§17.1): saves the Git settings (and the remote URL when it is given). */
  saveGit(id: string, body: GitSettingsPut): Promise<GitSaveResult>;
  /** FG-2 (§5.8): the push of the session is due at once. */
  pushNow(station: string, session: string): Promise<PushNowAnswer>;
  /** FG-5 (§8.4): adds a home of the project on a station; the station clones in the background. */
  addHome(id: string, station: string): Promise<HomeStarted>;
  /** FG-5 (§8.4): retries the failed home of a station. */
  retryHome(id: string, station: string): Promise<HomeStarted>;
  /** FG-5 (§8.4): removes the home of a station; no file is deleted. */
  removeHome(id: string, station: string): Promise<void>;
  /** FG-5 (§8.4): changes the remote URL; `rebind` confirms another repository. */
  setRemote(id: string, remoteUrl: string, rebind: boolean): Promise<RemoteSaveResult>;
  /** FG-6 (§9.3): the stations for `count` new sessions, best first. */
  place(body: PlacementBody): Promise<string[]>;
  /** FG-11 (§14.3): the preferences of Fleet Git (the config reference station). */
  fleetGitPrefs(): Promise<FleetGitPrefs>;
  /** FG-11 (§14.3): sets (or with `null` clears) the config reference station. */
  setDriftReference(station: string | null): Promise<FleetGitPrefs>;
};

/** A command id of 32 characters from `[0-9a-f]` (the deck accepts `[0-9A-Za-z_-]{1,64}`). */
function commandId(): string {
  return globalThis.crypto.randomUUID().replaceAll('-', '');
}

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

function shaped<T>(v: unknown, fields: readonly string[], what: string): T {
  if (!record(v) || fields.some((f) => !(f in v))) {
    throw new Error(`The deck sent an invalid ${what}.`);
  }
  return v as T;
}

const seg = encodeURIComponent;

/** `/api/projects/<id>`: the base of one project's routes. */
export const projectPath = (id: string): string => `/api/projects/${seg(id)}`;

/** `/api/sessions/<station>/<session>`: the base of the routes of one session. */
const sessionPath = (station: string, session: string): string =>
  `/api/sessions/${seg(station)}/${seg(session)}`;

/** `?source=&query=&mine=&status=` of the issue list, only the filters that are set. */
export function issuesPath(id: string, filter: IssueQuery = {}): string {
  const query = new URLSearchParams();
  if (filter.source) query.set('source', filter.source);
  if (filter.query?.trim()) query.set('query', filter.query.trim());
  if (filter.mine === true) query.set('mine', 'true');
  if (filter.status?.trim()) query.set('status', filter.status.trim());
  const text = query.toString();
  return text === '' ? `${projectPath(id)}/issues` : `${projectPath(id)}/issues?${text}`;
}

/** The route of one image of an issue body, through the station. */
export function assetPath(id: string, source: string | undefined, url: string): string {
  const query = new URLSearchParams();
  if (source) query.set('source', source);
  query.set('url', url);
  return `${projectPath(id)}/assets?${query.toString()}`;
}

function prefs(v: unknown): FleetGitPrefs {
  const o = shaped<{ drift_reference: unknown }>(v, ['drift_reference'], 'preferences answer');
  return { drift_reference: typeof o.drift_reference === 'string' ? o.drift_reference : null };
}

const PROJECT_FIELDS = ['project_id', 'name', 'station_id', 'path', 'trackers'];
const RESULT_FIELDS = ['url'];

/** The project calls (`/api/projects/...`) over a transport and the scopes of the login. */
export function integrationActions(
  io: IntegrationTransport,
  scopes: readonly string[],
): IntegrationActions {
  const result = async (call: Promise<unknown>): Promise<WriteResult> =>
    shaped<WriteResult>(await call, RESULT_FIELDS, 'write result');
  const project = async (call: Promise<unknown>): Promise<Project> =>
    shaped<Project>(await call, PROJECT_FIELDS, 'project');
  return {
    canDrive: scopes.includes('drive'),
    canAdmin: scopes.includes('admin'),
    projects: async () => {
      const v = shaped<{ projects: unknown }>(await io.get('/api/projects'), ['projects'], 'list');
      if (!Array.isArray(v.projects)) throw new Error('The deck sent an invalid project list.');
      return v.projects.map((p) => shaped<Project>(p, PROJECT_FIELDS, 'project'));
    },
    createProject: (input) => project(io.post('/api/projects', input)),
    updateProject: (id, input) => project(io.put(projectPath(id), input)),
    deleteProject: async (id) => {
      await io.del(projectPath(id));
    },
    preflight: async (id) =>
      shaped<ProjectPreflight>(
        await io.post(`${projectPath(id)}/preflight`, {}),
        ['checks'],
        'preflight answer',
      ),
    issues: async (id, query) => {
      const v = shaped<{ issues: unknown }>(
        await io.get(issuesPath(id, query)),
        ['issues'],
        'list',
      );
      if (!Array.isArray(v.issues)) throw new Error('The deck sent an invalid issue list.');
      return v.issues as IssueSummary[];
    },
    issue: async (id, key, source) =>
      shaped<IssueDetail>(
        await io.get(
          `${projectPath(id)}/issues/${seg(key)}${source ? `?source=${seg(source)}` : ''}`,
        ),
        ['summary', 'body_markdown', 'comments'],
        'issue',
      ),
    asset: async (id, source, url) =>
      shaped<Asset>(
        await io.get(assetPath(id, source, url)),
        ['content_type', 'data_base64'],
        'image',
      ),
    prs: async (id) =>
      shaped<{ groups: PrGroups }>(
        await io.get(`${projectPath(id)}/prs`),
        ['groups'],
        'pull request list',
      ).groups,
    pr: async (id, number) =>
      shaped<PrDetail>(
        await io.get(`${projectPath(id)}/prs/${number}`),
        ['summary', 'body_markdown', 'files', 'comments'],
        'pull request',
      ),
    prChecks: async (id, number) =>
      shaped<ChecksFailing>(
        await io.get(`${projectPath(id)}/prs/${number}/checks`),
        ['failures'],
        'check list',
      ).failures,
    startSessions: async (id, body) =>
      shaped<StartedSessions>(
        await io.post(`${projectPath(id)}/sessions`, body),
        ['started', 'failed'],
        'start answer',
      ),
    commentIssue: (id, key, body) =>
      result(io.post(`${projectPath(id)}/issues/${seg(key)}/comments`, body)),
    moveIssue: (id, key, body) =>
      result(io.post(`${projectPath(id)}/issues/${seg(key)}/transition`, body)),
    commentPr: (id, number, body) =>
      result(io.post(`${projectPath(id)}/prs/${number}/comments`, body)),
    reviewPr: (id, number, body) =>
      result(io.post(`${projectPath(id)}/prs/${number}/reviews`, body)),
    openPr: (id, body) => result(io.post(`${projectPath(id)}/prs`, body)),
    assignSession: async (id, station, session) => {
      await io.post(`${projectPath(id)}/sessions/assign`, { station, session_id: session });
    },
    clearSessionProject: async (station, session) => {
      await io.post(`${sessionPath(station, session)}/project/clear`, {});
    },
    autoSessionProject: async (station, session) => {
      await io.post(`${sessionPath(station, session)}/project/auto`, {});
    },
    saveGit: async (id, body) => {
      try {
        const answer = shaped<GitSettingsAnswer>(
          await io.put(`${projectPath(id)}/git`, body),
          ['git', 'git_revision'],
          'Git settings answer',
        );
        return { kind: 'saved', answer };
      } catch (e) {
        if (e instanceof ApiError) {
          if (
            e.code === 'invalid_git_settings' ||
            e.code === 'share_exposed' ||
            e.code === 'share_unverified'
          ) {
            return { kind: 'invalid', field: e.field, message: e.message };
          }
          if (e.code === 'stale_revision') return { kind: 'stale', message: e.message };
          if (e.code === 'rebind_required') return { kind: 'rebind', message: e.message };
        }
        throw e;
      }
    },
    addHome: async (id, station) =>
      shaped<HomeStarted>(
        await io.post(`${projectPath(id)}/homes`, { station, command_id: commandId() }),
        ['generation', 'attempt_id'],
        'home answer',
      ),
    retryHome: async (id, station) =>
      shaped<HomeStarted>(
        await io.post(`${projectPath(id)}/homes/${seg(station)}/retry`, {
          command_id: commandId(),
        }),
        ['generation', 'attempt_id'],
        'home answer',
      ),
    removeHome: async (id, station) => {
      await io.del(`${projectPath(id)}/homes/${seg(station)}`);
    },
    setRemote: async (id, remoteUrl, rebind) => {
      try {
        const answer = shaped<RemoteChanged>(
          await io.put(`${projectPath(id)}/remote`, {
            remote_url: remoteUrl,
            rebind,
            command_id: commandId(),
          }),
          ['remote_url', 'repo', 'git_revision'],
          'remote answer',
        );
        return { kind: 'saved', answer };
      } catch (e) {
        if (e instanceof ApiError) {
          if (e.code === 'rebind_needed') return { kind: 'rebind_needed', message: e.message };
          if (e.code === 'invalid_git_settings') return { kind: 'invalid', message: e.message };
        }
        throw e;
      }
    },
    place: async (body) => {
      const v = shaped<{ stations: unknown }>(
        await io.post('/api/placement', body),
        ['stations'],
        'placement answer',
      );
      if (!Array.isArray(v.stations) || v.stations.some((x) => typeof x !== 'string')) {
        throw new Error('The deck sent an invalid placement answer.');
      }
      return v.stations as string[];
    },
    fleetGitPrefs: async () => prefs(await io.get('/api/fleet-git/prefs')),
    setDriftReference: async (station) =>
      prefs(await io.put('/api/fleet-git/drift-reference', { station })),
    pushNow: async (station, session) => {
      try {
        return shaped<PushNowAnswer>(
          await io.post(`${sessionPath(station, session)}/push`, {}),
          ['state'],
          'push answer',
        );
      } catch (e) {
        const text = e instanceof ApiError && e.code !== null ? PUSH_NOW_TEXT[e.code] : undefined;
        if (text !== undefined) throw new Error(text);
        throw e;
      }
    },
  };
}
