// Scenes shared by the desktop spec and the screenshots: three sessions on two
// stations, one of them waiting for an answer.
import type { DeckSession, OpenRequest } from '../src/gen';
import { claudeCaps } from '../src/testing/capabilities';
import { summary } from '../src/testing/summary';
import { FakeDeck, stationView } from './fake-deck';

export const ALPHA = 'st-alpha';
export const BETA = 'st-beta';
export const NOW = 1_760_000_000_000;

export const bashRequest: OpenRequest = {
  request_id: 'req-1',
  node_id: 'node-9',
  request_kind: 'command',
  title: 'Bash',
  details: 'rm -rf build',
  input: { command: 'rm -rf build' },
  options: [
    { option_id: 'allow', label: 'Allow' },
    { option_id: 'deny', label: 'Deny' },
  ],
  response: 'rpc_reply',
  requested_at: NOW,
};

function one(
  station: string,
  id: string,
  title: string,
  status: DeckSession['status'],
  extra: Partial<DeckSession['summary']> = {},
): DeckSession {
  return {
    station_id: station,
    status,
    summary: summary(id, status === 'needs_you' ? 'needs_you' : 'working', NOW, {
      title,
      cwd: '/work/repo',
      branch: 'main',
      model: 'claude-sonnet-4-5',
      capabilities: claudeCaps(),
      mode: 'default',
      current_run: status === 'idle' ? null : 'run-1',
      ...extra,
    }),
  };
}

/** Alpha: "Review the repo" needs you, "Write the docs" works. Beta: "Fix the build" is idle. */
export function threeSessions(): DeckSession[] {
  return [
    one(ALPHA, 's1', 'Review the repo', 'needs_you', { open_requests: [bashRequest] }),
    one(ALPHA, 's2', 'Write the docs', 'working'),
    one(BETA, 's3', 'Fix the build', 'idle', { activity: 'idle' }),
  ];
}

/** A deck with the two stations and the three sessions. */
export function threeSessionDeck(): FakeDeck {
  const deck = new FakeDeck();
  deck.stations = [stationView(ALPHA, 'Alpha'), stationView(BETA, 'Beta')];
  deck.sessions = threeSessions();
  return deck;
}
