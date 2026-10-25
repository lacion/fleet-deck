// B17 §3.3.2 item 3 and 5: the pad of the Remote page. Every control is a plain button with an icon
// and an accessible name. The keys act only while the focus is inside the pad: the pad owns the
// one key handler, and the document has none. The component has no state: the page owns it.
import type { KeyboardEvent, ReactElement } from 'react';
import type { WallMode } from '../app/routes';
import type { WallArea } from '../gen';
import {
  ArrowDown,
  ArrowLeft,
  ArrowRight,
  ArrowUp,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  ChevronUp,
  CornerDownLeft,
  Icon,
  type LucideIcon,
  Undo2,
} from '../ui/icons';
import { type PadButton, scrollAreas } from './mirror';

export const MODE_NAME: Readonly<Record<WallMode, string>> = {
  mission: 'Mission',
  focus: 'Focus',
  review: 'Review',
  race: 'Race',
  artifact: 'Artifact',
  page: 'Page',
  ambient: 'Ambient',
};

/** The buttons of the pad that the page can disable, in reading order. The small scroll step has a
 * key only (↑ ↓), no button. */
export type ShownButton = Exclude<PadButton, 'line_up' | 'line_down'>;

export const PAD_BUTTONS: readonly ShownButton[] = [
  'previous_view',
  'next_view',
  'previous_item',
  'next_item',
  'open',
  'back',
  'scroll_up',
  'scroll_down',
  'top',
  'bottom',
];

export const KEYS_TEXT =
  '← → views · J K items · Enter open · Esc back · ↑ ↓ scroll a little · Page Up, Page Down a screen · Home, End · [ ] scroll area';

type Face = { readonly icon: LucideIcon; readonly name: string };

const FACE: Readonly<Record<ShownButton, Face>> = {
  previous_view: { icon: ArrowLeft, name: 'Previous view' },
  next_view: { icon: ArrowRight, name: 'Next view' },
  previous_item: { icon: ChevronLeft, name: 'Previous item' },
  next_item: { icon: ChevronRight, name: 'Next item' },
  open: { icon: CornerDownLeft, name: 'Open' },
  back: { icon: Undo2, name: 'Back' },
  scroll_up: { icon: ArrowUp, name: 'Scroll up' },
  scroll_down: { icon: ArrowDown, name: 'Scroll down' },
  top: { icon: ChevronUp, name: 'To the top' },
  bottom: { icon: ChevronDown, name: 'To the end' },
};

export type PadProps = {
  readonly mode: WallMode;
  /** The buttons that are disabled now. */
  readonly off: ReadonlySet<PadButton>;
  /** The scroll area in use (`null`: the mode has none). */
  readonly area: WallArea | null;
  readonly onPress: (button: ShownButton) => void;
  readonly onArea: (area: WallArea) => void;
  readonly onKeyDown: (event: KeyboardEvent<HTMLElement>) => void;
};

/** The pad. It has no hook: a test can call it as a function and read the handler of its root. */
export function Pad({ mode, off, area, onPress, onArea, onKeyDown }: PadProps): ReactElement {
  const button = (b: ShownButton) => (
    <button key={b} type="button" data-pad={b} disabled={off.has(b)} onClick={() => onPress(b)}>
      <Icon of={FACE[b].icon} /> {FACE[b].name}
    </button>
  );
  const areas = scrollAreas(mode);
  return (
    <div
      className="fd-panel fd-remote-pad"
      role="group"
      aria-label="Remote pad"
      onKeyDown={onKeyDown}
    >
      <div className="fd-remote-row fd-remote-views">
        {button('previous_view')}
        <span className="fd-remote-mode" aria-label="Current view">
          {MODE_NAME[mode]}
        </span>
        {button('next_view')}
      </div>
      <div className="fd-remote-row">
        {button('previous_item')}
        {button('next_item')}
        {button('open')}
        {button('back')}
      </div>
      {areas.length === 0 ? null : (
        <div className="fd-seg fd-remote-areas" role="group" aria-label="Scroll area">
          {areas.map((a) => (
            <button
              key={a.area}
              type="button"
              data-area={a.area}
              aria-pressed={area === a.area}
              onClick={() => onArea(a.area)}
            >
              {a.label}
            </button>
          ))}
        </div>
      )}
      <div className="fd-remote-row">
        {button('scroll_up')}
        {button('scroll_down')}
        {button('top')}
        {button('bottom')}
      </div>
    </div>
  );
}
