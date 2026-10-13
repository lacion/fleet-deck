// DL-172, DL-173: the settings and account calls reach the pages, the Sidebar and the project form
// through one context. It is present only for the owner's login.
import { createContext, type ReactNode, useContext } from 'react';
import { type AccountActions } from '../accounts/actions';
import type { SettingsActions } from './actions';

export type SettingsEnv = {
  readonly settings: SettingsActions;
  readonly accounts: AccountActions;
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
