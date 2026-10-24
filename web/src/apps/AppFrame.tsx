// B15 (design `docs/v2/design/mcp-apps.md` §4.8): one mounted app frame: the opaque iframe, its
// bridge, the dialogs of the app and the notes under the frame. The frame has no `allow`
// attribute and no `allow-same-origin`: the app cannot reach the board.
import { useLayoutEffect, useRef, useState } from 'react';
import type { AppViewReply } from '../gen';
import { themeOf } from './theme';
import { AppDialogView, type AppDialog, createDialogHost, type DialogHost } from './AppDialogs';
import {
  type Bridge,
  type BridgeEnv,
  CHAT_POLICY,
  type CloseReason,
  createBridge,
  realTimers,
} from './bridge';
import type { AppsEnv } from './env';

/** The version of the board that the bridge tells the app. */
export const BOARD_VERSION = '1';
const START_HEIGHT = 160;

/** The note under the frame after a refused call (design §4.8.3). */
export const STOPPED_NOTE = 'The session is not running. Resume it to use this app.';

/** The network note under the frame; `null` when the app may connect nowhere. */
export function networkNote(domains: readonly string[]): string | null {
  return domains.length === 0 ? null : `This app can connect to: ${domains.join(', ')}`;
}

type Props = {
  readonly view: AppViewReply;
  readonly appId: string;
  readonly env: AppsEnv;
  /** Set by the row: tear the app down (Hide, an eviction); `onStopped` runs after the teardown. */
  readonly stop: string | null;
  readonly onStopped: () => void;
  readonly onClosed: (reason: CloseReason) => void;
  /** A call answered `app_gone`. */
  readonly onGone: () => void;
};

export function AppFrame(props: Props) {
  const { view, appId, env } = props;
  const frame = useRef<HTMLIFrameElement>(null);
  const bridge = useRef<Bridge | null>(null);
  const [height, setHeight] = useState(START_HEIGHT);
  const [dialog, setDialog] = useState<AppDialog | null>(null);
  const [stopped, setStopped] = useState(false);
  const host = useRef<DialogHost | null>(null);
  if (host.current === null) host.current = createDialogHost(setDialog);
  const latest = useRef(props);
  latest.current = props;

  useLayoutEffect(() => {
    const el = frame.current;
    const dialogs = host.current;
    if (el === null || dialogs === null) return undefined;
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
      policy: CHAT_POLICY,
      view,
      version: BOARD_VERSION,
      style: () => getComputedStyle(root),
      theme: () => themeOf(root),
      width: () => el.clientWidth,
      device: () => ({
        locale: navigator.language,
        timeZone: Intl.DateTimeFormat().resolvedOptions().timeZone,
        touch: window.matchMedia('(pointer: coarse)').matches,
        hover: window.matchMedia('(hover: hover)').matches,
      }),
      call: (request, confirmed) =>
        env.actions.call(env.station, env.session, appId, request, confirmed),
      refreshCalls: () => env.refreshCalls(),
      confirmTool: (tool, title, args) =>
        dialogs.ask({ kind: 'tool', server: view.server, tool, title, args }),
      confirmMessage: (text) => dialogs.ask({ kind: 'message', text }),
      confirmLink: (url) => dialogs.ask({ kind: 'link', url }),
      sendMessage: (text) => env.sendMessage(text),
      openLink: (url) => {
        window.open(url, '_blank', 'noopener,noreferrer');
      },
      setHeight,
      onCallError: (code) => {
        if (code === 'session_stopped') setStopped(true);
        if (code === 'app_gone') latest.current.onGone();
      },
      closed: (reason) => {
        dialogs.dropAll();
        latest.current.onClosed(reason);
      },
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
      dialogs.dropAll();
      void b.teardown('The view closed.');
      bridge.current = null;
    };
    // One bridge per mount: a new view token is a new mount (the row keys the frame).
  }, [view.url]);

  const { stop, onStopped } = props;
  useLayoutEffect(() => {
    if (stop === null) return;
    const b = bridge.current;
    if (b === null) {
      onStopped();
      return;
    }
    void b.teardown(stop).then(onStopped);
  }, [stop, onStopped]);

  const note = networkNote(view.csp.connect_domains);
  return (
    <div className="fd-app-frame-wrap">
      <iframe
        ref={frame}
        className="fd-app-frame"
        data-border={view.prefers_border === true ? 'true' : undefined}
        sandbox="allow-scripts"
        referrerPolicy="no-referrer"
        title={`App from ${view.server}`}
        src={view.url}
        style={{ height }}
      />
      {stopped ? <p className="fd-note fd-app-note">{STOPPED_NOTE}</p> : null}
      {note === null ? null : <p className="fd-note fd-app-note">{note}</p>}
      {dialog === null ? null : (
        <AppDialogView dialog={dialog} onAnswer={(ok) => host.current?.answer(ok)} />
      )}
    </div>
  );
}
