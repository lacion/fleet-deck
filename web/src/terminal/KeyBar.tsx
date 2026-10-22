import type { ReactNode } from 'react';
import { ArrowDown, ArrowLeft, ArrowRight, ArrowUp, Icon, type LucideIcon } from '../ui/icons';

type Props = {
  /** Write bytes to the terminal stream. */
  readonly send: (bytes: string) => void;
  /** Ctrl is armed: the next typed character goes out as a control code. */
  readonly ctrl: boolean;
  readonly setCtrl: (on: boolean) => void;
  /** The Paste key: the terminal reads the clipboard and pastes it. */
  readonly onPaste: () => void;
};

/** The control byte of one typed character (Ctrl+<ch>); other characters stay as they are. */
export function ctrlByte(ch: string): string {
  if (/^[a-zA-Z]$/.test(ch)) return String.fromCharCode(ch.charCodeAt(0) & 0x1f);
  switch (ch) {
    case '@':
      return '\x00';
    case '[':
      return '\x1b';
    case '\\':
      return '\x1c';
    case ']':
      return '\x1d';
    case '^':
      return '\x1e';
    case '_':
      return '\x1f';
    case '?':
      return '\x7f';
    default:
      return ch;
  }
}

const KEYS: readonly { readonly label: string; readonly aria?: string; readonly bytes: string }[] =
  [
    { label: 'Esc', bytes: '\x1b' },
    { label: 'Tab', bytes: '\t' },
  ];

/** The arrows are Lucide icons (DESIGN-SYSTEM: one icon set); the aria-label names each. */
const ARROWS: readonly {
  readonly icon: LucideIcon;
  readonly aria: string;
  readonly bytes: string;
}[] = [
  { icon: ArrowUp, aria: 'Up', bytes: '\x1b[A' },
  { icon: ArrowDown, aria: 'Down', bytes: '\x1b[B' },
  { icon: ArrowLeft, aria: 'Left', bytes: '\x1b[D' },
  { icon: ArrowRight, aria: 'Right', bytes: '\x1b[C' },
];

/** After the arrows: paging and the characters that a touch keyboard hides. */
const MORE: readonly { readonly label: string; readonly aria?: string; readonly bytes: string }[] =
  [
    { label: 'PgUp', bytes: '\x1b[5~' },
    { label: 'PgDn', bytes: '\x1b[6~' },
    { label: 'Home', bytes: '\x1b[H' },
    { label: 'End', bytes: '\x1b[F' },
    { label: '|', bytes: '|' },
    { label: '/', bytes: '/' },
    { label: '~', bytes: '~' },
    { label: '-', bytes: '-' },
  ];

/** Keys that a touch keyboard lacks. A press never takes the focus from the terminal. */
export function KeyBar({ send, ctrl, setCtrl, onPaste }: Props) {
  const keep = (e: { preventDefault(): void }) => e.preventDefault();
  const key = (id: string, label: ReactNode, bytes: string, aria?: string) => (
    <button
      key={id}
      type="button"
      className="fd-keybar-key"
      aria-label={aria}
      onPointerDown={keep}
      onClick={() => send(bytes)}
    >
      {label}
    </button>
  );
  return (
    <div className="fd-keybar" role="toolbar" aria-label="Terminal keys">
      {KEYS.map((k) => key(k.label, k.label, k.bytes, k.aria))}
      <button
        type="button"
        className="fd-keybar-key"
        aria-pressed={ctrl}
        onPointerDown={keep}
        onClick={() => setCtrl(!ctrl)}
      >
        Ctrl
      </button>
      {ARROWS.map((k) => key(k.aria, <Icon of={k.icon} size={16} />, k.bytes, k.aria))}
      {MORE.map((k) => key(k.label, k.label, k.bytes, k.aria))}
      <button type="button" className="fd-keybar-key" onPointerDown={keep} onClick={onPaste}>
        Paste
      </button>
    </div>
  );
}
