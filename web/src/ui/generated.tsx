// DL-167 (R10): text that the station's model help wrote. The board marks it with the sparkles
// icon of model help, and a missing text is simply not shown (no error state).
import { Icon, Sparkles } from './icons';

/** The small mark next to a text that the model help wrote (a title that the owner did not change,
 * a summary): a 12px --faint sparkles icon with a name (a word pill read as a tag of the session). */
export function GeneratedMark({
  label = 'Named by model help',
  title = 'Named by model help from the first prompt',
}: {
  readonly label?: string;
  readonly title?: string;
} = {}) {
  return (
    <span className="fd-generated" role="img" aria-label={label} title={title}>
      <Icon of={Sparkles} size={12} />
    </span>
  );
}

/** The bullets of a digest, without the list marker; blank lines are dropped. */
export function digestLines(digest: string | null | undefined): string[] {
  if (digest === null || digest === undefined) return [];
  return digest
    .split('\n')
    .map((line) => line.replace(/^\s*[-*•]\s*/, '').trim())
    .filter((line) => line !== '');
}

/** The first line of a digest, for a card; `null` when there is none. */
export function digestLine(digest: string | null | undefined): string | null {
  return digestLines(digest)[0] ?? null;
}
