// env-scrub.mjs — the single source of truth for the Claude/agent session
// markers that must be stripped from a child process's environment before it
// boots a fresh, unrelated Claude (or a tmux server that would bake them into
// its global env). Two callers share this list:
//   - fleetd/derive.mjs claudeEnvArgvPrefix() — the `env -u` argv wrapper for
//     spawned/revived `claude` panes.
//   - fleet-sessionstart.mjs bootEnv() — the environment the SessionStart hook
//     hands the detached daemon it launches.
// Each caller adds its OWN context-specific vars (tmux plumbing, FLEETDECK_*
// tuning knobs) on top; only the markers common to both live here so they can
// never drift out of sync again.
export const CLAUDE_ENV_MARKERS = [
  'CLAUDECODE', 'CLAUDE_CODE_SESSION_ID', 'CLAUDE_CODE_CHILD_SESSION',
  'CLAUDE_CODE_BRIDGE_SESSION_ID', 'CLAUDE_CODE_ENTRYPOINT', 'CLAUDE_CODE_EXECPATH',
  'CLAUDE_ENV_FILE', 'CLAUDE_PROJECT_DIR', 'CLAUDE_PLUGIN_ROOT', 'CLAUDE_PLUGIN_DATA',
  'CLAUDE_EFFORT', 'AI_AGENT', 'CODEX_COMPANION_TRANSCRIPT_PATH',
  'CODEX_COMPANION_SESSION_ID',
];

// The LLM-gateway variables (0.15.0). Claude Code reads these to route its API
// traffic somewhere other than Anthropic — a local CLIProxyAPI, a corporate
// gateway, anything Anthropic-compatible. Fleet Deck now OWNS them per spawn
// (the `gateway` flag → settings.mjs resolveGatewayEnv), which means a pane must
// never also inherit an ambient copy: whether a session bills your Anthropic
// account or a proxy is not something to decide by accident.
//
// The accident was real before this list existed. The SessionStart hook boots
// the daemon from whatever shell the human happened to be in, tmux bakes that
// first client's environment into the SERVER's global env, and every later pane
// inherits it — so exporting ANTHROPIC_BASE_URL once, in one terminal, silently
// rerouted every board-spawned session on the machine for as long as that tmux
// server lived (the 2026-07-11 env-poisoning scar, new tenants). Scrubbing here
// makes the pane's routing exactly what the spawn asked for and nothing else.
//
// Both credential variables are listed because Claude Code sends them in
// DIFFERENT headers — ANTHROPIC_AUTH_TOKEN as `Authorization: Bearer …`,
// ANTHROPIC_API_KEY as `x-api-key` — so a stale one of either kind is a live
// credential leaking into a pane that did not ask for it.
export const GATEWAY_ENV_VARS = [
  'ANTHROPIC_BASE_URL', 'ANTHROPIC_AUTH_TOKEN', 'ANTHROPIC_API_KEY',
  'CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY',
];

// Per-launch setup is deliberately supplied to one tmux window via `-e`.
// Ambient copies must never reach another pane, and the wrapper unsets the
// value before exec'ing Claude.
export const SPAWN_ENV_VARS = ['FLEETDECK_SETUP_CMD'];
