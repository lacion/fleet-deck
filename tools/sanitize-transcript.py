#!/usr/bin/env python3
"""Sanitize a provider transcript (JSONL) for use as a test fixture.

Usage: sanitize-transcript.py [--lines START:END]... FILE > OUT

Every record keeps its keys, value types, order, ids and timestamps. A string
value is kept only when its key is on the KEEP_KEYS list (record types, roles,
ids, times, status names). Every other string is replaced by a placeholder of
its kind: "<text 1>", "<path 2>", "<command 3>", "<pattern 4>". A placeholder
keeps the wrapper tags and the injected-block prefixes that the history parser
looks at.

--lines START:END selects the lines START to END-1 of FILE (0-based); the
option can repeat, and the selected lines are written in the given order.
Python 3 standard library only.
"""

import json
import re
import sys

KEEP_KEYS = {
    "type", "role", "subtype", "uuid", "parentUuid", "id", "sessionId",
    "session_id", "timestamp", "tool_use_id", "call_id", "status", "phase",
    "stop_reason", "userType", "entrypoint", "version", "cli_version",
    "operation", "turn_id", "root_turn_id", "thread_id", "promptId", "agentId",
    "requestId", "message_id", "leafUuid", "sourceToolAssistantUUID",
}
PATH_KEYS = {
    "cwd", "path", "file_path", "filePath", "notebook_path", "workingDirectory",
    "workspace_roots", "runtime_workspace_roots", "additionalWorkingDirectories",
}
COMMAND_KEYS = {"command", "cmd"}
PATTERN_KEYS = {"pattern", "query"}
TOOL_SIBLINGS = {"input", "arguments", "call_id", "tool_use_id"}
CLAUDE_WRAPPERS = [
    "command-name", "command-message", "command-args", "local-command-stdout",
    "local-command-caveat", "system-reminder",
]
CODEX_TAGS = ["environment_context", "skill", "user_shell_command", "user_instructions"]
AGENTS_PREFIX = "# AGENTS.md instructions"


class Sanitizer:
    def __init__(self):
        self.counts = {}

    def placeholder(self, kind):
        self.counts[kind] = self.counts.get(kind, 0) + 1
        return "<%s %d>" % (kind, self.counts[kind])

    def text(self, value, kind):
        stripped = value.lstrip()
        # A text made only of wrappers keeps its wrappers.
        wrapped = re.fullmatch(
            r"(?:<(%s)>.*?</\1>\s*)+" % "|".join(CLAUDE_WRAPPERS),
            stripped,
            re.DOTALL,
        )
        if wrapped:
            parts = re.findall(r"<(%s)>.*?</\1>" % "|".join(CLAUDE_WRAPPERS), stripped, re.DOTALL)
            return "".join("<%s>%s</%s>" % (t, self.placeholder(kind), t) for t in parts)
        for tag in CODEX_TAGS:
            if stripped.startswith("<%s>" % tag):
                return "<%s>%s</%s>" % (tag, self.placeholder(kind), tag)
        if stripped.startswith(AGENTS_PREFIX):
            return "%s %s" % (AGENTS_PREFIX, self.placeholder(kind))
        return self.placeholder(kind)

    def walk(self, value, key=None):
        if isinstance(value, dict):
            is_tool = any(k in value for k in TOOL_SIBLINGS)
            out = {}
            for k, v in value.items():
                if k == "name" and is_tool and isinstance(v, str):
                    out[k] = v
                else:
                    out[k] = self.walk(v, k)
            return out
        if isinstance(value, list):
            return [self.walk(v, key) for v in value]
        if not isinstance(value, str):
            return value
        if key in KEEP_KEYS:
            return value
        if key == "arguments":
            try:
                inner = json.loads(value)
            except ValueError:
                inner = None
            if isinstance(inner, (dict, list)):
                return json.dumps(self.walk(inner), separators=(",", ":"))
        if key in PATH_KEYS:
            return self.placeholder("path")
        if key in COMMAND_KEYS:
            return self.placeholder("command")
        if key in PATTERN_KEYS:
            return self.placeholder("pattern")
        return self.text(value, "text")


def main(argv):
    ranges = []
    path = None
    i = 0
    while i < len(argv):
        if argv[i] == "--lines":
            start, _, end = argv[i + 1].partition(":")
            ranges.append((int(start), int(end)))
            i += 2
        else:
            path = argv[i]
            i += 1
    if path is None:
        print(__doc__, file=sys.stderr)
        return 2
    with open(path, encoding="utf-8") as handle:
        lines = handle.readlines()
    if not ranges:
        ranges = [(0, len(lines))]
    sanitizer = Sanitizer()
    for start, end in ranges:
        for line in lines[start:end]:
            line = line.strip()
            if not line:
                continue
            record = json.loads(line)
            sys.stdout.write(json.dumps(sanitizer.walk(record), ensure_ascii=False, separators=(",", ":")) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
