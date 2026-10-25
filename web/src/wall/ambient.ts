// R13 P1 (design §4.4): the ambient view moves by a few pixels every 5 minutes, so that an OLED
// screen does not burn in. The offset is a pure function of the minute.

/** The wall goes to `ambient` after this many minutes with no change and no key press. */
export const AMBIENT_AFTER_MIN = 30;
/** The pixels that the content moves at most, on each axis. */
export const AMBIENT_MAX_PX = 8;
/** The minutes between two moves. */
export const AMBIENT_STEP_MIN = 5;

const SPOTS: readonly (readonly [number, number])[] = [
  [0, 0],
  [6, -4],
  [-8, 3],
  [4, 8],
  [-5, -7],
  [8, 2],
  [-3, 6],
  [7, -8],
];

/** The offset in px for `minute` (minutes since the epoch): one spot for each 5 minutes. */
export function ambientOffset(minute: number): { readonly x: number; readonly y: number } {
  const step = Math.floor(Math.max(0, minute) / AMBIENT_STEP_MIN);
  const spot = SPOTS[step % SPOTS.length] ?? SPOTS[0] ?? [0, 0];
  return { x: spot[0], y: spot[1] };
}
