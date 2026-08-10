// helpText.js — the single source of truth for what the board can do, shared by
// the HelpOverlay (the "?" button) and Compose's command chips. The hotkey list
// here IS the list useBoardHotkeys implements; when a key is added there, add it
// here in the same commit — the overlay is how anyone finds it.
//
// Concepts and jargon are single-sourced here too: CONCEPTS feeds the overlay
// (and the first-run auto-open), GLOSSARY is where every `title=` tooltip and
// footer hint in the board copy ultimately resolves — define a term HERE, never
// inline in a component.

export const HOTKEYS = [
  { keys: 'j / k · ↓ / ↑', does: 'move the inbox selection' },
  { keys: 'y / n', does: 'allow / deny the selected permission — live even while a floating terminal is open (its header says where your keys go)' },
  { keys: '1-9', does: 'pick the n-th option of the selected choice' },
  { keys: 'Enter', does: 'focus the selected freeform question’s answer box' },
  { keys: 'c', does: 'open Compose (to all)' },
  { keys: '?', does: 'open this help' },
  { keys: 'Esc', does: 'close the topmost overlay — never a live terminal (Esc belongs to the agent’s TUI)' },
];

// The orchestrator's whole grammar (POST /command — parseCommand in
// scripts/fleetd/helpers.mjs). `chip` is the prefix Compose inserts when its
// example chip is clicked; commands without a chip are overlay-only.
export const ORCH_COMMANDS = [
  { syntax: 'broadcast <text>', does: 'mail every live session at once', chip: 'broadcast ' },
  { syntax: 'assign <callsign> <text>', does: 'route a task to one session as an assignment', chip: 'assign ' },
  { syntax: 'assign auto <text>', does: 'auto-route to the best available session (never spawns)', chip: 'assign auto ' },
  { syntax: 'assign auto:<repo> <text>', does: 'auto-route, but only within one repo' },
  { syntax: 'ticket <callsign> <PROJ-123|clear>', does: 'pin a ticket callsign to a session (clear reverts to the birth name)', chip: 'ticket ' },
  { syntax: 'name <callsign> <suffix|clear>', does: 'rename a card’s suffix — the animal stays', chip: 'name ' },
  { syntax: '(anything else)', does: 'logged as an orchestrator note' },
];

// One line per board affordance — header buttons first, then the card chips.
export const BOARD_ACTIONS = [
  { icon: '✉', name: 'Compose', does: 'mail a session, a repo, or everyone — or command the orchestrator (grammar below)' },
  { icon: '▦', name: 'Terminals', does: 'the wall of screens: every live agent at once; exactly one tile takes your keystrokes' },
  { icon: '+', name: 'Spawn', does: 'start a fresh claude in a daemon-owned tmux pane; batch mode runs one agent per line, each in its own worktree' },
  { icon: '⌸', name: 'Files', does: 'read-only file explorer from the browse root' },
  { icon: '⌫', name: 'Clear', does: 'archive every offline card, expire its mail, kill dead panes — worktrees are never touched' },
  { icon: '⑂', name: 'Worktrees', does: 'worktrees spawns left behind; the only place to remove one' },
  { icon: '⇄', name: 'Share', does: 'open this board from another device on your network' },
  { icon: '▣', name: 'terminal (card chip)', does: 'a live terminal onto that agent’s pane — floating: drag to move, corner to resize, ─ to minimize to the dock. To copy out of it: ⇧drag (⌥drag on a Mac) selects, then Ctrl+C (⌘C) copies — the agent’s TUI owns a plain drag, and a plain Ctrl+C is still its interrupt' },
  { icon: '⌗', name: 'move to tmux (card chip)', does: 'adopt a session you started yourself into a board-owned pane' },
  { icon: '⟲', name: 'revive (card chip)', does: 'an offline agent whose worktree + transcript survived — resume it (card returns to QUEUED)' },
  { icon: '☠', name: 'kill (card chip)', does: 'stop a spawned agent — asks first; worktree and branch are left alone' },
  { icon: '✎', name: 'rename (card chip)', does: 'rename the session’s suffix; `ticket` in Compose pins a ticket name' },
  { icon: '▸', name: 'git output (card chip)', does: 'on a spawn that died cloning: what git actually printed — the note is only the last line, the remedy is usually just above it' },
];

// The six core nouns — one line each, ordered the way a newcomer meets them.
export const CONCEPTS = [
  { term: 'session', def: 'one running Claude Code conversation — wherever it was started, the hooks report it here' },
  { term: 'card', def: 'a session on the board; its column is the daemon’s judgement, derived from hook telemetry, never self-reported' },
  { term: 'pane', def: 'the tmux pane a spawned session runs in — the board’s live terminals and “move to tmux” both attach to it' },
  { term: 'worktree', def: 'an isolated git checkout for a spawned agent, so two agents never edit the same checkout' },
  { term: 'plan', def: 'a proposal captured from a plan-mode session; approve it, or capture it and run it later from the PLANS library' },
  { term: 'mail vs orchestrator', def: 'mail is text delivered to a session at its next turn boundary; the ORCHESTRATOR is the daemon itself — it runs commands immediately, never replies' },
];

// The jargon. Full definitions live here for touch devices; components may show
// a title= tooltip as a bonus affordance, but must never carry a definition of
// their own.
export const GLOSSARY = [
  { term: 'turn boundary', def: 'the moment the agent stops and waits for input — queued mail and remote control apply only here' },
  { term: 'whisper', def: 'context the daemon injects into an agent’s hook reply (e.g. “a peer is editing this file”) — the agent reads it, you don’t' },
  { term: 'callsign', def: 'a session’s short human name (ember-fc8e) — ticket pins a task key to it, name renames its suffix' },
  { term: 'arm', def: 'pre-authorizing a session you started yourself to move into a board-owned pane at your next CLI exit — one-shot, cancellable, expires in ~30 min' },
  { term: 'rail', def: 'the NEEDS YOU column on the right — permissions and questions parked until a human answers' },
  { term: 'capture & release', def: 'approve a proposed plan into the PLANS library and let the planner session end — the plan outlives its author' },
  { term: 'worktree', def: 'an isolated git checkout per spawn — the agent’s branch, commits, and conflicts live there, not in your main checkout' },
];

// One line per column head — the title= on BoardLanes lane headers. Where a
// column lands is derived (see derive.mjs), so these describe, never promise.
export const COL_HINTS = {
  queued: 'session reported in, no prompt yet — waiting for its first task',
  working: 'the agent is mid-turn: prompting or using tools right now',
  verifying: 'the agent is running a test suite (Bash matched a test runner)',
  idle: 'at a turn boundary, waiting for input — queued mail delivers now',
  offline: 'session ended; ⟲ revive resumes it if the worktree + transcript survived',
};
