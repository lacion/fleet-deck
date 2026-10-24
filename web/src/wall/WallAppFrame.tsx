// Wall apps (C2, design `docs/v2/design/wall-control.md` §3.4): one HTML artifact that runs as an
// app on a wall display. The frame is opaque (`allow-scripts` only, no `allow`, no
// `allow-same-origin`). The bridge has the wall role: the app may only initialize and receive the
// feed of its session cards.
import { useEffect, useLayoutEffect, useRef } from 'react';
import type { AppViewReply, WallAppFeed } from '../gen';
import { BOARD_VERSION } from '../apps/AppFrame';
import { type Bridge, type BridgeEnv, WALL_POLICY, createBridge, realTimers } from '../apps/bridge';
import { themeOf } from '../apps/theme';

/** The view of a wall app: no server, no tool, no stored input, no network. */
export const wallView = (src: string): AppViewReply => ({
  url: src,
  expires_at: 0,
  server: 'wall',
  tool: '',
  resource_uri: '',
  csp: { connect_domains: [], resource_domains: [] },
  prefers_border: null,
  tool_input: null,
  tool_result: null,
});

const NOT_ON_A_WALL = 'Not available on a wall';

export function WallAppFrame({
  src,
  title,
  data,
}: {
  readonly src: string;
  readonly title: string;
  readonly data: WallAppFeed | undefined;
}) {
  const frame = useRef<HTMLIFrameElement>(null);
  const bridge = useRef<Bridge | null>(null);

  useLayoutEffect(() => {
    const el = frame.current;
    if (el === null) return undefined;
    const root = document.documentElement;
    const bridgeEnv: BridgeEnv = {
      win: {
        addEventListener: (type, l) =>
          window.addEventListener(type, l as unknown as (ev: MessageEvent) => void),
        removeEventListener: (type, l) =>
          window.removeEventListener(type, l as unknown as (ev: MessageEvent) => void),
      },
      frame: {
        get contentWindow() {
          return el.contentWindow;
        },
        addEventListener: (type, l) => el.addEventListener(type, l),
        removeEventListener: (type, l) => el.removeEventListener(type, l),
      },
      timers: realTimers,
      policy: WALL_POLICY,
      view: wallView(src),
      version: BOARD_VERSION,
      style: () => getComputedStyle(root),
      theme: () => themeOf(root),
      width: () => el.clientWidth,
      height: () => el.clientHeight,
      device: () => ({
        locale: navigator.language,
        timeZone: Intl.DateTimeFormat().resolvedOptions().timeZone,
        touch: window.matchMedia('(pointer: coarse)').matches,
        hover: window.matchMedia('(hover: hover)').matches,
      }),
      call: () => Promise.reject(new Error(NOT_ON_A_WALL)),
      refreshCalls: () => {},
      confirmTool: () => Promise.resolve(false),
      confirmMessage: () => Promise.resolve(false),
      confirmLink: () => Promise.resolve(false),
      sendMessage: () => Promise.reject(new Error(NOT_ON_A_WALL)),
      openLink: () => {},
      setHeight: () => {},
      // The frame stays; the app only loses its host.
      closed: () => {},
    };
    const b = createBridge(bridgeEnv);
    bridge.current = b;
    b.start();
    const watch = new MutationObserver(() => b.contextChanged());
    watch.observe(root, { attributeFilter: ['data-theme'] });
    const sizes =
      typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(() => b.contextChanged());
    sizes?.observe(el);
    return () => {
      watch.disconnect();
      sizes?.disconnect();
      void b.teardown('The view closed.');
      bridge.current = null;
    };
  }, [src]);

  useEffect(() => {
    if (data !== undefined) bridge.current?.wallData(data);
  }, [data]);

  return (
    <div className="fd-wall-fit" data-fit="one">
      <iframe
        ref={frame}
        className="fd-art-frame"
        sandbox="allow-scripts"
        referrerPolicy="no-referrer"
        title={title}
        src={src}
      />
    </div>
  );
}
