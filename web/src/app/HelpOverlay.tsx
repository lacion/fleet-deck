import { useRef } from 'react';
import { trapTab, useFocusReturn } from './focus';

export const SHORTCUTS: readonly { readonly keys: string; readonly does: string }[] = [
  { keys: 'j', does: 'Next session' },
  { keys: 'k', does: 'Previous session' },
  { keys: '/', does: 'Focus the message box or search' },
  { keys: 'n', does: 'New session' },
  { keys: 'f', does: 'Fleet' },
  { keys: 't', does: 'Show or hide terminals' },
  { keys: '?', does: 'Show this help' },
  { keys: 'Ctrl+K or ⌘K', does: 'Go to session' },
];

/** The keyboard shortcuts of the board. Focus moves in and goes back on close. */
export function HelpOverlay({ onClose }: { readonly onClose: () => void }) {
  const root = useRef<HTMLDivElement>(null);
  const close = useRef<HTMLButtonElement>(null);
  useFocusReturn(root, close);
  return (
    <div className="fd-overlay">
      <div
        ref={root}
        className="fd-dialog"
        role="dialog"
        aria-modal="true"
        aria-label="Keyboard shortcuts"
        onKeyDown={(e) => {
          if (e.key === 'Escape') {
            e.stopPropagation();
            onClose();
          } else trapTab(e, root.current);
        }}
      >
        <h2>Keyboard shortcuts</h2>
        <table className="fd-keys">
          <thead>
            <tr>
              <th scope="col">Key</th>
              <th scope="col">Action</th>
            </tr>
          </thead>
          <tbody>
            {SHORTCUTS.map((row) => (
              <tr key={row.keys}>
                <td>
                  <kbd>{row.keys}</kbd>
                </td>
                <td>{row.does}</td>
              </tr>
            ))}
          </tbody>
        </table>
        <button ref={close} type="button" onClick={onClose}>
          Close
        </button>
      </div>
    </div>
  );
}
