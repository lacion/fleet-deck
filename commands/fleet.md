---
description: Show the Fleet Deck board URL and a live summary of every Claude Code session on this machine
allowed-tools: Bash(curl:*)
---

## Fleet state (raw)

!`curl -sf -m 2 http://127.0.0.1:4711/state || echo FLEET_DAEMON_DOWN`

## Your task

Report on the fleet using ONLY the raw state above. Print exactly this, nothing more:

If the raw state is `FLEET_DAEMON_DOWN` (or empty / not JSON):

> Board: http://127.0.0.1:4711/
> fleet daemon not running — it starts with your next session

Otherwise a compact summary (aim for under 15 lines):

1. `Board: http://127.0.0.1:4711/`
2. Sessions grouped by column, in this order: needsyou, working, verifying, queued, idle, offline. One line per column that has sessions: `working (2): NOVA — fleetdeck (main) · editing derive.mjs, ...` using each session's `callsign`, `repo_name`, `branch`, and `note`/`task` when present. Skip empty columns; summarize offline as a count only (e.g. `offline: 3`).
3. Conflicts: if `conflicts` is non-empty, one line per conflict: `⚠ <rel_path> — <severity> — sessions: <callsigns or ids>`. Otherwise `conflicts: none`.
4. Mail: if `mail_pending` > 0: `mail: N queued — delivered at the next turn boundary`. Otherwise omit the line.
