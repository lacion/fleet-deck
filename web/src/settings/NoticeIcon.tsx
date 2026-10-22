// The tone icon of a settings notice (`.fd-set-notice`): the tone shows by a shape and a color, not
// only by the 3px holder (WCAG 1.4.1: red and green holders; forced colors drop the holder).
// `ui/icons.tsx` is the one icon set of the board; it does not export Info, OctagonAlert and
// CircleCheck yet, so this module takes them from the same package (as integrations/icons.ts does).
import { CircleCheck, Info, OctagonAlert } from 'lucide-react';
import { Icon, type LucideIcon, TriangleAlert } from '../ui/icons';

/** The tones of a settings notice (the `data-tone` of `.fd-set-notice`). */
export type NoticeTone = 'info' | 'warn' | 'error' | 'ok';

export const TONE_ICON: Readonly<Record<NoticeTone, LucideIcon>> = {
  info: Info,
  warn: TriangleAlert,
  error: OctagonAlert,
  ok: CircleCheck,
};

/** The first child of a `.fd-set-notice`: 14px, hidden from assistive technology (the text says it). */
export function NoticeIcon({ tone }: { readonly tone: NoticeTone }) {
  return <Icon of={TONE_ICON[tone]} size={14} />;
}
