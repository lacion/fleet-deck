// DL-172, DL-173: the settings and account calls reach the pages, the Sidebar and the project form
// through one context. It is present only for the owner's login.
import { createContext, type ReactNode, useContext } from 'react';
import { type AccountActions } from '../accounts/actions';
import type { DeleteActions } from '../artifacts/DeleteArtifact';
import type { DesktopActions, DisplayActions } from '../displays/actions';
import type { CaptureActions } from '../desktop/capture';
import type { TailnetActions } from '../tailnet/api';
import type { SettingsActions } from './actions';
import type { DesktopToolsActions } from './desktopTools';
import type { OmarchyActions } from './omarchy';

export type SettingsEnv = {
  readonly settings: SettingsActions;
  readonly accounts: AccountActions;
  readonly tailnet: TailnetActions;
  /** The wall displays and the desktop relay (R13 P2c, P3c). */
  readonly displays: DisplayActions;
  readonly desktop: DesktopActions;
  /** The owner screenshot and the artifact deletion (R13 P5a). */
  readonly capture: CaptureActions;
  readonly deletion: DeleteActions;
  /** The Omarchy card of a station tab (Omarchy admin task D); absent in tests that need none. */
  readonly omarchy?: OmarchyActions;
  /** The desktop tools card of a station tab (BG-1); absent in tests that need none. */
  readonly desktopTools?: DesktopToolsActions;
};

const Context = createContext<SettingsEnv | undefined>(undefined);

/** The calls, or `undefined` for a login that is not the owner's (and in tests without a deck). */
export const useSettings = (): SettingsEnv | undefined => useContext(Context);

export function SettingsProvider({
  env,
  children,
}: {
  readonly env: SettingsEnv;
  readonly children: ReactNode;
}) {
  return <Context.Provider value={env}>{children}</Context.Provider>;
}
