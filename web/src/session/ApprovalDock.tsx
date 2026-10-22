// DL-140: the approval dock, directly above the composer. "Flight Deck" §4.5: each request is an
// approval card (needs-you holder, outline and glow; the first option is the decision primary).
import { relativeAge } from '../app/sessions';
import type { ApprovalOption, OpenRequest } from '../gen';
import { CircleHelp, Icon, Shield } from '../ui/icons';
import { useNow } from '../ui/useNow';
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

/** The reason that the provider gave with a request (`input.description`), if it is a text. */
function reasonOf(request: OpenRequest): string | null {
  const input = request.input;
  if (input === null || typeof input !== 'object' || Array.isArray(input)) return null;
  const description = (input as Record<string, unknown>)['description'];
  return typeof description === 'string' && description.trim() !== '' ? description : null;
}

/** The look of an option: the first is the decision primary, a deny is a danger button. */
export function optionClass(option: Pick<ApprovalOption, 'kind'>, n: number): string | undefined {
  if (n === 0) return 'fd-decide';
  return option.kind === 'deny' ? 'fd-danger' : undefined;
}

/** The key hints of the card (§4.5): `a` on the decision primary, `d` on the deny. */

export function ApprovalDock(props: Props) {
  const now = useNow(undefined, 30000);
  if (props.requests.length === 0) return null;
  return (
    <section className="fd-dock-approvals" aria-label="Needs you">
      <h3 className="fd-vh">Needs you · {props.requests.length}</h3>
      <ul>
        {props.requests.map((r) => {
          const form = props.answerable ? questionForm(r) : null;
          const reason = form ? null : reasonOf(r);
          return (
            <li key={r.request_id} className="fd-q-dock fd-glow" data-request={r.request_id}>
              <div className="fd-q-dock-head">
                <span className="fd-q-dock-kind">
                  <Icon of={form ? CircleHelp : Shield} size={14} />
                  {form ? 'Question' : 'Permission request'}
                </span>
                <strong className="fd-q-dock-title">{r.title}</strong>
                <span className="fd-q-dock-age">{relativeAge(r.requested_at, now)}</span>
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
              ) : reason !== null || props.answerable ? (
                <div className="fd-q-dock-foot">
                  {reason === null ? null : <p className="fd-q-dock-reason">{reason}</p>}
                  {props.answerable ? (
                    <div className="fd-options">
                      {r.options.map((o, n) => {
                        const look = optionClass(o, n);
                        return (
                          <button
                            key={o.option_id}
                            type="button"
                            className={look}
                            disabled={!props.enabled || props.busy}
                            onClick={() => props.onAnswer(r.request_id, o.option_id, null)}
                          >
                            {o.label}
                          </button>
                        );
                      })}
                    </div>
                  ) : null}
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
