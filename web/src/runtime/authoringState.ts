// NV-05: unsent drafts and legacy notes belong to one login. They go at every account change.

/** The `sessionStorage` key prefix of the composer drafts (see `session/Composer.tsx`). */
const DRAFT_PREFIX = 'fd-draft:';
/** The `localStorage` key prefix of the legacy review notes (see `chat/notes.ts`). */
const NOTES_PREFIX = 'fd-notes:';

function removePrefixed(open: () => Storage, prefix: string): void {
  try {
    const storage = open();
    const keys: string[] = [];
    for (let i = 0; i < storage.length; i++) {
      const key = storage.key(i);
      if (key?.startsWith(prefix)) keys.push(key);
    }
    for (const key of keys) storage.removeItem(key);
  } catch {
    // Storage can be missing or blocked. There is then nothing to clear.
  }
}

/** Remove every composer draft and every legacy note from this browser. */
export function clearAuthoringState(): void {
  removePrefixed(() => sessionStorage, DRAFT_PREFIX);
  removePrefixed(() => localStorage, NOTES_PREFIX);
}
