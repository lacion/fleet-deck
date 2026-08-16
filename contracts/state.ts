// The `/state` snapshot — the shape the daemon broadcasts and the board reads.
// This module REPLACES the prose comment-contract that used to live in
// `board/src/useFleetState.js`: the board now imports these types, so a field
// the daemon moves can no longer silently disagree with what the board expects
// (F1a: "kill the useFleetState.js comment-contract").
//
// Typed EXACTLY against the emitter, `scripts/fleetd/snapshot.mjs`
// (`createSnapshot().snapshot()`), plus the `/state` and `/ws` envelopes in
// `scripts/fleetd/http.mjs`. When `snapshot.mjs` converts to `.ts` it must
// satisfy `Snapshot`; when `http.mjs` converts, its two envelopes must satisfy
// `StateResponse` / `WsSnapshot`. Any mismatch surfaced then is a real finding
// for `ts-migration-bugs.md`, and the fix tightens THIS file.
//
// Pure module (no node/bun/DOM globals) — inlined by esbuild into the daemon
// bundle and by Vite into the board bundle. Where a field's exact interior is
// not yet load-bearing for the board (e.g. `Settings` sub-choices, `mdns`), it
// is modelled from the emitter and marked provisional; it tightens as the
// owning daemon module converts.

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

// The spawn facet of a session — present ONLY when a /api/spawn row owns the
// session (snapshot.mjs attaches it conditionally). Credential-safe: the
// origin_url is deliberately NOT surfaced here (it is in repo_catalog, scrubbed).
export interface SessionSpawn {
  spawn_id: string;
  tmux_window: string | null;
  status: string;
  kind: 'claude' | 'shell';
  setup_cmd: string | null;
  stalled: boolean;
  stall_detail: string | null;
  fail_detail: string | null;
  skip_permissions: boolean;
  remote: { enabled: boolean; url: string | null };
  gateway: boolean;
  requested_branch: string | null;
  branch_mode: string | null;
  revivable: boolean;
}

// Adoption state — ALWAYS present on a session entry (snapshot.mjs emits it
// unconditionally). `eligible` is the tri-state the board's adopt affordance
// keys on.
export interface AdoptState {
  eligible: 'now' | 'arm' | null;
  armed: boolean;
  armed_until: number | null;
  armed_skip: boolean;
}

// One row in the board's session grid. Every `?? null` field in the emitter is
// present-but-nullable (`T | null`), not optional; only `spawn` is an optional
// KEY (absent when no spawn row owns the session).
export interface SessionEntry {
  session_id: string;
  callsign: string;
  ticket: string | null;
  ticket_source: string | null;
  prev_callsign: string | null;
  model: string | null;
  cwd: string | null;
  branch: string | null;
  col: string;
  note: string | null;
  task: string | null;
  files: string[];
  lastTool: string | null;
  events: number;
  startedAt: number | null;
  lastSeen: number | null;
  endedAt: number | null;
  repo_id: string | null;
  repo_name: string | null;
  worktree: string | null;
  source: string | null;
  notification_type: string | null;
  sparkline: number[];
  stale: boolean;
  spawn?: SessionSpawn;
  adopt: AdoptState;
}

// ---------------------------------------------------------------------------
// Repos
// ---------------------------------------------------------------------------

// Live per-repo activity counts (repos with at least one session).
export interface RepoEntry {
  repo_id: string;
  repo_name: string;
  active: number;
  total: number;
}

// A known checkout in the catalog. `origin_url` is credential-scrubbed by the
// emitter (scrubUrlCredentials) before it ever reaches the wire.
export interface RepoCatalogEntry {
  repo_id: string;
  repo_name: string;
  root: string;
  origin_url: string | null;
  default_branch: string | null;
  last_used_at: number;
}

// ---------------------------------------------------------------------------
// Ticker / conflicts
// ---------------------------------------------------------------------------

export interface TickerEntry {
  at: number;
  msg: string;
}

// A file-collision warning. `file` duplicates `rel_path` (legacy board field);
// both are the repo-relative path. Guarded-parse on the emitter drops corrupt
// rows rather than 500-ing the snapshot.
export interface ConflictEntry {
  at: number;
  repo_id: string;
  rel_path: string;
  file: string;
  severity: string;
  sessions: string[];
  callsigns: string[];
}

// ---------------------------------------------------------------------------
// Mail
// ---------------------------------------------------------------------------

// How a piece of mail is expected to reach the session — the board renders a
// different chip per route.
export type MailRoute = 'watcher' | 'pane' | 'offline-queued' | 'turn-boundary';

export interface MailMeta {
  queued: number;
  oldest_at: number | null;
  route: MailRoute;
}

// ---------------------------------------------------------------------------
// Questions (questions.listForState)
// ---------------------------------------------------------------------------

// A live or recently-resolved AskUserQuestion / permission / elicitation card.
// `payload` and `answer` are guarded-parsed JSON (object or null); their
// interior is provider-shaped and stays `unknown` until questions.mjs converts.
export interface QuestionEntry {
  id: string;
  kind: string;
  session_id: string;
  callsign: string | null;
  payload: unknown;
  status: string;
  created_at: number;
  expires_at: number | null;
  answered_at: number | null;
  answer: unknown;
  held: boolean;
  plan_id?: number;
}

// ---------------------------------------------------------------------------
// Spawn capability / orphans / plans
// ---------------------------------------------------------------------------

// Whether the daemon can spawn right now, and how many spawns are active.
// `reason` is present only when unavailable or on a test/override path.
export interface SpawnCapability {
  available: boolean;
  active: number;
  reason?: string;
}

// A scoped tmux window with no owning row — a candidate for adoption.
export interface SpawnOrphan {
  window: string;
}

// A captured plan (the plan library). `via` is the emitter's `executed_via`.
export interface PlanEntry {
  plan_id: number;
  session_id: string;
  callsign: string | null;
  repo_id: string | null;
  repo_name: string | null;
  plan_md: string;
  created_at: number;
  status: string;
  via: string | null;
}

// ---------------------------------------------------------------------------
// Settings (resolveSettings) — the shared board settings contract
// ---------------------------------------------------------------------------

export interface PathChoice {
  value: string;
  source: 'default' | 'env' | 'override' | 'detected';
  resolved: string;
}

export interface Settings {
  repos_dir: PathChoice;
  repo_transport: { value: 'ssh' | 'https'; source: 'default' | 'override' };
  repo_default_org: {
    value: string | null;
    source: 'override' | 'env' | 'coder' | 'default';
  };
  // browse_root's value/resolved go null only if the daemon user has no home.
  browse_root: {
    value: string | null;
    source: 'override' | 'env' | 'detected' | 'default';
    resolved: string | null;
  };
  fav_dirs: string[];
  repo_setup: Record<string, string>;
  hold_ms: { value: number; source: 'env' | 'override' | 'default' };
  gateway: {
    base_url: string | null;
    auth_style: 'bearer' | 'api-key';
    token_set: boolean;
    model_discovery: boolean;
    default: boolean;
    ready: boolean;
  };
}

// ---------------------------------------------------------------------------
// LAN / legacy-upgrade envelope fields
// ---------------------------------------------------------------------------

// The LAN-reachability block — folded in ONLY on the `/state` poll (http.mjs
// currentLan()); the `/ws` frame omits it, which is why the board defaults it
// to null and preserves the prior value across WS pushes.
export type Lan =
  | { enabled: true; urls: string[]; mdns: string | null }
  | { enabled: false; urls: string[] };

// Pre-0.16.0 hook sessions the daemon has seen (bare session ids) and how many
// have upgraded. NOT secret — rides even the tokenless /ws frame.
export interface LegacyUpgrade {
  sessions: string[];
  upgraded: number;
}

// ---------------------------------------------------------------------------
// The snapshot + its two wire envelopes
// ---------------------------------------------------------------------------

// The core object `core.snapshot()` returns. `schema_version` is REQUIRED here
// (F1a: on every shape, from day one) — when snapshot.mjs converts to `.ts` the
// type forces it to stamp WIRE_SCHEMA_VERSION on every broadcast.
export interface Snapshot {
  schema_version: number;
  up_ms: number;
  uptime_ms: number;
  version: string;
  sessions: SessionEntry[];
  repos: RepoEntry[];
  repo_catalog: RepoCatalogEntry[];
  settings: Settings;
  home_dir: string;
  ticker: TickerEntry[];
  conflicts: ConflictEntry[];
  mail_pending: Record<string, number>;
  mail_meta: Record<string, MailMeta>;
  questions: QuestionEntry[];
  spawn: SpawnCapability;
  spawn_orphans: SpawnOrphan[];
  plans: PlanEntry[];
}

// GET /state — the snapshot plus the LAN block and the legacy-upgrade banner
// (http.mjs snapshotWithLan()).
export type StateResponse = Snapshot & {
  lan: Lan;
  legacy_upgrade: LegacyUpgrade;
};

// The /ws push frame — the snapshot tagged with a discriminator and the
// legacy-upgrade banner, but NO lan (http.mjs line ~1349).
export type WsSnapshot = Snapshot & {
  type: 'snapshot';
  legacy_upgrade: LegacyUpgrade;
};
