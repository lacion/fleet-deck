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
