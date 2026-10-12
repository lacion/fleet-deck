import type { Freshness } from '../runtime/state';

export type Notice = { readonly text: string; readonly tone: 'info' | 'error' };

/** What RuntimeApp tells the shell about the connection (DL-140). */
export type Chrome = {
  readonly label: string;
  readonly freshness: Freshness;
  readonly onLogout: () => void;
  readonly notices: readonly Notice[];
  /** The deck's version (DL-142), when the login answer carries it. */
  readonly version?: string | undefined;
};
