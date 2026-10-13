// DL-167 (R10): text that the station's model help wrote. The board labels it
// as generated, and a missing text is simply not shown (no error state).

/** The small mark next to a title that the model help set and the owner did not change. */
export function GeneratedMark() {
  return (
    <span className="fd-generated" title="Named by model help from the first prompt">
      generated
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
