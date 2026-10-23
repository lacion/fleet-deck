// The key hint of a button that ⌘↵ (Ctrl+Enter) presses: the New session Start button and the Composer
// "Steer now" button show the key of the platform of the person.

/**
 * "⌘↵" on an Apple system, "Ctrl+Enter" on every other system (Linux, Windows): the help overlay
 * says the same, and the ↵ glyph is not in Mona Sans (it drew small and raised after "Ctrl+").
 */
export const submitKeyHint = (): string =>
  typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform ?? '')
    ? '⌘↵'
    : 'Ctrl+Enter';
