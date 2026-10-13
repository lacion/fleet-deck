// DL-175: the filter bar at the top of the chat tab: one row of chips.
import { type ChatFilter, FILTER_KEYS, FILTER_LABEL, type FilterCounts } from './filter';

export function FilterBar(props: {
  readonly filter: ChatFilter;
  readonly counts: FilterCounts;
  readonly onChange: (next: ChatFilter) => void;
}) {
  const { filter, counts, onChange } = props;
  return (
    <div className="fd-chat-filters">
      <div className="fd-chat-filters-row" role="group" aria-label="Chat filters">
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
        <span className="fd-chat-filter-sep" aria-hidden="true" />
        <button
          type="button"
          className="fd-chip"
          aria-pressed={filter.compact}
          onClick={() => onChange({ ...filter, compact: !filter.compact })}
        >
          Compact tools
        </button>
      </div>
    </div>
  );
}
