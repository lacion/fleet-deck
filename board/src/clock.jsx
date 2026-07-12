import React, { createContext, useContext } from 'react';
import { human } from './util.js';

// M-P4 — one 1 s clock, confined to the leaves that actually need 1 s
// resolution. App owns the `now` state (it re-renders once a second for the
// header clock), but the BOARD — BoardLanes + every SessionCard — is memoized
// and does NOT take `now` as a prop, so a tick no longer re-renders the whole
// wall of cards. Instead the age text is an <Age> leaf that reads `now` from
// this context: a context change re-renders only its consumers, even across a
// React.memo boundary, so exactly the age spans update each second and nothing
// else on the card does.
export const ClockContext = createContext(Date.now());

export function useNow() {
  return useContext(ClockContext);
}

// The one card element that needs 1 s resolution. `from` is the timestamp we
// age from; when it is missing we age from `now` itself, i.e. "0s" — matching
// the old `human(now - (lastSeen || startedAt || now))`.
export function Age({ from, className }) {
  const now = useNow();
  return <span className={className}>{human(now - (from || now))}</span>;
}
