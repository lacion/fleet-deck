"""S-EVENTS helper: drive `codex app-server` (JSON-RPC over stdio) against the events server.

Run it with `python3 -I codex_events.py <mode> <out-dir> <work-dir> <server-url> [--flag NAME ...]`.
It writes every frame to `<out-dir>/frames.jsonl`, a summary line per frame with
the seconds since the start to `<out-dir>/timeline.txt`, and prints the findings.
It never writes outside <out-dir> and <work-dir> (and the state that the CLI keeps itself).

Modes:
  events  Turn 1 "Call tick.", wait 3 s, turn 2 "Is there a tool named late_tool?
          If yes, call it.", then one `mcpServerStatus/list` (it opens a NEW MCP
          connection per call: it is a probe of a fresh client, not of the session).
  slow    One turn "Call slow_count with seconds 90.", wait for `turn/completed`.
  cancel  One turn "Call slow_count with seconds 60."; when the MCP tool call starts,
          wait 5 s, send `turn/interrupt`, wait for `turn/completed`, then keep
          watching 3 s.
`--flag NAME` starts the app-server with `--enable NAME` (repeatable).
Environment: `EVENTS_CODEX_ARGS` (extra app-server arguments, for example
`-c mcp_servers.events.tool_timeout_sec=10`), `EVENTS_SLOW_SECONDS` (the count of `slow`
mode, default 90), `EVENTS_WATCH_AFTER` (seconds that `cancel` mode keeps watching after
the interrupt, default 3).
"""

import json
import os
import queue
import shlex
import subprocess
import sys
import threading
import time

BOUND = 300
T0 = time.time()


def now():
    return time.time() - T0


class Client:
    def __init__(self, args, out_dir, env):
        self.out = open(os.path.join(out_dir, "frames.jsonl"), "w")
        self.timeline = open(os.path.join(out_dir, "timeline.txt"), "w")
        self.proc = subprocess.Popen(
            args,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=open(os.path.join(out_dir, "stderr.txt"), "w"),
            text=True,
            env=env,
        )
        self.frames = queue.Queue()
        self.next_id = 1
        self.methods = {}
        threading.Thread(target=self._read, daemon=True).start()

    def mark(self, text):
        self.timeline.write("t=%7.2f  [driver] %s\n" % (now(), text))
        self.timeline.flush()

    def _read(self):
        for line in self.proc.stdout:
            stamp = now()
            self.out.write(line)
            self.out.flush()
            try:
                frame = json.loads(line)
            except ValueError:
                continue
            method = frame.get("method")
            if method:
                self.methods[method] = self.methods.get(method, 0) + 1
                self.timeline.write("t=%7.2f  %s %s\n" % (stamp, method, json.dumps(frame.get("params"))[:260]))
            else:
                self.timeline.write("t=%7.2f  reply id=%s %s\n" % (stamp, frame.get("id"), json.dumps(frame)[:200]))
            self.timeline.flush()
            self.frames.put(frame)
        self.frames.put(None)

    def send(self, obj):
        self.out.write("> " + json.dumps(obj) + "\n")
        self.out.flush()
        self.mark("send " + json.dumps(obj)[:200])
        self.proc.stdin.write(json.dumps(obj) + "\n")
        self.proc.stdin.flush()

    def request(self, method, params, limit=60):
        rid = self.next_id
        self.next_id += 1
        self.send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        deadline = time.time() + limit
        while time.time() < deadline:
            frame = self.pump(deadline - time.time())
            if frame is None:
                return {"error": "closed or timeout"}
            if frame.get("id") == rid and "method" not in frame:
                return frame
        return {"error": "timeout"}

    def pump(self, wait):
        """Next notification or answer; refuses server requests."""
        try:
            frame = self.frames.get(timeout=max(wait, 0.1))
        except queue.Empty:
            return None
        if frame is None:
            return None
        if "method" in frame and "id" in frame:
            self.send(
                {
                    "jsonrpc": "2.0",
                    "id": frame["id"],
                    "error": {"code": -32601, "message": "spike client: not handled"},
                }
            )
            print("server-request-refused:", frame["method"])
            return {"serverRequest": frame["method"]}
        return frame

    def close(self):
        try:
            self.proc.stdin.close()
        except OSError:
            pass
        try:
            self.proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()


def handshake(client):
    answer = client.request(
        "initialize",
        {
            "clientInfo": {"name": "s-events", "version": "0"},
            "capabilities": {"experimentalApi": True},
        },
    )
    client.send({"jsonrpc": "2.0", "method": "initialized"})
    return answer


def start_thread(client, work):
    answer = client.request(
        "thread/start",
        {"cwd": work, "approvalPolicy": "never", "sandbox": "read-only", "ephemeral": True},
    )
    return (answer.get("result") or {}).get("thread", {}).get("id"), answer


def item_line(item):
    kind = item.get("type")
    if kind == "mcpToolCall":
        return "mcpToolCall server=%s tool=%s status=%s args=%s result=%s error=%s" % (
            item.get("server"),
            item.get("tool"),
            item.get("status"),
            json.dumps(item.get("arguments")),
            json.dumps(item.get("result"))[:200],
            json.dumps(item.get("error"))[:300],
        )
    if kind == "agentMessage":
        return "agentMessage " + item.get("text", "")[:300].replace("\n", " ")
    return kind


def run_turn(client, thread_id, prompt, interrupt_after=None, limit=BOUND):
    """Runs one turn. Returns (turn-id, [item lines], status)."""
    answer = client.request(
        "turn/start",
        {"threadId": thread_id, "input": [{"type": "text", "text": prompt}]},
    )
    turn_id = ((answer.get("result") or {}).get("turn") or {}).get("id")
    items = []
    status = "timeout"
    deadline = time.time() + limit
    interrupt_at = None
    interrupted = False
    while time.time() < deadline:
        if interrupt_at is not None and not interrupted and time.time() >= interrupt_at:
            client.mark("interrupt now")
            reply = client.request("turn/interrupt", {"threadId": thread_id, "turnId": turn_id}, limit=15)
            print("turn/interrupt reply:", json.dumps(reply)[:200])
            interrupted = True
        frame = client.pump(0.5)
        if frame is None:
            if client.proc.poll() is not None:
                status = "closed"
                break
            continue
        method = frame.get("method")
        params = frame.get("params") or {}
        if method == "turn/started" and not turn_id:
            turn_id = (params.get("turn") or {}).get("id")
        if method == "item/started":
            item = params.get("item") or {}
            if item.get("type") == "mcpToolCall":
                items.append("started: " + item_line(item))
                if interrupt_after is not None and interrupt_at is None:
                    interrupt_at = time.time() + interrupt_after
                    client.mark("mcpToolCall started: interrupt in %s s" % interrupt_after)
        if method == "item/completed":
            items.append("completed: " + item_line(params.get("item") or {}))
        if method == "turn/completed":
            status = (params.get("turn") or {}).get("status", "completed")
            break
    return turn_id, items, status


def show_status(client, thread_id, label):
    answer = client.request("mcpServerStatus/list", {"threadId": thread_id})
    for server in (answer.get("result") or {}).get("data") or []:
        if server.get("name") == "events":
            print(
                label,
                "mcpServerStatus events: tools=",
                sorted((server.get("tools") or {}).keys()),
                "resources=",
                [r.get("uri") for r in server.get("resources") or []],
                "authStatus=",
                server.get("authStatus"),
            )
            return
    print(label, "mcpServerStatus: no `events` server:", json.dumps(answer)[:300])


def main():
    mode, out_dir, work, url = sys.argv[1:5]
    flags = [sys.argv[i + 1] for i, a in enumerate(sys.argv) if a == "--flag"]
    os.makedirs(out_dir, exist_ok=True)
    os.makedirs(work, exist_ok=True)
    env = dict(os.environ)
    env["NO_COLOR"] = "1"
    args = ["codex", "app-server", "-c", 'model_reasoning_effort="low"']
    for flag in flags:
        args += ["--enable", flag]
    args += shlex.split(os.environ.get("EVENTS_CODEX_ARGS", ""))
    seconds = int(os.environ.get("EVENTS_SLOW_SECONDS", "90"))
    after = int(os.environ.get("EVENTS_WATCH_AFTER", "3"))
    args += [
        "-c", 'mcp_servers.events.url="%s"' % url,
        "-c", 'mcp_servers.events.default_tools_approval_mode="approve"',
    ]
    client = Client(args, out_dir, env)
    print("initialize:", json.dumps(handshake(client))[:200])
    thread_id, started = start_thread(client, work)
    print("thread:", thread_id)
    if not thread_id:
        print("thread/start answer:", json.dumps(started)[:400])
        client.close()
        return
    if mode == "events":
        _, items, status = run_turn(client, thread_id, "Call tick.", limit=120)
        print("turn 1:", status, items)
        client.mark("sleep 3")
        time.sleep(3)
        _, items, status = run_turn(
            client, thread_id, "Is there a tool named late_tool? If yes, call it.", limit=120
        )
        print("turn 2:", status, items)
        show_status(client, thread_id, "after turn 2:")
    elif mode == "slow":
        started_at = time.time()
        _, items, status = run_turn(client, thread_id, "Call slow_count with seconds %d." % seconds, limit=BOUND)
        print("turn:", status, "after %.1f s" % (time.time() - started_at))
        for item in items:
            print("  ", item)
    elif mode == "cancel":
        started_at = time.time()
        _, items, status = run_turn(
            client, thread_id, "Call slow_count with seconds 60.", interrupt_after=5, limit=150
        )
        print("turn:", status, "after %.1f s" % (time.time() - started_at))
        for item in items:
            print("  ", item)
        client.mark("watching %d s after the interrupt" % after)
        time.sleep(after)
    print("notification methods seen:", json.dumps(client.methods, sort_keys=True))
    client.close()


if __name__ == "__main__":
    main()
