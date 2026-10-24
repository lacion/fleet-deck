// B29: the five learnings of the analytics page (design analytics.md §3.12). Each one has a rule
// and a minimum sample; one that does not pass is not in the list. No invented history: a
// learning computed over a part of the window says `(since <Mon D>)`.
import { durationText } from '../chat/labels';
import { usdText } from '../ui/format';
import { HOUR_MS, type Merged } from './merge';

export type Learning = {
  readonly id: 'cache' | 'tool_failures' | 'approval_wait' | 'cost_per_turn' | 'unfinished_turns';
  readonly text: string;
  /** The rule, for the `title` of the item. */
  readonly title: string;
};

const KINDS: readonly (readonly [string, string])[] = [
  ['shell', 'Shell'],
  ['edit', 'Edit'],
  ['read', 'Read'],
  ['search', 'Search'],
  ['web', 'Web'],
  ['mcp', 'MCP'],
  ['subagent', 'Subagent'],
  ['other', 'Other'],
];

const percent = (part: number, whole: number): number => Math.round((part / whole) * 100);

/** `Oct 3`: a day in the browser's zone. */
export const monthDay = (ms: number): string =>
  new Date(ms).toLocaleDateString('en-US', { month: 'short', day: 'numeric' });

export function learnings(m: Merged): Learning[] {
  const out: Learning[] = [];
  const count = (key: string): number => m.counts.get(key) ?? 0;
  // A suffix when the coverage of a family starts after the window start. `all` has no start.
  const since = (start: number | null): string =>
    start !== null && m.window.since > 0 && start > m.window.since
      ? ` (since ${monthDay(start)})`
      : '';

  // cache
  const t = m.total;
  const input = t.input + t.cacheRead + t.cacheWrite;
  if (t.requests >= 100 && input > 0) {
    out.push({
      id: 'cache',
      text: `${percent(t.cacheRead, input)} % of the input came from the prompt cache.${since(m.usageSince)}`,
      title:
        'Cache read tokens divided by all input tokens (new input, cache read and cache write), from the provider transcripts.',
    });
  }

  // tool_failures: the kind with the highest failed share, among kinds with 20 calls or more.
  let worst: { label: string; calls: number; share: number } | null = null;
  for (const [kind, label] of KINDS) {
    const calls = count(`tools.${kind}`);
    if (calls < 20) continue;
    const share = count(`tools.failed.${kind}`) / calls;
    if (share >= 0.05 && (worst === null || share > worst.share)) worst = { label, calls, share };
  }
  if (worst !== null) {
    out.push({
      id: 'tool_failures',
      text: `${worst.label} calls fail most often: ${Math.round(worst.share * 100)} % of ${worst.calls} failed.${since(m.eventsSince)}`,
      title:
        'Failed tool calls divided by all tool calls of each kind, kinds with 20 calls or more.',
    });
  }

  // approval_wait
  const waited = count('approvals.waited');
  if (waited >= 5) {
    out.push({
      id: 'approval_wait',
      text: `An approval waited ${durationText(count('approvals.wait_ms') / waited)} on average for its answer (${waited} answered).${since(m.eventsSince)}`,
      title: 'The time from the request to the answer, averaged over the answered approvals.',
    });
  }

  // cost_per_turn: no partial value; any failed check hides it.
  const periodStart = Math.ceil(m.learningSince / HOUR_MS) * HOUR_MS;
  if (m.usageComplete && periodStart < m.window.until) {
    const turned = [...m.perSession.values()].filter((s) => s.turnsEnded > 0);
    const turns = turned.reduce((sum, s) => sum + s.turnsEnded, 0);
    const allUsed = turned.every((s) => s.totals.requests > 0);
    const noneUnpriced = turned.every((s) => s.totals.unpricedTokens === 0);
    if (turns >= 20 && allUsed && noneUnpriced) {
      const cost = turned.reduce((sum, s) => sum + s.totals.cost, 0);
      out.push({
        id: 'cost_per_turn',
        text: `A turn cost about ${usdText(cost / turns)} at API prices (estimate, ${turns} turns).${since(m.learningSince)}`,
        title:
          'The cost estimate of the proven runs of the sessions with ended turns, divided by those turns, in the same period; shown only when the transcripts of every station are fully read and every such session has usage.',
      });
    }
  }

  // unfinished_turns
  const done = count('turns.done');
  const bad = count('turns.interrupted') + count('turns.failed');
  if (done + bad >= 20) {
    out.push({
      id: 'unfinished_turns',
      text: `${percent(bad, done + bad)} % of ${done + bad} turns ended interrupted or failed.${since(m.eventsSince)}`,
      title: 'Interrupted and failed turns divided by all turns that ended.',
    });
  }
  return out;
}
