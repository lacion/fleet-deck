import type { CardStatus, DeckSession, StationView } from '../gen';
import { summary } from './summary';

export function card(
  id: string,
  status: CardStatus = 'working',
  stationId = 'station-a',
  extra: Partial<DeckSession['summary']> = {},
): DeckSession {
  return { station_id: stationId, status, summary: summary(id, 'working', 1, extra), unread: 0 };
}
export function station(id = 'station-a', extra: Partial<StationView> = {}): StationView {
  return {
    station_id: id,
    name: id === 'station-a' ? 'Alpha' : 'Beta',
    key: 'fingerprint',
    direction: 'station_dials',
    dial_url: null,
    reachable: true,
    last_seen: null,
    cursor: 0,
    info: null,
    info_at: null,
    ...extra,
  };
}
