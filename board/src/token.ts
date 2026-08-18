// React adapter over the board's framework-free token store.
//
// Transport code imports tokenStore.ts directly, so root-level daemon tests do
// not need React installed. UI callers keep this stable public surface.

import { useSyncExternalStore } from 'react';
import { authSnapshot, subscribeAuth, type AuthState } from './tokenStore.ts';

export {
  authHeaders,
  hasToken,
  initToken,
  markUnauthorized,
  saveToken,
  wsUrl,
} from './tokenStore.ts';

export function useAuth(): AuthState {
  return useSyncExternalStore(subscribeAuth, authSnapshot);
}
