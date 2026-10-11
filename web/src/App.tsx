import { FleetBoard } from './board/FleetBoard';
import type { SessionSummary } from './gen';

type Props = {
  readonly sessions?: readonly SessionSummary[];
};

/** The board shell. The deck connection comes in Phase 4 (`src/runtime/`). */
export function App({ sessions = [] }: Props) {
  return (
    <div className="fd-app">
      <header className="fd-header">
        <span className="fd-wordmark">FLEET DECK</span>
      </header>
      <FleetBoard sessions={sessions} />
    </div>
  );
}
