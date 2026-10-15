// DL-175: the "Show" menu at the top of the chat tab: what the chat shows (design ui-pass X2).
import { type KeyboardEvent, useRef, useState } from 'react';
import { useDismiss } from '../ui/dismiss';
import { type ChatFilter, FILTER_KEYS, FILTER_LABEL, type FilterCounts } from './filter';

export function FilterBar(props: {
  readonly filter: ChatFilter;
  readonly counts: FilterCounts;
  readonly onChange: (next: ChatFilter) => void;
}) {
  const { filter, counts, onChange } = props;
  const ref = useRef<HTMLDetailsElement>(null);
  const [open, setOpen] = useState(false);
  const close = () => {
    if (ref.current) ref.current.open = false;
  };
  useDismiss(ref, open, close);
  const onKeyDown = (event: KeyboardEvent<HTMLDetailsElement>) => {
    if (event.key !== 'Escape' || !ref.current?.open) return;
    event.stopPropagation();
    close();
    ref.current.querySelector('summary')?.focus();
  };
  const hidden = FILTER_KEYS.filter((key) => !filter.show[key]).length;
  return (
    <div className="fd-chat-filters">
      <details
        className="fd-menu fd-chat-show"
        ref={ref}
        onKeyDown={onKeyDown}
        onToggle={(event) => setOpen(event.currentTarget.open)}
      >
        <summary className="fd-ghost fd-sm">
          {hidden > 0 ? `Show (${hidden} hidden)` : 'Show'}
        </summary>
        <div className="fd-menu-body" role="group" aria-label="Chat filters">
          {FILTER_KEYS.map((key) => {
            const on = filter.show[key];
            return (
              <button
                key={key}
                type="button"
                className="fd-chip"
                aria-pressed={on}
                onClick={() => onChange({ ...filter, show: { ...filter.show, [key]: !on } })}
              >
                {FILTER_LABEL[key]}
                {!on && counts[key] > 0 ? (
                  <span className="fd-chat-filter-count">{counts[key]}</span>
                ) : null}
              </button>
            );
          })}
          <div className="fd-menu-sep" />
          <button
            type="button"
            className="fd-chip"
            aria-pressed={filter.compact}
            onClick={() => onChange({ ...filter, compact: !filter.compact })}
          >
            Compact tools
          </button>
        </div>
      </details>
    </div>
  );
}
