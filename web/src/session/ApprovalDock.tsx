// DL-140: the approval dock, directly above the composer. The same logic as the
// `Requests` list of SessionView (kept there until wave 2).
import type { OpenRequest } from '../gen';
import { QuestionAnswer } from './QuestionAnswer';
import { questionForm, type Reply } from './question';

type Props = {
  readonly requests: readonly OpenRequest[];
  readonly answerable: boolean;
  readonly enabled: boolean;
  readonly busy: boolean;
  readonly onAnswer: (request: string, option: string, reply: Reply | null) => void;
};

export function ApprovalDock(props: Props) {
  if (props.requests.length === 0) return null;
  return (
    <section className="fd-dock-approvals" aria-label="Needs you">
      <h3>Needs you · {props.requests.length}</h3>
      <ul>
        {props.requests.map((r) => {
          const form = props.answerable ? questionForm(r) : null;
          return (
            <li key={r.request_id} data-request={r.request_id}>
              <strong>{r.title}</strong>
              {r.details ? (
                <details className="fd-dock-details">
                  <summary>Details</summary>
                  <pre className="fd-details">{r.details}</pre>
                </details>
              ) : null}
              {/* Without the approvals capability the provider answers under its
                own policy: the board shows the request but no controls (§6.2). */}
              {/* A question gets an answer form (DL-068). */}
              {form ? (
                <QuestionAnswer
                  request={r}
                  form={form}
                  enabled={props.enabled}
                  busy={props.busy}
                  onAnswer={(o, reply) => props.onAnswer(r.request_id, o, reply)}
                />
              ) : props.answerable ? (
                <div className="fd-options">
                  {r.options.map((o, n) => (
                    <button
                      key={o.option_id}
                      type="button"
                      className={n === 0 ? 'fd-primary' : undefined}
                      disabled={!props.enabled || props.busy}
                      onClick={() => props.onAnswer(r.request_id, o.option_id, null)}
                    >
                      {o.label}
                    </button>
                  ))}
                </div>
              ) : null}
            </li>
          );
        })}
      </ul>
      {!props.answerable ? (
        <p className="fd-note">This provider has no approval controls.</p>
      ) : props.enabled ? null : (
        <p className="fd-note">Answering needs the approve scope.</p>
      )}
    </section>
  );
}
