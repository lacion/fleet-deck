// A scripted board API for browser tests of UI interactions (DL-059). The
// page talks to it through Playwright request and WebSocket routing, so the
// built board runs unchanged. The real deck/station flows of P4-5 use real
// processes instead.
import { expect, type Page, type Request, type WebSocketRoute } from '@playwright/test';
import type {
  AppOpenCall,
  DeckCommand,
  DeckSession,
  DisplayEvent,
  EventEnvelope,
  NativePreview,
  NativeSessionList,
  ReviewComment,
  ReviewSummary,
  StationView,
} from '../src/gen';
import { anchorText, parseDiff } from '../src/review/diffModel';

export type Reply = { status: number; body: unknown };
export type Call = { path: string; body: unknown; csrf: string | null };
/** `body` is the JSON body of a POST; `query` holds the URL parameters. */
type Handler = (body: unknown, query: URLSearchParams) => Reply;

/** One shell of a station, as the shell list reports it. */
export type ShellRow = {
  shell_id: string;
  cwd: string;
  state: 'running' | 'exited';
  cols: number;
  rows: number;
  created_at: number;
  exit_code: number | null;
  session_id?: string;
};

/** The body of a POST to a station's shells (the `change` is what counts). */
export type ShellPost = {
  command_id: string;
  change: { op: string; shell_id: string; session_id?: string; cols?: number; rows?: number };
};

/** A route with a path pattern (an id in the path) and a method; `match` holds the groups. */
type Route = {
  method: string;
  re: RegExp;
  handler: (match: RegExpExecArray, body: unknown, query: URLSearchParams) => Reply;
};

/** The review state that `FakeDeck.review` keeps in memory. */
export type FakeReview = {
  comments: ReviewComment[];
  reviews: ReviewSummary[];
  /** The bodies of the POSTs to `…/review/comments`, `…/review/send` and so on. */
  posted: { path: string; body: unknown }[];
};

/** The tailnet and pairing state that `FakeDeck.tailnet` keeps in memory (DL-178). */
export type FakeTailnet = {
  /** `GET /api/tailnet` answers this; "Use tailnet" fills `current` from `managed`. */
  view: {
    available: boolean;
    reason?: string;
    program?: 'configured' | 'path' | 'macos_app' | 'windows';
    self?: { dns_name: string; ips: string[] };
    userspace: boolean;
    managed: { listen: string[]; link_listen: string[] };
    current: { listen: string[]; link_listen: string[] };
    auto: {
      enabled: boolean;
      start_reason?: string;
      skipped: { address: string; reason: string }[];
      changed: boolean;
    };
    /** The trusted certificate from Tailscale (DL-190); absent: an older deck. */
    certificate?: {
      enabled: boolean;
      state: 'off' | 'valid' | 'renewing' | 'failed' | 'expired' | 'name_mismatch';
      name?: string;
      not_after?: number;
      reason_code?: string;
      owner_files?: boolean;
    };
  };
  windowOpen: boolean;
  generation: number;
  devices: {
    host_name: string;
    dns_name: string;
    ips: string[];
    os?: string;
    tags: string[];
    online: boolean;
    state: 'paired' | 'request waiting' | 'none';
  }[];
  requests: {
    request_id: string;
    requested_name: string;
    station_fingerprint: string;
    offered_scopes: string[];
    source_ip: string;
    admitted_at: number;
    expires_at: number;
    digest: string;
    tailscale: null | { host_name?: string; login?: string; tags: string[] };
  }[];
  /** The bodies of the POSTs, with the path. */
  posted: { path: string; body: unknown }[];
};

/** The elevation that the fake deck's `/api/auth/elevate` gives. */
export const ELEVATION = 'elevation-e2e';

const SHORT = 'abcdefghijkmnpqrstuvwxyz23456789';
const shortId = (prefix: string, n: number): string => {
  let id = '';
  let rest = n + 1000;
  for (let i = 0; i < 6; i += 1) {
    id = SHORT.charAt(rest % 32) + id;
    rest = Math.floor(rest / 32);
  }
  return `${prefix}-${id}`;
};

export const CSRF = 'csrf-e2e';
export const PASSWORD = 'correct horse battery staple';

export class FakeDeck {
  stations: StationView[] = [];
  sessions: DeckSession[] = [];
  scopes: string[] = ['read', 'drive', 'approve', 'admin'];
  /** The `deck_version` of the login answers; `null` leaves the field out (an older deck). */
  deckVersion: string | null = null;
  calls: Call[] = [];
  private version = 1;
  private sockets: WebSocketRoute[] = [];
  private signedIn = false;
  /** R13 P2c: the wall display page signs in with a display cookie, not the owner password. */
  displayIn = false;
  /** The wall display page gets `display_timeline_events` frames in place of `timeline_events`. */
  displayFrames = false;
  private displayTimelines = new Map<string, DisplayEvent[]>();
  private handlers = new Map<string, Handler>();
  private routes: Route[] = [];
  /** Timeline events by `station/session`; `station_seq` must grow. */
  private timelines = new Map<string, EventEnvelope[]>();
  private subscribed = new Map<WebSocketRoute, Set<string>>();
  /** Requests to `path` wait for the release of their hold before the answer is sent. */
  private holds = new Map<string, Promise<void>>();

  /** Answers `path` with `handler` (the default is 404). A POST needs the
   * CSRF token, like the real deck. */
  on(path: string, handler: Handler): void {
    this.handlers.set(path, handler);
  }

  /** Answers the requests whose method is `method` and whose path matches `re`. */
  route(method: string, re: RegExp, handler: Route['handler']): void {
    this.routes.push({ method, re, handler });
  }

  /**
   * The review routes of one session (DL-179), with the comments in memory: list, add, edit,
   * delete and send. `patch` is the Since-spawn diff; the deck reads each anchor from it, as the
   * real deck does. The caller is the owner.
   */
  review(stationId: string, sessionId: string, patch: string): FakeReview {
    const state: FakeReview = { comments: [], reviews: [], posted: [] };
    const base = `/api/sessions/${stationId}/${sessionId}/review`;
    let counter = 0;
    const parsed = parseDiff(patch);
    const threads = () => {
      const ids = [...new Set(state.comments.map((c) => c.thread_id))];
      return ids.map((id) => ({
        thread_id: id,
        comments: state.comments
          .filter((c) => c.thread_id === id)
          .sort((a, b) => a.created_at - b.created_at),
      }));
    };
    this.on(base, () => ({
      status: 200,
      body: {
        threads: threads(),
        reviews: state.reviews,
        rev: state.reviews.length,
        more_reviews: false,
      },
    }));
    this.on(`${base}/comments`, (raw) => {
      state.posted.push({ path: `${base}/comments`, body: raw });
      const b = raw as {
        snapshot: string;
        path: string;
        side: 'new' | 'old';
        line_start: number;
        line_end: number;
        body: string;
        thread_id?: string;
      };
      const text = anchorText(parsed, b.side, b.path, b.line_start, b.line_end);
      if (b.thread_id === undefined && text === null) {
        return {
          status: 422,
          body: { error: 'not_in_diff', message: 'The lines are not in the diff.' },
        };
      }
      counter += 1;
      const id = `01J${String(counter).padStart(23, '0')}`;
      const reply = b.thread_id !== undefined;
      const comment: ReviewComment = {
        comment_id: id,
        short_id: shortId('c', counter),
        thread_id: b.thread_id ?? id,
        snapshot: b.snapshot,
        path: b.path,
        side: b.side,
        line_start: b.line_start,
        line_end: b.line_end,
        anchor_text: reply
          ? (state.comments.find((c) => c.comment_id === b.thread_id)?.anchor_text ?? '')
          : (text ?? ''),
        anchor_kind: 'diff',
        body: b.body,
        author_kind: 'owner',
        state: 'draft',
        resolved: false,
        created_at: counter,
        updated_at: Date.now(),
      };
      state.comments.push(comment);
      return { status: 201, body: comment };
    });
    this.route('PATCH', new RegExp(`^${base}/comments/([^/]+)$`), (m, raw) => {
      const found = state.comments.find((c) => c.comment_id === m[1]);
      if (found === undefined)
        return { status: 404, body: { error: 'not_found', message: 'No such comment.' } };
      const patchBody = raw as { body?: string; resolved?: boolean };
      if (patchBody.body !== undefined) found.body = patchBody.body;
      if (patchBody.resolved !== undefined) found.resolved = patchBody.resolved;
      return { status: 200, body: found };
    });
    this.route('DELETE', new RegExp(`^${base}/comments/([^/]+)$`), (m) => {
      state.comments = state.comments.filter((c) => c.comment_id !== m[1]);
      return { status: 204, body: null };
    });
    this.on(`${base}/send`, (raw) => {
      state.posted.push({ path: `${base}/send`, body: raw });
      const b = raw as { send_key: string; comment_ids: string[] };
      const review_id = `01R${String(state.reviews.length + 1).padStart(23, '0')}`;
      const picked = state.comments.filter((c) => b.comment_ids.includes(c.comment_id));
      for (const c of picked) {
        c.state = 'sent';
        c.review_id = review_id;
      }
      const summary: ReviewSummary = {
        review_id,
        short_id: shortId('r', state.reviews.length + 1),
        principal: 'owner',
        delivery: 'delivered',
        attempts: 1,
        sent_at: Date.now(),
        comment_ids: picked.map((c) => c.short_id),
        command_id: b.send_key,
      };
      state.reviews.unshift(summary);
      return { status: 200, body: summary };
    });
    return state;
  }

  /**
   * The tailnet and pairing routes (DL-178) with the state in memory, and the elevation route of
   * the settings (the password of `PASSWORD` gives `ELEVATION`). A deck with no Tailscale
   * detail is not covered here: set `state.view.available`. The caller is the owner.
   */
  tailnet(initial: Partial<FakeTailnet> = {}): FakeTailnet {
    const state: FakeTailnet = {
      view: {
        available: true,
        self: { dns_name: 'deck.tail1234.ts.net', ips: ['100.64.0.1'] },
        userspace: false,
        managed: { listen: ['100.64.0.1:7420'], link_listen: ['100.64.0.1:7421'] },
        current: { listen: ['127.0.0.1:7420'], link_listen: ['127.0.0.1:7421'] },
        // The key is absent from deck.toml: the mode is on (design 15.2).
        auto: { enabled: true, skipped: [], changed: false },
      },
      windowOpen: false,
      generation: 0,
      devices: [],
      requests: [],
      posted: [],
      ...initial,
    };
    const window = () => ({
      open: state.windowOpen,
      generation: state.generation,
      deadline_ms: state.windowOpen ? Date.now() + 10 * 60_000 : null,
    });
    this.on('/api/auth/elevate', (raw) => {
      const ok = (raw as { password?: unknown } | null)?.password === PASSWORD;
      return ok
        ? { status: 200, body: { elevation: ELEVATION, expires_at: Date.now() + 300_000 } }
        : { status: 401, body: { code: 'bad_password', message: 'The password is wrong.' } };
    });
    this.on('/api/settings/deck', (raw) => {
      if (raw === null) {
        return {
          status: 200,
          body: { values: {}, fields: [], revision: 'rev-e2e', overrides: [] },
        };
      }
      // A save of one key from the Tailnet card: turning `tailnet_auto` on
      // needs the elevation, as in the real deck.
      state.posted.push({ path: '/api/settings/deck', body: raw });
      const b = raw as { changes?: Record<string, unknown>; elevation?: unknown };
      const changes = b.changes ?? {};
      const needs = changes['tailnet_auto'] === true || changes['tailscale_cert'] === true;
      if (needs && b.elevation !== ELEVATION) {
        return {
          status: 400,
          body: { error: 'elevation_required', message: 'Confirm it is you.', keys: [] },
        };
      }
      if (typeof changes['tailnet_auto'] === 'boolean') {
        state.view.auto.enabled = changes['tailnet_auto'];
      }
      if (typeof changes['tailscale_cert'] === 'boolean') {
        // The setting is live: the certificate task starts at once.
        state.view.certificate = changes['tailscale_cert']
          ? { enabled: true, state: 'renewing' }
          : { enabled: false, state: 'off' };
      }
      return {
        status: 200,
        body: {
          values: {},
          applied: [],
          restart_needed: Object.keys(changes),
          unapplied: [],
          revision: 'rev-e2e-2',
          durability: 'confirmed',
        },
      };
    });
    this.on('/api/tailnet', () => ({ status: 200, body: state.view }));
    this.on('/api/tailnet/use', (raw) => {
      state.posted.push({ path: '/api/tailnet/use', body: raw });
      if ((raw as { elevation?: unknown } | null)?.elevation !== ELEVATION) {
        return { status: 400, body: { code: 'elevation_required', message: 'Confirm it is you.' } };
      }
      state.view.current = {
        listen: [...state.view.current.listen, ...state.view.managed.listen],
        link_listen: [...state.view.current.link_listen, ...state.view.managed.link_listen],
      };
      return {
        status: 200,
        body: { saved: true, restart_needed: true, ...state.view.managed },
      };
    });
    this.on('/api/tailnet/devices', () => ({
      status: 200,
      body: {
        available: state.view.available,
        ...(state.view.reason === undefined ? {} : { reason: state.view.reason }),
        devices: state.devices.map((d) => ({
          ...d,
          command:
            state.windowOpen && d.state === 'none'
              ? `fleetdeck station join --tailnet ${state.view.self?.dns_name ?? ''}`
              : null,
        })),
      },
    }));
    this.on('/api/pairing/window', (raw) => {
      if (raw === null) return { status: 200, body: window() };
      state.posted.push({ path: '/api/pairing/window', body: raw });
      const b = raw as { open?: boolean; elevation?: unknown };
      if (b.open === true) {
        if (b.elevation !== ELEVATION) {
          return {
            status: 400,
            body: { code: 'elevation_required', message: 'Confirm it is you.' },
          };
        }
        if (!state.windowOpen) state.generation += 1;
        state.windowOpen = true;
      } else {
        state.windowOpen = false;
        state.requests = [];
      }
      return { status: 200, body: window() };
    });
    // BG-3: the setup-report window stays closed and lists no report.
    this.on('/api/provision-reports', () => ({
      status: 200,
      body: { window: { open: false, deadline_ms: null }, reports: [] },
    }));
    this.on('/api/pairing/requests', () => ({
      status: 200,
      body: { window: window(), requests: state.windowOpen ? state.requests : [] },
    }));
    this.route('POST', /^\/api\/pairing\/requests\/([^/]+)\/approve$/, (m, raw) => {
      state.posted.push({ path: m[0], body: raw });
      const b = raw as { fingerprint?: string; digest?: string; elevation?: unknown };
      if (b.elevation !== ELEVATION) {
        return { status: 400, body: { code: 'elevation_required', message: 'Confirm it is you.' } };
      }
      const found = state.requests.find((r) => r.request_id === decodeURIComponent(m[1] ?? ''));
      if (found === undefined) {
        return { status: 404, body: { code: 'unknown_request', message: 'No such request.' } };
      }
      if (b.fingerprint !== found.station_fingerprint || b.digest !== found.digest) {
        return { status: 409, body: { code: 'digest_mismatch', message: 'The request changed.' } };
      }
      state.requests = state.requests.filter((r) => r !== found);
      return { status: 200, body: { approved: true, station_id: 'st-new' } };
    });
    this.route('POST', /^\/api\/pairing\/requests\/([^/]+)\/deny$/, (m, raw) => {
      state.posted.push({ path: m[0], body: raw });
      state.requests = state.requests.filter(
        (r) => r.request_id !== decodeURIComponent(m[1] ?? ''),
      );
      return { status: 200, body: { denied: true } };
    });
    return state;
  }

  /**
   * Holds the answers to `path` (a POST or a GET) until the returned function is called, so that
   * a test sees the state of the board while the request is pending. The handler still runs at once.
   */
  hold(path: string): () => void {
    let release = () => {};
    this.holds.set(
      path,
      new Promise<void>((resolve) => {
        release = resolve;
      }),
    );
    return release;
  }

  /** `firstRun: true` keeps the help notice of a first visit (every other test has seen it). */
  async install(page: Page, options: { firstRun?: boolean } = {}): Promise<void> {
    if (options.firstRun !== true) {
      await page.addInitScript(() => {
        try {
          localStorage.setItem('fd-help-seen', '1');
        } catch {
          // The help notice may show; the test sees it.
        }
      });
    }
    await page.route('**/api/**', async (route) => {
      const request = route.request();
      const reply = this.answer(request);
      await this.holds.get(new URL(request.url()).pathname);
      await route.fulfill({
        status: reply.status,
        contentType: 'application/json',
        ...(reply.status === 204 ? {} : { body: JSON.stringify(reply.body) }),
      });
    });
    // The board adds `?features=` to the socket URL (R13 P5b §14); a glob must match the query too.
    await page.routeWebSocket(/\/api\/ws(\?|$)/, (ws) => {
      this.sockets.push(ws);
      this.subscribed.set(ws, new Set());
      ws.onMessage((data) => {
        const msg = JSON.parse(String(data)) as {
          type: string;
          station_id: string;
          session_id: string;
          after: number | null;
        };
        const id = `${msg.station_id}/${msg.session_id}`;
        const subs = this.subscribed.get(ws);
        if (msg.type === 'unsubscribe') {
          subs?.delete(id);
          return;
        }
        if (msg.type !== 'subscribe') return;
        subs?.add(id);
        const { station_id, session_id } = msg;
        ws.send(
          JSON.stringify({
            type: 'timeline_start',
            station_id,
            session_id,
            incarnation: 'inc-e2e',
            reset: false,
          }),
        );
        if (this.displayFrames) {
          const shown = (this.displayTimelines.get(id) ?? []).filter(
            (e) => e.station_seq > (msg.after ?? 0),
          );
          ws.send(
            JSON.stringify({
              type: 'display_timeline_events',
              station_id,
              session_id,
              events: shown,
              high_water: shown.at(-1)?.station_seq ?? msg.after ?? 0,
            }),
          );
          return;
        }
        const events = (this.timelines.get(id) ?? []).filter(
          (e) => e.station_seq > (msg.after ?? 0),
        );
        ws.send(JSON.stringify({ type: 'timeline_events', station_id, session_id, events }));
      });
      ws.send(JSON.stringify({ type: 'hello', protocol: 1, deck_incarnation: 'deck-e2e' }));
      ws.send(
        JSON.stringify({
          type: 'snapshot',
          version: this.version,
          stations: this.stations,
          sessions: this.sessions,
        }),
      );
    });
  }

  /** Replaces one session (same station and id) and pushes it to the board sockets. */
  setSession(next: DeckSession): void {
    const same = (d: DeckSession) =>
      d.station_id === next.station_id && d.summary.session_id === next.summary.session_id;
    this.sessions = this.sessions.some(same)
      ? this.sessions.map((d) => (same(d) ? next : d))
      : [...this.sessions, next];
    this.publish();
  }

  /**
   * The shells of one station: GET lists `rows`, a POST is kept in `posts` and
   * answered as accepted, after `onPost` ran (it may change `rows` or push a session).
   */
  shells(stationId: string, onPost: (post: ShellPost) => void = () => {}) {
    const state = { rows: [] as ShellRow[], posts: [] as ShellPost[] };
    this.on(`/api/stations/${stationId}/shells`, (body) => {
      if (body === null) return { status: 200, body: state.rows };
      const post = body as ShellPost;
      state.posts.push(post);
      onPost(post);
      return { status: 200, body: { state: 'accepted' } };
    });
    return state;
  }

  /** Answers a session command as accepted; `onCall` sees its `command` body first. */
  accept(method: string, stationId: string, onCall: (command: unknown) => void = () => {}): void {
    this.on(`/api/commands/${method}`, (body) => {
      const b = body as { command_id: string; command: unknown };
      onCall(b.command);
      const done: DeckCommand = {
        command_id: b.command_id,
        station_id: stationId,
        method,
        origin: 'board',
        envelope: null,
        retry_until: 0,
        state: 'done',
        attempts: 1,
        sent_at: 1,
        receipt: {
          command_id: b.command_id,
          state: 'accepted',
          station_seq: null,
          result: null,
          reject: null,
          at: 1,
        },
        reason: null,
        created_at: 1,
        updated_at: 1,
      };
      return { status: 200, body: done };
    });
  }

  /** Sends the current stations and sessions to every open board socket. */
  publish(): void {
    this.version += 1;
    const delta = JSON.stringify({
      type: 'delta',
      version: this.version,
      stations: this.stations,
      sessions: this.sessions,
      removed: [],
    });
    for (const ws of this.sockets) ws.send(delta);
  }

  /** Appends timeline events and sends them to the subscribed sockets. */
  emit(stationId: string, sessionId: string, events: EventEnvelope[]): void {
    const id = `${stationId}/${sessionId}`;
    this.timelines.set(id, [...(this.timelines.get(id) ?? []), ...events]);
    const msg = JSON.stringify({
      type: 'timeline_events',
      station_id: stationId,
      session_id: sessionId,
      events,
    });
    for (const [ws, subs] of this.subscribed) if (subs.has(id)) ws.send(msg);
  }

  /** R13 P2c: appends display timeline events and sends them to the subscribed sockets. */
  emitDisplay(stationId: string, sessionId: string, events: DisplayEvent[]): void {
    const id = `${stationId}/${sessionId}`;
    this.displayTimelines.set(id, [...(this.displayTimelines.get(id) ?? []), ...events]);
    const msg = JSON.stringify({
      type: 'display_timeline_events',
      station_id: stationId,
      session_id: sessionId,
      events,
      high_water: events.at(-1)?.station_seq ?? 0,
    });
    for (const [ws, subs] of this.subscribed) if (subs.has(id)) ws.send(msg);
  }

  /**
   * B7: the native-sessions routes of one station. The list is the answer of the GET; the preview
   * answers every POST. `previews` holds the bodies of the POSTs (the pin included).
   */
  nativeSessions(
    stationId: string,
    list: NativeSessionList,
    preview: NativePreview,
  ): { previews: unknown[] } {
    const state = { previews: [] as unknown[] };
    this.on(`/api/stations/${stationId}/native-sessions`, () => ({ status: 200, body: list }));
    this.on(`/api/stations/${stationId}/native-sessions/preview`, (raw) => {
      state.previews.push(raw);
      return { status: 200, body: preview };
    });
    return state;
  }

  private answer(request: Request): Reply {
    const url = new URL(request.url());
    const path = url.pathname;
    const method = request.method();
    const body: unknown = method === 'POST' || method === 'PATCH' ? request.postDataJSON() : null;
    this.calls.push({ path, body, csrf: request.headers()['x-csrf-token'] ?? null });
    const login = {
      actor: 'owner',
      scopes: this.scopes,
      csrf: CSRF,
      expires_at: Date.now() + 3_600_000,
      ...(this.deckVersion === null ? {} : { deck_version: this.deckVersion }),
    };
    // R13 P2c: the display page names itself with a header; its routes need no CSRF token.
    const display = request.headers()['x-fleetdeck-display'] === '1';
    if (display && path === '/api/auth/session') {
      return this.displayIn
        ? {
            status: 200,
            body: {
              actor: 'display:Wall',
              scopes: ['read'],
              csrf: null,
              expires_at: Date.now() + 3_600_000,
            },
          }
        : { status: 401, body: { error: 'login_required', message: 'Log in.' } };
    }
    if (path === '/api/auth/session') {
      return this.signedIn
        ? { status: 200, body: login }
        : { status: 401, body: { code: 'unauthenticated', message: 'Log in.' } };
    }
    if (path === '/api/auth/login') {
      const ok = (body as { password?: unknown } | null)?.password === PASSWORD;
      this.signedIn = ok;
      return ok
        ? { status: 200, body: login }
        : { status: 401, body: { code: 'bad_password', message: 'The password is wrong.' } };
    }
    const csrf = request.headers()['x-csrf-token'] === CSRF;
    for (const r of this.routes) {
      const match = method === r.method ? r.re.exec(path) : null;
      if (match === null) continue;
      if (!csrf && !display) {
        return { status: 403, body: { code: 'csrf', message: 'Missing CSRF token.' } };
      }
      return r.handler(match, body, url.searchParams);
    }
    const handler = this.handlers.get(path);
    // DL-189: a session without earlier history, unless a test sets this path with `on`.
    if (
      handler === undefined &&
      method === 'GET' &&
      /^\/api\/sessions\/[^/]+\/[^/]+\/history$/.test(path)
    ) {
      return { status: 200, body: { items: [], before: null, state: { kind: 'empty' } } };
    }
    // DL-191: a repository without other branches, and a subagent that wrote nothing, unless a test sets the path with `on`.
    if (handler === undefined && method === 'GET') {
      if (/^\/api\/sessions\/[^/]+\/[^/]+\/branches$/.test(path)) {
        return {
          status: 200,
          body: { head: null, branch: 'main', branches: [], truncated: false },
        };
      }
      if (/^\/api\/sessions\/[^/]+\/[^/]+\/subagents\/[^/]+$/.test(path)) {
        return { status: 200, body: { items: [], state: { kind: 'empty' } } };
      }
    }
    // BG-1: a station build without the desktop tools; the card stays hidden.
    if (
      handler === undefined &&
      method === 'GET' &&
      /^\/api\/stations\/[^/]+\/(desktop-tools|maintenance)$/.test(path)
    ) {
      return { status: 501, body: { error: 'unsupported', message: 'Not supported.' } };
    }
    if (handler === undefined) return { status: 404, body: { code: 'not_found', message: path } };
    if (method === 'POST' && !csrf && !display) {
      return { status: 403, body: { code: 'csrf', message: 'Missing CSRF token.' } };
    }
    return handler(body, url.searchParams);
  }
}

export function stationView(id: string, name: string, extra: Partial<StationView> = {}) {
  const view: StationView = {
    station_id: id,
    name,
    key: `${id}-fingerprint`,
    direction: 'station_dials',
    dial_url: null,
    reachable: true,
    last_seen: 1_760_000_000_000,
    cursor: 12,
    info: null,
    info_at: null,
    ...extra,
  };
  return view;
}

export async function logIn(page: Page): Promise<void> {
  await page.goto('/');
  await page.getByLabel('Owner password').fill(PASSWORD);
  await page.getByRole('button', { name: 'Log in' }).click();
}

/** The page never scrolls sideways at the phone width. */
export async function fitsWidth(page: Page): Promise<boolean> {
  return page.evaluate(
    () => document.documentElement.scrollWidth <= document.documentElement.clientWidth,
  );
}

/**
 * Opens the "Add a device" dialog. The sidebar has it in the Account menu; the More page has it
 * as a button.
 */
export async function openAddDevice(page: Page): Promise<void> {
  const account = page.locator('summary[aria-label="Account"]').first();
  if (await account.isVisible()) await account.click();
  await page.getByRole('button', { name: 'Add a device' }).first().click();
}

/** The phone's bottom bar: the Fleet page. */
export async function openFleet(page: Page): Promise<void> {
  await page
    .getByRole('navigation', { name: 'Board' })
    .getByRole('link', { name: 'Fleet' })
    .click();
  await expect(page).toHaveURL(/#\/fleet$/);
}

/** A tab of the fleet home's ops panel (DESIGN-SYSTEM §4.4): Activity, Mail & tasks, Assign or Usage. */
export async function openPanel(
  page: Page,
  tab: 'Activity' | 'Mail & tasks' | 'Assign' | 'Usage',
): Promise<void> {
  await page
    .getByRole('tablist', { name: 'Fleet panels' })
    .getByRole('tab', { name: tab, exact: true })
    .click();
  await expect(
    page
      .getByRole('tablist', { name: 'Fleet panels' })
      .getByRole('tab', { name: tab, exact: true }),
  ).toHaveAttribute('aria-selected', 'true');
}

/** The phone's More page, then one of its links ("Plans" or "Stations"). */
export async function openMore(page: Page, link: 'Plans' | 'Stations'): Promise<void> {
  await page.getByRole('navigation', { name: 'Board' }).getByRole('link', { name: 'More' }).click();
  await page
    .getByRole('navigation', { name: 'More pages' })
    .getByRole('link', { name: link === 'Stations' ? /^Stations/ : link })
    .click();
  await expect(page).toHaveURL(link === 'Plans' ? /#\/plans$/ : /#\/stations$/);
}

/** A session row of the list (sidebar, or the phone's home), by its title. */
export async function openSession(page: Page, title: string): Promise<void> {
  const rows = page.locator('a.fd-srow').filter({ hasText: title });
  await rows.first().waitFor({ state: 'attached' });
  const ended = page.locator('details.fd-sended').filter({ has: rows });
  if ((await ended.count()) > 0) {
    await ended.locator('> summary').click();
  }
  await rows.first().click();
  await expect(page).toHaveURL(/#\/s\//);
}

/** A terminal stream that stays open and silent (the board attaches to a shell). */
export async function quietShells(page: Page): Promise<void> {
  await page.routeWebSocket('**/shells/*/attach', () => {});
}

// ---- B15 MCP Apps (design docs/v2/design/mcp-apps.md §4.7, §4.8) ----

/** The bootstrap that the deck puts first in an app document (`APP_BOOTSTRAP` of app_public.rs). */
export const APP_BOOTSTRAP = `<script>(()=>{"use strict";
try{void document.cookie}catch(e){if(e&&e.name==="SecurityError"){Object.defineProperty(document,"cookie",{configurable:true,get:()=>"",set:()=>{}})}}
const host=window.parent,ch=new MessageChannel(),port=ch.port1,send=MessagePort.prototype.postMessage;
port.postMessage=function(m,_o,t){send.call(port,m,Array.isArray(t)?t:[])};
Object.defineProperty(window,"parent",{configurable:true,enumerable:true,get:()=>port});
port.onmessage=(ev)=>{const d=ev.data;
const c=d&&((d.result&&d.result.hostContext)||(d.method==="ui/notifications/host-context-changed"&&d.params));
if(c&&(c.theme==="light"||c.theme==="dark"))document.documentElement.style.colorScheme=c.theme;
window.dispatchEvent(new MessageEvent("message",{data:d,source:port}))};
port.start();
host.postMessage({"fleet-deck-app-hello":1},"*",[ch.port2]);
})();</script>`;

/** The CSP of `GET /app/{token}` with no domains (§4.7). */
export const APP_CSP =
  "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; font-src data:; media-src data: blob:; connect-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'self'";

/** The CSP of an artifact page (`SANDBOX_CSP` of artifact_routes.rs). */
export const ARTIFACT_CSP =
  "sandbox allow-scripts allow-popups allow-forms allow-modals; default-src * data: blob: 'unsafe-inline' 'unsafe-eval'";

/** A small counter app: it initializes, then calls `bump` when its button is pressed. */
export const DEMO_APP = `<!doctype html><html><body style="margin:8px;font:14px sans-serif">
<p id="counter">Count: 0</p><button id="bump" type="button">Bump</button>
<script>
let n=0;const pending=new Map();
window.addEventListener("message",e=>{const d=e.data;if(d&&d.id!==undefined&&pending.has(d.id)){pending.get(d.id)(d);pending.delete(d.id)}});
const rpc=(method,params)=>new Promise(res=>{const id=++n;pending.set(id,res);window.parent.postMessage({jsonrpc:"2.0",id,method,params},"*")});
rpc("ui/initialize",{protocolVersion:"2026-01-26",appInfo:{name:"demo",version:"1"},appCapabilities:{}}).then(()=>{
window.parent.postMessage({jsonrpc:"2.0",method:"ui/notifications/initialized",params:{}},"*");
window.parent.postMessage({jsonrpc:"2.0",method:"ui/notifications/size-changed",params:{height:120}},"*")});
document.getElementById("bump").onclick=async()=>{const r=await rpc("tools/call",{name:"bump",arguments:{}});
document.getElementById("counter").textContent=r.error?"Error: "+r.error.message:"Count: "+r.result.content[0].text};
</script></body></html>`;

/** The app routes of one session and the documents that the fake deck serves. */
export type FakeApps = {
  readonly appId: string;
  /** The view token of the document: `GET /app/{token}`. */
  token: string;
  counter: number;
  /** The bodies of the POSTs of the call route. */
  calls: { request: { method: string; name?: string }; confirmed: boolean }[];
  /** What `GET …/app-calls` answers. */
  open: AppOpenCall[];
  /** The releases that were posted: the call's own session and the call id. */
  released: { session: string; callId: string }[];
  /** `/app/{token}` documents by token (the bootstrap goes in front of them). */
  documents: Record<string, string>;
  /** `/a/{path}` pages (the artifact CSP); `delay` holds the request of `/a/slow.png` for 3 s. */
  pages: Record<string, string>;
  /** The paths `/a/beacon?...` that a page requested, in order. */
  beacons: string[];
};

/** The prefix of `with_bootstrap` of app_public.rs: after a leading doctype, else in front. */
export function withBootstrap(html: string): string {
  const m = /^[\s\ufeff]*<!doctype[^>]*>/i.exec(html);
  const at = m === null ? 0 : m[0].length;
  return html.slice(0, at) + APP_BOOTSTRAP + html.slice(at);
}

/** Answers the app routes of `station/session`: the view, the call (confirm first), the list, the release. */
export function fakeApps(
  deck: FakeDeck,
  station: string,
  session: string,
  appId: string,
  open: AppOpenCall[] = [],
): FakeApps {
  const state: FakeApps = {
    appId,
    token: 'demo',
    counter: 0,
    calls: [],
    open,
    released: [],
    documents: { demo: DEMO_APP },
    pages: {},
    beacons: [],
  };
  const base = `^/api/sessions/${station}/${session}`;
  deck.route('GET', new RegExp(`${base}/apps/${appId}$`), () => ({
    status: 200,
    body: {
      url: `/app/${state.token}`,
      expires_at: Date.now() + 600_000,
      server: 'demo',
      tool: 'show_counter',
      resource_uri: 'ui://demo/counter',
      csp: { connect_domains: [], resource_domains: [] },
      prefers_border: true,
      tool_input: {},
      tool_result: { content: [{ type: 'text', text: String(state.counter) }] },
    },
  }));
  deck.route('POST', new RegExp(`${base}/apps/${appId}/call$`), (_m, raw) => {
    const body = raw as {
      request: { method: string; name?: string };
      confirmed: boolean;
    };
    state.calls.push({ request: body.request, confirmed: body.confirmed });
    if (!body.confirmed) {
      return { status: 200, body: { outcome: 'confirm', tool: 'bump', title: null } };
    }
    state.counter += 1;
    return {
      status: 200,
      body: {
        outcome: 'done',
        result: { content: [{ type: 'text', text: String(state.counter) }] },
      },
    };
  });
  deck.route('GET', new RegExp(`${base}/app-calls$`), () => ({
    status: 200,
    body: { calls: state.open },
  }));
  deck.route(
    'POST',
    new RegExp(`^/api/sessions/${station}/([^/]+)/app-calls/([^/]+)/release$`),
    (m) => {
      const callId = decodeURIComponent(m[2] ?? '');
      state.released.push({ session: decodeURIComponent(m[1] ?? ''), callId });
      state.open = state.open.filter((c) => c.call_id !== callId);
      return { status: 200, body: { released: true } };
    },
  );
  return state;
}

/**
 * Serves `/app/{token}` (the §4.7 headers, the bootstrap first) and `/a/{path}` (the artifact CSP)
 * of `apps` to `page`. A request of `/a/slow.png` waits for `release()`; `/a/beacon` is recorded.
 */
export async function installApps(page: Page, apps: FakeApps): Promise<{ release: () => void }> {
  let release = () => {};
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route(/^https?:\/\/[^/]+\/app\/[A-Za-z0-9_-]+$/, async (route) => {
    const token = new URL(route.request().url()).pathname.slice('/app/'.length);
    const html = apps.documents[token];
    if (html === undefined) return route.fulfill({ status: 404, body: 'not found' });
    return route.fulfill({
      status: 200,
      headers: {
        'content-type': 'text/html; charset=utf-8',
        'cache-control': 'no-store',
        'x-content-type-options': 'nosniff',
        'referrer-policy': 'no-referrer',
        'content-security-policy': APP_CSP,
      },
      body: withBootstrap(html),
    });
  });
  await page.route(/^https?:\/\/[^/]+\/a\//, async (route) => {
    const url = new URL(route.request().url());
    const path = url.pathname.slice('/a/'.length);
    if (path === 'slow.png') {
      await gate;
      return route.fulfill({ status: 404, body: 'late' });
    }
    if (path === 'beacon') {
      apps.beacons.push(url.search);
      return route.fulfill({ status: 204 });
    }
    const html = apps.pages[path];
    if (html === undefined) return route.fulfill({ status: 404, body: 'not found' });
    return route.fulfill({
      status: 200,
      headers: {
        'content-type': 'text/html; charset=utf-8',
        'content-security-policy': ARTIFACT_CSP,
      },
      body: html,
    });
  });
  return { release };
}
