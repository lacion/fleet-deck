"""S-EVENTS helper: drive one multi-turn `claude -p` stream-json session.

Run it with `python3 -I claude_session.py <out-dir> <work-dir> <mcp-config-json> <plan-json> [KEY=VALUE ...]`.

The plan is a JSON list of steps:
  ["turn", "<text>"]                  send a user message, wait for its `result`
  ["sleep", <seconds>]                wait
  ["touch", "<path>"]                 create an empty file (the server's `bump` trigger)
  ["turn_interrupt", "<text>", <s>]   send a user message; when the model calls
                                      `slow_count`, wait <s> seconds, then send
                                      the interrupt control request that Fleet
                                      Deck sends (`control_request` `interrupt`),
                                      then wait for the `result`
The extra KEY=VALUE arguments go into the environment of the child.

Writes `<out-dir>/out.jsonl` (every stdout line), `<out-dir>/timeline.txt` (one
summary line per frame with the seconds since the start) and
`<out-dir>/stderr.txt`. The whole run has a 300 s bound. It never writes
outside <out-dir> and <work-dir> (and the state that the CLI keeps itself).
"""

import json
import os
import queue
import subprocess
import sys
import threading
import time

BOUND = 300
MODEL = os.environ.get("CLAUDE_MODEL", "claude-haiku-5-5")


def summarize(frame):
    kind = frame.get("type")
    sub = frame.get("subtype")
    if kind == "assistant":
        parts = []
        for block in frame.get("message", {}).get("content", []):
            if block.get("type") == "tool_use":
                parts.append("tool_use " + block.get("name", "") + " " + json.dumps(block.get("input")))
            elif block.get("type") == "text":
                parts.append("text " + block.get("text", "")[:200].replace("\n", " "))
            elif block.get("type") == "thinking":
                parts.append("thinking")
        return "assistant: " + " | ".join(parts)
    if kind == "user":
        parts = []
        content = frame.get("message", {}).get("content")
        if isinstance(content, str):
            return "user: " + content[:200]
        for block in content or []:
            if block.get("type") == "tool_result":
                body = block.get("content")
                if isinstance(body, list):
                    body = " ".join(b.get("text", "") for b in body if isinstance(b, dict))
                parts.append("tool_result " + str(body)[:200].replace("\n", " "))
            elif block.get("type") == "text":
                parts.append("text " + block.get("text", "")[:200])
        return "user: " + " | ".join(parts)
    if kind == "result":
        return "result: " + json.dumps(
            {
                "is_error": frame.get("is_error"),
                "result": (frame.get("result") or "")[:300],
                "turns": frame.get("num_turns"),
                "duration_ms": frame.get("duration_ms"),
            }
        )
    if kind == "stream_event":
        event = frame.get("event", {})
        etype = event.get("type")
        return "stream_event/" + str(etype)
    if kind == "system":
        if sub == "init":
            return "system/init mcp_servers=" + json.dumps(frame.get("mcp_servers")) + " tools=" + str(
                [t for t in frame.get("tools", []) if t.startswith("mcp__")]
            )
        return "system/" + str(sub) + " " + json.dumps(frame)[:300]
    return str(kind) + "/" + str(sub) + " " + json.dumps(frame)[:300]


class Session:
    def __init__(self, out_dir, work, mcp_config, env_extra):
        self.t0 = time.time()
        self.out = open(os.path.join(out_dir, "out.jsonl"), "w")
        self.timeline = open(os.path.join(out_dir, "timeline.txt"), "w")
        env = dict(os.environ)
        env.update(env_extra)
        args = [
            "claude",
            "-p",
            "--input-format", "stream-json",
            "--output-format", "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--replay-user-messages",
            "--permission-mode", "dontAsk",
            "--model", MODEL,
            "--no-session-persistence",
            "--strict-mcp-config",
            "--mcp-config", mcp_config,
            "--allowedTools", "mcp__events", "ListMcpResourcesTool", "ReadMcpResourceTool",
        ]
        self.proc = subprocess.Popen(
            args,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=open(os.path.join(out_dir, "stderr.txt"), "w"),
            text=True,
            cwd=work,
            env=env,
        )
        self.frames = queue.Queue()
        threading.Thread(target=self._read, daemon=True).start()
        self.tool_uses = []

    def now(self):
        return time.time() - self.t0

    def mark(self, text):
        self.timeline.write("t=%7.2f  [driver] %s\n" % (self.now(), text))
        self.timeline.flush()

    def _read(self):
        for line in self.proc.stdout:
            stamp = self.now()
            self.out.write(line)
            self.out.flush()
            try:
                frame = json.loads(line)
            except ValueError:
                self.timeline.write("t=%7.2f  unparsed: %s\n" % (stamp, line[:200]))
                continue
            text = summarize(frame)
            # The text deltas of partial messages are noise.
            if not text.startswith("stream_event/content_block_delta"):
                self.timeline.write("t=%7.2f  %s\n" % (stamp, text))
                self.timeline.flush()
            self.frames.put((stamp, frame))
        self.frames.put((self.now(), None))

    def send(self, obj):
        self.mark("send " + json.dumps(obj)[:200])
        self.proc.stdin.write(json.dumps(obj) + "\n")
        self.proc.stdin.flush()

    def wait_for(self, predicate, limit):
        """Next frame that satisfies predicate, or None on the limit / the end."""
        deadline = min(time.time() + limit, self.t0 + BOUND)
        while time.time() < deadline:
            try:
                stamp, frame = self.frames.get(timeout=max(deadline - time.time(), 0.1))
            except queue.Empty:
                return "timeout"
            if frame is None:
                return "closed"
            if predicate(frame):
                return frame
        return "timeout"

    def close(self):
        try:
            self.proc.stdin.close()
        except OSError:
            pass
        try:
            self.proc.wait(timeout=20)
        except subprocess.TimeoutExpired:
            self.mark("child did not exit after stdin close: terminate")
            self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        self.mark("child exit code %s" % self.proc.returncode)


def user_message(text):
    return {"type": "user", "message": {"role": "user", "content": text}}


def is_result(frame):
    return frame.get("type") == "result"


def is_slow_tool_use(frame):
    if frame.get("type") != "assistant":
        return False
    return any(
        b.get("type") == "tool_use" and "slow_count" in b.get("name", "")
        for b in frame.get("message", {}).get("content", [])
    )


def main():
    out_dir, work, mcp_config, plan = sys.argv[1:5]
    env_extra = dict(a.split("=", 1) for a in sys.argv[5:])
    os.makedirs(out_dir, exist_ok=True)
    os.makedirs(work, exist_ok=True)
    session = Session(out_dir, work, mcp_config, env_extra)
    for step in json.loads(plan):
        kind = step[0]
        if kind == "sleep":
            session.mark("sleep %s" % step[1])
            time.sleep(step[1])
        elif kind == "touch":
            session.mark("touch %s" % step[1])
            open(step[1], "w").close()
        elif kind == "turn":
            session.send(user_message(step[1]))
            got = session.wait_for(is_result, BOUND)
            session.mark("turn finished: %s" % (got if isinstance(got, str) else "result"))
            if isinstance(got, str):
                break
        elif kind == "turn_interrupt":
            session.send(user_message(step[1]))
            got = session.wait_for(is_slow_tool_use, 120)
            if isinstance(got, str):
                session.mark("no slow_count tool_use: %s" % got)
                break
            session.mark("slow_count tool_use seen: sleep %s then interrupt" % step[2])
            time.sleep(step[2])
            session.send(
                {"type": "control_request", "request_id": "spike-interrupt-1", "request": {"subtype": "interrupt"}}
            )
            got = session.wait_for(is_result, 60)
            session.mark("after interrupt: %s" % (got if isinstance(got, str) else "result"))
        else:
            session.mark("unknown step %r" % (step,))
    session.close()


if __name__ == "__main__":
    main()
