// The key hint of a button that ⌘↵ (Ctrl+↵) presses: the New session Start button and the Composer
// "Steer now" button show the key of the platform of the person.

/** "⌘↵" on an Apple system, "Ctrl+↵" on every other system (Linux, Windows). */
export const submitKeyHint = (): string =>
  typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform ?? '')
    ? '⌘↵'
    : 'Ctrl+↵';
