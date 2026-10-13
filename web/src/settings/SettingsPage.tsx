// DL-172, DL-173: `#/settings`: the Deck tab, one tab for each station and the Accounts tab.
import { AccountsTab } from '../accounts/AccountsTab';
import type { DeckSession, StationView } from '../gen';
import type { Navigate, Route } from '../app/routes';
import type { ListWorkspaces } from '../ui/FolderPicker';
import { Page } from '../ui/page';
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

type TabLink = { readonly id: string; readonly label: string; readonly route: SettingsRoute };

/** The tabs of the page: Deck, the stations by name, Accounts. */
export function tabsOf(stations: readonly StationView[]): TabLink[] {
  return [
    { id: 'deck', label: 'Deck', route: { name: 'settings', tab: 'deck' } },
    ...stations.map(
      (s): TabLink => ({
        id: `station:${s.station_id}`,
        label: s.name,
        route: { name: 'settings', tab: 'station', station: s.station_id },
      }),
    ),
    {
      id: 'accounts',
      label: 'Accounts',
      route: { name: 'settings', tab: 'accounts', station: null, finish: null, oauthError: null },
    },
  ];
}

export const tabId = (route: SettingsRoute): string =>
  route.tab === 'station' ? `station:${route.station}` : route.tab;

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
  return (
    <div className="fd-pane-tabs fd-set-tabs" role="tablist" aria-label="Settings">
      {tabsOf(stations).map((t) => (
        <button
          key={t.id}
          type="button"
          role="tab"
          id={`fd-set-tab-${t.id}`}
          aria-selected={current === t.id}
          aria-controls="fd-set-panel"
          onClick={() => navigate(t.route)}
        >
          {t.label}
        </button>
      ))}
    </div>
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
      />
    );
  } else if (route.tab === 'station') {
    panel =
      station === undefined ? (
        <p className="fd-note">This station is not on the board.</p>
      ) : (
        <SettingsTab
          key={station.station_id}
          stationId={station.station_id}
          name={station.name}
          actions={env.settings}
          live={live && station.reachable}
          list={list}
          sessions={sessions}
        />
      );
  } else {
    panel = (
      <AccountsTab route={route} stations={stations} navigate={navigate} actions={env.accounts} />
    );
  }
  return (
    <Page
      title="Settings"
      className="fd-settings"
      description="Change how this deck and its stations run. A change applies at once unless it says it needs a restart."
    >
      <SettingsTabs route={route} stations={stations} navigate={navigate} />
      <div id="fd-set-panel" role="tabpanel" aria-labelledby={`fd-set-tab-${current}`}>
        {panel}
      </div>
    </Page>
  );
}
