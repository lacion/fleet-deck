// ADR-008 §5.3 "Owner visibility": the answers of a fleet-control coordinator, newest first.
import { useRef } from 'react';
import { trapTab, useFocusReturn } from '../app/focus';
import { useOverlay } from '../app/overlay';
import { Icon, X } from '../ui/icons';
import { type FleetAnswers, resultText } from './fleetAnswers';

type Props = {
  readonly data: FleetAnswers;
  readonly onClose: () => void;
};

/** The list of the last answers of the coordinator, with the answer limit of its grant. */
export function FleetAnswersDialog({ data, onClose }: Props) {
  const root = useRef<HTMLDivElement>(null);
  const close = useRef<HTMLButtonElement>(null);
  useFocusReturn(root, close);
  useOverlay();
  return (
    // The overlay click is a mouse shortcut; the Close button and Escape serve the keyboard.
    <div
      className="fd-overlay"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        ref={root}
        className="fd-dialog"
        role="dialog"
        aria-modal="true"
        aria-label="Answers of this coordinator"
        onKeyDown={(e) => {
          if (e.key === 'Escape') {
            e.stopPropagation();
            onClose();
          } else trapTab(e, root.current);
        }}
      >
        <h2>Answers of this coordinator</h2>
        {data.answer_cap === null ? null : (
          <p className="fd-note">
            Answer limit: {data.answer_used} of {data.answer_cap} used
          </p>
        )}
        {data.answers.length === 0 ? (
          <p className="fd-note">No answers yet.</p>
        ) : (
          <div className="fd-fleet-answers-wrap">
            <table className="fd-fleet-answers">
              <thead>
                <tr>
                  <th scope="col">Time</th>
                  <th scope="col">Session</th>
                  <th scope="col">Result</th>
                  <th scope="col">Details</th>
                </tr>
              </thead>
              <tbody>
                {data.answers.map((a, i) => (
                  // The list is newest first and never reordered: the position is stable.
                  <tr key={i}>
                    <td className="fd-fleet-answers-time">{new Date(a.at).toLocaleString()}</td>
                    <td>{a.target ?? '—'}</td>
                    <td>{resultText(a.result)}</td>
                    <td>{a.detail ?? ''}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        <div className="fd-exec-foot">
          <button ref={close} type="button" className="fd-ghost" onClick={onClose}>
            <Icon of={X} size={14} /> Close
          </button>
        </div>
      </div>
    </div>
  );
}
