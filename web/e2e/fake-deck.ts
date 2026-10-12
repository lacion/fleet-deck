// A scripted board API for browser tests of UI interactions (DL-059). The
// page talks to it through Playwright request and WebSocket routing, so the
// built board runs unchanged. The real deck/station flows of P4-5 use real
// processes instead.
import { expect, type Page, type Request, type WebSocketRoute } from '@playwright/test';
import type { DeckCommand, DeckSession, EventEnvelope, StationView } from '../src/gen';

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

export const CSRF = 'csrf-e2e';
export const PASSWORD = 'correct horse battery staple';

export class FakeDeck {
  stations: StationView[] = [];
  sessions: DeckSession[] = [];
  scopes: string[] = ['read', 'drive', 'approve', 'admin'];
  calls: Call[] = [];
  private version = 1;
  private sockets: WebSocketRoute[] = [];
  private signedIn = false;
  private handlers = new Map<string, Handler>();
  /** Timeline events by `station/session`; `station_seq` must grow. */
  private timelines = new Map<string, EventEnvelope[]>();
  private subscribed = new Map<WebSocketRoute, Set<string>>();

  /** Answers `path` with `handler` (the default is 404). A POST needs the
   * CSRF token, like the real deck. */
  on(path: string, handler: Handler): void {
    this.handlers.set(path, handler);
  }

  async install(page: Page): Promise<void> {
    await page.route('**/api/**', async (route) => {
      const request = route.request();
      const reply = this.answer(request);
      await route.fulfill({
        status: reply.status,
        contentType: 'application/json',
        body: JSON.stringify(reply.body),
      });
    });
    await page.routeWebSocket('**/api/ws', (ws) => {
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

  private answer(request: Request): Reply {
    const url = new URL(request.url());
    const path = url.pathname;
    const body: unknown = request.method() === 'POST' ? request.postDataJSON() : null;
    this.calls.push({ path, body, csrf: request.headers()['x-csrf-token'] ?? null });
    const login = {
      actor: 'owner',
      scopes: this.scopes,
      csrf: CSRF,
      expires_at: Date.now() + 3_600_000,
    };
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
    const handler = this.handlers.get(path);
    if (handler === undefined) return { status: 404, body: { code: 'not_found', message: path } };
    if (request.method() === 'POST' && request.headers()['x-csrf-token'] !== CSRF) {
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

/** The phone's bottom bar: the Fleet page. */
export async function openFleet(page: Page): Promise<void> {
  await page
    .getByRole('navigation', { name: 'Board' })
    .getByRole('link', { name: 'Fleet' })
    .click();
  await expect(page).toHaveURL(/#\/fleet$/);
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
