import type { Capabilities } from '../gen';
import { hasChanges } from './changes';
import type { FinalizedTurn } from './workspace';

/** What the row of a turn says about its files. */
export type TurnFiles =
  | { readonly kind: 'changes' }
  | { readonly kind: 'no_changes' }
  | { readonly kind: 'no_snapshot'; readonly reason: string | null };

export function turnFiles(t: FinalizedTurn): TurnFiles {
  if (t.diff !== null && hasChanges(t.diff)) return { kind: 'changes' };
  if (t.end?.state === 'unavailable') return { kind: 'no_snapshot', reason: t.end.reason };
  if (t.start?.state === 'unavailable') return { kind: 'no_snapshot', reason: t.start.reason };
  if (t.start === null || t.end === null) return { kind: 'no_snapshot', reason: null };
  return { kind: 'no_changes' };
}

/**
 * A quiet turn offers nothing to open or to revert: no diff, not reverted, and no revert (no
 * rollback, or no start snapshot: its "Revert to here" is disabled).
 */
export function quietTurn(t: FinalizedTurn, caps: Capabilities | null | undefined): boolean {
  return (
    turnFiles(t).kind !== 'changes' &&
    !t.reverted &&
    (caps?.rollback !== true || t.start?.state !== 'available')
  );
}

/** The fewest quiet turns in a row that fold. */
export const FOLD_MIN = 3;

export type TurnEntry =
  | { readonly kind: 'turn'; readonly index: number; readonly turn: FinalizedTurn }
  | {
      readonly kind: 'fold';
      /** 0-based list positions of the first and the last folded turn. */
      readonly first: number;
      readonly last: number;
      readonly turns: readonly { readonly index: number; readonly turn: FinalizedTurn }[];
    };

/**
 * The rows of the turn list: a run of FOLD_MIN or more quiet turns is one fold. The last turn of
 * the list never folds (it is the newest work).
 */
export function foldTurns(
  turns: readonly FinalizedTurn[],
  caps: Capabilities | null | undefined,
): TurnEntry[] {
  const out: TurnEntry[] = [];
  let run: { index: number; turn: FinalizedTurn }[] = [];
  const flush = () => {
    const head = run[0];
    const tail = run[run.length - 1];
    if (head !== undefined && tail !== undefined && run.length >= FOLD_MIN) {
      out.push({
        kind: 'fold',
        first: head.index,
        last: tail.index,
        turns: run,
      });
    } else {
      for (const r of run) out.push({ kind: 'turn', ...r });
    }
    run = [];
  };
  turns.forEach((turn, index) => {
    if (index < turns.length - 1 && quietTurn(turn, caps)) run.push({ index, turn });
    else {
      flush();
      out.push({ kind: 'turn', index, turn });
    }
  });
  flush();
  return out;
}

/**
 * The words after "Turns 4–9": one cause when every folded turn has it, else the kind.
 * `sharedBy`: the list says this reason once above (the row then says "No snapshot").
 */
export function foldText(
  turns: readonly FinalizedTurn[],
  sharedBy: (reason: string) => boolean,
  text: (reason: string) => string,
): string {
  const files = turns.map(turnFiles);
  if (files.every((f) => f.kind === 'no_changes')) return 'No file changes';
  if (files.every((f) => f.kind === 'no_snapshot')) {
    const reasons = new Set(files.map((f) => (f.kind === 'no_snapshot' ? f.reason : null)));
    const [only] = [...reasons];
    if (reasons.size === 1 && only !== null && only !== undefined) {
      return sharedBy(only) ? 'No snapshot' : text(only);
    }
    return 'No snapshot';
  }
  return 'No changes to show';
}
