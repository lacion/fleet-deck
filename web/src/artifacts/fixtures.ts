// Test fixtures of the artifacts board.
import type {
  Artifact,
  ArtifactPublished,
  ArtifactVersion,
  DeckArtifact,
  DeckArtifactDetail,
  ShareView,
} from '../gen';
import type { ArtifactActions } from './actions';

export const NOW = 1_760_000_000_000;

export const deckArtifact = (id: string, extra: Partial<DeckArtifact> = {}): DeckArtifact => ({
  station_id: 'station-a',
  station_name: 'Alpha',
  artifact_id: id,
  session_id: 's1',
  title: `Report ${id}`,
  kind: 'html',
  latest_version: 2,
  updated_at: NOW - 5 * 60_000,
  ...extra,
});

export const version = (
  id: string,
  n: number,
  files: string[] = ['index.html'],
): ArtifactVersion => ({
  artifact_id: id,
  version: n,
  size: 1200,
  sha256: 'ab'.repeat(32),
  files,
  created_at: NOW - n * 60_000,
});

export const share = (id: string, extra: Partial<ShareView> = {}): ShareView => ({
  share_id: id,
  version: null,
  created_at: NOW - 86_400_000,
  expires_at: NOW + 6 * 86_400_000,
  revoked: false,
  views: 3,
  ...extra,
});

export const detail = (
  id: string,
  extra: Partial<DeckArtifact> = {},
  shares: ShareView[] = [],
  files: string[] = ['index.html'],
): DeckArtifactDetail => {
  const artifact = deckArtifact(id, extra);
  return {
    artifact,
    versions: Array.from({ length: artifact.latest_version }, (_, n) => version(id, n + 1, files)),
    shares,
  };
};

export const published = (id: string, created = true): ArtifactPublished => {
  const a = deckArtifact(id);
  const artifact: Artifact = {
    artifact_id: id,
    station_id: a.station_id,
    session_id: a.session_id,
    title: a.title,
    kind: a.kind,
    latest_version: 1,
    created_at: NOW,
    updated_at: NOW,
    author: 'owner',
  };
  return { artifact, version: version(id, 1), created };
};

/** Actions that answer from fixtures and record each call. */
export function fakeActions(
  calls: string[] = [],
  over: Partial<ArtifactActions> = {},
): ArtifactActions {
  return {
    canPublish: true,
    canShare: true,
    list: async () => [deckArtifact('a1')],
    detail: async (_s, id) => detail(id),
    view: async () => ({ url: '/a/tok/', expires_at: NOW + 3_600_000 }),
    text: async () => '# Notes',
    publish: async () => published('a1'),
    share: async () => ({
      url: 'https://deck.test/a/tok/',
      share_id: 'sh1',
      version: null,
      expires_at: NOW + 7 * 86_400_000,
    }),
    revoke: async () => {
      calls.push('revoke');
    },
    ...over,
  };
}
