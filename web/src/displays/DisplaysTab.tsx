// R13 P2c (design §5.2): Settings › Displays. The owner's wall displays first, with "Add a wall
// display" at the right of their heading (the add panel opens under the heading), then the
// pairings that wait for a code, only when there are some. It is the owner's page: the settings
// context is present only for the owner's login.
import { type ReactNode, useEffect, useId, useRef, useState } from 'react';
import type { StationView } from '../gen';
import type { SettingsEnv } from '../settings/context';
import { type ElevationAsk, useElevation } from '../settings/elevation';
import { NoticeIcon } from '../settings/NoticeIcon';
import { Icon, Monitor, Plus, RefreshCw } from '../ui/icons';
import { EmptyState } from '../ui/page';
import { useNow } from '../ui/useNow';
import { NO_DISPLAYS_TEXT } from './actions';
import { AddDisplay } from './AddDisplay';
import { DisplayRow } from './DisplayRow';
import { PendingPairings } from './PendingPairings';
import { type ListState, useDisplayList } from './useDisplayList';
import { WALL_PAGES_FEATURE, WallPages } from './WallPages';

/** The text of the button that opens the add panel (the heading row and the empty state). */
export const ADD_TEXT = 'Add a wall display';

/** The deck has the wall pages (`GET /api/features` lists `wall_pages.v1`). */
function useWallPagesFeature(capture: SettingsEnv['capture']): boolean {
  const [has, setHas] = useState(false);
  useEffect(() => {
    let live = true;
    capture.features().then(
      (list) => live && setHas(list?.includes(WALL_PAGES_FEATURE) === true),
      () => live && setHas(false),
    );
    return () => {
      live = false;
    };
  }, [capture]);
  return has;
}

export function DisplaysTab({
  env,
  stations,
  live,
}: {
  readonly env: SettingsEnv;
  readonly stations: readonly StationView[];
  /** The board has a live connection to the deck. */
  readonly live: boolean;
}) {
  const { state, refresh } = useDisplayList(env.displays, live);
  const elevation = useElevation(env.settings);
  const now = useNow(undefined, 30_000);
  const wallPages = useWallPagesFeature(env.capture);
  return (
    <DisplaysView
      wallPages={wallPages}
      state={state}
      refresh={refresh}
      env={env}
      stations={stations}
      ask={elevation.ask}
      now={now}
    >
      {elevation.node}
    </DisplaysView>
  );
}

/** The page for a state of the list (the tests render it without the deck). */
export function DisplaysView({
  state,
  refresh,
  env,
  stations,
  ask,
  now,
  wallPages = false,
  children,
}: {
  readonly state: ListState;
  readonly refresh: () => void;
  readonly env: Pick<SettingsEnv, 'displays' | 'settings'>;
  readonly stations: readonly StationView[];
  readonly ask: ElevationAsk;
  readonly now: number;
  /** The deck has the wall pages: "Show a page…" and the section "Wall pages" show. */
  readonly wallPages?: boolean;
  /** The elevation dialog. */
  readonly children?: ReactNode;
}) {
  const [adding, setAdding] = useState(false);
  const panelId = useId();
  const opener = useRef<HTMLButtonElement>(null);
  if (state.kind === 'none') {
    return <p className="fd-note">{NO_DISPLAYS_TEXT}.</p>;
  }
  const close = () => {
    setAdding(false);
    // The panel had the focus; it goes back to the button that opened it.
    requestAnimationFrame(() => opener.current?.focus());
  };
  const empty = state.kind === 'ready' && state.list.displays.length === 0 && !adding;
  // One button shows at a time: in the heading row, or in the empty state.
  const addButton = (
    <button
      type="button"
      className="fd-sm"
      ref={opener}
      aria-expanded={adding}
      aria-controls={adding ? panelId : undefined}
      onClick={() => (adding ? close() : setAdding(true))}
    >
      <Icon of={Plus} size={14} /> {ADD_TEXT}
    </button>
  );
  return (
    <div className="fd-disp">
      {children}
      <section aria-label="Wall displays" className="fd-disp-section">
        <header>
          <h2 className="fd-form-title fd-disp-title">Wall displays</h2>
          {empty ? null : addButton}
        </header>
        {adding ? (
          <AddDisplay
            id={panelId}
            actions={env.displays}
            settings={env.settings}
            ask={ask}
            stations={stations}
            onDone={refresh}
            onClose={close}
          />
        ) : null}
        {state.kind === 'loading' ? (
          <p className="fd-note fd-set-loading" role="status">
            Loading displays…
          </p>
        ) : null}
        {/* A failed read: the error row of every settings tab, with the one name of the action
            (§5: "Try again" everywhere). */}
        {state.kind === 'error' ? (
          <div className="fd-set-notice" data-tone="error" role="alert">
            <NoticeIcon tone="error" />
            <span>{state.text}</span>
            <button type="button" className="fd-sm" onClick={refresh}>
              <Icon of={RefreshCw} size={14} /> Try again
            </button>
          </div>
        ) : null}
        {empty ? (
          <EmptyState
            icon={Monitor}
            title="No wall display yet"
            body="Add one to show the fleet on a screen."
            action={addButton}
          />
        ) : null}
        {state.kind === 'ready' && state.list.displays.length > 0 ? (
          <ul className="fd-disp-list">
            {state.list.displays.map((d) => (
              <DisplayRow
                key={d.display_id}
                display={d}
                stations={stations}
                actions={env.displays}
                settings={env.settings}
                ask={ask}
                onDone={refresh}
                now={now}
                pages={wallPages}
              />
            ))}
          </ul>
        ) : null}
      </section>
      {state.kind === 'ready' ? (
        <PendingPairings
          pending={state.list.pending}
          displays={state.list.displays}
          stations={stations}
          actions={env.displays}
          settings={env.settings}
          ask={ask}
          onDone={refresh}
        />
      ) : null}
      {wallPages && state.kind === 'ready' ? (
        <WallPages actions={env.displays} settings={env.settings} ask={ask} />
      ) : null}
    </div>
  );
}
