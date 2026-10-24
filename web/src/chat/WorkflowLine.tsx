// B12: the one line under the tool card of a Workflow call: the counts of its agents. No button:
// the Agents switcher is the way in (design `workflow-agents.md` §4.6).
import { type WorkflowView, workflowTitle } from '../session/workflows';

/** "<name> · <done> of <total> agents done · <running> running · Stopped". */
export function workflowLineText(w: WorkflowView): string {
  const done = [...w.agents.values()].filter((a) => a.state === 'done').length;
  const parts = [
    workflowTitle(w) || 'Workflow',
    w.agents_total === 0 ? 'No agents yet' : `${done} of ${w.agents_total} agents done`,
  ];
  if (w.running_agents > 0) parts.push(`${w.running_agents} running`);
  if (w.status === 'stopped') parts.push('Stopped');
  else if (w.status === 'failed') parts.push('Failed');
  return parts.join(' · ');
}

/** The line of the workflow whose tool card has this node id; nothing for any other card. */
export function WorkflowLine(props: {
  readonly workflows: ReadonlyMap<string, WorkflowView> | undefined;
  readonly nodeId: string;
}) {
  const w = [...(props.workflows?.values() ?? [])].find((x) => x.node_id === props.nodeId);
  if (w === undefined) return null;
  return (
    <p className="fd-note" data-workflow-line={w.task_id}>
      {workflowLineText(w)}
    </p>
  );
}
