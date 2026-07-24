// helpText.js — the single source of truth for what the board can do, shared by
// the HelpOverlay (the "?" button) and Compose's command chips. The hotkey list
// here IS the list useBoardHotkeys implements; when a key is added there, add it
// here in the same commit — the overlay is how anyone finds it.

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
  { icon: '▣', name: 'terminal (card chip)', does: 'a live terminal onto that agent’s pane — floating: drag to move, corner to resize, ─ to minimize to the dock' },
  { icon: '⌗', name: 'move to tmux (card chip)', does: 'adopt a session you started yourself into a board-owned pane' },
  { icon: '⟲', name: 'revive (card chip)', does: 'an offline agent whose worktree + transcript survived — resume it (card returns to QUEUED)' },
  { icon: '☠', name: 'kill (card chip)', does: 'stop a spawned agent — asks first; worktree and branch are left alone' },
  { icon: '✎', name: 'rename (card chip)', does: 'rename the session’s suffix; `ticket` in Compose pins a ticket name' },
  { icon: '▸', name: 'git output (card chip)', does: 'on a spawn that died cloning: what git actually printed — the note is only the last line, the remedy is usually just above it' },
];
