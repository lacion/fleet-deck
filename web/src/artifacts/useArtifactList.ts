import { useEffect, useState } from 'react';
import type { DeckArtifact } from '../gen';
import type { ArtifactActions, ArtifactFilter } from './actions';

export type ListState =
  | { readonly kind: 'loading' }
  | { readonly kind: 'error'; readonly text: string }
  | { readonly kind: 'ok'; readonly items: readonly DeckArtifact[] };

const message = (e: unknown) => (e instanceof Error ? e.message : 'The request failed.');

/**
 * The deck's artifact list for a filter. It reads again when the filter changes, when `refresh`
 * changes (a session's artifact count) and when `retry` changes.
 */
export function useArtifactList(
  actions: ArtifactActions,
  filter: ArtifactFilter,
  refresh: number,
  retry: number,
): ListState {
  const [state, setState] = useState<ListState>({ kind: 'loading' });
  const { station = '', session = '', q = '' } = filter;
  useEffect(() => {
    let live = true;
    actions.list({ station, session, q }).then(
      (items) => live && setState({ kind: 'ok', items }),
      (e: unknown) => live && setState({ kind: 'error', text: message(e) }),
    );
    return () => {
      live = false;
    };
  }, [actions, station, session, q, refresh, retry]);
  return state;
}
