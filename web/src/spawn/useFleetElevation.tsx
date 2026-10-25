import type { ReactNode } from 'react';
import type { SettingsEnv } from '../settings/context';
import { type ElevationAsk, useElevation } from '../settings/elevation';

/** ADR-008: "Confirm it is you" for a fleet-control start; a login that is not the owner's never asks. */
// The settings context is fixed for the life of the login.
export function useFleetElevation(env: SettingsEnv | undefined): {
  readonly ask: ElevationAsk;
  readonly node: ReactNode;
} {
  if (env === undefined) return { ask: () => Promise.resolve(null), node: null };
  // biome-ignore lint/correctness/useHookAtTopLevel: the settings context is fixed for the life of the login.
  return useElevation(env.settings);
}
