---
description: Show the Fleet Deck board URL and a live summary of every Claude Code session on this machine
allowed-tools: Bash(curl:*), Bash(cat:*)
---

## Fleet endpoint and state (raw)

!`PORT=${FLEETDECK_PORT:-4711}; case "$PORT" in ''|*[!0-9]*|??????*) PORT=4711;; *) if [ "$PORT" -lt 1 ] || [ "$PORT" -gt 65535 ]; then PORT=4711; fi;; esac; printf 'FLEET_BOARD_URL=http://127.0.0.1:%s/\n' "$PORT"; TOKEN=$(cat "${FLEETDECK_HOME:-$HOME/.fleetdeck}/token" 2>/dev/null); curl -sf -m 2 ${TOKEN:+-H "Authorization: Bearer $TOKEN"} "http://127.0.0.1:$PORT/state" || echo FLEET_DAEMON_DOWN`

## Your task

Report on the fleet using ONLY the endpoint and raw state above. The first line
is the exact board URL for this environment; use its value verbatim. Print
exactly this, nothing more:

If the raw state is `FLEET_DAEMON_DOWN` (or empty / not JSON):

> Board: <the FLEET_BOARD_URL value above>
> fleet daemon not running — it starts with your next session

Otherwise a compact summary (aim for under 15 lines):

1. `Board: <the FLEET_BOARD_URL value above>`
2. Sessions grouped by column, in this order: needsyou, working, verifying, queued, idle, offline. One line per column that has sessions: `working (2): NOVA — fleetdeck (main) · editing derive.mjs, ...` using each session's `callsign`, `repo_name`, `branch`, and `note`/`task` when present. Skip empty columns; summarize offline as a count only (e.g. `offline: 3`).
3. Conflicts: if `conflicts` is non-empty, one line per conflict: `⚠ <rel_path> — <severity> — sessions: <callsigns or ids>`. Otherwise `conflicts: none`.
4. Mail: `mail_pending` is a per-session count map `{<session_id>: <count>}` (sessions with no queued mail appear with 0). Compute N as the total across all sessions — the `sum` of every value in the map. If N > 0: `mail: N queued — ` followed by a per-route breakdown derived from `mail_meta` (only list routes that have queued mail): `watcher` = wakes immediately, `pane` = typed into the pane after a grace window, `turn-boundary` = delivered at the next turn boundary, `offline-queued` = held until the session is resumed. Example: `mail: 3 queued — 1 wakes immediately (watcher), 1 typed into pane, 1 held until resume (offline)`. Otherwise omit the line.
