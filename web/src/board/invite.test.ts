import { describe, expect, test } from 'bun:test';
import { station } from '../testing/fleet';
import { joinedName } from './InviteView';

describe('the joined station name (B4)', () => {
  test('the line follows the station: first its key, then the real name', () => {
    const id = 'station-a';
    // The deck names a new station by its key fingerprint at first.
    expect(joinedName([station(id, { name: 'ad9dca5e76ffca69' })], id)).toBe('ad9dca5e76ffca69');
    // The real name arrives in the next update of the stations.
    expect(joinedName([station(id, { name: 'bravo' })], id)).toBe('bravo');
  });

  test('a station that is gone shows its id', () => {
    expect(joinedName([station('station-b')], 'station-a')).toBe('station-a');
  });
});
