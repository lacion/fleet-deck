import { useMemo } from 'react';
import type { DeckSession, SessionKey, StationView } from '../gen';
import type { Freshness, Timeline } from '../runtime/state';
import type { SessionActions } from '../session/commands';
import { SessionView } from '../session/SessionView';
import type { ShellActions } from '../shell/shell';
import type { Watch } from '../terminal/handover';
import type { TerminalStore } from '../terminal/store';
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
  readonly onBack?: () => void;
  readonly onTerminalOpened?: () => void;
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
};

/**
 * All props of the session pane. The tab comes from the route and `onTab` writes it back;
 * `onBack` exists on phone and tablet only (the desktop sidebar is always there).
 */
export function buildPaneProps(input: BuildInput): PaneProps {
  const { route, navigate, layout, onTerminalOpened, ...rest } = input;
  return {
    ...rest,
    layout,
    tab: route.tab,
    onTab: (tab) => navigate({ ...route, tab }),
    ...(layout === 'desktop' ? {} : { onBack: () => navigate({ name: 'home' }) }),
    ...(onTerminalOpened === undefined ? {} : { onTerminalOpened }),
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
};

/** One open session: computes the pane props and renders the session view. */
export function SessionRoute({ summaryWatch, ...props }: Props) {
  const { station, session: id } = props.route;
  const watch = useMemo(
    () => summaryWatch?.({ station_id: station, session_id: id }),
    [summaryWatch, station, id],
  );
  const paneProps = buildPaneProps({ ...props, watch });
  // DL-140 merge: spread paneProps once the new SessionView lands.
  return (
    <SessionView
      key={`${station}/${id}`}
      session={paneProps.session}
      stations={paneProps.stations}
      timeline={paneProps.timeline}
      freshness={paneProps.freshness}
      actions={paneProps.actions}
      onBack={paneProps.onBack ?? (() => props.navigate({ name: 'home' }))}
    />
  );
}
