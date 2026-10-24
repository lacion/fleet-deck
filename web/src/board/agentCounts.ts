// The agent counts of a card and of the fleet (workflow-agents design §4.7, B19).
import type { DeckSession, SessionSummary } from '../gen';
import { runningSubagents } from '../session/timeline';
import { isLiveSession } from './fleet';

/** Running agents of a card: the summary's `running_agents` (new decks), else the older sum. */
export function runningAgents(s: SessionSummary): number {
  return Math.max(s.running_agents ?? 0, runningSubagents(s));
}

/** True when the count of the card is a lower bound (design §4.3). */
export function agentsPartial(s: SessionSummary): boolean {
  return s.agents_partial === true;
}

/** Background tasks that are neither subagents nor workflows. */
export function backgroundCommands(s: SessionSummary): number {
  return Math.max(
    0,
    s.background_tasks.length - s.background_agents - (s.background_workflows ?? 0),
  );
}

/**
 * The fleet total: live cards whose activity is working or background, plus the running agents of
 * every live card.
 */
export function atWork(cards: readonly DeckSession[]): {
  sessions: number;
  agents: number;
  partial: boolean;
} {
  const live = cards.filter(isLiveSession);
  return {
    sessions: live.filter(
      (c) => c.summary.activity === 'working' || c.summary.activity === 'background',
    ).length,
    agents: live.reduce((n, c) => n + runningAgents(c.summary), 0),
    partial: live.some((c) => agentsPartial(c.summary)),
  };
}
