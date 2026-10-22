// B6: the agents switcher of the chat tab: the main chat, every subagent and every background
// command of the session, each with its status. The header row of an agent view lives here too.
import type { MouseEvent } from 'react';
import {
  commandTasks,
  isRunningStatus,
  runningCount,
  shownStatus,
  type ShownStatus,
  type TaskState,
  taskStatus,
} from '../session/tasks';
import type { Subagent } from '../session/timeline';
import { Check, Icon } from '../ui/icons';
import { Menu } from '../ui/Menu';

export type AgentChoice =
  | { kind: 'main' }
  | { kind: 'agent'; node_id: string }
  | { kind: 'task'; node_id: string };

export type AgentsMenuProps = {
  readonly subagents: readonly Subagent[];
  readonly tasks: ReadonlyMap<string, TaskState>;
  /** The input summary of each tool card, by node id. */
  readonly toolTitles: ReadonlyMap<string, string>;
  /** The node id of the chosen subagent; `null`: the main chat. */
  readonly agent: string | null;
  readonly onChoose: (choice: AgentChoice) => void;
};

const Chip = ({ status }: { status: ShownStatus }) => (
  <span className="fd-badge" data-tone={status.tone}>
    {status.label}
  </span>
);

/** The "Agents" control with its menu. The caller shows it only when `hasAgents` is true. */
export function AgentsMenu(props: AgentsMenuProps) {
  const { subagents, tasks, toolTitles, agent, onChoose } = props;
  const running = runningCount(subagents, tasks);
  const commands = commandTasks(tasks);
  const choose = (close: () => void, choice: AgentChoice) => (event: MouseEvent) => {
    event.currentTarget.closest('details')?.querySelector('summary')?.focus();
    close();
    onChoose(choice);
  };
  // The summary always says what the menu holds: what runs now, else how many it lists.
  return (
    <Menu
      summaryClass="fd-ghost fd-sm"
      summary={
        <>
          Agents{' '}
          {running > 0 ? (
            <span className="fd-badge" data-tone="ok">{`${running} running`}</span>
          ) : (
            <span className="fd-count">{subagents.length + commands.length}</span>
          )}
        </>
      }
    >
      {(close) => (
        <>
          <button
            type="button"
            className="fd-agent-row fd-selbar"
            aria-current={agent === null ? 'true' : undefined}
            onClick={choose(close, { kind: 'main' })}
          >
            <span className="fd-agent-line">
              <span className="fd-agent-name">Main chat</span>
              {agent === null ? <Icon of={Check} size={14} /> : null}
            </span>
          </button>
          {subagents.map((sub) => (
            <button
              key={sub.node_id}
              type="button"
              className="fd-agent-row fd-selbar"
              data-agent={sub.node_id}
              aria-current={agent === sub.node_id ? 'true' : undefined}
              onClick={choose(close, { kind: 'agent', node_id: sub.node_id })}
            >
              <span className="fd-agent-line">
                <span className="fd-agent-name">{sub.title}</span>
                <Chip status={shownStatus(sub, tasks)} />
              </span>
              {sub.input ? <span className="fd-agent-dim">{sub.input}</span> : null}
            </button>
          ))}
          {commands.map((task) => {
            const node = task.node_id;
            const has = node !== null && toolTitles.has(node);
            const detail = task.description ?? (node === null ? '' : (toolTitles.get(node) ?? ''));
            return (
              <button
                key={task.task_id}
                type="button"
                className="fd-agent-row"
                data-task={task.task_id}
                disabled={!has}
                onClick={
                  has && node !== null ? choose(close, { kind: 'task', node_id: node }) : undefined
                }
              >
                <span className="fd-agent-line">
                  <span className="fd-agent-name">Background command</span>
                  <Chip status={taskStatus(task.status)} />
                </span>
                {detail ? <span className="fd-agent-dim">{detail}</span> : null}
                {!isRunningStatus(task.status) && task.summary ? (
                  <span className="fd-agent-dim">{task.summary}</span>
                ) : null}
              </button>
            );
          })}
        </>
      )}
    </Menu>
  );
}

/** The header of an agent view: the way back, the title and the status. */
export function AgentHead(props: {
  readonly title: string;
  readonly status: ShownStatus;
  readonly onBack: () => void;
}) {
  return (
    <div className="fd-agent-head">
      <button type="button" className="fd-ghost fd-sm" onClick={props.onBack}>
        ‹ Main chat
      </button>
      <span className="fd-agent-title">{props.title}</span>
      <Chip status={props.status} />
    </div>
  );
}
