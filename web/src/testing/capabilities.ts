// M2 P5-3: the capabilities of a new Claude and a new Codex session under
// the station defaults (no bypass mode, no full access), as plain values so
// the Playwright fake deck can import them. `capabilities.test.ts` checks
// that they equal the `session.started` events of the recorded fixtures,
// which the Rust replay tests write from the production cores.
import type { Capabilities } from '../gen';

export function claudeCaps(): Capabilities {
  return {
    resume: true,
    fork: true,
    rollback: false,
    requests_survive_restart: false,
    interrupt: true,
    steer: true,
    native_follow_up: true,
    terminal_status: 'strong',
    live_deltas: true,
    file_changes: true,
    approvals: true,
    response_channels: ['rpc_reply'],
    edit_input: true,
    modes: ['default', 'plan', 'acceptEdits', 'dontAsk', 'auto'],
    set_mode: true,
    set_model: true,
    set_effort: true,
    plan_capture: true,
    subagents: true,
    background_work: 'pushed',
    native_turn_ids: false,
    usage: { tokens: true, cost: true },
  };
}

export function codexCaps(): Capabilities {
  return {
    resume: true,
    fork: true,
    rollback: true,
    requests_survive_restart: false,
    interrupt: true,
    steer: true,
    native_follow_up: false,
    terminal_status: 'strong',
    live_deltas: true,
    file_changes: true,
    approvals: true,
    response_channels: ['rpc_reply', 'message'],
    edit_input: false,
    modes: [
      'untrusted/read-only',
      'on-request/read-only',
      'never/read-only',
      'untrusted/workspace-write',
      'on-request/workspace-write',
      'never/workspace-write',
    ],
    set_mode: true,
    set_model: true,
    set_effort: true,
    plan_capture: true,
    subagents: true,
    background_work: 'polled',
    native_turn_ids: true,
    usage: { tokens: true, cost: false },
  };
}

/** A provider that reports no optional capability (`Capabilities::minimal`). */
export function minimalCaps(): Capabilities {
  return {
    resume: false,
    fork: false,
    rollback: false,
    requests_survive_restart: false,
    interrupt: false,
    steer: false,
    native_follow_up: false,
    terminal_status: 'none',
    live_deltas: false,
    file_changes: false,
    approvals: false,
    response_channels: [],
    edit_input: false,
    modes: [],
    set_mode: false,
    set_model: false,
    set_effort: false,
    plan_capture: false,
    subagents: false,
    background_work: 'none',
    native_turn_ids: false,
    usage: { tokens: false, cost: false },
  };
}
