import type { SpaceView } from '../gen';
import { openShare } from './shareStore';

/** The longest name of a space, in characters (`MAX_SPACE_NAME_CHARS` of the proto). */
export const MAX_SPACE_NAME_CHARS = 80;

/** "Share space…": opens the share dialog for a space. */
export function openSpaceShare(space: SpaceView): void {
  openShare({ kind: 'space', target: space.space_id, label: space.name });
}
