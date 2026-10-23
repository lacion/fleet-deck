// B6: the agents switcher of the chat tab: the main chat, every subagent and every background
// command of the session, each with its status. The header row of an agent view lives here too.
import type { MouseEvent } from 'react';
import {
  commandTasks,
  isLive,
  isRunningStatus,
  runningCount,
  shownStatus,
  type ShownStatus,
  type TaskState,
  taskOf,
  taskStatus,
} from '../session/tasks';
import type { Subagent } from '../session/timeline';
import { countText, homeShortIn } from '../ui/format';
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
  /** The running count of the pane (it also counts the agents of the summary that started before
      the loaded history); absent: the count of the loaded timeline. */
  readonly running?: number | undefined;
};

/** "1 command", "33 commands": the background commands next to the subagent count (the Agents
    summary and the Properties panel say it the same way). */
export const commandsText = (n: number): string =>
  `${countText(n)} ${n === 1 ? 'command' : 'commands'}`;

const Chip = ({ status }: { status: ShownStatus }) => (
  <span className="fd-badge" data-tone={status.tone}>
    {status.label}
  </span>
);

/** The first line of a text that has words, trimmed; '' when there is none. */
const firstLine = (text: string): string =>
  text
    .split('\n')
    .map((line) => line.trim())
    .find((line) => line !== '') ?? '';

/**
 * The name of a subagent: its task (the description of a Claude agent, the prompt of a Codex one),
 * else its tool name ("Agent", "spawnAgent"), which every subagent of a provider shares.
 */
export const subagentName = (sub: Pick<Subagent, 'input' | 'title'>): string =>
  firstLine(sub.input) || sub.title;

/**
 * The name of a background command: its description, else the command of its card. A command
 * reads as code (with `~` for a home folder): a description that only repeats the command (Claude
 * names a command that has no description by the command) does too. '' when it has neither.
 */
export function commandName(
  task: Pick<TaskState, 'description' | 'node_id'>,
  toolTitles: ReadonlyMap<string, string>,
): { readonly name: string; readonly code: boolean; readonly raw: string } {
  const input = task.node_id === null ? '' : firstLine(toolTitles.get(task.node_id) ?? '');
  const description = firstLine(task.description ?? '');
  // The station cuts a long input (`…`): the description then starts with what is left of it.
  const cut = input.endsWith('…') ? input.slice(0, -1) : null;
  const repeats =
    input !== '' && (description === input || (cut !== null && description.startsWith(cut)));
  if (description !== '' && !repeats) return { name: description, code: false, raw: description };
  const command = description || input;
  return { name: homeShortIn(command), code: command !== '', raw: command };
}

/** Claude's summary of a background command: `Background command "<description>" <outcome>`. */
const COMMAND_SUMMARY = 'Background command "';
/** An outcome that the "done" chip already says. */
const PLAIN_DONE = /^completed(?: \(exit code 0\))?\.?$/i;

/**
 * The second line of a finished row: its summary, when it says more than the row. A summary that
 * repeats the name (also cut by the station at 200 characters, `…`; or quoting it in Claude's
 * "Background command" sentence: then only the outcome counts, "Failed with exit code 1") or a
 * plain success says nothing new.
 */
export function summaryNote(summary: string | null, name: string): string | null {
  const text = firstLine(summary ?? '');
  if (text === '' || text === name) return null;
  if (text.endsWith('…') && name.startsWith(text.slice(0, -1))) return null;
  if (!text.startsWith(COMMAND_SUMMARY)) return text;
  // The station cuts a long summary: a sentence cut inside the quote has no outcome.
  const close = text.indexOf('" ', COMMAND_SUMMARY.length);
  const outcome = close < 0 ? '' : text.slice(close + 2).trim();
  if (outcome === '' || PLAIN_DONE.test(outcome)) return null;
  return outcome.charAt(0).toUpperCase() + outcome.slice(1);
}

/** The place of a row in its group: what runs first, then what failed, then the rest. */
const rank = (status: ShownStatus): number => {
  if (isLive(status)) return 0;
  return status.tone === 'hazard' ? 1 : 2;
};

/** The rows of a group in that order; rows of one rank keep the order in which they started. */
function ranked<T>(rows: readonly T[], status: (row: T) => ShownStatus): T[] {
  return rows
    .map((row, at) => ({ row, at, rank: rank(status(row)) }))
    .sort((a, b) => a.rank - b.rank || a.at - b.at)
    .map(({ row }) => row);
}

/** A group heading of the menu: its name and its count. */
const GroupHead = ({ label, count }: { label: string; count: number }) => (
  <p className="fd-label fd-agent-group">
    {label} <span className="fd-count">{countText(count)}</span>
  </p>
);

/**
 * The summary of the menu. The pill counts the subagents (the Properties panel shows the same
 * number), the background commands follow in quiet words, and what runs now (both kinds) ends it
 * with the working glyph. A session with commands only says "Commands" and their count.
 */
function Summary(props: { subagents: number; commands: number; running: number }) {
  const { subagents, commands, running } = props;
  return (
    <>
      {subagents > 0 ? 'Agents' : 'Commands'}{' '}
      <span className="fd-count">{countText(subagents > 0 ? subagents : commands)}</span>
      {subagents > 0 && commands > 0 ? (
        <span className="fd-agents-more">{`· ${commandsText(commands)}`}</span>
      ) : null}
      {running > 0 ? (
        <span className="fd-agents-running">
          <span className="fd-dot" data-status="working" aria-hidden="true" />
          {`${countText(running)} running`}
        </span>
      ) : null}
    </>
  );
}

/** The "Agents" control with its menu. The caller shows it only when `hasAgents` is true. */
export function AgentsMenu(props: AgentsMenuProps) {
  const { subagents, tasks, toolTitles, agent, onChoose } = props;
  const running = props.running ?? runningCount(subagents, tasks);
  const commands = commandTasks(tasks);
  const agentRows = ranked(subagents, (sub) => shownStatus(sub, tasks));
  const commandRows = ranked(commands, (task) => taskStatus(task.status));
  const choose = (close: () => void, choice: AgentChoice) => (event: MouseEvent) => {
    event.currentTarget.closest('details')?.querySelector('summary')?.focus();
    close();
    onChoose(choice);
  };
  return (
    <Menu
      summaryClass="fd-ghost fd-sm"
      variant="toggle"
      summary={
        <Summary subagents={subagents.length} commands={commands.length} running={running} />
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
          {agentRows.length > 0 ? <GroupHead label="Agents" count={agentRows.length} /> : null}
          {agentRows.map((sub) => {
            const name = subagentName(sub);
            const status = shownStatus(sub, tasks);
            const note = isLive(status)
              ? null
              : summaryNote(taskOf(sub, tasks)?.summary ?? null, name);
            return (
              <button
                key={sub.node_id}
                type="button"
                className="fd-agent-row fd-selbar"
                data-agent={sub.node_id}
                aria-current={agent === sub.node_id ? 'true' : undefined}
                onClick={choose(close, { kind: 'agent', node_id: sub.node_id })}
              >
                <span className="fd-agent-line">
                  <span className="fd-agent-name" title={name}>
                    {name}
                  </span>
                  <Chip status={status} />
                </span>
                {note === null ? null : <span className="fd-agent-dim">{note}</span>}
              </button>
            );
          })}
          {commandRows.length > 0 ? (
            <GroupHead label="Background commands" count={commandRows.length} />
          ) : null}
          {commandRows.map((task) => {
            const node = task.node_id;
            const has = node !== null && toolTitles.has(node);
            const { name, code, raw } = commandName(task, toolTitles);
            const note = isRunningStatus(task.status) ? null : summaryNote(task.summary, raw);
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
                  <span
                    className="fd-agent-name"
                    data-code={code ? '' : undefined}
                    title={name || undefined}
                  >
                    {name || 'Background command'}
                  </span>
                  <Chip status={taskStatus(task.status)} />
                </span>
                {note === null ? null : <span className="fd-agent-dim">{note}</span>}
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
