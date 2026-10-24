"""A tiny stdio MCP server that ships one MCP App (B15 T9 live smoke).

Tools:
  show_counter  (model + app, readOnlyHint)  -> names ui://demo/counter.html
  bump          (app only, not read-only)    -> adds 1 to the counter
  slow_bump     (app only, not read-only)    -> sleeps 5 s, writes slow_bump.txt
                                                into the working directory, then
                                                adds 1 to the counter
Resource:
  ui://demo/counter.html  (text/html;profile=mcp-app, _meta.ui.csp)

Every request it receives is appended to $DEMO_LOG (one JSON line each),
so the test can see what the MCP client (Codex or Claude) sent, for
example the client capabilities of `initialize`.
"""

import json
import os
import sys
import time

LOG = os.environ.get("DEMO_LOG", "/tmp/demo_app_server.log")
COUNTER = {"n": 0}
URI = "ui://demo/counter.html"
HTML = """<!doctype html><html><head><meta charset="utf-8"><title>Counter</title></head>
<body><p id="n">count: ?</p><button id="b">bump</button>
<script>
let id = 0; const pending = {};
function req(method, params) { id += 1; window.parent.postMessage({jsonrpc:"2.0", id, method, params}, "*");
  return new Promise(r => { pending[id] = r; }); }
window.addEventListener("message", e => { const m = e.data; if (m && m.id && pending[m.id]) { pending[m.id](m); delete pending[m.id]; } });
req("ui/initialize", {protocolVersion:"2026-01-26", appCapabilities:{}, clientInfo:{name:"counter", version:"1"}})
  .then(() => window.parent.postMessage({jsonrpc:"2.0", method:"ui/notifications/initialized"}, "*"));
document.getElementById("b").onclick = () => req("tools/call", {name:"bump", arguments:{}})
  .then(m => { document.getElementById("n").textContent = "count: " + JSON.stringify(m.result && m.result.structuredContent); });
</script></body></html>"""


def log(obj):
    with open(LOG, "a", encoding="utf-8") as f:
        f.write(json.dumps(obj) + "\n")


TOOLS = [
    {
        "name": "show_counter",
        "description": "Shows the demo counter. Call it when asked to show the counter.",
        "inputSchema": {"type": "object", "properties": {}},
        "annotations": {"readOnlyHint": True, "title": "Show counter"},
        "_meta": {"ui": {"resourceUri": URI, "visibility": ["model", "app"]}},
    },
    {
        "name": "bump",
        "description": "Adds one to the counter (app only).",
        "inputSchema": {"type": "object", "properties": {}},
        "_meta": {"ui": {"resourceUri": URI, "visibility": ["app"]}},
    },
    {
        "name": "slow_bump",
        "description": "Waits 5 s, writes slow_bump.txt, adds one to the counter (app only).",
        "inputSchema": {"type": "object", "properties": {}},
        "_meta": {"ui": {"resourceUri": URI, "visibility": ["app"]}},
    },
]


def result(rid, res):
    return {"jsonrpc": "2.0", "id": rid, "result": res}


def handle(msg):
    method = msg.get("method")
    rid = msg.get("id")
    log({"method": method, "params": msg.get("params")})
    if rid is None:
        return None
    if method == "initialize":
        return result(rid, {
            "protocolVersion": msg.get("params", {}).get("protocolVersion", "2025-06-18"),
            "capabilities": {"tools": {}, "resources": {}},
            "serverInfo": {"name": "demo-apps", "version": "0.1.0"},
        })
    if method == "ping":
        return result(rid, {})
    if method == "tools/list":
        return result(rid, {"tools": TOOLS})
    if method == "tools/call":
        name = msg["params"].get("name")
        if name == "show_counter":
            return result(rid, {
                "content": [{"type": "text", "text": f"The counter is {COUNTER['n']}."}],
                "structuredContent": {"count": COUNTER["n"]},
            })
        if name == "bump":
            COUNTER["n"] += 1
            return result(rid, {
                "content": [{"type": "text", "text": f"The counter is now {COUNTER['n']}."}],
                "structuredContent": {"count": COUNTER["n"]},
            })
        if name == "slow_bump":
            time.sleep(5)
            with open("slow_bump.txt", "w", encoding="utf-8") as f:
                f.write("slow_bump wrote this\n")
            COUNTER["n"] += 1
            return result(rid, {
                "content": [{"type": "text", "text": f"The counter is now {COUNTER['n']}."}],
                "structuredContent": {"count": COUNTER["n"]},
            })
        return {"jsonrpc": "2.0", "id": rid, "error": {"code": -32602, "message": "unknown tool"}}
    if method == "resources/list":
        return result(rid, {"resources": [{"uri": URI, "name": "counter", "mimeType": "text/html;profile=mcp-app"}]})
    if method == "resources/templates/list":
        return result(rid, {"resourceTemplates": []})
    if method == "prompts/list":
        return result(rid, {"prompts": []})
    if method == "resources/read":
        if msg["params"].get("uri") != URI:
            return {"jsonrpc": "2.0", "id": rid, "error": {"code": -32002, "message": "resource not found"}}
        return result(rid, {"contents": [{
            "uri": URI,
            "mimeType": "text/html;profile=mcp-app",
            "text": HTML,
            "_meta": {"ui": {"csp": {"connectDomains": [], "resourceDomains": []}, "prefersBorder": True}},
        }]})
    return {"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": "method not found"}}


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        out = handle(msg)
        if out is not None:
            sys.stdout.write(json.dumps(out) + "\n")
            sys.stdout.flush()


if __name__ == "__main__":
    main()
