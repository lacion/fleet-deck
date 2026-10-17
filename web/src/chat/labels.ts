/** Who started a run; `continuation` is the station's restart input (ADR-007 §2.5). */
export function runLabel(origin: string): string {
  if (origin === 'provider') return 'Provider turn';
  if (origin === 'continuation') return 'Station continuation';
  if (origin === 'shell') return 'Shell command';
  return 'You';
}
