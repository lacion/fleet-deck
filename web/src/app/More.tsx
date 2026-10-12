import { FileText, Icon, LogOut, Server, SquareTerminal } from '../ui/icons';
import type { Chrome } from './chrome';
import { PrefsRow } from './Prefs';

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
            <a href="#/plans">
              <Icon of={FileText} />
              Plans
            </a>
          </li>
          <li>
            <a href="#/stations">
              <Icon of={Server} />
              Stations
              <span className="fd-count">{stationCount}</span>
            </a>
          </li>
          <li>
            <a href="#/terminals">
              <Icon of={SquareTerminal} />
              Terminals
              <span className="fd-count">{terminalCount}</span>
            </a>
          </li>
        </ul>
      </nav>
      <PrefsRow />
      {chrome === undefined ? null : (
        <button type="button" className="fd-ghost fd-sm" onClick={chrome.onLogout}>
          <Icon of={LogOut} size={14} /> Log out
        </button>
      )}
      {chrome?.version === undefined ? null : (
        <p className="fd-sidebar-version" title="Deck version">
          deck {chrome.version}
        </p>
      )}
    </section>
  );
}
