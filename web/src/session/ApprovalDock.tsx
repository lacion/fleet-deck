// DL-140: the approval dock, directly above the composer (DL-141: one bordered item per request).
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

/** The longest details text that the dock shows at once, in characters. */
const SHORT_DETAILS = 600;

export function ApprovalDock(props: Props) {
  if (props.requests.length === 0) return null;
  return (
    <section className="fd-dock-approvals" aria-label="Needs you">
      <h3 className="fd-vh">Needs you · {props.requests.length}</h3>
      <ul>
        {props.requests.map((r) => {
          const form = props.answerable ? questionForm(r) : null;
          return (
            <li key={r.request_id} className="fd-q-dock" data-request={r.request_id}>
              <div className="fd-q-dock-head">
                <span className="fd-badge fd-caps" data-tone={form ? 'info' : 'act'}>
                  {form ? 'Question' : 'Permission'}
                </span>
                <strong className="fd-q-dock-title">{r.title}</strong>
              </div>
              {r.details && r.details.length <= SHORT_DETAILS ? (
                <pre className="fd-code fd-dock-cmd">{r.details}</pre>
              ) : r.details ? (
                <details className="fd-dock-details">
                  <summary>Show details</summary>
                  <pre className="fd-code fd-dock-cmd">{r.details}</pre>
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
