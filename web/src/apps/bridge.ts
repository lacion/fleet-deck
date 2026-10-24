// B15 (design `docs/v2/design/mcp-apps.md` §4.8): the host bridge of one mounted app frame. Pure
// TypeScript, no React, no DOM types: the window, the frame, the port and the timers are small
// interfaces, so a test drives it with fakes. One instance per mounted frame.
//
// The authority is the port, not the window: the app document sends one hello (a
// `MessageChannel` port) before any app script runs. The bridge reads requests only from that port
// and sends only on it. Every other window message of the frame is dropped.
import type { AppCallReply, AppRequest, AppViewReply } from '../gen';
import { type BoardTheme, themeVariables, type StyleReader } from './theme';

export const PROTOCOL_VERSION = '2026-01-26';
export const HELLO_KEY = 'fleet-deck-app-hello';
export const MAX_MESSAGE_BYTES = 256 * 1024;
export const MAX_PENDING = 16;
export const MAX_QUEUED_DIALOGS = 4;
export const GRACE_MS = 1000;
export const TEARDOWN_WAIT_MS = 2000;
export const MIN_HEIGHT = 80;
export const MAX_HEIGHT = 800;

/** The port that the app document sent (a `MessagePort`). */
export type PortLike = {
  onmessage: ((ev: { data: unknown }) => void) | null;
  postMessage(message: unknown): void;
  start(): void;
  close(): void;
};

/** The `message` event of the window. */
export type HelloEvent = {
  readonly source: unknown;
  readonly origin: string;
  readonly data: unknown;
  readonly ports: readonly PortLike[];
};

export type WindowLike = {
  addEventListener(type: 'message', listener: (ev: HelloEvent) => void): void;
  removeEventListener(type: 'message', listener: (ev: HelloEvent) => void): void;
};

export type FrameLike = {
  readonly contentWindow: unknown;
  addEventListener(type: 'load', listener: () => void): void;
  removeEventListener(type: 'load', listener: () => void): void;
};

export type Timers = {
  set(fn: () => void, ms: number): unknown;
  clear(handle: unknown): void;
};

export const realTimers: Timers = {
  set: (fn, ms) => setTimeout(fn, ms),
  clear: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
};

/** What an app may ask for. The chat policy allows all four; a later wall policy allows none. */
export type BridgePolicy = {
  readonly tools: boolean;
  readonly resources: boolean;
  readonly message: boolean;
  readonly link: boolean;
  /** The role of the host: absent is the chat role (the wall role is the wall apps of C2). */
  readonly role?: 'chat' | 'wall';
};

export const CHAT_POLICY: BridgePolicy = {
  tools: true,
  resources: true,
  message: true,
  link: true,
};

/** The wall role: an app may only initialize and receive the wall feed. */
export const WALL_POLICY: BridgePolicy = {
  tools: false,
  resources: false,
  message: false,
  link: false,
  role: 'wall',
};

/** Why the bridge closed by itself (the owner's Hide or leaving the view calls `teardown`). */
export type CloseReason = 'left' | 'no-hello' | 'teardown';

/** The device and locale facts of the host context. */
export type DeviceFacts = {
  readonly locale: string;
  readonly timeZone: string;
  readonly touch: boolean;
  readonly hover: boolean;
};

export type BridgeEnv = {
  readonly win: WindowLike;
  readonly frame: FrameLike;
  readonly timers: Timers;
  readonly policy: BridgePolicy;
  /** The view of the app: its stored input and result, its CSP and its tool. */
  readonly view: AppViewReply;
  /** The version of the board, for `hostInfo` and the user agent. */
  readonly version: string;
  /** The tokens of the board (`getComputedStyle(document.documentElement)`). */
  style(): StyleReader;
  theme(): BoardTheme;
  width(): number;
  /** The height of the frame (the wall role tells it in the container size). */
  height?(): number;
  device(): DeviceFacts;
  /** The call route. A station error is a thrown error with the station's message. */
  call(request: AppRequest, confirmed: boolean): Promise<AppCallReply>;
  /** The open-calls strip asks the list route again. */
  refreshCalls(): void;
  /** The consent dialog of a tool call: true is Allow. */
  confirmTool(tool: string, title: string | null, args: unknown): Promise<boolean>;
  confirmMessage(text: string): Promise<boolean>;
  confirmLink(url: string): Promise<boolean>;
  /** The composer's send path. */
  sendMessage(text: string): Promise<void>;
  openLink(url: string): void;
  setHeight(px: number): void;
  /** The code of a refused or failed call (`session_stopped`, `app_gone`, ...), for the row. */
  onCallError?(code: string | null): void;
  /** The bridge closed by itself. */
  closed(reason: CloseReason): void;
};

type Json = Record<string, unknown>;
const record = (v: unknown): v is Json => typeof v === 'object' && v !== null && !Array.isArray(v);

/** An error that becomes a JSON-RPC error answer. */
class RpcError extends Error {
  constructor(
    readonly code: number,
    message: string,
  ) {
    super(message);
  }
}
const declined = (): RpcError => new RpcError(-32000, 'Declined by the user');
const unsupported = (wall = false): RpcError =>
  new RpcError(-32601, wall ? 'Not available on a wall' : 'Not supported by Fleet Deck.');

function sizeOk(data: unknown): boolean {
  try {
    const text = JSON.stringify(data);
    return typeof text === 'string' && text.length <= MAX_MESSAGE_BYTES;
  } catch {
    return false;
  }
}

function isHello(data: unknown): boolean {
  if (!record(data)) return false;
  const keys = Object.keys(data);
  return keys.length === 1 && keys[0] === HELLO_KEY && data[HELLO_KEY] === 1;
}

const hasHelloKey = (data: unknown): boolean => record(data) && HELLO_KEY in data;

/** The text blocks of `ui/message` joined with a blank line (`null`: no text block). */
function messageText(content: unknown): string | null {
  const blocks = Array.isArray(content) ? content : [content];
  const texts: string[] = [];
  for (const b of blocks) {
    if (record(b) && b['type'] === 'text' && typeof b['text'] === 'string') texts.push(b['text']);
  }
  return texts.length === 0 ? null : texts.join('\n\n');
}

/** The tool result for `ui/notifications/tool-result`. */
function toolResult(stored: unknown): Json {
  if (!record(stored)) {
    return { content: [{ type: 'text', text: 'The result is too large to show in the app.' }] };
  }
  const out: Json = { ...stored };
  if ('_meta' in out && !record(out['_meta'])) delete out['_meta'];
  return out;
}

export type Bridge = {
  /** Listens for the hello and the frame's `load` events. */
  start(): void;
  /** Sends the changed host context fields (a theme or width change). */
  contextChanged(): void;
  /** The owner's Hide or leaving the view: `ui/resource-teardown`, wait at most 2 s, then close. */
  teardown(reason: string): Promise<void>;
  /** Closes for good without a message. */
  close(): void;
  /** The wall role: sends the feed to the app (a no-op in the chat role). */
  wallData(data: unknown): void;
  /** True while the bridge accepts messages. */
  isOpen(): boolean;
};

export function createBridge(env: BridgeEnv): Bridge {
  let open = true;
  let started = false;
  let port: PortLike | null = null;
  let loads = 0;
  let graceTimer: unknown = null;
  let initRequested = false;
  let replayed = false;
  let pending = 0;
  let dialogs = 0;
  let dialogTail: Promise<void> = Promise.resolve();
  let teardownAck: (() => void) | null = null;
  let lastContext: Json = {};
  const wall = env.policy.role === 'wall';
  let wallKept: unknown;
  let wallSent: string | null = null;

  const csp = env.view.csp;

  function hostContext(): Json {
    const d = env.device();
    if (wall) {
      return {
        theme: env.theme(),
        styles: { variables: themeVariables(env.style()) },
        displayMode: 'fullscreen',
        availableDisplayModes: ['fullscreen'],
        containerDimensions: { width: env.width(), height: env.height?.() ?? 0 },
        locale: d.locale,
        timeZone: d.timeZone,
        platform: 'web',
        userAgent: `fleet-deck/${env.version}`,
        deviceCapabilities: { touch: d.touch, hover: d.hover },
      };
    }
    return {
      theme: env.theme(),
      styles: { variables: themeVariables(env.style()) },
      displayMode: 'inline',
      availableDisplayModes: ['inline'],
      containerDimensions: { width: env.width(), maxHeight: MAX_HEIGHT },
      locale: d.locale,
      timeZone: d.timeZone,
      platform: 'web',
      userAgent: `fleet-deck/${env.version}`,
      deviceCapabilities: { touch: d.touch, hover: d.hover },
      toolInfo: { tool: { name: env.view.tool } },
    };
  }

  const send = (message: Json): void => {
    if (open && port !== null) port.postMessage({ jsonrpc: '2.0', ...message });
  };
  const notify = (method: string, params: Json): void => send({ method, params });

  function sendWallData(): void {
    if (!wall || !open || port === null || !initRequested || wallKept === undefined) return;
    let text: string | undefined;
    try {
      text = JSON.stringify(wallKept);
    } catch {
      return;
    }
    if (typeof text !== 'string' || text === wallSent) return;
    wallSent = text;
    notify('notifications/fleetdeck/wall-data', wallKept as Json);
  }

  function shut(reason: CloseReason | null): void {
    if (!open) return;
    open = false;
    if (graceTimer !== null) env.timers.clear(graceTimer);
    graceTimer = null;
    env.win.removeEventListener('message', onWindow);
    env.frame.removeEventListener('load', onLoad);
    if (port !== null) {
      port.onmessage = null;
      port.close();
    }
    pending = 0;
    teardownAck?.();
    if (reason !== null) env.closed(reason);
  }

  function onLoad(): void {
    if (!open) return;
    loads += 1;
    if (loads > 1) {
      shut('left');
      return;
    }
    if (port === null) {
      graceTimer = env.timers.set(() => {
        graceTimer = null;
        if (open && port === null) shut('no-hello');
      }, GRACE_MS);
    }
  }

  function onWindow(ev: HelloEvent): void {
    if (!open || ev.source !== env.frame.contentWindow) return;
    if (port !== null) {
      // A second hello of the frame window: a replacement document or a reload inside the frame.
      if (hasHelloKey(ev.data)) shut('left');
      return;
    }
    if (ev.origin !== 'null' || !isHello(ev.data) || ev.ports.length !== 1) return;
    const accepted = ev.ports[0];
    if (accepted === undefined) return;
    port = accepted;
    if (graceTimer !== null) env.timers.clear(graceTimer);
    graceTimer = null;
    accepted.onmessage = (m) => onPort(m.data);
    accepted.start();
  }

  /** Dialogs of one app run one at a time; at most 4 wait. */
  async function dialog<T>(run: () => Promise<T>): Promise<T> {
    if (dialogs > MAX_QUEUED_DIALOGS) throw new RpcError(-32000, 'Too many requests');
    dialogs += 1;
    const before = dialogTail;
    let done: () => void = () => {};
    dialogTail = new Promise<void>((resolve) => {
      done = resolve;
    });
    try {
      await before;
      if (!open) throw new RpcError(-32000, 'The app was stopped.');
      return await run();
    } finally {
      dialogs -= 1;
      done();
    }
  }

  function failure(e: unknown): RpcError {
    if (e instanceof RpcError) return e;
    const code = record(e) && typeof e['code'] === 'string' ? e['code'] : null;
    env.onCallError?.(code);
    return new RpcError(-32000, e instanceof Error ? e.message : 'The call failed.');
  }

  async function callTool(params: unknown): Promise<unknown> {
    if (!env.policy.tools) throw unsupported(wall);
    if (!record(params) || typeof params['name'] !== 'string') {
      throw new RpcError(-32602, 'Invalid params.');
    }
    const request: AppRequest = {
      method: 'tools/call',
      name: params['name'],
      arguments: params['arguments'] ?? {},
    };
    try {
      let reply = await env.call(request, false);
      if (reply.outcome === 'confirm') {
        const { tool, title } = reply;
        const allowed = await dialog(() => env.confirmTool(tool, title, request.arguments));
        if (!allowed) throw declined();
        reply = await env.call(request, true);
        if (reply.outcome === 'confirm') {
          throw new RpcError(-32000, 'The station asked for a confirmation again.');
        }
      }
      return reply.result;
    } catch (e) {
      if (!(e instanceof RpcError)) env.refreshCalls();
      throw failure(e);
    }
  }

  async function readResource(params: unknown): Promise<unknown> {
    if (!env.policy.resources) throw unsupported(wall);
    if (!record(params) || typeof params['uri'] !== 'string') {
      throw new RpcError(-32602, 'Invalid params.');
    }
    try {
      const reply = await env.call({ method: 'resources/read', uri: params['uri'] }, false);
      if (reply.outcome !== 'done')
        throw new RpcError(-32000, 'The station asked for a confirmation.');
      return reply.result;
    } catch (e) {
      throw failure(e);
    }
  }

  async function message(params: unknown): Promise<unknown> {
    if (!env.policy.message) throw unsupported(wall);
    const text = record(params) ? messageText(params['content']) : null;
    if (text === null) throw new RpcError(-32602, 'Only text messages are supported.');
    if (!(await dialog(() => env.confirmMessage(text)))) throw declined();
    try {
      await env.sendMessage(text);
    } catch (e) {
      throw failure(e);
    }
    return {};
  }

  async function openLink(params: unknown): Promise<unknown> {
    if (!env.policy.link) throw unsupported(wall);
    let url: URL | null = null;
    try {
      if (record(params) && typeof params['url'] === 'string') url = new URL(params['url']);
    } catch {
      url = null;
    }
    if (url === null || (url.protocol !== 'http:' && url.protocol !== 'https:')) {
      throw new RpcError(-32602, 'Only web links can be opened.');
    }
    const href = url.href;
    if (!(await dialog(() => env.confirmLink(href)))) throw declined();
    env.openLink(href);
    return {};
  }

  function initialize(): unknown {
    initRequested = true;
    const context = hostContext();
    lastContext = context;
    return {
      protocolVersion: PROTOCOL_VERSION,
      hostInfo: { name: 'fleet-deck', version: env.version },
      hostCapabilities: {
        ...(wall ? {} : { openLinks: {}, serverTools: {}, serverResources: {} }),
        logging: {},
        sandbox: {
          permissions: {},
          csp: { connectDomains: csp.connect_domains, resourceDomains: csp.resource_domains },
        },
      },
      hostContext: context,
    };
  }

  async function answer(method: string, params: unknown): Promise<unknown> {
    if (method === 'ping') return {};
    if (method === 'ui/initialize') return initialize();
    if (!initRequested) throw new RpcError(-32002, 'Not initialized');
    switch (method) {
      case 'tools/call':
        return callTool(params);
      case 'resources/read':
        return readResource(params);
      case 'ui/message':
        return message(params);
      case 'ui/open-link':
        return openLink(params);
      case 'ui/request-display-mode':
        if (wall) throw unsupported(true);
        return { mode: 'inline' };
      default:
        throw unsupported(wall);
    }
  }

  async function request(id: string | number, method: string, params: unknown): Promise<void> {
    if (pending >= MAX_PENDING) {
      send({ id, error: { code: -32000, message: 'Too many requests' } });
      return;
    }
    pending += 1;
    try {
      const result = await answer(method, params);
      send({ id, result });
    } catch (e) {
      const err = e instanceof RpcError ? e : new RpcError(-32000, 'The request failed.');
      send({ id, error: { code: err.code, message: err.message } });
    } finally {
      if (open) pending -= 1;
    }
  }

  function notification(method: string, params: unknown): void {
    if (!initRequested) return;
    switch (method) {
      case 'ui/notifications/initialized':
        if (replayed) return;
        replayed = true;
        if (wall) {
          sendWallData();
          return;
        }
        notify('ui/notifications/tool-input', {
          arguments: record(env.view.tool_input) ? env.view.tool_input : {},
        });
        notify('ui/notifications/tool-result', toolResult(env.view.tool_result));
        return;
      case 'ui/notifications/size-changed': {
        if (wall) return;
        const h = record(params) ? params['height'] : undefined;
        if (typeof h === 'number' && Number.isFinite(h)) {
          env.setHeight(Math.min(MAX_HEIGHT, Math.max(MIN_HEIGHT, Math.round(h))));
        }
        return;
      }
      case 'ui/notifications/request-teardown':
        if (wall) return;
        void bridge.teardown('The app asked to close.').then(() => env.closed('teardown'));
        return;
      default:
      // `notifications/message` and the rest are dropped.
    }
  }

  function onPort(data: unknown): void {
    if (!open || !sizeOk(data) || !record(data) || data['jsonrpc'] !== '2.0') return;
    const id = data['id'];
    const hasId = typeof id === 'string' || typeof id === 'number';
    const method = data['method'];
    if (typeof method === 'string') {
      if (hasId) void request(id, method, data['params']);
      else notification(method, data['params']);
    } else if (hasId) {
      // The answer of the app to `ui/resource-teardown`.
      teardownAck?.();
    }
  }

  const bridge: Bridge = {
    start() {
      if (started || !open) return;
      started = true;
      env.win.addEventListener('message', onWindow);
      env.frame.addEventListener('load', onLoad);
    },
    contextChanged() {
      if (!open || port === null || !initRequested) return;
      const next = hostContext();
      const changed: Json = {};
      for (const [k, v] of Object.entries(next)) {
        if (JSON.stringify(v) !== JSON.stringify(lastContext[k])) changed[k] = v;
      }
      lastContext = next;
      if (Object.keys(changed).length > 0) notify('ui/notifications/host-context-changed', changed);
    },
    async teardown(reason) {
      if (!open) return;
      if (port !== null && initRequested) {
        const waited = new Promise<void>((resolve) => {
          teardownAck = resolve;
        });
        const timer = env.timers.set(() => teardownAck?.(), TEARDOWN_WAIT_MS);
        send({ id: 'fleet-deck-teardown', method: 'ui/resource-teardown', params: { reason } });
        await waited;
        env.timers.clear(timer);
      }
      shut(null);
    },
    close() {
      shut(null);
    },
    wallData(data) {
      if (!wall) return;
      wallKept = data;
      sendWallData();
    },
    isOpen: () => open,
  };
  return bridge;
}
