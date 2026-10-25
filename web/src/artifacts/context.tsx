// R12 (DL-165): the artifact calls reach the session view, the chat and the Files panel
// through one context, so that no prop crosses the session route.
import { createContext, useContext } from 'react';
import type { ArtifactActions } from './actions';

export type ArtifactsEnv = {
  readonly actions: ArtifactActions;
  /** The session that the surrounding view shows: a publish and a chip belong to it. */
  readonly session?: { readonly station: string; readonly session: string };
  /** Opens the viewer of the session's Artifacts tab (a chat chip calls it). */
  readonly open?: (id: string, version: number) => void;
  /**
   * `<id>@<version>` of the artifacts that a "Keep as artifact" chip published. The event of that
   * publish would show a second chip; the chat hides it.
   */
  readonly kept?: ReadonlySet<string>;
  readonly keep?: (key: string) => void;
};

/** The key of one version in `kept`. */
export const keptKey = (id: string, version: number): string => `${id}@${version}`;

const Context = createContext<ArtifactsEnv | undefined>(undefined);

export const ArtifactsProvider = Context.Provider;

/** The artifact calls, or `undefined` when the board has no deck. */
export const useArtifacts = (): ArtifactsEnv | undefined => useContext(Context);
