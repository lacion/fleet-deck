// B15 T7: the host bridge of one app frame (design §4.8). The window, the frame, the port and the
// timers are fakes.
import { describe, expect, test } from 'bun:test';
import type { AppCallReply, AppRequest, AppViewReply } from '../gen';
import { ApiError } from '../runtime/api';
import {
  type Bridge,
  type BridgeEnv,
  type CloseReason,
  CHAT_POLICY,
  type HelloEvent,
  type PortLike,
  createBridge,
} from './bridge';

type Posted = Record<string, unknown>;

class FakePort implements PortLike {
  onmessage: ((ev: { data: unknown }) => void) | null = null;
  posted: Posted[] = [];
  started = false;
  closed = false;
  postMessage(message: unknown): void {
    this.posted.push(message as Posted);
  }
  start(): void {
    this.started = true;
  }
  close(): void {
    this.closed = true;
  }
  /** The app sends a message to the board. */
  deliver(data: unknown): void {
    this.onmessage?.({ data });
  }
}

const flush = async (): Promise<void> => {
  for (let i = 0; i < 8; i += 1) await Promise.resolve();
};

const VIEW: AppViewReply = {
  url: '/app/tok',
  expires_at: 1,
  server: 'demo',
  tool: 'show_counter',
  resource_uri: 'ui://demo/counter',
  csp: { connect_domains: ['api.example.com'], resource_domains: [] },
  prefers_border: null,
  tool_input: { start: 1 },
  tool_result: { content: [{ type: 'text', text: 'ok' }], _meta: null },
};

type Rig = {
  bridge: Bridge;
  port: FakePort;
  frameWindow: { posted: unknown[]; postMessage(m: unknown): void };
  closes: CloseReason[];
  calls: { request: AppRequest; confirmed: boolean }[];
  refreshed: number;
  dialogs: string[];
  sent: string[];
  opened: string[];
  heights: number[];
  errorCodes: (string | null)[];
  emit(ev: Partial<HelloEvent>): void;
  load(): void;
  advance(ms: number): void;
  hello(): void;
  rpc(id: number | string, method: string, params?: unknown): Promise<Posted | undefined>;
  initialize(): Promise<void>;
};

function rig(over: Partial<BridgeEnv> = {}, view: AppViewReply = VIEW): Rig {
  const frameWindow = {
    posted: [] as unknown[],
    postMessage(m: unknown) {
      this.posted.push(m);
    },
  };
  let winListener: ((ev: HelloEvent) => void) | null = null;
  const loadListeners = new Set<() => void>();
  const timerList: { at: number; fn: () => void; live: boolean }[] = [];
  let now = 0;
  const port = new FakePort();
  const state = {
    closes: [] as CloseReason[],
    calls: [] as { request: AppRequest; confirmed: boolean }[],
    refreshed: 0,
    dialogs: [] as string[],
    sent: [] as string[],
    opened: [] as string[],
    heights: [] as number[],
    errorCodes: [] as (string | null)[],
  };
  const env: BridgeEnv = {
    win: {
      addEventListener: (_t, l) => {
        winListener = l;
      },
      removeEventListener: () => {
        winListener = null;
      },
    },
    frame: {
      contentWindow: frameWindow,
      addEventListener: (_t, l) => loadListeners.add(l),
      removeEventListener: (_t, l) => loadListeners.delete(l),
    },
    timers: {
      set: (fn, ms) => {
        const t = { at: now + ms, fn, live: true };
        timerList.push(t);
        return t;
      },
      clear: (h) => {
        (h as { live: boolean }).live = false;
      },
    },
    policy: CHAT_POLICY,
    view,
    version: '9.9',
    style: () => ({ getPropertyValue: (n: string) => (n === '--bg' ? '#111821' : '') }),
    theme: () => 'dark',
    width: () => 600,
    device: () => ({ locale: 'en', timeZone: 'UTC', touch: false, hover: true }),
    call: async (request, confirmed) => {
      state.calls.push({ request, confirmed });
      return { outcome: 'done', result: { n: state.calls.length } } as AppCallReply;
    },
    refreshCalls: () => {
      state.refreshed += 1;
    },
    confirmTool: async (tool) => {
      state.dialogs.push(`tool:${tool}`);
      return true;
    },
    confirmMessage: async (text) => {
      state.dialogs.push(`message:${text}`);
      return true;
    },
    confirmLink: async (url) => {
      state.dialogs.push(`link:${url}`);
      return true;
    },
    sendMessage: async (text) => {
      state.sent.push(text);
    },
    openLink: (url) => {
      state.opened.push(url);
    },
    setHeight: (px) => {
      state.heights.push(px);
    },
    onCallError: (code) => {
      state.errorCodes.push(code);
    },
    closed: (reason) => {
      state.closes.push(reason);
    },
    ...over,
  };
  const bridge = createBridge(env);
  bridge.start();
  const emit = (ev: Partial<HelloEvent>): void =>
    winListener?.({
      source: frameWindow,
      origin: 'null',
      data: { 'fleet-deck-app-hello': 1 },
      ports: [port],
      ...ev,
    });
  const r: Rig = {
    bridge,
    port,
    frameWindow,
    get closes() {
      return state.closes;
    },
    get calls() {
      return state.calls;
    },
    get refreshed() {
      return state.refreshed;
    },
    get dialogs() {
      return state.dialogs;
    },
    get sent() {
      return state.sent;
    },
    get opened() {
      return state.opened;
    },
    get heights() {
      return state.heights;
    },
    get errorCodes() {
      return state.errorCodes;
    },
    emit,
    load: () => {
      for (const l of [...loadListeners]) l();
    },
    advance: (ms) => {
      now += ms;
      for (const t of timerList) {
        if (t.live && t.at <= now) {
          t.live = false;
          t.fn();
        }
      }
    },
    hello: () => emit({}),
    rpc: async (id, method, params) => {
      const before = port.posted.length;
      port.deliver({ jsonrpc: '2.0', id, method, params });
      await flush();
      return port.posted.slice(before).find((m) => m['id'] === id);
    },
    initialize: async () => {
      await r.rpc(1, 'ui/initialize', {});
    },
  };
  return r;
}

const errorOf = (m: Posted | undefined): { code: number; message: string } =>
  (m?.['error'] ?? { code: 0, message: '' }) as { code: number; message: string };

describe('the handshake', () => {
  test('accepts one hello with one port from the frame window', () => {
    const r = rig();
    // Wrong source, wrong origin, extra key, no port and two ports are all refused.
    r.emit({ source: {} });
    r.emit({ origin: 'https://x.example' });
    r.emit({ data: { 'fleet-deck-app-hello': 1, extra: 1 } });
    r.emit({ data: { 'fleet-deck-app-hello': 2 } });
    r.emit({ ports: [] });
    r.emit({ ports: [r.port, new FakePort()] });
    expect(r.port.started).toBe(false);
    r.hello();
    expect(r.port.started).toBe(true);
    expect(r.port.onmessage).not.toBeNull();
    expect(r.closes).toEqual([]);
  });

  test('drops window messages without the handshake, also JSON-RPC requests from the frame window', async () => {
    const r = rig();
    r.emit({ data: { jsonrpc: '2.0', id: 1, method: 'ping' } });
    r.emit({ data: { jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'bump' } } });
    await flush();
    expect(r.port.posted).toEqual([]);
    expect(r.frameWindow.posted).toEqual([]);
    expect(r.calls).toEqual([]);
    // After the hello, window requests are still dropped.
    r.hello();
    r.emit({ data: { jsonrpc: '2.0', id: 3, method: 'ping' } });
    await flush();
    expect(r.port.posted).toEqual([]);
    expect(r.closes).toEqual([]);
  });

  test('answers only on the port, never on the frame window', async () => {
    const r = rig();
    r.hello();
    const answer = await r.rpc(7, 'ping');
    expect(answer).toEqual({ jsonrpc: '2.0', id: 7, result: {} });
    await r.initialize();
    await r.rpc(8, 'ui/request-display-mode', { mode: 'fullscreen' });
    expect(r.frameWindow.posted).toEqual([]);
  });

  test('a second hello closes the bridge and drops pending answers', async () => {
    const gate: { release: () => void } = { release: () => {} };
    const r = rig({
      call: () =>
        new Promise<AppCallReply>((resolve) => {
          gate.release = () => resolve({ outcome: 'done', result: 1 });
        }),
    });
    r.hello();
    await r.initialize();
    r.port.deliver({
      jsonrpc: '2.0',
      id: 5,
      method: 'tools/call',
      params: { name: 'bump', arguments: {} },
    });
    await flush();
    const before = r.port.posted.length;
    r.hello();
    expect(r.closes).toEqual(['left']);
    expect(r.port.closed).toBe(true);
    gate.release();
    await flush();
    expect(r.port.posted.length).toBe(before);
    expect(r.bridge.isOpen()).toBe(false);
  });

  test('no hello 1 s after the first load closes the bridge', () => {
    const r = rig();
    r.load();
    r.advance(999);
    expect(r.closes).toEqual([]);
    r.advance(1);
    expect(r.closes).toEqual(['no-hello']);
  });

  test('a hello within the grace second keeps the bridge open', () => {
    const r = rig();
    r.load();
    r.advance(500);
    r.hello();
    r.advance(1000);
    expect(r.closes).toEqual([]);
    expect(r.bridge.isOpen()).toBe(true);
  });

  test('second load closes', async () => {
    const r = rig();
    r.hello();
    r.load();
    expect(r.closes).toEqual([]);
    r.load();
    expect(r.closes).toEqual(['left']);
    r.port.deliver({ jsonrpc: '2.0', id: 1, method: 'ping' });
    await flush();
    expect(r.port.posted).toEqual([]);
  });
});

describe('the port protocol', () => {
  test('refuses requests before ui/initialize except ping', async () => {
    const r = rig();
    r.hello();
    expect(errorOf(await r.rpc(1, 'tools/call', { name: 'bump' })).code).toBe(-32002);
    expect(errorOf(await r.rpc(2, 'resources/read', { uri: 'ui://x' })).message).toBe(
      'Not initialized',
    );
    expect((await r.rpc(3, 'ping'))?.['result']).toEqual({});
    const init = await r.rpc(4, 'ui/initialize', {});
    const result = init?.['result'] as Record<string, unknown>;
    expect(result['protocolVersion']).toBe('2026-01-26');
    expect(result['hostInfo']).toEqual({ name: 'fleet-deck', version: '9.9' });
    const context = result['hostContext'] as Record<string, unknown>;
    expect(context['theme']).toBe('dark');
    expect(context['displayMode']).toBe('inline');
    expect(context['containerDimensions']).toEqual({ width: 600, maxHeight: 800 });
    expect(context['toolInfo']).toEqual({ tool: { name: 'show_counter' } });
    expect(context['userAgent']).toBe('fleet-deck/9.9');
    expect((context['styles'] as { variables: Record<string, string> }).variables).toEqual({
      '--color-background-primary': '#111821',
    });
    const caps = result['hostCapabilities'] as { sandbox: { csp: unknown } };
    expect(caps.sandbox.csp).toEqual({
      connectDomains: ['api.example.com'],
      resourceDomains: [],
    });
  });

  test('replays tool-input then tool-result after initialized and strips a null _meta', async () => {
    const r = rig();
    r.hello();
    await r.initialize();
    const before = r.port.posted.length;
    r.port.deliver({ jsonrpc: '2.0', method: 'ui/notifications/initialized' });
    await flush();
    const sent = r.port.posted.slice(before);
    expect(sent.map((m) => m['method'])).toEqual([
      'ui/notifications/tool-input',
      'ui/notifications/tool-result',
    ]);
    expect(sent[0]?.['params']).toEqual({ arguments: { start: 1 } });
    expect(sent[1]?.['params']).toEqual({ content: [{ type: 'text', text: 'ok' }] });
    // A second initialized does not replay.
    r.port.deliver({ jsonrpc: '2.0', method: 'ui/notifications/initialized' });
    await flush();
    expect(r.port.posted.length).toBe(before + 2);
  });

  test('a missing input and a missing result get the plain forms', async () => {
    const r = rig({}, { ...VIEW, tool_input: null, tool_result: null });
    r.hello();
    await r.initialize();
    const before = r.port.posted.length;
    r.port.deliver({ jsonrpc: '2.0', method: 'ui/notifications/initialized' });
    await flush();
    const sent = r.port.posted.slice(before);
    expect(sent[0]?.['params']).toEqual({ arguments: {} });
    expect(sent[1]?.['params']).toEqual({
      content: [{ type: 'text', text: 'The result is too large to show in the app.' }],
    });
  });

  test('too many pending requests', async () => {
    const r = rig({ call: () => new Promise<AppCallReply>(() => {}) });
    r.hello();
    await r.initialize();
    for (let i = 10; i < 26; i += 1) {
      r.port.deliver({ jsonrpc: '2.0', id: i, method: 'tools/call', params: { name: 'bump' } });
    }
    await flush();
    expect(r.port.posted.filter((m) => m['id'] !== 1)).toEqual([]);
    const refused = await r.rpc(99, 'tools/call', { name: 'bump' });
    expect(errorOf(refused)).toEqual({ code: -32000, message: 'Too many requests' });
  });

  test('oversized message is dropped', async () => {
    const r = rig();
    r.hello();
    const big = 'x'.repeat(256 * 1024 + 1);
    expect(await r.rpc(1, 'ping', { big })).toBeUndefined();
    // A message that is not JSON-RPC 2.0 is dropped too.
    r.port.deliver({ id: 2, method: 'ping' });
    r.port.deliver('ping');
    await flush();
    expect(r.port.posted).toEqual([]);
    expect(r.bridge.isOpen()).toBe(true);
  });

  test('size-changed sets the frame height inside 80 to 800', async () => {
    const r = rig();
    r.hello();
    await r.initialize();
    for (const height of [10, 300.4, 5000]) {
      r.port.deliver({
        jsonrpc: '2.0',
        method: 'ui/notifications/size-changed',
        params: { height },
      });
    }
    r.port.deliver({ jsonrpc: '2.0', method: 'ui/notifications/size-changed', params: {} });
    await flush();
    expect(r.heights).toEqual([80, 300, 800]);
  });

  test('request-display-mode answers inline', async () => {
    const r = rig();
    r.hello();
    await r.initialize();
    expect((await r.rpc(2, 'ui/request-display-mode', { mode: 'fullscreen' }))?.['result']).toEqual(
      { mode: 'inline' },
    );
  });

  test('unsupported methods answer -32601', async () => {
    const r = rig();
    r.hello();
    await r.initialize();
    for (const [i, method] of [
      'ui/update-model-context',
      'ui/download-file',
      'sampling/createMessage',
    ].entries()) {
      const e = errorOf(await r.rpc(10 + i, method, {}));
      expect(e).toEqual({ code: -32601, message: 'Not supported by Fleet Deck.' });
    }
  });

  test('a policy without tools, messages and links refuses them', async () => {
    const r = rig({
      policy: { tools: false, resources: false, message: false, link: false },
    });
    r.hello();
    await r.initialize();
    expect(errorOf(await r.rpc(2, 'tools/call', { name: 'bump' })).code).toBe(-32601);
    expect(errorOf(await r.rpc(3, 'resources/read', { uri: 'ui://x' })).code).toBe(-32601);
    expect(
      errorOf(await r.rpc(4, 'ui/message', { content: { type: 'text', text: 'a' } })).code,
    ).toBe(-32601);
    expect(errorOf(await r.rpc(5, 'ui/open-link', { url: 'https://a.example' })).code).toBe(-32601);
    expect(r.calls).toEqual([]);
  });
});

describe('tool calls', () => {
  test('a done answer returns the result and does not refresh the calls', async () => {
    const r = rig();
    r.hello();
    await r.initialize();
    const answer = await r.rpc(2, 'tools/call', { name: 'bump', arguments: { by: 1 } });
    expect(answer?.['result']).toEqual({ n: 1 });
    expect(r.calls[0]).toEqual({
      request: { method: 'tools/call', name: 'bump', arguments: { by: 1 } },
      confirmed: false,
    });
    expect(r.refreshed).toBe(0);
    const read = await r.rpc(3, 'resources/read', { uri: 'ui://demo/x' });
    expect(read?.['result']).toEqual({ n: 2 });
    expect(r.calls[1]?.request).toEqual({ method: 'resources/read', uri: 'ui://demo/x' });
  });

  test('confirm outcome asks, Cancel answers Declined by the user', async () => {
    let allow = false;
    const r = rig({
      confirmTool: async (tool) => {
        r.dialogs.push(`tool:${tool}`);
        return allow;
      },
      call: async (request, confirmed) => {
        r.calls.push({ request, confirmed });
        return confirmed
          ? { outcome: 'done', result: { ran: true } }
          : { outcome: 'confirm', tool: 'bump', title: 'Bump' };
      },
    });
    r.hello();
    await r.initialize();
    const cancelled = await r.rpc(2, 'tools/call', { name: 'bump', arguments: {} });
    expect(errorOf(cancelled)).toEqual({ code: -32000, message: 'Declined by the user' });
    expect(r.calls.map((c) => c.confirmed)).toEqual([false]);
    allow = true;
    const allowed = await r.rpc(3, 'tools/call', { name: 'bump', arguments: {} });
    expect(allowed?.['result']).toEqual({ ran: true });
    expect(r.calls.map((c) => c.confirmed)).toEqual([false, false, true]);
    expect(r.dialogs).toEqual(['tool:bump', 'tool:bump']);
  });

  test('a station error becomes -32000 with its text', async () => {
    const r = rig({
      call: async () => {
        throw new ApiError(409, 'The session is not running.', 'session_stopped');
      },
    });
    r.hello();
    await r.initialize();
    const e = errorOf(await r.rpc(2, 'tools/call', { name: 'bump' }));
    expect(e).toEqual({ code: -32000, message: 'The session is not running.' });
    expect(r.errorCodes).toEqual(['session_stopped']);
  });

  test('a timeout or provider_error answer calls refreshCalls, a done answer does not', async () => {
    const codes = ['timeout', 'provider_error'];
    let n = 0;
    const r = rig({
      call: async () => {
        const code = codes[n];
        n += 1;
        if (code === undefined) return { outcome: 'done', result: {} } as AppCallReply;
        throw new ApiError(502, 'x', code);
      },
    });
    r.hello();
    await r.initialize();
    await r.rpc(2, 'tools/call', { name: 'slow_bump' });
    expect(r.refreshed).toBe(1);
    await r.rpc(3, 'tools/call', { name: 'slow_bump' });
    expect(r.refreshed).toBe(2);
    await r.rpc(4, 'tools/call', { name: 'slow_bump' });
    expect(r.refreshed).toBe(2);
  });

  test('bad params answer -32602', async () => {
    const r = rig();
    r.hello();
    await r.initialize();
    expect(errorOf(await r.rpc(2, 'tools/call', { name: 3 })).code).toBe(-32602);
    expect(errorOf(await r.rpc(3, 'resources/read', {})).code).toBe(-32602);
    expect(r.calls).toEqual([]);
  });

  test('dialogs run one at a time and more than four waiting are refused', async () => {
    const waiting: ((ok: boolean) => void)[] = [];
    const r = rig({
      confirmLink: (url) =>
        new Promise<boolean>((resolve) => {
          r.dialogs.push(url);
          waiting.push(resolve);
        }),
    });
    r.hello();
    await r.initialize();
    for (let i = 0; i < 6; i += 1) {
      r.port.deliver({
        jsonrpc: '2.0',
        id: 10 + i,
        method: 'ui/open-link',
        params: { url: `https://a.example/${i}` },
      });
    }
    await flush();
    // One dialog is open, four wait, the sixth request is refused.
    expect(r.dialogs).toHaveLength(1);
    const refused = r.port.posted.find((m) => m['id'] === 15);
    expect(errorOf(refused)).toEqual({ code: -32000, message: 'Too many requests' });
    waiting[0]?.(true);
    await flush();
    expect(r.dialogs).toHaveLength(2);
    expect(r.opened).toEqual(['https://a.example/0']);
  });
});

describe('messages and links', () => {
  test('ui/message joins text blocks and refuses non-text', async () => {
    const r = rig();
    r.hello();
    await r.initialize();
    const joined = await r.rpc(2, 'ui/message', {
      role: 'user',
      content: [
        { type: 'text', text: 'one' },
        { type: 'image', data: 'x' },
        { type: 'text', text: 'two' },
      ],
    });
    expect(joined?.['result']).toEqual({});
    expect(r.sent).toEqual(['one\n\ntwo']);
    expect(r.dialogs).toEqual(['message:one\n\ntwo']);
    const single = await r.rpc(3, 'ui/message', { content: { type: 'text', text: 'solo' } });
    expect(single?.['result']).toEqual({});
    expect(r.sent).toEqual(['one\n\ntwo', 'solo']);
    const image = await r.rpc(4, 'ui/message', { content: { type: 'image', data: 'x' } });
    expect(errorOf(image)).toEqual({ code: -32602, message: 'Only text messages are supported.' });
    expect(r.sent).toHaveLength(2);
  });

  test('a declined message is not sent', async () => {
    const r = rig({ confirmMessage: async () => false });
    r.hello();
    await r.initialize();
    const e = errorOf(await r.rpc(2, 'ui/message', { content: { type: 'text', text: 'a' } }));
    expect(e).toEqual({ code: -32000, message: 'Declined by the user' });
    expect(r.sent).toEqual([]);
  });

  test('open-link refuses non-web schemes', async () => {
    const r = rig();
    r.hello();
    await r.initialize();
    for (const [i, url] of [
      'javascript:alert(1)',
      'data:text/html,x',
      'file:///etc/passwd',
      'not a url',
    ].entries()) {
      const e = errorOf(await r.rpc(10 + i, 'ui/open-link', { url }));
      expect(e).toEqual({ code: -32602, message: 'Only web links can be opened.' });
    }
    expect(r.opened).toEqual([]);
    const ok = await r.rpc(20, 'ui/open-link', { url: 'https://example.com/a' });
    expect(ok?.['result']).toEqual({});
    expect(r.opened).toEqual(['https://example.com/a']);
    expect(r.dialogs).toEqual(['link:https://example.com/a']);
  });
});

describe('teardown and context', () => {
  test('teardown sends ui/resource-teardown and waits for the answer', async () => {
    const r = rig();
    r.hello();
    await r.initialize();
    const done = r.bridge.teardown('hide');
    await flush();
    const sent = r.port.posted.find((m) => m['method'] === 'ui/resource-teardown');
    expect(sent?.['params']).toEqual({ reason: 'hide' });
    expect(r.bridge.isOpen()).toBe(true);
    r.port.deliver({ jsonrpc: '2.0', id: sent?.['id'], result: {} });
    await done;
    expect(r.bridge.isOpen()).toBe(false);
    expect(r.closes).toEqual([]);
  });

  test('teardown stops waiting after 2 s', async () => {
    const r = rig();
    r.hello();
    await r.initialize();
    const done = r.bridge.teardown('hide');
    await flush();
    r.advance(2000);
    await done;
    expect(r.bridge.isOpen()).toBe(false);
  });

  test('request-teardown tears down and reports it', async () => {
    const r = rig();
    r.hello();
    await r.initialize();
    r.port.deliver({ jsonrpc: '2.0', method: 'ui/notifications/request-teardown' });
    await flush();
    r.advance(2000);
    await flush();
    expect(r.closes).toEqual(['teardown']);
  });

  test('a theme change sends only the changed fields', async () => {
    let theme: 'dark' | 'light' = 'dark';
    const r = rig({ theme: () => theme });
    r.hello();
    await r.initialize();
    const before = r.port.posted.length;
    r.bridge.contextChanged();
    expect(r.port.posted.length).toBe(before);
    theme = 'light';
    r.bridge.contextChanged();
    const sent = r.port.posted[before];
    expect(sent?.['method']).toBe('ui/notifications/host-context-changed');
    expect(sent?.['params']).toEqual({ theme: 'light' });
  });
});
