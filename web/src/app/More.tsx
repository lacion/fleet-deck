import type { Chrome } from './chrome';

type Props = {
  readonly stationCount: number;
  readonly terminalCount: number;
  readonly chrome?: Chrome | undefined;
};

/** The phone's "More" page: the pages that have no place in the bottom bar. */
export function More({ stationCount, terminalCount, chrome }: Props) {
  return (
    <section className="fd-screen fd-more" aria-label="More">
      <h1 className="fd-screen-title">More</h1>
      <nav aria-label="More pages">
        <ul className="fd-more-links">
          <li>
            <a href="#/plans">Plans</a>
          </li>
          <li>
            <a href="#/stations">Stations · {stationCount}</a>
          </li>
          <li>
            <a href="#/terminals">Terminals · {terminalCount}</a>
          </li>
        </ul>
      </nav>
      {chrome === undefined ? null : (
        <div className="fd-sidebar-conn" data-freshness={chrome.freshness}>
          <span role="status">{chrome.label}</span>
          <button type="button" onClick={chrome.onLogout}>
            Log out
          </button>
        </div>
      )}
    </section>
  );
}
