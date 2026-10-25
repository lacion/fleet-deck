#!/bin/sh
# Offline self-test of the SDK recorder (no network, no real `claude`).
# 1. An unknown scenario must exit 1.
# 2. A scenario whose CLI never answers must time out, abort the session,
#    terminate the CLI process, write `<scenario>.failed.jsonl`, and exit 1.
set -u
cd "$(dirname "$0")"
tmp=$(mktemp -d)
printf '#!/bin/sh\necho $$ > "%s/fake.pid"\nexec sleep 600\n' "$tmp" > "$tmp/fake-claude"
chmod +x "$tmp/fake-claude"
fail=0

bun record.ts no_such_scenario >/dev/null
[ $? -eq 1 ] || { echo "FAIL: unknown scenario did not exit 1"; fail=1; }

FD_REAL_CLAUDE="$tmp/fake-claude" FD_OUT_DIR="$tmp" FD_TURN_LIMIT_MS=3000 timeout 60 bun record.ts simple >/dev/null
code=$?
[ $code -eq 1 ] || { echo "FAIL: timed-out scenario exit code $code, expected 1"; fail=1; }
[ -f "$tmp/simple.failed.jsonl" ] || { echo "FAIL: no simple.failed.jsonl"; fail=1; }
grep -q '"host aborted the session"' "$tmp/simple.failed.jsonl" || { echo "FAIL: session was not aborted"; fail=1; }
grep -q '"dir":"exit"' "$tmp/simple.failed.jsonl" || { echo "FAIL: no exit record for the CLI process"; fail=1; }
if [ -f "$tmp/fake.pid" ] && kill -0 "$(cat "$tmp/fake.pid")" 2>/dev/null; then
  echo "FAIL: the fake CLI process still runs"
  kill "$(cat "$tmp/fake.pid")"
  fail=1
fi

[ $fail -eq 0 ] && echo "ok sdk-recorder selftest"
exit $fail
