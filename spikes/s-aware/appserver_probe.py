"""S-AWARE helper: drive `codex app-server` (JSON-RPC over stdio) for one probe.

Run it with `python3 -I appserver_probe.py <mode> <out-dir> <work-dir> ...`.
It writes every frame to `<out-dir>/frames.jsonl` and prints one summary line
per finding. It never writes outside `<out-dir>` and `<work-dir>`.

Modes:
  mcp     <out> <work> <aware-server-binary>
          Start the app-server with the aware server as an MCP server. Call
          `mcpServerStatus/list` and `mcpServer/resource/read`. No model turn.
  skills  <out> <work> <skill-root> <prompt>
          Call `skills/extraRoots/set` with <skill-root>, then `skills/list`,
          then run one turn with <prompt> and print the final answer.
  config  <out> <work> <skill-md-path>
          Start the app-server with `-c skills.config=[{path=...,enabled=true}]`
          and call `skills/list`. No model turn.
"""

import json
import os
import queue
import subprocess
import sys
import threading
import time

TIMEOUT = 120


class Client:
    def __init__(self, args, out_dir, env):
        self.out = open(os.path.join(out_dir, "frames.jsonl"), "w")
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
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for line in self.proc.stdout:
            self.out.write(line)
            self.out.flush()
            try:
                self.frames.put(json.loads(line))
            except ValueError:
                pass
        self.frames.put(None)

    def send(self, obj):
        self.out.write("> " + json.dumps(obj) + "\n")
        self.out.flush()
        self.proc.stdin.write(json.dumps(obj) + "\n")
        self.proc.stdin.flush()

    def request(self, method, params):
        rid = self.next_id
        self.next_id += 1
        self.send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        deadline = time.time() + TIMEOUT
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
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.terminate()


def handshake(client):
    answer = client.request(
        "initialize",
        {
            "clientInfo": {"name": "s-aware", "version": "0"},
            "capabilities": {"experimentalApi": True},
        },
    )
    client.send({"jsonrpc": "2.0", "method": "initialized"})
    return answer


def start_thread(client, work, extra=None):
    params = {
        "cwd": work,
        "approvalPolicy": "never",
        "sandbox": "read-only",
        "ephemeral": True,
    }
    params.update(extra or {})
    answer = client.request("thread/start", params)
    return (answer.get("result") or {}).get("thread", {}).get("id"), answer


def run_turn(client, thread_id, prompt):
    answer = client.request(
        "turn/start",
        {"threadId": thread_id, "input": [{"type": "text", "text": prompt}]},
    )
    deadline = time.time() + TIMEOUT
    text = ""
    usage = None
    while time.time() < deadline:
        frame = client.pump(deadline - time.time())
        if frame is None:
            break
        method = frame.get("method")
        params = frame.get("params") or {}
        if method == "item/completed":
            item = params.get("item") or {}
            if item.get("type") == "agentMessage":
                text += item.get("text", "")
        if method == "thread/tokenUsage/updated":
            usage = params.get("tokenUsage")
        if method == "turn/completed":
            return answer, text, usage
    return answer, text, usage


def base_env(work):
    env = dict(os.environ)
    env["NO_COLOR"] = "1"
    return env


def main():
    mode, out_dir, work = sys.argv[1:4]
    os.makedirs(out_dir, exist_ok=True)
    os.makedirs(work, exist_ok=True)
    env = base_env(work)
    args = ["codex", "app-server", "-c", 'model_reasoning_effort="low"']
    if mode == "mcp":
        binary = sys.argv[4]
        args += ["-c", f'mcp_servers.aware.command="{binary}"']
        # The app-server gives an MCP child a filtered environment: set the
        # log path in the server table.
        log_path = os.environ["AWARE_LOG"]
        args += ["-c", f'mcp_servers.aware.env={{AWARE_LOG="{log_path}"}}']
    if mode == "config":
        skill_md = sys.argv[4]
        args += ["-c", f'skills.config=[{{path="{skill_md}",enabled=true}}]']
    client = Client(args, out_dir, env)
    print("initialize:", json.dumps(handshake(client))[:300])
    if mode == "mcp":
        thread_id, _ = start_thread(client, work)
        print("thread:", thread_id)
        status = client.request("mcpServerStatus/list", {"threadId": thread_id})
        data = (status.get("result") or {}).get("data") or []
        for server in data:
            if server.get("name") != "aware":
                print(
                    "mcpServerStatus (other server):",
                    server.get("name"),
                    "tools=",
                    len(server.get("tools") or {}),
                )
                continue
            print(
                "mcpServerStatus:",
                server.get("name"),
                "tools=",
                list((server.get("tools") or {}).keys()),
                "resources=",
                [r.get("uri") for r in server.get("resources") or []],
                "templates=",
                [t.get("uriTemplate") for t in server.get("resourceTemplates") or []],
            )
        if not data:
            print("mcpServerStatus raw:", json.dumps(status)[:500])
        read = client.request(
            "mcpServer/resource/read",
            {"server": "aware", "uri": "fleet://stations", "threadId": thread_id},
        )
        print("resource/read:", json.dumps(read)[:400])
    elif mode == "skills":
        root, prompt = sys.argv[4], sys.argv[5]
        print("extraRoots/set:", json.dumps(client.request("skills/extraRoots/set", {"extraRoots": [root]}))[:300])
        listed = client.request("skills/list", {"cwds": [work], "forceReload": True})
        names = [
            s.get("name")
            for entry in (listed.get("result") or {}).get("data") or []
            for s in entry.get("skills") or []
        ]
        print("skills/list names:", names)
        print("fleet-guide listed:", any(n and n.endswith("fleet-guide") for n in names))
        thread_id, started = start_thread(client, work)
        print("thread:", thread_id)
        _, text, usage = run_turn(client, thread_id, prompt)
        print("answer:", text[:300].replace("\n", " "))
        print("GUAVA in answer:", "GUAVA" in text)
        print("usage:", json.dumps(usage))
    elif mode == "config":
        listed = client.request("skills/list", {"cwds": [work], "forceReload": True})
        names = [
            s.get("name")
            for entry in (listed.get("result") or {}).get("data") or []
            for s in entry.get("skills") or []
        ]
        print("skills/list names:", names)
        print("fleet-guide listed:", any(n and n.endswith("fleet-guide") for n in names))
    client.close()


if __name__ == "__main__":
    main()
