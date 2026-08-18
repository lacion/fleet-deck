#!/bin/sh
# Final failure-silence boundary for Claude Code hooks. Bun and the bundled
# scripts are optional integration dependencies: their crashes, diagnostics,
# or partial writes must never leak into an ordinary Claude session.

umask 077

mode=${1-}
shift 2>/dev/null || true
bundle=${1-}
shift 2>/dev/null || true

fallback() {
  if [ "$mode" = "decision" ]; then
    printf '{}'
  fi
  exit 0
}

case "$mode" in
  decision | sessionstart | watch) ;;
  *) exit 0 ;;
esac

# Claude Code 2.1.206 does not export CLAUDE_PID. It does directly parent this
# launcher, though, so derive one stable run identity here and overwrite any
# stale/untrusted inherited value. SessionStart authenticates this PID by
# resolving its exact executable, version, and process generation before it
# writes the compatibility lease used by later launchers.
launcher_parent=${PPID-}
case "$launcher_parent" in
  '' | *[!0-9]*) fallback ;;
esac
if [ "$launcher_parent" -le 1 ] 2>/dev/null; then
  fallback
fi
CLAUDE_PID=$launcher_parent
export CLAUDE_PID

if ! command -v bun >/dev/null 2>&1 || [ ! -f "$bundle" ]; then
  fallback
fi

capture_dir=$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/fleetdeck-hook.XXXXXX" 2>/dev/null) || fallback
trap '/bin/rm -rf "$capture_dir"' EXIT HUP INT TERM
stdout_file=$capture_dir/stdout
stderr_file=$capture_dir/stderr

bun "$bundle" "$@" >"$stdout_file" 2>"$stderr_file"
status=$?

case "$mode" in
  decision)
    if [ "$status" -eq 0 ]; then
      /bin/cat "$stdout_file" 2>/dev/null || true
    else
      printf '{}'
    fi
    exit 0
    ;;
  sessionstart)
    if [ "$status" -eq 0 ]; then
      /bin/cat "$stdout_file" 2>/dev/null || true
    fi
    exit 0
    ;;
  watch)
    if [ "$status" -eq 2 ]; then
      /bin/cat "$stderr_file" >&2 2>/dev/null || true
      exit 2
    fi
    exit 0
    ;;
esac
