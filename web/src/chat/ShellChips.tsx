// B8: the results of finished `!` shell commands that the next prompt of a Claude session carries.
// The chips show above the composer; the owner removes the ones that should stay out.
import { useState } from 'react';
import type { ProviderKind } from '../gen';
import type { Item } from '../session/timeline';

export type ShellChip = { readonly id: string; readonly command: string };

const NODE_PREFIX = 'shell-';
/** The most results that one prompt carries (B8); the rest wait for the next prompt. */
const MAX_RESULTS = 64;

/** A shell item that has ended (a command that the session left unfinished has no result). */
const completed = (tool: Extract<Item, { type: 'tool' }>): boolean =>
  tool.status !== 'running' && !(tool.status === 'ended' && tool.shell === undefined);

/**
 * The pending results of a Claude session: every completed shell card after the start of the
 * latest board run (all of them when no board run exists yet). Codex keeps its own context.
 */
export function pendingResults(
  items: readonly Item[],
  provider: ProviderKind | null | undefined,
): ShellChip[] {
  if (provider !== 'claude') return [];
  const from = items.findLastIndex((i) => i.type === 'run' && i.origin === 'board') + 1;
  const out: ShellChip[] = [];
  for (const item of items.slice(from)) {
    if (item.type === 'tool' && item.node_id.startsWith(NODE_PREFIX) && completed(item)) {
      out.push({ id: item.node_id.slice(NODE_PREFIX.length), command: item.input });
    }
  }
  return out;
}

export type ShellChipState = {
  readonly chips: readonly ShellChip[];
  remove(id: string): void;
  /** The ids of the first 64 chips: the results that the next prompt carries. */
  toSend(): string[];
  /** The chips of `ids` went with a prompt: they show no more. */
  consume(ids: readonly string[]): void;
};

/** The chips of one session; what the owner removed or sent is kept in memory per session. */
export function useShellChips(
  sessionKey: string,
  items: readonly Item[],
  provider: ProviderKind | null | undefined,
): ShellChipState {
  const [gone, setGone] = useState<Readonly<Record<string, readonly string[]>>>({});
  const hidden = gone[sessionKey] ?? [];
  const chips = pendingResults(items, provider).filter((c) => !hidden.includes(c.id));
  const hide = (ids: readonly string[]) =>
    setGone((now) => ({ ...now, [sessionKey]: [...(now[sessionKey] ?? []), ...ids] }));
  return {
    chips,
    remove: (id) => hide([id]),
    toSend: () => chips.slice(0, MAX_RESULTS).map((c) => c.id),
    consume: hide,
  };
}

type Props = {
  readonly chips: readonly ShellChip[];
  readonly onRemove: (id: string) => void;
};

/** One chip per pending result, with a remove button. */
export function ShellChips({ chips, onRemove }: Props) {
  if (chips.length === 0) return null;
  return (
    <ul className="fd-shell-chips" aria-label="Shell results for the next prompt">
      {chips.map((chip) => (
        <li key={chip.id} className="fd-shell-chip">
          <span className="fd-shell-chip-text">
            Includes the output of <code>!{chip.command}</code>
          </span>
          <button
            type="button"
            className="fd-sm"
            aria-label={`Remove the output of !${chip.command}`}
            onClick={() => onRemove(chip.id)}
          >
            Remove
          </button>
        </li>
      ))}
    </ul>
  );
}
