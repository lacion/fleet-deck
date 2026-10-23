// B6, B12: which agent view the chat tab shows. The `?agent=` value names a subagent (its node id)
// or a workflow agent (`wf:<task id>:<index>`); any other value, or an agent that is not known,
// shows the main chat. The hook also owns the workflows of the session and the mark of a chosen
// background command (its card is scrolled to and marked for 2.5 s).
import { useEffect, useState } from 'react';
import type { AgentChoice } from '../chat/AgentsMenu';
import type { DeckSession, EventEnvelope } from '../gen';
import type { SessionActions } from './commands';
import type { Subagent } from './timeline';
import { useWorkflows } from './useWorkflows';
import {
  parseWorkflowAgentParam,
  type WorkflowAgentView,
  type WorkflowsSnapshot,
  type WorkflowView,
  workflowAgentParam,
} from './workflows';

const HIGHLIGHT_HOLD_MS = 2500;

export type AgentChoiceParams = {
  readonly session: DeckSession;
  readonly subagents: readonly Subagent[];
  readonly events: readonly EventEnvelope[];
  readonly actions: SessionActions | undefined;
  /** A member of a shared session: no subagent log. */
  readonly member: boolean;
  /** The tab of the pane; an agent view shows only on the chat tab. */
  readonly tab: string | undefined;
  /** The `?agent=` value. */
  readonly agent: string | undefined;
  readonly onAgent: (value: string | null) => void;
};

export type AgentChoiceState = {
  readonly agentSub: Subagent | undefined;
  /** The chosen workflow agent with its workflow; `null`: none, or one that is not known. */
  readonly wf: { workflow: WorkflowView; agent: WorkflowAgentView } | null;
  readonly workflows: ReadonlyMap<string, WorkflowView>;
  /** What the Agents menu needs of the workflows (spread into its props). */
  readonly menu: {
    readonly workflows: ReadonlyMap<string, WorkflowView>;
    readonly snapshot: WorkflowsSnapshot | null;
    readonly loading: boolean;
    readonly workflowAgent: string | null;
  };
  readonly highlight: { node_id: string; nonce: number } | null;
  readonly loadSubagent: NonNullable<SessionActions['subagent']> | null;
  /** The line of an agent view that takes no input; `null` in the main chat. */
  readonly note: string | null;
  readonly chooseAgent: (choice: AgentChoice) => void;
};

export function useAgentChoice(p: AgentChoiceParams): AgentChoiceState {
  const { onAgent } = p;
  const [highlight, setHighlight] = useState<AgentChoiceState['highlight']>(null);
  useEffect(() => {
    if (highlight === null) return;
    const timer = setTimeout(() => setHighlight(null), HIGHLIGHT_HOLD_MS);
    return () => clearTimeout(timer);
  }, [highlight]);
  const { workflows, snapshot, loading } = useWorkflows(p.session, p.events, p.actions);
  const onChat = p.tab === 'chat' || p.tab === undefined;
  const agentSub = onChat ? p.subagents.find((sub) => sub.node_id === p.agent) : undefined;
  const named = onChat ? parseWorkflowAgentParam(p.agent) : null;
  const workflow = named === null ? undefined : workflows.get(named.task_id);
  const agent = named === null ? undefined : workflow?.agents.get(named.index);
  const wf = workflow === undefined || agent === undefined ? null : { workflow, agent };
  const chooseAgent = (choice: AgentChoice) => {
    if (choice.kind === 'task') {
      onAgent(null);
      setHighlight({ node_id: choice.node_id, nonce: Date.now() });
      return;
    }
    setHighlight(null);
    if (choice.kind === 'main') onAgent(null);
    else if (choice.kind === 'agent') onAgent(choice.node_id);
    else onAgent(workflowAgentParam(choice.task_id, choice.index));
  };
  let note: string | null = null;
  if (agentSub !== undefined) note = 'A subagent takes no input. Write in the main chat.';
  else if (wf !== null) note = 'A workflow agent takes no input. Write in the main chat.';
  return {
    agentSub,
    wf,
    workflows,
    menu: {
      workflows,
      snapshot,
      loading,
      workflowAgent: wf === null ? null : `${wf.workflow.task_id}:${wf.agent.index}`,
    },
    highlight,
    loadSubagent: p.member ? null : (p.actions?.subagent ?? null),
    note,
    chooseAgent,
  };
}
