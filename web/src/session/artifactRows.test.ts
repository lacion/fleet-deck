// The count of the session's Artifacts tab: the rows of its list, not the artifacts.
import { describe, expect, test } from 'bun:test';
import { deckArtifact } from '../artifacts/fixtures';
import { artifactRows } from './artifactRows';

describe('artifact rows of the Artifacts tab', () => {
  test('the copies of one title are one row', () => {
    const items = [
      deckArtifact('a', { title: 'phone-session.html', updated_at: 3 }),
      deckArtifact('b', { title: 'phone-session.html', updated_at: 2 }),
      deckArtifact('c', { title: 'phone-session.html', updated_at: 1 }),
      deckArtifact('d', { title: 'plan.md', updated_at: 4 }),
    ];
    expect(artifactRows(items)).toBe(2);
    expect(artifactRows([])).toBe(0);
  });

  test('an artifact that the owner deleted is not a row', () => {
    const items = [deckArtifact('a', { title: 'one' }), deckArtifact('b', { title: 'two' })];
    expect(artifactRows(items, new Set(['station-a/b']))).toBe(1);
  });
});
