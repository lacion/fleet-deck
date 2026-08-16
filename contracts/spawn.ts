// The POST /api/spawn request body — the second HOSTILE boundary. This is the
// RCE-adjacent surface (it launches processes), so its rich semantic parser
// already lives in `scripts/fleetd/spawns.mjs` and returns `{ ok:false, reason }`
// for every malformed field. This module contributes the FORMAL shared type and
// a fail-open structural gate; the deep field validation folds into a single
// typed pass when spawns.mjs converts to `.ts` (Phase 5), and any mismatch found
// then is a `ts-migration-bugs.md` entry.
//
// The structural gate here rejects ONLY a non-object body — the one condition
// spawns.mjs cannot meaningfully parse — so wiring it in ahead of the existing
// parser cannot change a test outcome.
//
// Pure module — no node/bun/DOM globals.

import type { ValidationResult } from './validate.ts';
import { fail, isRecord, ok } from './validate.ts';

export type SpawnKind = 'claude' | 'shell';

// Accepted case-insensitively on the wire; spawns.mjs normalises the casing.
export const PERMISSION_MODES = ['default', 'acceptEdits', 'plan', 'bypassPermissions'] as const;
export type PermissionMode = (typeof PERMISSION_MODES)[number];

export type RepoHost = 'github' | 'gitlab';
export type RepoTransport = 'ssh' | 'https';
export type BranchMode = 'existing' | 'new' | 'detached';

// The full spawn request. Every field is optional on the WIRE (the parser
// applies defaults and enforces the claude/shell XOR rules); `kind` selects the
// two mutually-exclusive field sets. `permission_mode` is typed as the raw
// string because it arrives case-insensitively and is normalised server-side.
export interface SpawnRequest {
  schema_version?: number;
  kind?: SpawnKind;
  // cwd XOR repo — one names an existing directory, the other a repo to clone.
  cwd?: string;
  repo?: string;
  branch?: string;
  branch_mode?: BranchMode;
  prompt?: string;
  model?: string;
  permission_mode?: string;
  repo_host?: RepoHost;
  repo_transport?: RepoTransport;
  repo_org?: string;
  worktree?: boolean;
  dangerously_skip_permissions?: boolean;
  remote_control?: boolean;
  gateway?: boolean;
  arm_token?: string;
  setup_cmd?: string;
  plan_id?: number;
}

// Structural gate only — object-ness. Semantic validation (XOR rules, enum
// membership, setup_cmd control-char ban, plan_id positivity) stays in
// spawns.mjs until Phase 5 consolidates it here against SpawnRequest.
export function validateSpawnRequest(input: unknown): ValidationResult<SpawnRequest> {
  if (!isRecord(input)) {
    return fail('spawn body must be a JSON object');
  }
  return ok(input as SpawnRequest);
}
