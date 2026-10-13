// Test fixtures. A summary shaped exactly like the Rust `SessionSummary::new`
// output: if the generated type changes, this literal stops type-checking.
import type { Activity, SessionSummary } from '../gen';

export function summary(
  id: string,
  activity: Activity,
  updatedAt: number,
  extra: Partial<SessionSummary> = {},
): SessionSummary {
  return {
    session_id: id,
    provider: 'claude',
    provider_instance: null,
    provider_version: null,
    cwd: null,
    branch: null,
    callsign: null,
    worktree: null,
    title: null,
    state: 'running',
    activity,
    mode: null,
    model: null,
    capabilities: null,
    current_run: null,
    current_turn: null,
    open_requests: [],
    background_tasks: [],
    background_agents: 0,
    running_tools: [],
    usage: { input_tokens: null, output_tokens: null, cost_usd: null, cost_inferred: false },
    rate_limits: [],
    repo: null,
    last_error: null,
    revert: null,
    auto_continue: false,
    session_seq: 0,
    updated_at: updatedAt,
    ...extra,
  };
}
