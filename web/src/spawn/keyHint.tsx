// The key hint of a button that ⌘↵ (Ctrl+Enter) presses: the New session Start button and the
// Composer "Steer now" button. Each key is one short chip, as every other key hint of the board
// (N, /, a, d): the modifier of the platform of the person, then Return.
// `ui/icons.tsx` is the one icon set of the board; it does not export this icon yet, so this module
// takes it from the same package (as `integrations/icons.ts` does).
import { CornerDownLeft } from 'lucide-react';
import { Icon } from '../ui/icons';

const applePlatform = (): boolean =>
  typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform ?? '');

/** The modifier of the start key: "⌘" on an Apple system, "Ctrl" on every other system. */
export const submitKeyMod = (): '⌘' | 'Ctrl' => (applePlatform() ? '⌘' : 'Ctrl');

/**
 * The hint as one text: "⌘↵", or "Ctrl+Enter" off an Apple system.
 * @deprecated Use `SubmitKeys`; the Composer "Steer now" button still renders this text.
 */
export const submitKeyHint = (): string => (applePlatform() ? '⌘↵' : 'Ctrl+Enter');

/**
 * The Return key as a key chip: the Lucide `corner-down-left` icon. Mona Sans has no ↵ glyph (a
 * fallback font drew it small and raised in its chip).
 */
export function ReturnKey({ className = 'fd-kbd' }: { readonly className?: string }) {
  return (
    <span className={className} aria-hidden="true">
      <Icon of={CornerDownLeft} size={12} />
    </span>
  );
}

/**
 * The hint of the start key: two chips, the modifier and Return. The button names the keys for
 * assistive technology (`aria-keyshortcuts`); the chips are hidden from it.
 */
export function SubmitKeys({ className = 'fd-kbd' }: { readonly className?: string }) {
  return (
    <>
      <span className={className} aria-hidden="true">
        {submitKeyMod()}
      </span>
      <ReturnKey className={className} />
    </>
  );
}
