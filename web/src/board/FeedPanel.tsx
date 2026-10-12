import { useState } from 'react';
import type { SessionKey } from '../gen';
import { clock, FEED_FILTERS, type FeedItem, type FeedKind } from './feed';

type Props = {
  readonly items: readonly FeedItem[];
  readonly onOpen: (key: SessionKey) => void;
};

/** How many feed items the board shows at a time (newest first). */
export const FEED_SHOWN = 50;

/** The event feed of the fleet (v0 `Feed.tsx` parity, DL-095). */
export function Feed({ items, onOpen }: Props) {
  const [kind, setKind] = useState<'all' | FeedKind>('all');
  const shown = items
    .filter((i) => kind === 'all' || i.kind === kind)
    .slice(-FEED_SHOWN)
    .reverse();
  return (
    <section className="fd-feed" aria-label="Event feed">
      <h2>Event feed</h2>
      <div className="fd-feed-filters" role="group" aria-label="Feed filter">
        {FEED_FILTERS.map((f) => (
          <button
            key={f.id}
            type="button"
            aria-pressed={kind === f.id}
            onClick={() => setKind(f.id)}
          >
            {f.label}
          </button>
        ))}
      </div>
      {shown.length === 0 ? (
        <p className="fd-note">Quiet so far.</p>
      ) : (
        <ol>
          {shown.map((i) => (
            <li key={i.id} data-feed-kind={i.kind}>
              <time dateTime={new Date(i.at).toISOString()}>{clock(i.at)}</time>{' '}
              <button type="button" className="fd-link" onClick={() => onOpen(i.key)}>
                {i.name}
              </button>{' '}
              {i.text}
            </li>
          ))}
        </ol>
      )}
    </section>
  );
}
