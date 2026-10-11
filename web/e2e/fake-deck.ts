// A scripted board API for browser tests of UI interactions (DL-059). The
// page talks to it through Playwright request and WebSocket routing, so the
// built board runs unchanged. The real deck/station flows of P4-5 use real
// processes instead.
import type { Page, Request, WebSocketRoute } from '@playwright/test';
import type { DeckSession, EventEnvelope, StationView } from '../src/gen';

export type Reply = { status: number; body: unknown };
export type Call = { path: string; body: unknown; csrf: string | null };
/** `body` is the JSON body of a POST; `query` holds the URL parameters. */
type Handler = (body: unknown, query: URLSearchParams) => Reply;

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
