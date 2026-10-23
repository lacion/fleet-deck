// The status filter of the live sessions (DESIGN-SYSTEM §4.3): one segmented control for one
// filter, the same on the Fleet and the Watch headers (All, Working, Idle, Needs you).
import type { DeckSession } from '../gen';

/** The status filter of a page header; not stored. */
export type StatusFilter = 'all' | 'working' | 'idle' | 'needs_you';

export const STATUS_FILTERS: readonly { readonly id: StatusFilter; readonly label: string }[] = [
  { id: 'all', label: 'All' },
  { id: 'working', label: 'Working' },
  { id: 'idle', label: 'Idle' },
  { id: 'needs_you', label: 'Needs you' },
];

/** True when `s` passes the status filter: Working holds a running, checking or queued
 * session; Needs you holds a session with an open request. */
export function statusMatches(s: DeckSession, status: StatusFilter): boolean {
  if (status === 'all') return true;
  if (status === 'needs_you') return s.status === 'needs_you' || s.summary.open_requests.length > 0;
  if (status === 'idle') return s.status === 'idle';
  return s.status === 'working' || s.status === 'verifying' || s.status === 'queued';
}

/** The status segments (`.fd-seg`, base.css): the pressed one is the filter. */
export function StatusSeg({
  value,
  onChange,
  className,
}: {
  readonly value: StatusFilter;
  readonly onChange: (next: StatusFilter) => void;
  /** An extra class of the group (its place in a header). */
  readonly className?: string | undefined;
}) {
  return (
    <div
      className={className === undefined ? 'fd-seg' : `fd-seg ${className}`}
      role="group"
      aria-label="Status"
    >
      {STATUS_FILTERS.map((f) => (
        <button
          key={f.id}
          type="button"
          aria-pressed={value === f.id}
          onClick={() => onChange(f.id)}
        >
          {f.label}
        </button>
      ))}
    </div>
  );
}
