import { useMemo } from 'react';
import type { DeckSession, SessionKey, StationView } from '../gen';
import type { Freshness, Timeline } from '../runtime/state';
import type { FleetActions } from '../fleet/fleet';
import type { SessionActions } from '../session/commands';
import { SessionView } from '../session/SessionView';
import type { ShellActions } from '../shell/shell';
import type { Watch } from '../terminal/handover';
import type { TerminalStore } from '../terminal/store';
import { canGoBack } from './backNav';
import type { Navigate, Route, Tab } from './routes';
import type { Layout } from './viewport';

/** The props contract of the new SessionView (DL-140 wave 2). */
export type PaneProps = {
  readonly session: DeckSession;
  readonly stations: readonly StationView[];
  readonly timeline: Timeline | undefined;
  readonly freshness: Freshness;
  readonly actions: SessionActions | undefined;
  readonly shells: ShellActions | undefined;
  readonly terminals: TerminalStore | undefined;
  readonly watch: Watch | undefined;
  readonly layout: Layout;
  readonly tab: Tab;
  readonly onTab: (tab: Tab) => void;
  /** B6: the subagent of the chat tab (`?agent=`); `onAgent(null)` returns to the main chat. */
  readonly agent?: string;
  readonly onAgent?: (nodeId: string | null) => void;
  readonly onBack?: () => void;
  readonly onTerminalOpened?: () => void;
  readonly onNewShell?: () => void;
  readonly fleet?: FleetActions | undefined;
};

type SessionRouteValue = Extract<Route, { name: 'session' }>;

type BuildInput = {
  readonly session: DeckSession;
  readonly stations: readonly StationView[];
  readonly timeline: Timeline | undefined;
  readonly freshness: Freshness;
  readonly actions: SessionActions | undefined;
  readonly shells: ShellActions | undefined;
  readonly terminals: TerminalStore | undefined;
  readonly watch: Watch | undefined;
  readonly layout: Layout;
  readonly route: SessionRouteValue;
  readonly navigate: Navigate;
  readonly onTerminalOpened?: (() => void) | undefined;
  readonly onNewShell?: (() => void) | undefined;
  readonly fleet?: FleetActions | undefined;
};

/**
 * All props of the session pane. The tab comes from the route and `onTab` writes it back;
 * `onBack` exists on phone and tablet only (the desktop sidebar is always there).
 */
export function buildPaneProps(input: BuildInput): PaneProps {
  const { route, navigate, layout, onTerminalOpened, onNewShell, ...rest } = input;
  return {
    ...rest,
    layout,
    tab: route.tab,
    // A tab change replaces the history entry: Back leaves the session. It drops `agent`.
    onTab: (tab) => {
      const { agent: _agent, ...rest } = route;
      navigate({ ...rest, tab }, { replace: true });
    },
    ...(route.agent === undefined ? {} : { agent: route.agent }),
    // An agent change replaces the history entry, like a tab change.
    onAgent: (nodeId) => {
      const { agent: _agent, ...rest } = route;
      navigate(
        nodeId === null ? { ...rest, tab: 'chat' } : { ...rest, tab: 'chat', agent: nodeId },
        {
          replace: true,
        },
      );
    },
    ...(layout === 'desktop'
      ? {}
      : {
          onBack: () => {
            if (canGoBack()) window.history.back();
            else navigate({ name: 'home' });
          },
        }),
    ...(onTerminalOpened === undefined ? {} : { onTerminalOpened }),
    ...(onNewShell === undefined ? {} : { onNewShell }),
  };
}

type Props = {
  readonly session: DeckSession;
  readonly stations: readonly StationView[];
  readonly timeline: Timeline | undefined;
  readonly freshness: Freshness;
  readonly actions: SessionActions | undefined;
  readonly shells: ShellActions | undefined;
  readonly terminals: TerminalStore | undefined;
  readonly summaryWatch: ((key: SessionKey) => Watch) | undefined;
  readonly layout: Layout;
  readonly route: SessionRouteValue;
  readonly navigate: Navigate;
  readonly onTerminalOpened?: (() => void) | undefined;
  readonly onNewShell?: (() => void) | undefined;
  readonly fleet?: FleetActions | undefined;
};

/** One open session: computes the pane props and renders the session view. */
export function SessionRoute({ summaryWatch, ...props }: Props) {
  const { station, session: id } = props.route;
  const watch = useMemo(
    () => summaryWatch?.({ station_id: station, session_id: id }),
    [summaryWatch, station, id],
  );
  const paneProps = buildPaneProps({ ...props, watch });
  return <SessionView key={`${station}/${id}`} {...paneProps} />;
}
