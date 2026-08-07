// Diagnosis for a terminal WebSocket that closed before its first frame.
//
// THE DIAGNOSTIC CONTRACT. A close with no frame before it is a REFUSED
// UPGRADE, and the daemon refuses one by destroying the socket — deliberately,
// so an unauthorized caller learns nothing, which also means the browser cannot
// tell 401 from "the network died". The board can still tell the human the one
// thing that distinguishes them: /ws/term is the only loopback route that
// demands the board key (gated since 0.16.0), and a board holding no key at all
// fails here and NOWHERE else — every other route on localhost is exempt, so
// the rest of the board looks perfectly healthy. Saying "connection closed" to
// that sent a user hunting a network fault for an afternoon.
//
// But "no key ⇒ needs a key" is only sound when the deployment actually DEMANDS
// one. FLEETDECK_PROXY_AUTH=trust and FLEETDECK_TRUST_LOOPBACK=on both
// authorize tokenless /ws/term upgrades (scripts/fleetd/http.mjs authorized()),
// and under either one the missing-key sentence sends the operator down an
// ineffective token-recovery path when the real fault is the proxy dropping the
// WebSocket upgrade or the transport dying. So the diagnosis is three-way,
// keyed on the daemon's OWN statement of its terminal-auth requirement:
//
//   local key held                  → "the key may be stale"
//   no local key, daemon gates term → "you need a key" (the 0.16.0 wording)
//   no local key, no gate           → deployment-neutral: name all three
//                                     conditional causes, in likeliest order
//
// The daemon says whether it gates via GET /health (auth.term_token, open to
// every caller). Pure by design — the fetch lives in TermPane, so this module
// loads under node --test with no bundler.

// No local key, and the daemon does not gate /ws/term on one: the refusal came
// from the transport, not from auth. Order the causes by likelihood and make
// each one conditional — the board cannot tell them apart, it can only stop
// swearing by the wrong one.
const NEUTRAL = 'the terminal connection was refused before it opened — possible causes: the proxy is not forwarding WebSocket upgrades, the daemon is unreachable, or the board key was rejected (reopen the board from its ?t=… URL)';

/**
 * The text for a pre-frame terminal close.
 * @param heldKey      does this board hold a key at all? (token.js hasToken)
 * @param daemonGates  true/false from the daemon's /health capability;
 *                     null/undefined when /health could not be asked (old
 *                     daemon, or the fetch itself failed) — the historical
 *                     key-based inference is the safe fallback there.
 */
export function refusedUpgradeText(heldKey, daemonGates) {
  if (heldKey || daemonGates !== false) {
    return heldKey
      ? 'the daemon refused this viewer before it opened — the board key may be stale (reopen the board from its ?t=… URL)'
      : 'this board has no key, and a live terminal is the one thing that needs one — reopen the board from its ?t=… URL (`fleetdeck token`)';
  }
  return NEUTRAL;
}

/** Give up after this many consecutive failed reconnects. */
export const MAX_RECONNECT = 5;

/**
 * What should a terminal viewer do when its socket closes?
 *
 * A pane that has ENDED is final — the agent is gone and the frozen screen is
 * the record. But a pane whose transport merely dropped is NOT: the daemon
 * restarts on every upgrade (version takeover), a laptop sleeps, a tailnet
 * re-routes. Before this, any such blip latched the pane dead — `end()` set
 * disableStdin and the socket effect only re-runs on a spawn change, so the
 * terminal kept rendering its last screen while silently refusing every
 * keystroke, with a one-line strip as the only clue. Reconnecting is the
 * honest response, and it is safe: if the agent really did end, the fresh
 * viewer is told so and settles on the exit path instead.
 *
 * Split out as a pure function so the policy is testable without a DOM.
 *
 * @param sawFrames  did this socket ever deliver a frame? false ⇒ the upgrade
 *                   itself was refused (auth/proxy) — retrying cannot fix that,
 *                   so diagnose instead of hammering the daemon.
 * @param attempts   consecutive reconnects already tried for this pane.
 * @returns {{action:'retry',delayMs:number}|{action:'give-up'}|{action:'diagnose'}}
 */
export function reconnectPlan(sawFrames, attempts = 0, max = MAX_RECONNECT) {
  if (!sawFrames) return { action: 'diagnose' };
  if (!(attempts < max)) return { action: 'give-up' };
  // Exponential backoff, capped: a daemon restart is back in ~1s, but a fleet
  // of open tiles must not become a reconnect storm against a daemon that is
  // still coming up.
  return { action: 'retry', delayMs: Math.min(500 * 2 ** attempts, 5000) };
}
