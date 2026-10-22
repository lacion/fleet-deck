// The props of the spawn form and the pointer check, moved out of
// `SpawnForm.tsx` for the module size rule.
import { useSyncExternalStore } from 'react';
import type { DeckSession, SessionKey, StationView, WorkspaceListing } from '../gen';
import type { Freshness } from '../runtime/state';
import type { SpawnActions } from './spawn';

export type SpawnFormProps = {
  readonly stations: readonly StationView[];
  readonly freshness: Freshness;
  readonly actions?: SpawnActions | undefined;
  readonly onOpen: (key: SessionKey) => void;
  /** The listing shown at the first render (tests). */
  readonly initialListing?: WorkspaceListing | null;
  /** The first prompt at the first render (a plan from the library, DL-301). */
  readonly initialPrompt?: string;
  /** DL-141: the station chosen at the first render (an unrouted task). */
  readonly initialStation?: string | undefined;
  /** R3: the directory chosen at the first render ("Fix checks" of a project). */
  readonly initialCwd?: string | undefined;
  /** DL-176: the workspace choice at the first render ("New session in this project": worktree). */
  readonly initialPlace?: 'here' | 'worktree' | 'clone' | undefined;
  /** The repository field at the first render (tests). */
  readonly initialRepo?: string | undefined;
  /** The branch field of a new worktree at the first render (tests). */
  readonly initialBranch?: string | undefined;
  /** DL-141: the fleet, for the recent directories of a station. */
  readonly sessions?: readonly DeckSession[] | undefined;
  /** Unix ms of now for the reset times of the limits (tests). */
  readonly now?: number | undefined;
};

/** True on a phone or a tablet, where the first field must not open the keyboard by itself. */
export const coarsePointer = (): boolean =>
  typeof window !== 'undefined' && window.matchMedia?.('(pointer: coarse)').matches === true;

/** The phone layout of the sheet: the same width as the `max-width: 640px` rules of spawn.css. */
const NARROW = '(max-width: 640px)';

const subscribeNarrow = (cb: () => void): (() => void) => {
  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return () => {};
  const list = window.matchMedia(NARROW);
  list.addEventListener('change', cb);
  return () => list.removeEventListener('change', cb);
};

/** True while the sheet has its phone layout (sticky footer). Without a window: false. */
export function useNarrow(): boolean {
  return useSyncExternalStore(
    subscribeNarrow,
    () => typeof window.matchMedia === 'function' && window.matchMedia(NARROW).matches,
    () => false,
  );
}
