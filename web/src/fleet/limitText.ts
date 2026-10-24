// B27 §3.7: the display of a grant cap. The deck stores "no limit" as the largest `u32`
// (`fleetdeck_proto::grant_limit::NO_LIMIT`); the board shows the words.

/** The cap of a grant that has no limit: the largest `u32`. Mirrors `grant_limit::NO_LIMIT`. */
export const NO_LIMIT = 4_294_967_295;

/** A cap as text: `no limit` for `NO_LIMIT`, else the number. */
export function limitText(n: number): string {
  return n === NO_LIMIT ? 'no limit' : String(n);
}
