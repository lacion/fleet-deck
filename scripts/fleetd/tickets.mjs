// tickets.mjs — pure Jira-key extraction (no deps, no per-core state). Two
// consumers: branch auto-detect (ticketFromBranch, permissive — pull the key
// out of a longer branch name) and the manual `ticket` command (normalizeTicket,
// strict — the whole argument must BE a key). Both agree on the same key grammar
// so `proj-55` typed by hand and `PROJ-55` auto-read off a branch land identical.

// Strict Atlassian issue-key shape: an uppercase letter, then 1–9 more
// uppercase alnum (key 2–10 chars), a hyphen, then an integer with NO leading
// zero. ONE source of truth — both regexes below are built from it, so the
// branch detector and the manual command can never drift apart on the grammar.
const KEY_CORE = '([A-Z][A-Z0-9]{1,9})-([1-9][0-9]*)';

// Embedded form for branch names. The lookbehind/lookahead are the boundary
// guard — the key must not be glued to a longer alphanumeric run, so
// `XPROJ-123abc` and `PROJ-001` must NOT partially match. Leftmost match wins
// (no /g), so `PROJ-1-ENG-2` yields `PROJ-1`. No global flag: exec carries no
// lastIndex state to reset.
export const TICKET_RE = new RegExp(`(?<![A-Za-z0-9])${KEY_CORE}(?![A-Za-z0-9])`);

// Whole-string form for the manual command (see normalizeTicket).
const TICKET_EXACT_RE = new RegExp(`^${KEY_CORE}$`);

// Auto-detect: extract the first ticket key embedded anywhere in a branch name.
// `feature/PROJ-123-checkout` → `PROJ-123`; `fd/raven-PROJ-123` → `PROJ-123`;
// `audit-cleanup` / `viper-c7a7` → null. Only ever fed the SERVER-derived branch
// (branchOf), never the client-supplied git_branch fallback.
export function ticketFromBranch(branch) {
  if (typeof branch !== 'string' || !branch) return null;
  const m = TICKET_RE.exec(branch);
  return m ? `${m[1]}-${m[2]}` : null;
}

// Manual command: the whole argument must be a single key (after trim +
// uppercase). `proj-55` → `PROJ-55`; `PROJ-007` → null (leading zero); anything
// with surrounding text → null. Returns the canonical uppercase key or null so
// the command handler can reject a malformed key loudly.
export function normalizeTicket(raw) {
  const s = String(raw ?? '').trim().toUpperCase();
  return TICKET_EXACT_RE.test(s) ? s : null;
}

// The callsign grammar (`<animal>-<suffix>`) is composed in assignCallsign;
// this is its ONE decomposer — the animal is everything before the first
// hyphen (the suffix itself may contain hyphens: `otter-PROJ-123`). Every
// consumer that needs the animal back out of a callsign goes through here so
// the format has a single owner.
export function animalOf(callsign) {
  return String(callsign).split('-')[0];
}
