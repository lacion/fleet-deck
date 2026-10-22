// DL-172, DL-173: `#/settings`: the Deck tab, one tab for each station and the Accounts tab.
import { Fragment, useEffect, useRef } from 'react';
import { AccountsTab } from '../accounts/AccountsTab';
import { DisplaysTab } from '../displays/DisplaysTab';
import type { DeckSession, StationView } from '../gen';
import type { Navigate, Route } from '../app/routes';
import type { ListWorkspaces } from '../ui/FolderPicker';
import { Page } from '../ui/page';
import { traitsText } from '../board/StationTraits';
import { TailnetCard } from '../tailnet/TailnetCard';
import { SettingsTab } from './SettingsTab';
import { useSettings } from './context';

export type SettingsRoute = Extract<Route, { name: 'settings' }>;

type Props = {
  readonly route: SettingsRoute;
  readonly stations: readonly StationView[];
  readonly navigate: Navigate;
  /** The board has a live connection to the deck. */
  readonly live: boolean;
  readonly list?: ListWorkspaces | undefined;
  readonly sessions?: readonly DeckSession[] | undefined;
};

type TabLink = {
  readonly id: string;
  readonly label: string;
  readonly route: SettingsRoute;
  /** A station tab: whether its station has a live link. */
  readonly reachable?: boolean;
};

/** The tabs of the page: Deck, the stations by name, Displays, Accounts. */
export function tabsOf(stations: readonly StationView[]): TabLink[] {
  return [
    { id: 'deck', label: 'Deck', route: { name: 'settings', tab: 'deck' } },
    ...stations.map(
      (s): TabLink => ({
        id: `station:${s.station_id}`,
        label: s.name,
        route: { name: 'settings', tab: 'station', station: s.station_id },
        reachable: s.reachable,
      }),
    ),
    { id: 'displays', label: 'Displays', route: { name: 'settings', tab: 'displays' } },
    {
      id: 'accounts',
      label: 'Accounts',
      route: { name: 'settings', tab: 'accounts', station: null, finish: null, oauthError: null },
    },
  ];
}

export const tabId = (route: SettingsRoute): string =>
  route.tab === 'station' ? `station:${route.station}` : route.tab;

/** The width of the fade at an edge of the tab row while it scrolls (settings.css). */
const TAB_FADE = 32;

/** Marks the edges of a scrolled tab row that hide tabs: the CSS fades only those edges. */
function markEdges(list: HTMLElement) {
  const more = list.scrollWidth - list.clientWidth;
  const start = more > 1 && list.scrollLeft > 1;
  const end = more > 1 && list.scrollLeft < more - 1;
  if (start) list.dataset['fadeStart'] = 'true';
  else delete list.dataset['fadeStart'];
  if (end) list.dataset['fadeEnd'] = 'true';
  else delete list.dataset['fadeEnd'];
}

/** Scrolls the selected tab into the row, clear of the edge fades, then marks the edges. */
function reveal(list: HTMLElement) {
  const tab = list.querySelector<HTMLElement>('[role="tab"][aria-selected="true"]');
  if (tab !== null) {
    const box = list.getBoundingClientRect();
    const at = tab.getBoundingClientRect();
    if (at.left < box.left + TAB_FADE) list.scrollLeft += at.left - box.left - TAB_FADE;
    else if (at.right > box.right - TAB_FADE) list.scrollLeft += at.right - box.right + TAB_FADE;
  }
  markEdges(list);
}

export function SettingsTabs({
  route,
  stations,
  navigate,
}: {
  readonly route: SettingsRoute;
  readonly stations: readonly StationView[];
  readonly navigate: Navigate;
}) {
  const current = tabId(route);
  const row = useRef<HTMLDivElement>(null);
  const tabs = tabsOf(stations);
  // The station tabs come after the first render: the row is measured again when they change.
  const names = tabs.map((t) => t.label).join('\n');
  // A row wider than the page scrolls: the selected tab comes into view, clear of the edge fades.
  useEffect(() => {
    const list = row.current;
    if (list !== null && names !== '' && current !== '') reveal(list);
  }, [current, names]);
  // The edge fades follow the scroll; a new width of the row or of a tab (a web font that loads
  // late) shows the selected tab again.
  useEffect(() => {
    const list = row.current;
    if (list === null || names === '') return undefined;
    const update = () => markEdges(list);
    list.addEventListener('scroll', update, { passive: true });
    if (typeof ResizeObserver === 'undefined') {
      return () => list.removeEventListener('scroll', update);
    }
    const sized = new ResizeObserver(() => reveal(list));
    sized.observe(list);
    for (const tab of list.querySelectorAll('[role="tab"]')) sized.observe(tab);
    return () => {
      list.removeEventListener('scroll', update);
      sized.disconnect();
    };
  }, [names]);
  return (
    <div ref={row} className="fd-pane-tabs fd-set-tabs" role="tablist" aria-label="Settings">
      {tabs.map((t, i) => {
        // A rule divides the three kinds of tabs: the deck, the stations, the other tabs.
        const kind = t.reachable === undefined ? 'page' : 'station';
        const next = tabs[i + 1];
        const nextKind = next === undefined || next.reachable === undefined ? 'page' : 'station';
        const divide = next !== undefined && (i === 0 || kind !== nextKind);
        return (
          <Fragment key={t.id}>
            <button
              type="button"
              role="tab"
              id={`fd-set-tab-${t.id}`}
              aria-selected={current === t.id}
              aria-controls="fd-set-panel"
              onClick={() => navigate(t.route)}
            >
              {t.reachable === false ? (
                <span className="fd-dot" data-status="unreachable" aria-hidden="true" />
              ) : null}
              {t.label}
              {t.reachable === false ? <span className="fd-vh"> (offline)</span> : null}
            </button>
            {divide ? <span className="fd-set-tabsep" aria-hidden="true" /> : null}
          </Fragment>
        );
      })}
    </div>
  );
}

/** The line above a station tab: reach, name, platform and build (the Stations page facts). */
export function StationIdent({ station }: { readonly station: StationView }) {
  const traits = station.info?.traits;
  const version = station.info?.version ?? null;
  return (
    <p className="fd-set-ident">
      <span
        className="fd-dot"
        data-status={station.reachable ? 'online' : 'unreachable'}
        aria-hidden="true"
      />
      <span className="fd-wide">{station.name}</span>
      {traits === undefined || !Array.isArray(traits.labels) ? null : (
        <span>{traitsText(traits)}</span>
      )}
      <span>{station.reachable ? 'online' : 'offline'}</span>
      {version === null ? null : <span>Fleet Deck {version}</span>}
      {station.reachable ? null : (
        <span className="fd-set-ident-note">
          The settings show and save while the station is online.
        </span>
      )}
    </p>
  );
}

export function SettingsPage({ route, stations, navigate, live, list, sessions }: Props) {
  const env = useSettings();
  if (env === undefined) {
    return (
      <Page title="Settings">
        <p className="fd-note">Settings need the owner’s login.</p>
      </Page>
    );
  }
  const current = tabId(route);
  const station =
    route.tab === 'station' ? stations.find((s) => s.station_id === route.station) : undefined;
  let panel: React.ReactNode;
  if (route.tab === 'deck') {
    panel = (
      <SettingsTab
        key="deck"
        stationId={null}
        name="deck"
        actions={env.settings}
        live={live}
        list={list}
        sessions={sessions}
        stations={stations}
        slots={{ Network: { top: <TailnetCard env={env} /> } }}
      />
    );
  } else if (route.tab === 'station') {
    panel =
      station === undefined ? (
        <p className="fd-note">This station is not on the board.</p>
      ) : (
        <>
          <StationIdent station={station} />
          <SettingsTab
            key={station.station_id}
            stationId={station.station_id}
            name={station.name}
            actions={env.settings}
            live={live && station.reachable}
            omarchy={env.omarchy}
            desktopTools={env.desktopTools}
            list={list}
            sessions={sessions}
            monitors={station.info?.desktop?.monitors}
            os={station.info?.traits?.os}
          />
        </>
      );
  } else if (route.tab === 'displays') {
    panel = <DisplaysTab env={env} stations={stations} live={live} />;
  } else {
    panel = (
      <AccountsTab route={route} stations={stations} navigate={navigate} actions={env.accounts} />
    );
  }
  return (
    <Page
      title="Settings"
      className="fd-settings"
      description="Change how this deck and its stations run. A saved change applies at once, unless the setting says it applies after a restart."
    >
      <SettingsTabs route={route} stations={stations} navigate={navigate} />
      <div id="fd-set-panel" role="tabpanel" aria-labelledby={`fd-set-tab-${current}`}>
        {panel}
      </div>
    </Page>
  );
}
