import { Fragment, type RefObject, useEffect, useRef, useState } from 'react';
import { useDismiss } from '../ui/dismiss';
import { Icon, X } from '../ui/icons';
import { trapTab, useFocusReturn } from './focus';
import { useOverlay } from './overlay';

type Row = { readonly keys: string; readonly does: string };

export const SHORTCUTS: readonly Row[] = [
  { keys: 'j', does: 'Next session' },
  { keys: 'k', does: 'Previous session' },
  { keys: '/', does: 'Focus the message box or search' },
  { keys: 'n', does: 'New session' },
  { keys: 'f', does: 'Fleet' },
  { keys: 'c', does: 'Open the mail form' },
  { keys: 't', does: 'Show or hide terminals' },
  { keys: '?', does: 'Show this help' },
  { keys: 'Ctrl+K or ⌘K', does: 'Go to session' },
];

/** The keys of the fleet home (the Needs you panel). */
export const FLEET_SHORTCUTS: readonly Row[] = [
  { keys: '[ ]', does: 'Select the previous or next request' },
  { keys: 'a', does: 'Answer the selected request with its first option' },
  { keys: 'd', does: 'Answer with its last option' },
  { keys: '1–9', does: 'Answer with option N' },
];

const CONCEPTS: readonly (readonly [string, string])[] = [
  [
    'Deck',
    "The service that this board talks to. It knows every station and stores the board's commands.",
  ],
  ['Station', 'A machine that runs agent sessions. A deck can have many stations.'],
  [
    'Session',
    "One agent conversation on a station. The board drives it through the provider's native protocol.",
  ],
  [
    'Needs you',
    'A session waits for an approval or an answer. Answer it in the Needs you panel, the session, or with the keys.',
  ],
  [
    'Chat and terminal',
    "The chat drives a session. Open in terminal stops the driven process and runs the provider's own TUI; Return to board control resumes it.",
  ],
  [
    'Mail, steer, follow-up',
    'A follow-up waits for the end of the turn. Steer goes into the running turn. Mail is a message from the fleet that the deck delivers.',
  ],
  ['Worktree', 'A session can work in its own git worktree on its own branch.'],
  [
    'Plan',
    'A plan that an agent proposed. The Plans page keeps it; you can start a session or assign it to one.',
  ],
];

const PAGES: readonly (readonly [string, string])[] = [
  ['Fleet', 'Every session by status, with the Needs you panel and the activity feed'],
  ['Plans', 'Proposed plans'],
  ['Stations', 'Machines, providers and shells'],
  ['New session', 'Start an agent on a station'],
];

/** One key chip for each key: "Ctrl+K or ⌘K" is two chips, "[ ]" is two chips. */
function Keys({ keys }: { readonly keys: string }) {
  return keys.split(' or ').map((group, at) => (
    <Fragment key={group}>
      {at > 0 ? ' or ' : null}
      {group.split(' ').map((key, i) => (
        <Fragment key={key}>
          {i > 0 ? ' ' : null}
          <kbd className="fd-kbd">{key}</kbd>
        </Fragment>
      ))}
    </Fragment>
  ));
}

function KeyTable({ rows, label }: { readonly rows: readonly Row[]; readonly label: string }) {
  return (
    <table className="fd-keys" aria-label={label}>
      <thead>
        <tr>
          <th scope="col">Key</th>
          <th scope="col">Action</th>
        </tr>
      </thead>
      <tbody>
        {rows.map((row) => (
          <tr key={row.keys}>
            <td>
              <Keys keys={row.keys} />
            </td>
            <td>{row.does}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function Terms({ items }: { readonly items: readonly (readonly [string, string])[] }) {
  return (
    <dl className="fd-help-terms">
      {items.map(([term, text]) => (
        <div key={term}>
          <dt>{term}</dt>
          <dd>{text}</dd>
        </div>
      ))}
    </dl>
  );
}

/** True while the element can scroll further down (the cue of a scroll body). */
function useMoreBelow(ref: RefObject<HTMLElement | null>): boolean {
  const [more, setMore] = useState(false);
  useEffect(() => {
    const el = ref.current;
    if (el === null) return undefined;
    const update = () => setMore(el.scrollTop + el.clientHeight < el.scrollHeight - 1);
    update();
    el.addEventListener('scroll', update, { passive: true });
    const sizes = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(update);
    sizes?.observe(el);
    return () => {
      el.removeEventListener('scroll', update);
      sizes?.disconnect();
    };
  }, [ref]);
  return more;
}

/** The keys, the concepts and the pages of the board. Focus moves in and goes back on close. */
export function HelpOverlay({ onClose }: { readonly onClose: () => void }) {
  const root = useRef<HTMLDivElement>(null);
  const close = useRef<HTMLButtonElement>(null);
  const body = useRef<HTMLDivElement>(null);
  const more = useMoreBelow(body);
  useFocusReturn(root, close);
  useOverlay();
  useDismiss(root, true, onClose);
  // The Close button of the header has the focus; the long body still opens at its top.
  useEffect(() => body.current?.scrollTo({ top: 0 }), []);
  return (
    <div className="fd-overlay">
      <div
        ref={root}
        className="fd-dialog fd-help"
        role="dialog"
        aria-modal="true"
        aria-label="Keyboard and help"
        onKeyDown={(e) => {
          if (e.key === 'Escape') {
            e.stopPropagation();
            onClose();
          } else trapTab(e, root.current);
        }}
      >
        <div className="fd-help-head">
          <h2>Keyboard and help</h2>
          <button
            ref={close}
            type="button"
            className="fd-iconbtn"
            aria-label="Close"
            title="Close"
            onClick={onClose}
          >
            <Icon of={X} />
          </button>
        </div>
        {/* The body scrolls under the fixed header; its bottom fades while more is below. */}
        <div ref={body} className="fd-help-body" data-more={more ? 'true' : undefined}>
          <section aria-label="Keys">
            <h3 className="fd-help-section">Keys</h3>
            <div className="fd-help-keys">
              <div className="fd-help-keygroup">
                <h4 className="fd-label">On every page</h4>
                <KeyTable rows={SHORTCUTS} label="General keys" />
              </div>
              <div className="fd-help-keygroup">
                <h4 className="fd-label">On the fleet home</h4>
                <KeyTable rows={FLEET_SHORTCUTS} label="Keys on the fleet home" />
              </div>
            </div>
          </section>
          <section aria-label="Concepts">
            <h3 className="fd-help-section">Concepts</h3>
            <Terms items={CONCEPTS} />
          </section>
          <section aria-label="Pages">
            <h3 className="fd-help-section">Pages</h3>
            <Terms items={PAGES} />
          </section>
        </div>
      </div>
    </div>
  );
}
