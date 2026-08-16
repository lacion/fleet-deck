// The single wire-protocol version stamped on the snapshot envelope the daemon
// broadcasts, so the board, the daemon, and the future Bun binary can DETECT
// skew instead of silently misreading a field that moved or vanished. This is
// F1a's "schema_version on every shape, from day one": the number is the
// contract's own version, not the product version.
//
// Bump ONLY on an incompatible change to an emitted shape (a field removed,
// renamed, or retyped). Purely additive fields — a new optional key a consumer
// can ignore — do NOT require a bump; that is the whole point of tolerant
// consumers (`{ ...EMPTY, ...data }` on the board).
//
// Pure module: no node/bun/DOM globals, so it inlines into both the esbuild
// daemon bundle and the Vite board bundle with zero runtime cost.
export const WIRE_SCHEMA_VERSION = 1;
