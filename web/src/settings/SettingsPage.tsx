// DL-172, DL-173: `#/settings`: the Deck tab, one tab for each station and the Accounts tab.
import { Fragment, useEffect, useMemo, useRef, useState } from 'react';
import { AccountsTab } from '../accounts/AccountsTab';
import { DisplaysTab } from '../displays/DisplaysTab';
import type { DeckSession, StationView } from '../gen';
import type { Navigate, Route } from '../app/routes';
import type { ListWorkspaces } from '../ui/FolderPicker';
import { Page } from '../ui/page';
import { traitsText } from '../board/StationTraits';
import { TailnetCard } from '../tailnet/TailnetCard';
import { CARD_KEYS } from './model';
import { SettingsTab } from './SettingsTab';
import { type SettingsEnv, useSettings } from './context';

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

/**
 * The scroll position that shows `tab` whole, clear of the end fade, with a whole tab at the start
 * edge: the snap position of a tab start (its start less the scroll padding, settings.css), so that
 * the row never stops on a fragment of a word. Only the row scrolls, never the page.
 */
export function snapLeft(
  starts: readonly number[],
  tab: { readonly start: number; readonly end: number },
  view: { readonly left: number; readonly width: number; readonly max: number },
  pad: number,
): number {
  const clamp = (x: number) => Math.min(Math.max(x, 0), view.max);
  // The tab is in view and clear of the fade at the end: the row stays.
  if (tab.start - pad >= view.left - 1 && tab.end + TAB_FADE <= view.left + view.width + 1) {
    return view.left;
  }
  // Before the view: the tab starts the row.
  if (tab.start - pad < view.left) return clamp(tab.start - pad);
  // After the view: the first tab start from which the tab ends clear of the fade.
  const fits = starts.find((s) => tab.end + TAB_FADE - (s - pad) <= view.width);
  return clamp((fits ?? tab.start) - pad);
}

/** Scrolls the selected tab into the row (snapLeft), then marks the edges. */
function reveal(list: HTMLElement) {
  const tab = list.querySelector<HTMLElement>('[role="tab"][aria-selected="true"]');
  if (tab !== null) {
    const box = list.getBoundingClientRect();
    const at = (el: Element) => el.getBoundingClientRect().left - box.left + list.scrollLeft;
    const starts = [...list.querySelectorAll('[role="tab"]')].map(at);
    const start = at(tab);
    const pad = Number.parseFloat(getComputedStyle(list).scrollPaddingLeft) || 0;
    list.scrollLeft = snapLeft(
      starts,
      { start, end: start + tab.getBoundingClientRect().width },
      { left: list.scrollLeft, width: list.clientWidth, max: list.scrollWidth - list.clientWidth },
      pad,
    );
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

/**
 * The release of a build without its build stamp: `2.0.0-dev.20261022+4b29e272ae` → `2.0.0-dev`.
 * The commit after `+` and a date stamp of 8 digits or more go; a pre-release name stays (`rc.1`).
 */
export function versionText(version: string): string {
  const core = version.split('+')[0] ?? version;
  const dash = core.indexOf('-');
  if (dash < 0) return core;
  const pre = core
    .slice(dash + 1)
    .split('.')
    .filter((part) => !/^\d{8,}$/.test(part));
  return pre.length === 0 ? core.slice(0, dash) : `${core.slice(0, dash)}-${pre.join('.')}`;
}

/**
 * The head of a station tab: the reach glyph and the station name (a heading in the wide face),
 * the platform in words after it, and the build on a line of its own. The glyph says "online" (its
 * words are for screen readers only); an offline station says it in --hazard words. A line breaks
 * only after a middle dot, so that a line never starts with one.
 */
export function StationIdent({ station }: { readonly station: StationView }) {
  const traits = station.info?.traits;
  const version = station.info?.version ?? null;
  return (
    <div className="fd-set-ident">
      <span
        className="fd-dot"
        data-status={station.reachable ? 'online' : 'unreachable'}
        aria-hidden="true"
      />
      <p className="fd-set-ident-line">
        <span className="fd-wide fd-set-ident-name">{station.name}</span>
        {station.reachable ? (
          <span className="fd-vh"> online</span>
        ) : (
          <span className="fd-set-ident-off">offline</span>
        )}
        {traits === undefined || !Array.isArray(traits.labels) ? null : (
          <span className="fd-set-ident-facts">
            {traitsText(traits).replaceAll(' · ', '\u00a0· ')}
          </span>
        )}
      </p>
      {version === null ? null : (
        <p className="fd-set-ident-version" title={`Fleet Deck ${version}`}>
          Fleet Deck {versionText(version)}
        </p>
      )}
      {station.reachable ? null : (
        <p className="fd-set-ident-note">The settings show and save while the station is online.</p>
      )}
    </div>
  );
}

/**
 * The deck tab: its fields and the Tailnet card in the Network section. While the card has its
 * view it shows and saves `tailnet_auto` and `tailscale_cert`, and the field list leaves them out.
 * When its first read fails, the card shows only the error: the two fields come back, so that the
 * keys can still change. The card's reads pass through `watchView`, which tells the tab how they
 * end (the card keeps a view that it had when a later read fails).
 */
function DeckTab({
  env,
  live,
  list,
  sessions,
  stations,
}: {
  readonly env: SettingsEnv;
  readonly live: boolean;
  readonly list?: ListWorkspaces | undefined;
  readonly sessions?: readonly DeckSession[] | undefined;
  readonly stations: readonly StationView[];
}) {
  // null: the first read runs; true: the card has a view; false: the first read failed.
  const [ready, setReady] = useState<boolean | null>(null);
  const cardEnv = useMemo<SettingsEnv>(() => {
    const view = async () => {
      try {
        const answer = await env.tailnet.view();
        setReady(true);
        return answer;
      } catch (e) {
        setReady((cur) => (cur === true ? cur : false));
        throw e;
      }
    };
    return { ...env, tailnet: { ...env.tailnet, view } };
  }, [env]);
  return (
    <SettingsTab
      stationId={null}
      name="deck"
      actions={env.settings}
      live={live}
      list={list}
      sessions={sessions}
      stations={stations}
      slots={{ Network: { top: <TailnetCard env={cardEnv} /> } }}
      omit={ready === false ? [] : CARD_KEYS}
    />
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
      <DeckTab
        key="deck"
        env={env}
        live={live}
        list={list}
        sessions={sessions}
        stations={stations}
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
