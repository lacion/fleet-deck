// B15 (design `docs/v2/design/mcp-apps.md` §4.8.2, §4.8.3): the app row under a tool row of the chat.
// It mounts its frame when it comes into view (at most 3 frames per chat), and shows the states of
// the app in the exact words of the design.
import {
  createContext,
  type ReactNode,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
} from 'react';
import type { AppViewReply } from '../gen';
import { Menu } from '../ui/Menu';
import { Ellipsis, Icon } from '../ui/icons';
import { Loading } from '../ui/Loading';
import { ErrorNotice } from '../ui/page';
import { ApiError } from '../runtime/api';
import { AppFrame } from './AppFrame';
import type { CloseReason } from './bridge';
import { type AppsEnv, createFrameSlots, type FrameSlots } from './env';

/** The app of a tool row (`tool.app` of the timeline). */
export type ToolApp = {
  readonly app_id: string;
  readonly server: string;
  readonly tool: string;
  readonly prefers_border: boolean | undefined;
};

/** What the row shows besides the caption. */
export type RowState =
  | { readonly kind: 'collapsed' }
  | { readonly kind: 'loading' }
  | { readonly kind: 'ready'; readonly view: AppViewReply }
  | { readonly kind: 'load-error' }
  | { readonly kind: 'offline' }
  | { readonly kind: 'gone' }
  | { readonly kind: 'left' }
  | { readonly kind: 'no-start' };

export const MEMBER_NOTE = 'Only the owner can open this app.';

type Shared = { readonly env: AppsEnv; readonly slots: FrameSlots };
const AppsContext = createContext<Shared | null>(null);

/** The chat gives its apps environment to the rows under it; the three-frame budget is its own. */
export function AppsProvider(props: {
  readonly env: AppsEnv | undefined;
  readonly children: ReactNode;
}) {
  const slots = useRef<FrameSlots | null>(null);
  if (slots.current === null) slots.current = createFrameSlots(3);
  const value = props.env === undefined ? null : { env: props.env, slots: slots.current };
  return <AppsContext.Provider value={value}>{props.children}</AppsContext.Provider>;
}

/** The caption: "App from {server}", the tool in mono, and the menu. */
function Caption(props: {
  readonly app: ToolApp;
  readonly menu: { readonly reload: () => void; readonly hide: () => void } | null;
}) {
  return (
    <div className="fd-app-cap">
      <span className="fd-label">{`App from ${props.app.server}`}</span>
      <code className="fd-app-tool">{props.app.tool}</code>
      {props.menu === null ? null : (
        <Menu summary={<Icon of={Ellipsis} />} summaryClass="fd-iconbtn fd-sm" label="App menu">
          {(close) => (
            <>
              <button
                type="button"
                onClick={() => {
                  close();
                  props.menu?.reload();
                }}
              >
                Reload app
              </button>
              <button
                type="button"
                onClick={() => {
                  close();
                  props.menu?.hide();
                }}
              >
                Hide app
              </button>
            </>
          )}
        </Menu>
      )}
    </div>
  );
}

/** The body of a state that is not a frame. */
export function StateBody(props: {
  readonly state: Exclude<RowState, { kind: 'ready' }>;
  readonly onShow: () => void;
  readonly onReload: () => void;
}) {
  switch (props.state.kind) {
    case 'collapsed':
      return (
        <button type="button" onClick={props.onShow}>
          Show app
        </button>
      );
    case 'loading':
      return <Loading text="Loading the app…" />;
    case 'load-error':
      return <ErrorNotice text="The app could not load." onRetry={props.onReload} />;
    case 'offline':
      return <ErrorNotice text="The station is offline. The app cannot load." />;
    case 'gone':
      return <p className="fd-note">This app is no longer stored.</p>;
    case 'left':
      return (
        <div className="fd-app-left">
          <p className="fd-note">The app left its page and was stopped.</p>
          <button type="button" onClick={props.onReload}>
            Reload app
          </button>
        </div>
      );
    case 'no-start':
      return <ErrorNotice text="The app could not start." onRetry={props.onReload} />;
  }
}

/** The row state that a failed view call gives (`null`: the station has no apps, show nothing). */
export function stateOfViewError(e: unknown): RowState | null {
  if (e instanceof ApiError) {
    if (e.code === 'apps_unsupported') return null;
    if (e.code === 'station_unreachable') return { kind: 'offline' };
    if (e.code === 'app_gone' || e.status === 410) return { kind: 'gone' };
  }
  return { kind: 'load-error' };
}

/** The state of a bridge that closed by itself (`teardown`: the app asked, the row collapses). */
export function stateOfClose(reason: CloseReason): RowState {
  if (reason === 'left') return { kind: 'left' };
  if (reason === 'no-hello') return { kind: 'no-start' };
  return { kind: 'collapsed' };
}

/** The member's row: the caption and the note, no frame. */
export function MemberRow({ app }: { readonly app: ToolApp }) {
  return (
    <div className="fd-app-row">
      <Caption app={app} menu={null} />
      <p className="fd-note">{MEMBER_NOTE}</p>
    </div>
  );
}

function OwnerRow(props: { readonly app: ToolApp; readonly shared: Shared }) {
  const { app } = props;
  const { env, slots } = props.shared;
  const root = useRef<HTMLDivElement>(null);
  const [visible, setVisible] = useState(typeof IntersectionObserver === 'undefined');
  const [state, setState] = useState<RowState>({ kind: 'loading' });
  const [unsupported, setUnsupported] = useState(false);
  const [nonce, setNonce] = useState(0);
  const [stop, setStop] = useState<string | null>(null);
  const key = app.app_id;

  useEffect(() => {
    const el = root.current;
    if (visible || el === null) return undefined;
    const seen = new IntersectionObserver((entries) => {
      if (entries.some((e) => e.isIntersecting)) setVisible(true);
    });
    seen.observe(el);
    return () => seen.disconnect();
  }, [visible]);

  // Loading: a new view token for each mount (first show, Reload, Show app).
  const loading = visible && state.kind === 'loading';
  useEffect(() => {
    if (!loading) return undefined;
    let live = true;
    void env.actions.view(env.station, env.session, app.app_id).then(
      (view) => {
        if (!live) return;
        slots.claim(key, () => setStop('Another app took the place.'));
        setState({ kind: 'ready', view });
      },
      (e) => {
        if (!live) return;
        const next = stateOfViewError(e);
        if (next === null) setUnsupported(true);
        else setState(next);
      },
    );
    return () => {
      live = false;
    };
  }, [loading, env, app.app_id, slots, key, nonce]);

  useEffect(() => () => slots.release(key), [slots, key]);

  const leave = useCallback(
    (next: RowState) => {
      slots.release(key);
      setStop(null);
      setState(next);
    },
    [slots, key],
  );
  const reload = () => {
    slots.release(key);
    setStop(null);
    setNonce((n) => n + 1);
    setState({ kind: 'loading' });
  };
  const onStopped = useCallback(() => leave({ kind: 'collapsed' }), [leave]);

  if (unsupported) return null;
  const menu =
    state.kind === 'ready' ? { reload, hide: () => setStop('The owner hid the app.') } : null;
  return (
    <div className="fd-app-row" ref={root} data-state={state.kind}>
      <Caption app={app} menu={menu} />
      {state.kind === 'ready' ? (
        <AppFrame
          key={`${state.view.url}:${nonce}`}
          view={state.view}
          appId={app.app_id}
          env={env}
          stop={stop}
          onStopped={onStopped}
          onClosed={(reason) => leave(stateOfClose(reason))}
          onGone={() => leave({ kind: 'gone' })}
        />
      ) : (
        <StateBody state={state} onShow={reload} onReload={reload} />
      )}
    </div>
  );
}

/** The row under a tool row that has an app; nothing without an apps environment. */
export function AppRow({ app }: { readonly app: ToolApp }) {
  const shared = useContext(AppsContext);
  if (shared === null) return null;
  if (!shared.env.owner) return <MemberRow app={app} />;
  return <OwnerRow app={app} shared={shared} />;
}
