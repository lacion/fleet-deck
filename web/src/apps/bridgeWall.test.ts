// Wall apps (C2): the wall role of the host bridge. Small fakes, copied from `bridge.test.ts`.
import { describe, expect, test } from 'bun:test';
import type { AppViewReply } from '../gen';
import {
  type BridgeEnv,
  type BridgePolicy,
  type HelloEvent,
  type PortLike,
  CHAT_POLICY,
  WALL_POLICY,
  createBridge,
} from './bridge';

type Posted = Record<string, unknown>;

class FakePort implements PortLike {
  onmessage: ((ev: { data: unknown }) => void) | null = null;
  posted: Posted[] = [];
  postMessage(message: unknown): void {
    this.posted.push(message as Posted);
  }
  start(): void {}
  close(): void {}
  deliver(data: unknown): void {
    this.onmessage?.({ data });
  }
}

const flush = async (): Promise<void> => {
  for (let i = 0; i < 8; i += 1) await Promise.resolve();
};

const VIEW: AppViewReply = {
  url: '/x',
  expires_at: 0,
  server: 'wall',
  tool: '',
  resource_uri: '',
  csp: { connect_domains: [], resource_domains: [] },
  prefers_border: null,
  tool_input: { start: 1 },
  tool_result: { content: [{ type: 'text', text: 'ok' }] },
};

const WALL_DATA = 'notifications/fleetdeck/wall-data';

function rig(policy: BridgePolicy = WALL_POLICY) {
  const frameWindow = { postMessage(_m: unknown) {} };
  const win: { listener: ((ev: HelloEvent) => void) | null } = {
    listener: null,
  };
  const port = new FakePort();
  const calls: unknown[] = [];
  const closes: string[] = [];
  const heights: number[] = [];
  const env: BridgeEnv = {
    win: {
      addEventListener: (_t, l) => {
        win.listener = l;
      },
      removeEventListener: () => {
        win.listener = null;
      },
    },
    frame: {
      contentWindow: frameWindow,
      addEventListener: () => {},
      removeEventListener: () => {},
    },
    timers: { set: () => ({}), clear: () => {} },
    policy,
    view: VIEW,
    version: '9.9',
    style: () => ({ getPropertyValue: () => '' }),
    theme: () => 'dark',
    width: () => 600,
    height: () => 300,
    device: () => ({
      locale: 'en',
      timeZone: 'UTC',
      touch: false,
      hover: true,
    }),
    call: async (request) => {
      calls.push(request);
      return { outcome: 'done', result: {} };
    },
    refreshCalls: () => {},
    confirmTool: async () => true,
    confirmMessage: async () => true,
    confirmLink: async () => true,
    sendMessage: async () => {},
    openLink: () => {},
    setHeight: (px) => {
      heights.push(px);
    },
    closed: (reason) => {
      closes.push(reason);
    },
  };
  const bridge = createBridge(env);
  bridge.start();
  win.listener?.({
    source: frameWindow,
    origin: 'null',
    data: { 'fleet-deck-app-hello': 1 },
    ports: [port],
  });
  const rpc = async (id: number, method: string, params?: unknown): Promise<Posted | undefined> => {
    const before = port.posted.length;
    port.deliver({ jsonrpc: '2.0', id, method, params });
    await flush();
    return port.posted.slice(before).find((m) => m['id'] === id);
  };
  const notify = (method: string, params?: unknown): void =>
    port.deliver({ jsonrpc: '2.0', method, params });
  const wallDataSent = (): Posted[] => port.posted.filter((m) => m['method'] === WALL_DATA);
  return { bridge, port, calls, closes, heights, rpc, notify, wallDataSent };
}

const errorOf = (m: Posted | undefined): { code: number; message: string } =>
  (m?.['error'] ?? { code: 0, message: '' }) as {
    code: number;
    message: string;
  };

describe('the wall role', () => {
  test('wall_role_answers_initialize_with_fullscreen_and_the_container_size', async () => {
    const r = rig();
    const answer = await r.rpc(1, 'ui/initialize', {});
    const result = answer?.['result'] as Posted;
    const context = result['hostContext'] as Posted;
    expect(context['displayMode']).toBe('fullscreen');
    expect(context['availableDisplayModes']).toEqual(['fullscreen']);
    expect(context['containerDimensions']).toEqual({ width: 600, height: 300 });
    expect('toolInfo' in context).toBe(false);
    const caps = result['hostCapabilities'] as Posted;
    expect(Object.keys(caps).sort()).toEqual(['logging', 'sandbox']);
  });

  test('wall_role_refuses_every_app_request_with_not_available_on_a_wall', async () => {
    const r = rig();
    expect(errorOf(await r.rpc(1, 'tools/call', {})).code).toBe(-32002);
    await r.rpc(2, 'ui/initialize', {});
    const methods = [
      'tools/call',
      'resources/read',
      'ui/message',
      'ui/open-link',
      'ui/request-display-mode',
      'ui/update-model-context',
      'ui/download-file',
    ];
    let id = 10;
    for (const method of methods) {
      id += 1;
      const e = errorOf(await r.rpc(id, method, { name: 'x', uri: 'u', url: 'https://a.b' }));
      expect(e.code).toBe(-32601);
      expect(e.message).toBe('Not available on a wall');
    }
    expect((await r.rpc(99, 'ping'))?.['result']).toEqual({});
    expect(r.calls).toEqual([]);
  });

  test('wall_role_ignores_size_changes', async () => {
    const r = rig();
    await r.rpc(1, 'ui/initialize', {});
    r.notify('ui/notifications/size-changed', { height: 400 });
    expect(r.heights).toEqual([]);
  });

  test('wall_role_sends_wall_data_after_initialized_and_on_each_change', async () => {
    const r = rig();
    r.bridge.wallData({ at: 1, sessions: [] });
    await r.rpc(1, 'ui/initialize', {});
    r.notify('ui/notifications/initialized', {});
    expect(r.wallDataSent().map((m) => m['params'])).toEqual([{ at: 1, sessions: [] }]);
    r.bridge.wallData({ at: 1, sessions: [] });
    expect(r.wallDataSent()).toHaveLength(1);
    r.bridge.wallData({ at: 2, sessions: [] });
    expect(r.wallDataSent().map((m) => m['params'])).toEqual([
      { at: 1, sessions: [] },
      { at: 2, sessions: [] },
    ]);
  });

  test('wall_role_sends_nothing_before_initialize', () => {
    const r = rig();
    r.bridge.wallData({ at: 1, sessions: [] });
    expect(r.wallDataSent()).toEqual([]);
  });

  test('wall_role_does_not_replay_tool_input_or_result', async () => {
    const r = rig();
    await r.rpc(1, 'ui/initialize', {});
    r.notify('ui/notifications/initialized', {});
    const methods = r.port.posted.map((m) => m['method']);
    expect(methods).not.toContain('ui/notifications/tool-input');
    expect(methods).not.toContain('ui/notifications/tool-result');
  });

  test('wall_role_drops_request_teardown', async () => {
    const r = rig();
    await r.rpc(1, 'ui/initialize', {});
    r.notify('ui/notifications/request-teardown', {});
    await flush();
    expect(r.closes).toEqual([]);
    expect(r.bridge.isOpen()).toBe(true);
    expect(r.port.posted.some((m) => m['method'] === 'ui/resource-teardown')).toBe(false);
  });

  test('chat_role_never_sends_wall_data', async () => {
    const r = rig(CHAT_POLICY);
    await r.rpc(1, 'ui/initialize', {});
    r.bridge.wallData({ at: 1, sessions: [] });
    r.notify('ui/notifications/initialized', {});
    expect(r.wallDataSent()).toEqual([]);
  });
});
