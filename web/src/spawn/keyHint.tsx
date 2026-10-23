// The key hint of a button that ⌘↵ (Ctrl+Enter) presses: the New session Start button and the
// Composer "Steer now" button. Each key is one short chip, as every other key hint of the board
// (N, /, a, d): the modifier of the platform of the person, then Return. The Composer Send and
// Queue follow-up buttons show the Return chip alone.
import { CornerDownLeft, Icon } from '../ui/icons';

const applePlatform = (): boolean =>
  typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform ?? '');

/** The modifier of the start key: "⌘" on an Apple system, "Ctrl" on every other system. */
export const submitKeyMod = (): '⌘' | 'Ctrl' => (applePlatform() ? '⌘' : 'Ctrl');

/**
 * The Return key as a key chip: the Lucide `corner-down-left` icon in the square chip of an icon
 * key (`.fd-kbd-icon`). Mona Sans has no ↵ glyph (a fallback font drew it small and raised in its
 * chip).
 */
export function ReturnKey({ className = 'fd-kbd' }: { readonly className?: string }) {
  return (
    <span className={`${className} fd-kbd-icon`} aria-hidden="true">
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
