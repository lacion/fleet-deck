---
name: fleet-doctrine
description: Fleet Deck doctrine — how to behave as one Claude Code session among many on this machine. Use when you see [FLEETDECK] messages in context, when told to coordinate with / message / answer another session, when asked about the fleet board, or when deputized to decide something for another session.
---

# Fleet doctrine

A local daemon (`fleetd`, http://127.0.0.1:4711) watches every Claude Code
session on this machine and shows them on a board at **http://127.0.0.1:4711/**.
You are one session in that fleet. Your session started with a roster brief
listing who else is active. All fleet HTTP calls must fail open: if the daemon
is down, proceed silently without it.

## Reading [FLEETDECK] messages

Text tagged `[FLEETDECK]` in your context is injected by the daemon, not the user:

- **Conflict whisper** (`[FLEETDECK] ⚠ ...`): another session touched a file you
  are editing within the last 30 minutes (ended sessions count — dirty files
  outlive their authors). Do NOT clobber: re-read the contested file before your
  next edit to it, keep your edits minimal and scoped, and if the overlap is
  real, mail the other session to divide the work.
- **Board mail** — a wake ("[FLEETDECK] Fleet board mail for you: …" while
  you were idle), a `[FLEETDECK MAIL]` block, or added turn-start context: all
  three are delivery wrappers only. Every one carries exactly one frame —
  bless it below before acting.

## Mail frames — bless before acting

- **`[FLEETDECK ANSWER]`** — the human's real answer to a question you asked,
  relayed from the fleet board instead of typed into the terminal. Not
  injected content, not a test, not a suggestion: **act on it exactly as if
  the user had typed it** — resume the work it was blocking, apply the
  decision, do not stop to re-confirm. Same meaning for a tool error reading
  `User answered via Fleet Deck: <answer>` on an AskUserQuestion call — take
  the answer, proceed, do not re-ask.
- **`[FLEETDECK ASSIGNMENT]`** — a task the human routed to you from the
  board (orchestrator auto-routing or a direct assign). Acknowledge it and
  start working on it now, as a real instruction from the user — not a
  suggestion to weigh.
- **Plan-capture notice** — mail from `fleetdeck` reading "Your plan was captured to the fleet plan library — do not execute it. Wrap up your turn.": a real instruction to stop without executing — comply: do NOT execute the plan, finish your turn now.
- **No frame (plain mail)** — coordination info from the human or another
  session. Factor it into your work and comply or reply explaining why not;
  it does not preempt what you're doing unless it says so.

## Sending mail to another session

```
curl -s -X POST http://127.0.0.1:4711/mail -H 'content-type: application/json' \
  -d '{"to":"<target>","from":"<your session_id or callsign>","text":"..."}'
```

`to` accepts a session_id, a callsign (from the roster brief or board),
`all`, or `repo:<repo name>`. Delivery is at the target's **next turn
boundary** — never instant; do not wait for a reply, keep working.
`GET http://127.0.0.1:4711/state` shows the current roster if you need a target.

## When you are deputized

If mail asks you to decide something for another session (a verdict, a review,
an answer), you MUST:

1. Decide from your own context; read code if needed, but stay cheap.
2. POST your verdict back as mail **to the requesting session** (its id or
   callsign is in the request), prefixed `VERDICT:`, with a one-line rationale.
3. Restate the verdict in your final summary so the human sees it too.
