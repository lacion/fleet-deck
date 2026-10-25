#!/usr/bin/env python3
"""FG-9 spike: drive `codex app-server` like Fleet Deck does (ADR-002).

Usage: codex_app_server.py start|resume|resume-no-cwd <cwd> <prompt> [<thread id>]

It sends `initialize` (experimentalApi), `initialized`, then `thread/start`
(start) or `thread/resume` with `threadId` (resume), both with `cwd`,
`approvalPolicy = "untrusted"` and `sandbox = "read-only"` (the safest Fleet
Deck mode), then one `turn/start`. It prints one JSON line with the thread
id, the thread cwd that the server reports, and the agent message text.
`resume-no-cwd` sends `thread/resume` without `cwd` (the process still runs
in <cwd>), prints the thread cwd that the server chose, and starts no turn.
Server requests (approvals) get a JSON-RPC error: the spike runs no tools.
Run it with `python3 -I`. It prints no environment value.
"""

import json
import subprocess
import sys
import time

TIMEOUT_S = 240


def main() -> int:
    if len(sys.argv) < 4:
        print(__doc__, file=sys.stderr)
        return 2
    action, cwd, prompt = sys.argv[1], sys.argv[2], sys.argv[3]
    thread = sys.argv[4] if len(sys.argv) > 4 else None
    proc = subprocess.Popen(
        ["codex", "app-server"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        cwd=cwd,
    )
    deadline = time.monotonic() + TIMEOUT_S

    def send(obj):
        proc.stdin.write(json.dumps(obj) + "\n")
        proc.stdin.flush()

    def read_until(pred):
        while time.monotonic() < deadline:
            line = proc.stdout.readline()
            if not line:
                raise SystemExit("app-server closed stdout")
            try:
                msg = json.loads(line)
            except json.JSONDecodeError:
                continue
            if "method" in msg and "id" in msg:
                send({"id": msg["id"], "error": {"code": -32000, "message": "spike: declined"}})
                continue
            out = pred(msg)
            if out is not None:
                return out
        raise SystemExit("timeout")

    def reply_of(req_id):
        def pred(msg):
            if msg.get("id") == req_id and "method" not in msg:
                if "error" in msg:
                    return {"error": msg["error"]}
                return {"result": msg.get("result")}
            return None
        return pred

    send({"id": "fd-1", "method": "initialize", "params": {
        "clientInfo": {"name": "fleetdeck-spike", "version": "0"},
        "capabilities": {"experimentalApi": True}}})
    init = read_until(reply_of("fd-1"))
    if "error" in init:
        print(json.dumps({"step": "initialize", "error": init["error"]}))
        return 1
    send({"method": "initialized"})

    params = {"approvalPolicy": "untrusted", "sandbox": "read-only", "cwd": cwd}
    if action == "resume-no-cwd":
        del params["cwd"]
    if action in ("resume", "resume-no-cwd"):
        params["threadId"] = thread
        method = "thread/resume"
    else:
        params["ephemeral"] = False
        method = "thread/start"
    send({"id": "fd-2", "method": method, "params": params})
    opened = read_until(reply_of("fd-2"))
    if "error" in opened:
        print(json.dumps({"step": method, "error": opened["error"]}))
        proc.kill()
        return 1
    th = (opened["result"] or {}).get("thread") or {}
    thread_id = th.get("id")
    reported_cwd = (opened["result"] or {}).get("cwd") or th.get("cwd")
    history_turns = len(th.get("turns") or [])
    if action == "resume-no-cwd":
        proc.stdin.close()
        try:
            proc.wait(timeout=20)
        except subprocess.TimeoutExpired:
            proc.kill()
        print(json.dumps({"method": method, "cwd_param": None, "thread_id": thread_id,
                          "thread_cwd": reported_cwd, "resumed_turns": history_turns}))
        return 0

    send({"id": "fd-3", "method": "turn/start", "params": {
        "threadId": thread_id, "clientUserMessageId": "spike-1",
        "input": [{"type": "text", "text": prompt}]}})
    started = read_until(reply_of("fd-3"))
    if "error" in started:
        print(json.dumps({"step": "turn/start", "error": started["error"]}))
        proc.kill()
        return 1

    texts = []

    def turn_done(msg):
        m = msg.get("method")
        p = msg.get("params") or {}
        if m == "item/completed":
            item = p.get("item") or {}
            if item.get("type") == "agentMessage":
                texts.append(item.get("text", ""))
        if m == "turn/completed":
            return (p.get("turn") or {}).get("status", "completed")
        return None

    status = read_until(turn_done)
    proc.stdin.close()
    try:
        proc.wait(timeout=20)
    except subprocess.TimeoutExpired:
        proc.kill()
    print(json.dumps({
        "method": method, "thread_id": thread_id, "thread_cwd": reported_cwd,
        "resumed_turns": history_turns, "turn_status": status,
        "agent_message": " ".join(texts)}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
