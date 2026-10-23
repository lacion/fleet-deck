// B12: the view of one workflow agent, in place of the chat list. A workflow agent has no chat in
// the main timeline: the view shows what the station knows of it (facts, task, result).
import { Fragment, type ReactNode } from 'react';
import type { SubagentLog } from '../gen';
import {
  agentStatus,
  type WorkflowAgentView as Agent,
  type WorkflowView,
  workflowTitle,
} from '../session/workflows';
import { compactCount, countText, shortModel } from '../ui/format';
import { AgentHead } from './AgentsMenu';
import { SubagentWork } from './SubagentWork';

/** The facts of the agent as label and value; a fact that the provider did not send has no row. */
function factsOf(w: WorkflowView, a: Agent): [string, string][] {
  const phase = a.phase_index === null ? undefined : w.phases.get(a.phase_index);
  const rows: [string, string | null][] = [
    ['Workflow', workflowTitle(w) || null],
    ['Phase', phase === undefined ? null : `${a.phase_index}. ${phase}`],
    ['Model', a.model === null ? null : shortModel(a.model)],
    ['Tool calls', a.tool_calls === null ? null : countText(a.tool_calls)],
    ['Tokens', a.tokens === null ? null : compactCount(a.tokens)],
    ['Attempt', a.attempt !== null && a.attempt > 1 ? countText(a.attempt) : null],
  ];
  return rows.flatMap(([label, value]) => (value === null ? [] : [[label, value]]));
}

export type WorkflowAgentViewProps = {
  readonly workflow: WorkflowView;
  readonly agent: Agent;
  readonly onBack: () => void;
  /** The Agents switcher: it stays in the bar above the facts. */
  readonly toolbar?: ReactNode;
  /** The owner's reader of the agent's own transcript; `null` or absent for a member. */
  readonly loadWork?: ((agentId: string) => Promise<SubagentLog>) | null;
};

export function WorkflowAgentView(props: WorkflowAgentViewProps) {
  const { workflow: w, agent: a } = props;
  const facts = factsOf(w, a);
  const failure = a.error !== null;
  const outcome = failure ? a.error : a.result_preview;
  const { loadWork } = props;
  const agentId = a.agent_id;
  return (
    <>
      <AgentHead title={a.label} status={agentStatus(a.state)} onBack={props.onBack} />
      <section className="fd-chat" aria-label="Workflow agent">
        {props.toolbar === undefined || props.toolbar === null ? null : (
          <div className="fd-chat-bar">
            <div className="fd-chat-toolbar">{props.toolbar}</div>
          </div>
        )}
        <div className="fd-chat-scroll">
          <div className="fd-chat-column">
            {facts.length > 0 ? (
              <dl className="fd-agent-facts">
                {facts.map(([label, value]) => (
                  <Fragment key={label}>
                    <dt>{label}</dt>
                    <dd>{value}</dd>
                  </Fragment>
                ))}
              </dl>
            ) : null}
            {a.prompt_preview === null ? null : (
              <section>
                <p className="fd-label">Task</p>
                <p className="fd-agent-text">{a.prompt_preview}</p>
              </section>
            )}
            {outcome === null ? null : (
              <section>
                <p className="fd-label">{failure ? 'Error' : 'Result'}</p>
                <p className="fd-agent-text" data-tone={failure ? 'hazard' : undefined}>
                  {outcome}
                </p>
              </section>
            )}
            {loadWork == null ? null : agentId === null ? (
              <p className="fd-note">Its work shows after it starts.</p>
            ) : (
              <SubagentWork key={agentId} load={() => loadWork(agentId)} />
            )}
          </div>
        </div>
      </section>
    </>
  );
}
