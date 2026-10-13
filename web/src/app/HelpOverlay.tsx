import { useEffect, useRef } from 'react';
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

/** The keys of the fleet home (the needs-you rail). */
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
    'A session waits for an approval or an answer. Answer it in the rail, the dock, or with the keys.',
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
  ['Fleet', 'Every session by status, with the rail of requests and the activity feed'],
  ['Plans', 'Proposed plans'],
  ['Stations', 'Machines, providers and shells'],
  ['New session', 'Start an agent on a station'],
];

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
              <kbd className="fd-kbd">{row.keys}</kbd>
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

/** The keys, the concepts and the pages of the board. Focus moves in and goes back on close. */
export function HelpOverlay({ onClose }: { readonly onClose: () => void }) {
  const root = useRef<HTMLDivElement>(null);
  const close = useRef<HTMLButtonElement>(null);
  useFocusReturn(root, close);
  useOverlay();
  // The Close button has the focus; the long dialog still opens at its top.
  useEffect(() => root.current?.scrollTo({ top: 0 }), []);
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
        <h2>Keyboard and help</h2>
        <section aria-label="Keys">
          <h3 className="fd-label">Keys</h3>
          <KeyTable rows={SHORTCUTS} label="General keys" />
          <h4 className="fd-label">On the fleet home</h4>
          <KeyTable rows={FLEET_SHORTCUTS} label="Keys on the fleet home" />
        </section>
        <section aria-label="Concepts">
          <h3 className="fd-label">Concepts</h3>
          <Terms items={CONCEPTS} />
        </section>
        <section aria-label="Pages">
          <h3 className="fd-label">Pages</h3>
          <Terms items={PAGES} />
        </section>
        <button ref={close} type="button" onClick={onClose}>
          Close
        </button>
      </div>
    </div>
  );
}
