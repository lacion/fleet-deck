#!/bin/sh
# Live smoke of MCP Apps (B15 T9, docs/v2/design/mcp-apps.md, T9). Manual: run
# it on the owner's machine with the real `codex` and `claude` CLIs signed in.
# It spends a small number of tokens. No test and no CI job runs it.
# The station and the deck run with a temporary HOME; the provider CLIs run
# with the owner's real HOME (their sign-in), so their transcripts land in the
# owner's provider homes.
#
# For each provider it starts a temporary station and a temporary deck, pairs
# them, and starts one session with the demo MCP server
# (spikes/mcp-apps/demo_app_server.py). The provider tool timeout is 1 s
# (Claude: "timeout": 1000 in the --mcp-config file; Codex:
# -c mcp_servers.demo.tool_timeout_sec=1). The steps:
#
#   1. The prompt asks for show_counter: a tool.app event must arrive.
#   2. A bump call with confirmed true through the board route: the counter
#      becomes 1.
#   3. A revert of the first turn is refused: app_call_ran_after_turn.
#   4. A slow_bump call with confirmed true must not answer done. The list of
#      open calls has it uncertain with the cause provider_error.
#   5. After slow_bump.txt appears, a revert is refused: app_call_running.
#   6. After the release, a revert is refused: app_call_ran_after_turn.
#
# Steps 3, 5 and 6 need the provider's conversation rollback: a provider
# without it (Claude, capabilities.rollback = false) skips them. A failed
# revert check does not stop the run; the run fails at its end.
#
# Environment:
#   FD_BIN                 the fleetdeck binary (default: target/debug/fleetdeck;
#                          build it first: umask 022 && sh tools/heavy.sh
#                          cargo build -p fleetdeck-cli)
#   FD_SMOKE_PROVIDERS     "codex claude" (default), or one of them
#   FD_SMOKE_CODEX_MODE    native mode of the Codex session (default never/read-only)
#   FD_SMOKE_CLAUDE_MODE   native mode of the Claude session (default bypassPermissions)
#   FD_SMOKE_OUT           where the frames go (default
#                          ~/fleetdeck-briefs/backlog/mcp-apps/smoke)
#   FD_SMOKE_STEP          seconds to wait for one step (default 180)
#   FD_SMOKE_KEEP          1: keep the temporary folder (the station and deck
#                          data) after the run and print its path
#
# A failed check prints "FAIL: <what>" and the script exits 1. The records of
# a run stay in $FD_SMOKE_OUT/<time>/<provider>/.

set -eu
umask 077

ROOT=$(cd "$(dirname "$0")/.." && pwd)
FD=${FD_BIN:-${CARGO_TARGET_DIR:-$ROOT/target}/debug/fleetdeck}
SERVER=$ROOT/spikes/mcp-apps/demo_app_server.py
PROVIDERS=${FD_SMOKE_PROVIDERS:-codex claude}
CODEX_MODE=${FD_SMOKE_CODEX_MODE:-never/read-only}
CLAUDE_MODE=${FD_SMOKE_CLAUDE_MODE:-bypassPermissions}
STEP=${FD_SMOKE_STEP:-180}
OUT=${FD_SMOKE_OUT:-$HOME/fleetdeck-briefs/backlog/mcp-apps/smoke}/$(date +%Y%m%d-%H%M%S)
KEEP=${FD_SMOKE_KEEP:-0}
TMP=$(mktemp -d)
PIDS=""
# The node and the session of the provider that runs now (for the records
# and the stop at the end).
NODE=""
REC=""
session=""

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

# save_records: the station database and its events go into the record
# folder (also after a failure).
save_records() {
    [ -n "$NODE" ] && [ -n "$REC" ] || return 0
    db=$NODE/station/station/station.db
    [ -f "$db" ] || return 0
    for f in "$db" "$db-wal" "$db-shm"; do
        [ -f "$f" ] && cp "$f" "$REC/" 2>/dev/null || true
    done
    python3 -I - "$db" "$REC/all-events.jsonl" <<'PY' || true
import json, sqlite3, sys
con = sqlite3.connect("file:%s?mode=ro" % sys.argv[1], uri=True, timeout=10)
with open(sys.argv[2], "w") as f:
    for row in con.execute(
        "SELECT session_id, kind, payload FROM events ORDER BY station_seq"
    ):
        f.write(json.dumps({"session": row[0], "kind": row[1], "event": json.loads(row[2])}) + "\n")
PY
    # The raw provider frames (the provider's answer kind, fact 21).
    cp -r "$NODE/station/station/raw" "$REC/raw" 2>/dev/null || true
}

# The PIDs of the processes that belong to this run: their command line
# names the temporary folder, or their working folder is in it (the provider
# and the demo server run in the workspace).
run_pids() {
    for d in /proc/[0-9]*; do
        pid=${d#/proc/}
        [ "$pid" != "$$" ] || continue
        cwd=$(readlink "$d/cwd" 2>/dev/null) || continue
        case $cwd in
        "$TMP" | "$TMP"/*) echo "$pid"; continue ;;
        esac
        if tr '\0' ' ' <"$d/cmdline" 2>/dev/null | grep -qF "$TMP"; then
            echo "$pid"
        fi
    done
    return 0
}

# stop_all: stops the session, then the station and the deck, then every
# process of the run that is still there (TERM, a wait, then KILL). The
# provider runs under the station's shim, which outlives the station by
# design, so killing the station alone does not stop it.
stop_all() {
    if [ -n "$session" ] && [ -n "$NODE" ] && [ -d "$NODE/deck" ]; then
        HOME="$NODE/home" timeout 20 "$FD" --data-dir "$NODE/deck" deck command smoke \
            "{\"op\":\"stop\",\"session_id\":\"$session\",\"how\":\"kill\"}" \
            >/dev/null 2>&1 || true
    fi
    for pid in $PIDS; do
        kill -TERM "$pid" 2>/dev/null || true
    done
    left=$(run_pids)
    [ -z "$left" ] || kill -TERM $left 2>/dev/null || true
    i=0
    while [ $i -lt 10 ]; do
        alive=""
        for pid in $PIDS $(run_pids); do
            kill -0 "$pid" 2>/dev/null && alive="$alive $pid"
        done
        [ -n "$alive" ] || break
        sleep 1
        i=$((i + 1))
    done
    for pid in $PIDS $(run_pids); do
        kill -KILL "$pid" 2>/dev/null || true
    done
    PIDS=""
}

cleanup() {
    status=$?
    set +e
    trap - EXIT INT TERM
    [ "$status" -eq 0 ] || save_records
    stop_all
    if [ "$KEEP" = 1 ]; then
        echo "kept: $TMP"
    else
        rm -rf "$TMP"
    fi
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

[ -x "$FD" ] || fail "no fleetdeck binary at $FD (set FD_BIN)"
[ -f "$SERVER" ] || fail "no demo server at $SERVER"
command -v python3 >/dev/null || fail "python3 is needed"
command -v curl >/dev/null || fail "curl is needed"

# The JSON and SQLite helper. Every subcommand prints plain text.
cat >"$TMP/h.py" <<'PY'
import json, os, shlex, socket, sqlite3, sys


def rows(db, session):
    con = sqlite3.connect("file:%s?mode=ro" % db, uri=True, timeout=10)
    try:
        for kind, payload in con.execute(
            "SELECT kind, payload FROM events WHERE session_id = ? ORDER BY station_seq",
            (session,),
        ):
            yield kind, json.loads(payload)
    finally:
        con.close()


def dig(value, path):
    for key in path.split("."):
        if key == "":
            continue
        if isinstance(value, list):
            value = value[int(key)]
        else:
            value = value[key]
    return value


cmd, args = sys.argv[1], sys.argv[2:]
if cmd == "free-port":
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    print(s.getsockname()[1])
elif cmd == "wrappers":
    node, ws, server, log, timeout = args
    argv = ["-c", 'cd "$1" && exec python3 -I "$2"', "sh", ws, server]
    env = {"DEMO_LOG": log}
    toml = lambda v: json.dumps(v)
    # The station passes "app-server -c ...": `codex app-server` ignores the
    # -c flags before the subcommand, so the demo flags come after "$@".
    codex = [
        "exec codex",
        '"$@"',
        "-c %s" % shlex.quote("mcp_servers.demo.command=" + toml("sh")),
        "-c %s" % shlex.quote("mcp_servers.demo.args=" + toml(argv)),
        "-c %s" % shlex.quote("mcp_servers.demo.env={DEMO_LOG=%s}" % toml(log)),
        "-c mcp_servers.demo.tool_timeout_sec=%s" % timeout,
    ]
    claude_cfg = {"mcpServers": {"demo": {
        "type": "stdio", "command": "sh", "args": argv, "env": env,
        "timeout": int(timeout) * 1000,
    }}}
    cfg = os.path.join(node, "claude-mcp.json")
    with open(cfg, "w") as f:
        json.dump(claude_cfg, f)
    # The station runs with a temporary HOME; the providers need the owner's
    # real one, where their sign-in lives.
    real_home = "HOME=%s " % shlex.quote(os.environ["HOME"])
    for name, line in (
        ("codex-demo", real_home + " ".join(codex)),
        ("claude-demo", real_home + "exec claude --mcp-config %s \"$@\"" % shlex.quote(cfg)),
    ):
        path = os.path.join(node, name)
        with open(path, "w") as f:
            f.write("#!/bin/sh\n" + line + "\n")
        os.chmod(path, 0o700)
elif cmd == "app-id":
    for kind, p in rows(*args):
        if kind == "tool.app":
            print(p["app_id"])
            break
elif cmd == "turn-id":
    for kind, p in rows(*args):
        if kind == "turn.started":
            print(p["turn_id"])
            break
elif cmd == "rollback":
    # "true" when the session's provider can roll back its conversation.
    for kind, p in rows(*args):
        if kind == "session.started":
            print("true" if p.get("capabilities", {}).get("rollback") else "false")
            break
elif cmd == "turn-done":
    sys.exit(0 if any(k == "turn.completed" for k, _ in rows(*args)) else 1)
elif cmd == "reverts":
    # One line per finished refusal: "<state> <reason>".
    for kind, p in rows(*args):
        if kind == "revert.updated" and p.get("state") in ("failed", "reconciliation_required"):
            print(p["state"], p.get("reason"))
elif cmd == "dump":
    db, session, out = args
    with open(out, "w") as f:
        for kind, p in rows(db, session):
            f.write(json.dumps({"kind": kind, "event": p}) + "\n")
elif cmd == "get":
    # get FILE PATH: a scalar as text, anything else as compact JSON.
    with open(args[0]) as f:
        try:
            v = dig(json.load(f), args[1])
        except (KeyError, IndexError, ValueError):
            sys.exit(1)
    print(v if isinstance(v, str) else json.dumps(v, separators=(",", ":")))
elif cmd == "call-of":
    # call-of FILE TOOL: "<call_id> <state> <cause>" of the open call.
    with open(args[0]) as f:
        for c in json.load(f).get("calls", []):
            if c.get("tool") == args[1]:
                print(c["call_id"], c.get("state"), c.get("cause", "-"))
                break
PY
py() { python3 -I "$TMP/h.py" "$@"; }

# wait_ok COMMAND...: runs COMMAND until it succeeds (0) or $STEP ends (1).
wait_ok() {
    end=$(($(date +%s) + STEP))
    until "$@" >/dev/null 2>&1; do
        [ "$(date +%s)" -lt "$end" ] || return 1
        sleep 1
    done
}

# wait_for LABEL COMMAND...: wait_ok, and a timeout fails the run.
wait_for() {
    label=$1
    shift
    wait_ok "$@" || fail "timeout: $label"
}

smoke_one() {
    driver=$1
    mode=$2
    echo "== $driver"
    command -v "$driver" >/dev/null || fail "$driver is not on PATH"
    NODE=$TMP/$driver
    REC=$OUT/$driver
    mkdir -p "$NODE/home/projects/demo" "$NODE/station" "$NODE/deck" "$REC"
    WS=$NODE/home/projects/demo
    (
        cd "$WS"
        git init -q -b main
        echo one >a.txt
        git add .
        git -c user.name=t -c user.email=t@t commit -q -m init
    )
    WS=$(cd "$WS" && pwd -P)
    py wrappers "$NODE" "$WS" "$SERVER" "$REC/demo-server.log" 1
    # A station offers the mode without permission checks only with
    # allow_bypass_permissions; else the provider falls back to its safest
    # mode and the show_counter call waits for an approval.
    bypass=false
    case $mode in
    bypassPermissions | *danger-full-access*) bypass=true ;;
    esac
    cat >"$NODE/station/station.toml" <<EOF
host = "direct"
name = "smoke"
allowed_roots = ["~/projects"]
max_sessions = 4
listen = []
allow_bypass_permissions = $bypass

[[provider_instance]]
id = "smoke-$driver"
driver = "$driver"
binary = "$NODE/$driver-demo"
EOF
    port=$(py free-port)
    cat >"$NODE/deck/deck.toml" <<EOF
name = "deck"
listen = ["127.0.0.1:0"]
link_listen = ["127.0.0.1:$port"]
tailnet_auto = false
EOF
    fds() { HOME="$NODE/home" RUST_LOG=warn "$FD" --data-dir "$NODE/station" "$@"; }
    fdd() { HOME="$NODE/home" RUST_LOG=warn "$FD" --data-dir "$NODE/deck" "$@"; }
    # Started without the functions, so that $! is the process itself.
    HOME="$NODE/home" RUST_LOG=warn "$FD" --data-dir "$NODE/station" station serve \
        >"$REC/station.log" 2>&1 &
    PIDS="$PIDS $!"
    wait_for "the station starts" fds station sessions
    HOME="$NODE/home" RUST_LOG=warn "$FD" --data-dir "$NODE/deck" deck serve \
        >"$REC/deck.log" 2>&1 &
    PIDS="$PIDS $!"
    wait_for "the deck starts" fdd deck stations
    fds station invite --listen 127.0.0.1:0 >"$NODE/invite.txt"
    fdd deck add-station --stdin <"$NODE/invite.txt" >/dev/null

    # The owner's board login (the board address is on loopback; its
    # certificate is self-signed).
    url=$(fdd open 2>/dev/null)
    board=${url%%/#login=*}
    token=${url##*/#login=}
    JAR=$NODE/jar
    curl -sk -c "$JAR" -H 'Content-Type: application/json' \
        -d "{\"token\":\"$token\"}" "$board/api/auth/link" >"$NODE/login.json"
    CSRF=$(py get "$NODE/login.json" csrf) || fail "no board login"

    # api METHOD PATH [BODY] -> prints the HTTP status; the body is in $RES.
    RES=$NODE/res.json
    api() {
        if [ "$#" -ge 3 ]; then
            curl -sk --max-time 120 -o "$RES" -w '%{http_code}' -X "$1" \
                -b "$JAR" -H "X-CSRF-Token: $CSRF" -H 'Content-Type: application/json' \
                -d "$3" "$board$2"
        else
            curl -sk --max-time 120 -o "$RES" -w '%{http_code}' -X "$1" \
                -b "$JAR" -H "X-CSRF-Token: $CSRF" "$board$2"
        fi
    }

    modeargs=""
    [ -z "$mode" ] || modeargs="--mode $mode"
    # shellcheck disable=SC2086
    session=$(fds station run --provider "smoke-$driver" --cwd "$WS" $modeargs \
        "Call the show_counter tool of the demo MCP server exactly once, then reply with the word done.")
    DB=$NODE/station/station/station.db
    echo "session $session"

    # 1. The tool.app event.
    wait_for "tool.app of $driver" sh -c "[ -n \"\$(python3 -I '$TMP/h.py' app-id '$DB' '$session')\" ]"
    app=$(py app-id "$DB" "$session")
    wait_for "the first turn of $driver ends" py turn-done "$DB" "$session"
    turn=$(py turn-id "$DB" "$session")
    echo "ok: tool.app $app arrived; first turn $turn"
    base="/api/sessions/smoke/$session"
    wait_for "the board knows the session and the feature" sh -c \
        "[ \"\$(curl -sk -o /dev/null -w '%{http_code}' -b '$JAR' '$board$base/app-calls')\" = 200 ]"

    # 2. A bump with confirmed true changes the counter.
    code=$(api POST "$base/apps/$app/call" \
        '{"request":{"method":"tools/call","name":"bump","arguments":{}},"confirmed":true}')
    cp "$RES" "$REC/bump-reply.json"
    [ "$code" = 200 ] || fail "$driver bump answered $code: $(cat "$RES")"
    [ "$(py get "$RES" outcome)" = done ] || fail "$driver bump did not answer done"
    case $(py get "$RES" result) in
    *'"count":1'*) echo "ok: bump changed the counter to 1" ;;
    *) fail "$driver bump did not give count 1: $(cat "$RES")" ;;
    esac

    # The revert checks need the provider's conversation rollback (Claude
    # has none: capabilities.rollback = false, and the station refuses every
    # revert with "not supported"). A failed revert check does not stop the
    # run: the later steps do not depend on it. The run fails at its end.
    rollback=$(py rollback "$DB" "$session")
    misses=0
    miss() {
        echo "FAIL (the run goes on): $*" >&2
        misses=$((misses + 1))
    }

    # revert_is REASON: a revert of the first turn is refused with REASON.
    revert_is() {
        want=$1
        if [ "$rollback" != true ]; then
            echo "skip: revert ($want): $driver has no rollback (capabilities.rollback = $rollback)"
            return 0
        fi
        before=$(py reverts "$DB" "$session" | wc -l)
        answer=$(fdd deck command smoke \
            "{\"op\":\"revert\",\"session_id\":\"$session\",\"turn_id\":\"$turn\"}")
        echo "$answer" >>"$REC/revert-answers.txt"
        # A refusal at the admission has no revert row and no revert.updated
        # event: the command answer holds it.
        case $answer in
        *rejected:*)
            miss "$driver revert was refused at the admission, wanted '$want' (a revert.updated event): $answer"
            return 0
            ;;
        esac
        if ! wait_ok sh -c \
            "[ \"\$(python3 -I '$TMP/h.py' reverts '$DB' '$session' | wc -l)\" -gt $before ]"; then
            miss "timeout: a refused revert of $driver (wanted '$want'): $answer"
            return 0
        fi
        got=$(py reverts "$DB" "$session" | tail -n 1)
        if [ "$got" = "failed $want" ]; then
            echo "ok: revert refused with $want"
        else
            miss "$driver revert gave '$got', wanted '$want'"
        fi
    }

    # 3. The revert meets the settled bump.
    revert_is app_call_ran_after_turn

    # 4. A slow_bump must not answer done; the call is uncertain.
    code=$(api POST "$base/apps/$app/call" \
        '{"request":{"method":"tools/call","name":"slow_bump","arguments":{}},"confirmed":true}')
    cp "$RES" "$REC/slow_bump-reply.json"
    echo "slow_bump answered $code: $(head -c 400 "$RES")"
    if [ "$code" = 200 ] && [ "$(py get "$RES" outcome 2>/dev/null || true)" = done ]; then
        fail "$driver slow_bump answered done"
    fi
    [ "$(api GET "$base/app-calls")" = 200 ] || fail "$driver app-calls: $(cat "$RES")"
    cp "$RES" "$REC/app-calls-uncertain.json"
    set -- $(py call-of "$RES" slow_bump)
    [ "$#" -eq 3 ] || fail "$driver has no open slow_bump call: $(cat "$RES")"
    call=$1
    [ "$2 $3" = "uncertain provider_error" ] || fail "$driver slow_bump is '$2 $3'"
    echo "ok: slow_bump is uncertain with cause provider_error (call $call)"
    # The provider's answer kind (fact 21): a Codex JSON-RPC error and an
    # isError result both land in slow_bump-reply.json and the station log.
    echo "record: $REC/slow_bump-reply.json and $REC/station.log hold the provider's answer"

    # 5. The server writes after the error answer; the barrier stays.
    wait_for "slow_bump.txt of $driver" test -f "$WS/slow_bump.txt"
    revert_is app_call_running

    # 6. After the release the barrier shows the ran-after-turn rule.
    code=$(api POST "$base/app-calls/$call/release" '{}')
    [ "$code" = 200 ] || fail "$driver release answered $code: $(cat "$RES")"
    cp "$RES" "$REC/release-reply.json"
    revert_is app_call_ran_after_turn

    py dump "$DB" "$session" "$REC/events.jsonl"
    save_records
    [ "$misses" -eq 0 ] || fail "$driver: $misses check(s) failed (records in $REC)"
    echo "PASS: $driver (records in $REC)"
}

for provider in $PROVIDERS; do
    case $provider in
    codex) smoke_one codex "$CODEX_MODE" ;;
    claude) smoke_one claude "$CLAUDE_MODE" ;;
    *) fail "unknown provider $provider" ;;
    esac
    stop_all
    session=""
done
echo "All smoke checks passed. Frames: $OUT"
