// Structured attention states for a pane that exists but cannot safely be
// nudged. Keep the human-facing note and the machine-facing snapshot value in
// one place: the board must not infer a security state by regexing prose.

export type SpawnAttention = 'folder-trust' | 'pane-unreadable';

export const SPAWN_FOLDER_TRUST_NOTE =
  'waiting on the folder-trust dialog — approve it in the terminal';
export const SPAWN_PANE_UNREADABLE_NOTE =
  'no bring-up keystroke sent — pane unreadable; check the terminal';

export function spawnAttentionForNote(note: string | null | undefined): SpawnAttention | null {
  if (note === SPAWN_FOLDER_TRUST_NOTE) return 'folder-trust';
  if (note === SPAWN_PANE_UNREADABLE_NOTE) return 'pane-unreadable';
  return null;
}
