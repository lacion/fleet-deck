# M3 lane B (orchestration) decisions

Lane B of M3 numbers its decisions DL-200 to DL-299. The format is the
format of [DECISIONS-LOG.md](../DECISIONS-LOG.md): context, options, choice,
reason, owner review.

## DL-200: Callsigns and workspace provisioning in the start effect (M3 6-1)

- Context: PLAN §6.6 asks for worktrees `<repo>--fd-<callsign>` on branch
  `fd/<callsign>`, a credential-safe clone, and filesystem work through the
  effect worker. v2 had no callsign. Mail (6-2), tasks (6-4) and the
  coordinator (6-5) need a short, unique address for a session. A larger
  `session.started` event made the `Event` enum too large for clippy
  (`large_enum_variant` on `EventBody` and on both provider cores).
- Options: (a) a new `session.provision` command and effect before the
  start; (b) an optional `workspace` field on `session.start`, planned and
  checked in the command transaction, executed by the start effect before
  the provider starts; (c) the board runs `git` through a shell.
- Choice: (b). `SessionCommand::Start` gets `workspace: Option<WorkspaceRequest>`
  (`{kind: "worktree"}` or `{kind: "clone", url}`; absent = the M2
  behaviour). The decision gives every new session a callsign: the v0
  rotation (`falcon` … `drift`, starting at the number of active sessions)
  plus the last four id characters, then the whole id when all twelve are
  taken; unique on the station. It is recorded by a new station event
  `session.named {callsign, worktree}` (older peers keep it as unknown), not
  by new fields on `session.started`. The worktree plan needs the top
  directory of a main checkout, a parent inside the allowed roots and a
  free path; the clone plan needs a URL without credentials (`https://`
  without user info, `ssh://` without a password, `user@host:path`, or
  `file://` inside the roots; `<helper>::` and other transports are
  `forbidden`) and a new plain directory name whose parent is inside the
  roots. The start effect runs `git worktree add -b fd/<callsign> -- <path>`
  or `git -c protocol.ext.allow=never clone --no-recurse-submodules -- <url>
  <path>` with the station's own Git configuration and credentials, before
  the provider starts. A failed Git step ends the session with
  `start_failed` ("the workspace was not created: …"); a repeated effect
  finds the worktree on its branch and continues.
- Reason: one command keeps the A9 idempotency (one command id, one
  receipt) and the PLAN §6.4 rule "filesystem work through the effect
  worker"; the decision only reads the file system. A separate event keeps
  the hot `Event` enum small and needs no change on the provider side.
- Owner review: pending

## DL-201: Repo catalog fields and repo grouping (M3 6-1)

- Context: PLAN §6.6 asks for a repo catalog (path, scrubbed origin,
  default branch) and repo grouping: a heuristic by normalized origin, and
  an explicit owner choice for repos with several remotes or none.
- Options: (a) a new `repo.list` link method; (b) optional fields on the
  existing `workspace.list` entries.
- Choice: (b). `WorkspaceEntry` gets optional `origin` (user info, query
  and fragment removed), `default_branch` (`refs/remotes/origin/HEAD`, else
  the current branch) and `group`. The station reads these facts from
  `.git/config` and `.git/refs` (no `git` process in a listing). `group` is
  the owner's choice from the new `[repo_groups]` table of `station.toml`
  (path → name), else the normalized origin (`host/path`, lower case,
  without `.git`) when the repo has exactly one remote, else absent.
- Reason: no new link method and no new board call; older boards ignore
  the fields. The conflict radar (6-7, lane C) reads `group`.
- Owner review: pending

## DL-202: Batch spawn and `Nx` are a board expansion (M3 6-1)

- Context: v0 batch spawn starts one agent for each line, `3x task` starts
  three, and each agent gets a forced worktree.
- Options: (a) a station `session.start_batch` command; (b) the board
  expands the text and sends one `session.start` for each prompt.
- Choice: (b). `batchPrompts` (`web/src/spawn/spawn.ts`) gives one prompt
  for each non-empty line; `Nx task` with N from 1 to 10 repeats the task.
  A batch always asks for a worktree. Each start has its own command id;
  the form stops at the first refusal and shows "k of n sessions started".
- Reason: every session stays one idempotent command; the station needs no
  new command and the cap check (`max_sessions`) works per start.
- Owner review: pending

## DL-204: `assign auto` is a deck method with a pure ranking (M3 6-3)

- Context: PLAN §7.3 asks for v0's SQL choice (idle first, least buried,
  right repo) plus station terms (connected, repo exists, under
  `max_sessions`, provider available), and "spawn on <best station>" when
  no session fits. The deck read model holds every card of every station.
- Options: (a) an SQL query on the deck read model; (b) a pure Rust ranking
  over `sessions()` and the station views; (c) a station method.
- Choice: (b). `POST /api/assign {command_id, text, repo?, provider?}`
  (`session.assign`, drive scope). Eligible cards: `idle` (rank 0),
  `queued` (1), `working`/`verifying` (2); `needs_you`, `ended`,
  `unreachable` and `lost` take no task. Then the fewest undelivered mails
  (an empty map until 6-2), then the newest `updated_at`. A repo matches the
  session `cwd` when it is the repo, inside it, or a `<repo>--fd-<callsign>`
  worktree. The deck sends `[FLEETDECK ASSIGNMENT] <task>` as
  `session.prompt` to an idle session, else as `session.follow_up`, with the
  board's command id; a resend with that id returns the stored command and
  sends nothing. Unrouted: `spawn_on` is the reachable station with the
  most free places under `max_sessions` (a station without `station.info`
  is skipped) and a `signed_in` instance of the provider, whose
  `workspace.list` finds the repo. The board offers "Spawn on <station>",
  which opens the spawn form.
- Reason: the read model is already folded in memory per request; a pure
  function is testable without a database and the rules stay in one place.
  The busy rank keeps v0's behaviour (a busy session still takes a queued
  follow-up).
- Owner review: pending

## DL-203: Fast rounds and parallel lanes (owner decision)

- Context: the owner wants a usable M3 soon (2026-10-12) and will give
  feedback from real use.
- Choice (owner, 2026-10-12): each item gets one review; its findings go to
  a "Review follow-ups (after M3)" list for the iterations after M3. Normal
  rounds run only the format and lint checks; the full `make check` runs
  once at the end, in lane A. Four lanes build M3 at the same time; this is
  lane B (orchestration: 6-1, 6-3, 6-4, 6-2, 6-5).
- Owner review: approved (owner, 2026-10-12)

## DL-205: Fleet mail is deck-owned intent in the outbox (6-2)

- Context: PLAN §16 keeps fleet mail "through the durable outbox, delivered
  as native follow-up/steer". The M2 outbox (A5) stores routed command
  intent; it had no mail record that the board can list.
- Options: (a) post a `session.follow_up` directly from the board; (b) a
  deck `mail` table whose row and outbox item are inserted in one
  transaction (schema V6), the outbox payload being the framed command.
- Choice: (b). `POST /api/mail {mail_id, station, session_id, text,
  delivery?}` (`mail.send`, drive scope, audited) checks that the session is
  in the read model, stores the mail once (a resend with the same id
  returns the stored mail and queues nothing) and queues
  `[FLEETDECK MAIL from <sender>] <text>` as `session.follow_up` (default)
  or `session.steer`, origin `mail`, valid for 24 h. `GET /api/mail`
  (`mail.list`, read scope) shows the newest 200 with the delivery state of
  the outbox item. `assign auto` counts the undelivered mails of a session
  as "buried" (DL-204).
- Reason: the outbox already gives once-only delivery, station-reset retry
  and expiry; the mail row only adds the who/what for the board.
- Owner review: pending

## DL-206: Tasks with waitable completions (6-4)

- Context: PLAN §16 row "Tasks and waitable completions"; v0 had
  `complete_task` and a blocking wait in its coordinator tools.
- Choice: a deck `tasks` table (schema V6) with `open`, `done`, `failed`.
  `POST /api/tasks {task_id, title, station, session_id}` (`tasks.create`,
  drive) stores the task and its outbox item in one transaction and
  delivers `[FLEETDECK TASK <id>] <title>` plus a completion hint as a
  follow-up. `POST /api/tasks/{id}/complete {state, result?}`
  (`tasks.complete`, drive) ends an open task once; the same completion
  again returns the task, a different one is `409 task_ended`.
  `GET /api/tasks/{id}/wait?timeout_ms=` (`tasks.wait`, read) returns the
  task when it ends or at the deadline (default 30 s, at most 60 s).
- Reason: completion is first-writer-wins, so two reporters cannot flip a
  result. The wait polls the store every 100 ms: simple, and bounded by the
  60 s cap. The coordinator MCP (6-5) exposes the same calls to agents.
- Owner review: pending
