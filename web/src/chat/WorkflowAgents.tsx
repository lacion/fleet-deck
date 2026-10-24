// B12: the workflow groups of the Agents switcher: one group for each workflow of the session, with
// its phases and its agents. The pieces that the group and the agent view share live here too.
import { Fragment, type MouseEvent } from 'react';
import { type ShownStatus, taskStatus } from '../session/tasks';
import {
  agentStatus,
  firstLine,
  type WorkflowAgentView,
  type WorkflowsSnapshot,
  type WorkflowView,
  workflowTitle,
} from '../session/workflows';
import { countText } from '../ui/format';

/** The choice of a workflow agent in the switcher; the URL is `agent=wf:<task id>:<index>`. */
export type WorkflowChoice = { kind: 'workflow_agent'; task_id: string; index: number };

/** The status chip of a row or a head. */
export const Chip = ({ status }: { status: ShownStatus }) => (
  <span className="fd-badge" data-tone={status.tone}>
    {status.label}
  </span>
);

export const LOADING_LINE = 'Loading the agents of this workflow…';
export const OLDER_STATION_LINE = 'This station does not report the agents of a workflow.';
export const REMOVED_LINE = 'Fleet Deck no longer keeps the agents of this workflow.';
export const UNTRACKED_LINE =
  'This workflow started while 64 others ran. Fleet Deck does not show or count its agents.';
export const INCOMPLETE_LINE =
  'Fleet Deck could not read every workflow of this session. Open the session again to retry.';

/** The one line under the head of a workflow that has no agents to list; `null`: list them. */
function asideOf(
  w: WorkflowView,
  snapshot: WorkflowsSnapshot | null,
  loading: boolean,
): string | null {
  if (w.untracked) return UNTRACKED_LINE;
  if (w.source === 'station') return null;
  if (snapshot !== null) return REMOVED_LINE;
  return loading ? LOADING_LINE : OLDER_STATION_LINE;
}

/** The second line of an agent row: what it runs now, its result or its error. */
function noteOf(a: WorkflowAgentView): string {
  if (a.state === 'running') return a.last_tool ?? '';
  if (a.state === 'done') return firstLine(a.result_preview ?? '');
  if (a.state === 'failed') return firstLine(a.error ?? '');
  return '';
}

type Choose = (choice: WorkflowChoice) => (event: MouseEvent) => void;

function AgentRow(props: {
  workflow: WorkflowView;
  agent: WorkflowAgentView;
  selected: string | null;
  choose: Choose;
}) {
  const { workflow: w, agent: a } = props;
  const key = `${w.task_id}:${a.index}`;
  const note = noteOf(a);
  return (
    <button
      type="button"
      className="fd-agent-row fd-selbar"
      data-wfagent={key}
      aria-current={props.selected === key ? 'true' : undefined}
      onClick={props.choose({ kind: 'workflow_agent', task_id: w.task_id, index: a.index })}
    >
      <span className="fd-agent-line">
        <span className="fd-agent-name" title={a.label}>
          {a.label}
        </span>
        <Chip status={agentStatus(a.state)} />
      </span>
      {note === '' ? null : <span className="fd-agent-dim">{note}</span>}
    </button>
  );
}

/** The phases of a workflow in index order, each with its agents; the agents of no known phase last. */
function Phases(props: { workflow: WorkflowView; selected: string | null; choose: Choose }) {
  const { workflow: w } = props;
  const agents = [...w.agents.values()].sort((a, b) => a.index - b.index);
  const inPhase = (phase: number) => agents.filter((a) => a.phase_index === phase);
  const other = agents.filter((a) => a.phase_index === null || !w.phases.has(a.phase_index));
  const row = (a: WorkflowAgentView) => (
    <AgentRow
      key={a.index}
      workflow={w}
      agent={a}
      selected={props.selected}
      choose={props.choose}
    />
  );
  return (
    <>
      {[...w.phases.keys()]
        .sort((a, b) => a - b)
        .map((phase) => (
          <Fragment key={`phase-${phase}`}>
            <p className="fd-agent-phase">{`${phase}. ${w.phases.get(phase) ?? ''}`}</p>
            {inPhase(phase).map(row)}
          </Fragment>
        ))}
      {other.length > 0 ? (
        <>
          <p className="fd-agent-phase">Other agents</p>
          {other.map(row)}
        </>
      ) : null}
    </>
  );
}

/**
 * The groups of the workflows of the session, oldest first, for the Agents menu. The heads, notes
 * and rows are direct children of the menu body (its row rules select `.fd-menu-body > button`).
 */
export function WorkflowGroups(props: {
  readonly workflows: ReadonlyMap<string, WorkflowView>;
  readonly snapshot: WorkflowsSnapshot | null;
  readonly loading: boolean;
  /** `<task id>:<index>` of the chosen workflow agent; `null`: none. */
  readonly selected: string | null;
  readonly choose: Choose;
}) {
  const { workflows, snapshot, loading } = props;
  return (
    <>
      {[...workflows.values()].map((w) => {
        const title = workflowTitle(w);
        const aside = asideOf(w, snapshot, loading);
        return (
          <Fragment key={w.task_id}>
            <p className="fd-label fd-agent-group">
              <span className="fd-agent-group-name">
                {title === '' ? 'Workflow' : `Workflow · ${title}`}
              </span>
              {w.source === 'station' ? (
                <span className="fd-count">{countText(w.agents_total)}</span>
              ) : null}
              <Chip status={taskStatus(w.status)} />
            </p>
            {aside === null ? (
              <Phases workflow={w} selected={props.selected} choose={props.choose} />
            ) : (
              <p className="fd-agent-aside">{aside}</p>
            )}
          </Fragment>
        );
      })}
      {snapshot?.incomplete === true ? <p className="fd-agent-aside">{INCOMPLETE_LINE}</p> : null}
    </>
  );
}
