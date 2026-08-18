# Hook reliability invariants

`hooks.json` uses Claude Code's exec form (`command` plus `args`) for every
hook and invokes one tiny POSIX launcher through `/bin/sh`. Keep the launcher
and bundle paths in `args` so spaces and shell metacharacters in the plugin
cache path cannot change what is executed. The launcher privately captures the
Bun process's streams and publishes output only after an allowed exit shape:
missing Bun, a missing bundle, loader errors, crashes, partial writes, and
diagnostic stderr all collapse to the same event-appropriate silent no-op.

Every automatic hook is gated by the exact Claude process that loaded it. A
supported stable version records an owner-only compatibility verdict for that
process; an unsupported, prerelease, missing, or corrupt verdict makes
`SessionStart` and the watcher exit 0 with empty streams, while decision hooks
exit 0 with exactly `{}` on stdout and empty stderr. The gate runs before token,
daemon, timer, or network work. That is the definition of *fail open* here:
Claude continues normally and Fleet Deck adds no warning, UI message, or model
context. The supported inclusive range lives only in `compatibility.json`.

`FileChanged` is intentionally not registered. Claude Code's watcher has no
exclusion contract: a dynamic cwd watch traverses `.git` and `node_modules`,
while exact matcher paths can themselves be large directories. `PostToolUse`
still records writes made through Claude's tools. Broad external-edit telemetry
stays disabled until Fleet Deck owns an ignore-capable watcher.

For Fleet Deck-owned board sessions, `FLEETDECK_BOARD_SESSION` contains the
exact pre-created Claude session id. The hook grants the long wait only when
that value equals the incoming payload's `session_id`; a legacy `1`, mismatch,
or nested/manual Claude process fails open on the short path. The daemon also
requires an authorized snapshot WebSocket at intake and releases all live holds
when the last board tab disconnects. Terminal viewer sockets never satisfy this
gate.

The three 720-second timeouts on `AskUserQuestion`, `PermissionRequest`, and
`Elicitation` form one invariant with `fleet-hook`'s 660-second watchdog and
the daemon question hold window (600 seconds by default, clamped to at most
650 seconds):

```text
daemon hold < hook watchdog < Claude Code timeout
```

If that ordering changes, a board answer can land on a dead socket. Ordinary
terminal sessions use the shim's short fail-open watchdog and the daemon does
not park them by default. While a legitimate long POST is parked, the shim
renews a separate `/health` lease; three consecutive misses abort the request
in roughly 12–17 seconds. This bounds a wedged daemon without shrinking the
healthy ten-minute answer window. Change all three outer limits together.

Daemon responses cross a trust boundary too. The shim accepts only the exact
event-specific Claude output shapes Fleet Deck intentionally produces and
canonicalizes them before writing stdout. A generic JSON object, universal
`continue`/`systemMessage`, mismatched event name, or malformed decision becomes
the same neutral `{}` result; a foreign or stale responder cannot inject a hook
message merely by owning the configured port.

`SessionEnd` has a separate one-second ceiling because Claude Code gives that
lifecycle event a short shutdown budget. Losing optional final telemetry is
preferable to delaying `/clear`, `/resume`, or terminal exit.
