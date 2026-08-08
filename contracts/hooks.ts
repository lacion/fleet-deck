// The canonical hook-event vocabulary and the intake body a Claude Code hook
// POSTs to `/hook/<Name>`. This is HOSTILE input: the body crosses a process
// boundary from a hook script we do not fully control, so a hand-written
// runtime validator sits beside the static type (F1a's second non-negotiable).
//
// The daemon's dispatch contract is FAIL-OPEN: a malformed body is rejected
// (dispatch skipped) but the endpoint still answers 200 `{}`, because a hook
// that hangs or errors stalls the user's Claude turn. `validateHookEvent`
// therefore gates only what makes a body undispatchable — a missing or blank
// `session_id` — and passes everything else through, exactly as http.mjs does
// today. Wiring it in later must not change a single test outcome.
//
// Pure module — safe in board, daemon source, Bun, and both bundles.

import type { ValidationResult } from './validate.ts';
import { fail, isNonEmptyString, isRecord, ok } from './validate.ts';

// The complete set of hook events the daemon recognises. `as const` so the
// union type is derived from the one list — add an event in exactly one place.
export const HOOK_EVENT_NAMES = [
  'SessionStart',
  'UserPromptSubmit',
  'PreToolUse',
  'PostToolUse',
  'PostToolUseFailure',
  'Stop',
  'SessionEnd',
  'Notification',
  'FileChanged',
  'CwdChanged',
  'PermissionRequest',
  'Elicitation',
  'AskUserQuestion',
] as const;

export type HookEventName = (typeof HOOK_EVENT_NAMES)[number];

export function isHookEventName(v: unknown): v is HookEventName {
  return typeof v === 'string' && (HOOK_EVENT_NAMES as readonly string[]).includes(v);
}

// The wire body of a hook POST. `session_id` is the only guaranteed field;
// everything else is optional and provider-shaped — the daemon reads each field
// defensively and telemetry ingests unknown extras. The interface names the
// fields the daemon actually reads; providers legitimately send more, which is
// present at runtime but intentionally not modelled here.
export interface HookEventBody {
  session_id: string;
  schema_version?: number;
  hook_event_name?: string;
  cwd?: string;
  git_branch?: string;
  // Claude sends the model either as a bare id or as a descriptor object.
  model?: string | { display_name?: string; id?: string };
  transcript_path?: string;
  source?: string;
  reason?: string;
  prompt?: string;
  tool_name?: string;
  tool_input?: Record<string, unknown>;
  file_path?: string;
  path?: string;
  notification_type?: string;
  message?: string;
  matcher?: string;
}

// Reject only what the daemon cannot safely dispatch; pass the rest through.
// Returns the body typed as HookEventBody on success — a structural assertion,
// not a deep clone, because the daemon reads the live object.
export function validateHookEvent(input: unknown): ValidationResult<HookEventBody> {
  if (!isRecord(input)) {
    return fail('hook body must be a JSON object');
  }
  if (!isNonEmptyString(input['session_id'])) {
    return fail('hook body requires a non-empty string session_id');
  }
  // Route through `unknown`: `input` is a `Record<string, unknown>` (from
  // `isRecord`), whose index signature does not structurally satisfy the
  // required `session_id`, even though we just proved it present. This is a
  // structural assertion over the LIVE object, not a clone — the daemon reads
  // the same reference it was handed.
  return ok(input as unknown as HookEventBody);
}
