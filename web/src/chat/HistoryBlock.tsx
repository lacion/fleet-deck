// DL-189: the earlier history of an imported session, above the timeline (design §3.7).
// Everything here is muted: no item is editable or answerable.
import type { HistoryItem } from '../gen';
import { Icon, Wrench } from '../ui/icons';
import type { HistoryView } from './history';
import { Markdown } from './Markdown';

function ItemView({ item }: { item: HistoryItem }) {
  const title = item.at > 0 ? new Date(item.at).toLocaleString() : undefined;
  if (item.kind === 'tool') {
    return (
      <div className="fd-history-item" data-kind="tool" title={title}>
        <Icon of={Wrench} size={14} />
        <code>{item.tool?.name ?? ''}</code>
        {item.tool !== null && item.tool.summary !== '' ? <span>{item.tool.summary}</span> : null}
        {item.tool?.status === 'failed' ? <span className="fd-history-failed">failed</span> : null}
      </div>
    );
  }
  // The same shapes as the live chat: a bubble for you, plain Markdown for the agent.
  if (item.kind === 'user') {
    return (
      <div className="fd-chat-run" data-history="true" title={title}>
        <div className="fd-chat-bubble">{item.text}</div>
      </div>
    );
  }
  return (
    <div className="fd-chat-message" data-history="true" title={title}>
      <Markdown text={item.text} />
    </div>
  );
}

/** The block of the loaded pages, its one muted line, and the gap line below it. */
export function HistoryBlock({ view, gap }: { readonly view: HistoryView; readonly gap: boolean }) {
  const { items, line } = view;
  if (items.length === 0 && line === null) return null;
  return (
    <>
      <section className="fd-history" aria-label="Earlier history">
        {items.length > 0 ? (
          // biome-ignore lint/a11y/useFocusableInteractive lint/a11y/useAriaPropsForRole: a plain separator, not a slider
          <div className="fd-history-divider" role="separator">
            <span>Earlier, in the terminal</span>
          </div>
        ) : null}
        {items.map((item) => (
          <ItemView key={item.id} item={item} />
        ))}
        {line === null ? null : (
          <p className="fd-note fd-history-line">
            {line.text}
            {line.action === 'retry' ? (
              <>
                {' '}
                <button type="button" className="fd-ghost fd-sm" onClick={view.retry}>
                  Try again
                </button>
              </>
            ) : null}
          </p>
        )}
      </section>
      {gap ? (
        <p className="fd-note fd-history-gap">
          Older messages of this session are not kept on the deck.
        </p>
      ) : null}
    </>
  );
}
