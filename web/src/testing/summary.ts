// Test fixtures. A summary shaped exactly like the Rust `SessionSummary::new`
// output: if the generated type changes, this literal stops type-checking.
import type { Activity, SessionSummary, Usage } from '../gen';

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
    title_generated: false,
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
    artifacts_count: 0,
    hibernated: false,
    session_seq: 0,
    updated_at: updatedAt,
    ...extra,
  };
}

/** A usage counted by the rules of scheme 2 (`USAGE_SCHEME`); a part that is not given is unknown. */
export function newUsage(parts: Partial<Usage> = {}): Usage {
  return {
    input_tokens: null,
    output_tokens: null,
    cost_usd: null,
    cost_inferred: false,
    scheme: 2,
    ...parts,
  };
}
