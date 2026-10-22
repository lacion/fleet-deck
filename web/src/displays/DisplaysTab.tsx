// R13 P2c (design §5.2): Settings › Displays. Add a wall display, approve the pairings that wait
// for a code, and manage each display. It is the owner's page: the settings context is present
// only for the owner's login.
import type { StationView } from '../gen';
import type { SettingsEnv } from '../settings/context';
import { useElevation } from '../settings/elevation';
import { useNow } from '../ui/useNow';
import { NO_DISPLAYS_TEXT } from './actions';
import { AddDisplay } from './AddDisplay';
import { DisplayRow } from './DisplayRow';
import { PendingPairings } from './PendingPairings';
import { useDisplayList } from './useDisplayList';

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
  if (state.kind === 'none') {
    return <p className="fd-note">{NO_DISPLAYS_TEXT}.</p>;
  }
  return (
    <div className="fd-disp">
      {elevation.node}
      <AddDisplay
        actions={env.displays}
        settings={env.settings}
        ask={elevation.ask}
        stations={stations}
        onDone={refresh}
      />
      {state.kind === 'loading' ? <p role="status">Loading…</p> : null}
      {state.kind === 'error' ? (
        <p role="alert" className="fd-fielderror">
          {state.text}{' '}
          <button type="button" className="fd-ghost" onClick={refresh}>
            Retry
          </button>
        </p>
      ) : null}
      {state.kind === 'ready' ? (
        <>
          <PendingPairings
            pending={state.list.pending}
            displays={state.list.displays}
            stations={stations}
            actions={env.displays}
            settings={env.settings}
            ask={elevation.ask}
            onDone={refresh}
          />
          <section aria-label="Wall displays" className="fd-disp-section">
            <h2 className="fd-form-title fd-disp-title">Wall displays</h2>
            {state.list.displays.length === 0 ? (
              <p className="fd-note">No wall display yet.</p>
            ) : (
              <ul className="fd-disp-list">
                {state.list.displays.map((d) => (
                  <DisplayRow
                    key={d.display_id}
                    display={d}
                    stations={stations}
                    actions={env.displays}
                    settings={env.settings}
                    ask={elevation.ask}
                    onDone={refresh}
                    now={now}
                  />
                ))}
              </ul>
            )}
          </section>
        </>
      ) : null}
    </div>
  );
}
