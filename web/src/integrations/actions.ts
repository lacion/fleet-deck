// R3 (DL-164): the project calls of the board. The shapes are the deck's
// (`crates/fleetdeck-deck/src/http/{projects_routes,project_writes}.rs`).
import type {
  Asset,
  CheckFailure,
  ChecksFailing,
  CommentBody,
  IssueDetail,
  IssueSummary,
  PrCreateBody,
  PrDetail,
  PrGroups,
  Project,
  ProjectInput,
  ProjectPreflight,
  ReviewBody,
  StartedSessions,
  StartSessions,
  TransitionBody,
  WriteResult,
} from '../gen';
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

/** `/api/projects/<id>`: the base of one project's routes. */
export const projectPath = (id: string): string => `/api/projects/${seg(id)}`;

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
  };
}
