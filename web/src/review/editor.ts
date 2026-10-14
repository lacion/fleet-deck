// DL-179: the state of the comment editor of a diff: where it is and which lines it covers.
import type { ReviewSide } from '../gen';

/** A comment covers at most this many lines (`max - min` must stay below it). */
export const MAX_RANGE_LINES = 200;

export const RANGE_TOO_LONG = 'A comment covers at most 200 lines.';
export const RANGE_HINT = 'Tap another line number in this file to select a range.';

/** A line number that was clicked. */
export type Spot = { readonly side: ReviewSide; readonly path: string; readonly line: number };

/** The open editor: the lines `start..end` of `side` of `path`; `first` is the line that opened it. */
export type EditorState = {
  readonly side: ReviewSide;
  readonly path: string;
  readonly first: number;
  readonly start: number;
  readonly end: number;
};

export const openEditor = (spot: Spot): EditorState => ({
  side: spot.side,
  path: spot.path,
  first: spot.line,
  start: spot.line,
  end: spot.line,
});

/**
 * The editor after a click (or a shift-click) on a line number. No editor: it opens there. Another
 * line of the same file and side: the range runs from the first line to the clicked line. Another
 * file or side: the editor moves there when its text is empty, else nothing happens.
 */
export function nextEditor(
  state: EditorState | null,
  spot: Spot,
  hasText: boolean,
): EditorState | null {
  if (state === null) return openEditor(spot);
  if (state.path === spot.path && state.side === spot.side) {
    return {
      ...state,
      start: Math.min(state.first, spot.line),
      end: Math.max(state.first, spot.line),
    };
  }
  return hasText ? state : openEditor(spot);
}

export const rangeTooLong = (state: EditorState): boolean =>
  state.end - state.start >= MAX_RANGE_LINES;

/** "Line 5" or "Lines 5–8", with " (old side)" for the old side. */
export function editorTitle(state: EditorState): string {
  const lines =
    state.start === state.end ? `Line ${state.start}` : `Lines ${state.start}–${state.end}`;
  return state.side === 'old' ? `${lines} (old side)` : lines;
}

/** The label of a line number button. */
export const spotLabel = (spot: Spot): string =>
  `Comment on ${spot.path} line ${spot.line}${spot.side === 'old' ? ' (old side)' : ''}`;
