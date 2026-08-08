// The mail / command / questions request+response shapes exchanged between the
// board (`board/src/api.js`) and the daemon (`http.mjs`, `commands.mjs`,
// `questions.mjs`). These are LESS hostile than the hook/spawn boundaries (the
// board is same-origin and token-gated), so they carry types but no dedicated
// runtime validator — the daemon's existing per-route parsing stands until each
// module converts.
//
// Response envelopes: the daemon speaks two error dialects historically —
// `{ ok:false, reason }` (newer) and `{ ok:false, err }` (older, still emitted
// by the questions-answer route). Both are modelled so a converted board can
// read either without the hand-written `reason || err` dance.
//
// Pure module — no node/bun/DOM globals.

// ---------------------------------------------------------------------------
// Shared response envelopes
// ---------------------------------------------------------------------------

export interface ApiOk {
  ok: true;
}

// Newer error dialect. `reason` is the human-readable refusal.
export interface ApiErrReason {
  ok: false;
  reason: string;
}

// Older error dialect, still emitted by /api/questions/:id/answer.
export interface ApiErrErr {
  ok: false;
  err: string;
}

export type ApiResult = ApiOk | ApiErrReason;

// ---------------------------------------------------------------------------
// Mail — POST /mail
// ---------------------------------------------------------------------------

// A mail target: a session_id, a callsign, the literal 'all', or 'repo:<name>'.
// Modelled as a string because the daemon resolves the routing; the shape is
// documented rather than enum-constrained.
export type MailTarget = string;

export interface MailRequest {
  schema_version?: number;
  to: MailTarget;
  from: string;
  text: string;
}

// The mail clamp reports a truncation receipt when the body exceeded the cap.
export type MailResponse =
  (ApiOk & { truncated?: boolean; original_length?: number }) | ApiErrReason;

// ---------------------------------------------------------------------------
// Command — POST /command (orchestrator DSL: broadcast / assign / assign_auto)
// ---------------------------------------------------------------------------

export interface CommandRequest {
  schema_version?: number;
  text: string;
}

// Success carries how many sessions were reached; a too-long body is refused
// atomically with the cap and the framed length (commands.mjs command()).
export type CommandResponse =
  | (ApiOk & { delivered?: number })
  | (ApiErrReason & { max_length?: number; original_length?: number });

// ---------------------------------------------------------------------------
// Questions — POST /api/questions/:id/answer  and  /dismiss
// ---------------------------------------------------------------------------

// The answer body MUST match the question's kind (questions.mjs rejects a
// mismatch with 400 `{ ok:false, err }`):
//   permission  → { behavior }               ('capture' only on an ExitPlanMode plan question)
//   elicitation → { action, content? }
//   choice      → { answers } | { text }
export type PermissionBehavior = 'allow' | 'deny' | 'capture';

export interface PermissionAnswer {
  behavior: PermissionBehavior;
}

export type ElicitationAnswer =
  { action: 'accept'; content: Record<string, unknown> } | { action: 'decline' };

export type ChoiceAnswer = { answers: Record<string, string> } | { text: string };

export type QuestionAnswerBody = PermissionAnswer | ElicitationAnswer | ChoiceAnswer;

// 200 `{ ok:true }` or 400 `{ ok:false, err }` (this route uses the older dialect).
export type QuestionAnswerResponse = ApiOk | ApiErrErr;
