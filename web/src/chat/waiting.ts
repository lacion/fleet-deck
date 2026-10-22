// The open request of a tool call (DESIGN-SYSTEM §4.5): the row of the call shows that it waits
// for your answer, and the separate "Waiting for your answer" line is left out.
import type { Item } from '../session/timeline';
import { isShellTool } from './ShellCard';

type Approval = Extract<Item, { type: 'approval' }>;
type Tool = Extract<Item, { type: 'tool' }>;

export type Waiting = {
  /** The keys of the tool rows that wait for an answer, with the title of the request. */
  readonly tools: ReadonlyMap<string, string>;
  /** The keys of the open requests that a tool row shows. */
  readonly merged: ReadonlySet<string>;
};

/** The node id of the call that a request belongs to, when the timeline gives it. */
const nodeOf = (request: Approval): string | null =>
  'node_id' in request && typeof request.node_id === 'string' ? request.node_id : null;

/** The request belongs to the call: the same node, else the same tool name. */
function sameCall(tool: Tool, request: Approval): boolean {
  const node = nodeOf(request);
  if (node !== null) return node === tool.node_id;
  return request.title === tool.title || request.title.startsWith(`${tool.title} `);
}

/**
 * Pairs each open request of `items` (one list of the timeline) with the running call before it
 * that it belongs to. A shell card keeps its own look, so it is never paired.
 */
export function pairWaiting(items: readonly Item[]): Waiting {
  const tools = new Map<string, string>();
  const merged = new Set<string>();
  items.forEach((item, at) => {
    if (item.type !== 'approval' || item.state !== 'open') return;
    for (let i = at - 1; i >= 0; i -= 1) {
      const prev = items[i];
      if (prev?.type !== 'tool' || prev.status !== 'running' || tools.has(prev.key)) continue;
      if (isShellTool(prev) || !sameCall(prev, item)) continue;
      tools.set(prev.key, item.title);
      merged.add(item.key);
      return;
    }
  });
  return { tools, merged };
}

/** True when a request of the timeline (or of a subagent in it) waits for an answer. */
export function hasOpenRequest(items: readonly Item[]): boolean {
  return items.some(
    (item) =>
      (item.type === 'approval' && item.state === 'open') ||
      (item.type === 'subagent' && hasOpenRequest(item.items)),
  );
}
