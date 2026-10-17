# `!` shell commands in a session (B8)

- **Status:** Accepted, revision 10 (2026-10-16). Round 10 (owner-extended): PASS, with two MAJOR open items that §2.0000000 resolves (added after the PASS, not re-reviewed; the PRE-PR review checks the code). Revision 9: FAIL (§11). Revision 8: FAIL (§10). Revision 7: FAIL (§9). Revision 6: FAIL (§8). Revision 5: FAIL (§7). Revision 4: FAIL (§6). The owner requires parity with the Claude Code and Codex terminal UIs ("we need full parity with CC and Codex") and so approved review rounds beyond three. Revision 1: FAIL. Revision 2 (station-run with hidden context attached at dispatch): FAIL. Revision 3 (Terminal keystrokes, no automatic context): withdrawn, no parity.
- **Goal (parity):** a composer line that starts with `!` runs the command in the session folder; the chat shows the command and its output; the model gets the result at the next prompt without the owner copying it — as `!` does in Claude Code and in Codex.

## 1. Facts

- **Claude Code** runs a `!` line itself (bash, the session folder), shows the output, and writes two user records into the conversation: `<bash-input>CMD</bash-input>` and `<bash-stdout>OUT</bash-stdout><bash-stderr>ERR</bash-stderr>` (HTML-escaped; empty stdout → `(Bash completed with no output)`; output cut near 30,000 characters). The model reads them with the next prompt. Its own rewind does not undo the command's file changes; descendants that leave the process group outlive a cancel.
- **Codex** has `thread/shellCommand { threadId, command, timeoutMs }` (app-server 0.160.0+, the same as its TUI `!`): it acknowledges at once, runs the command unsandboxed in the app-server's environment as a standalone turn (`turn/started`, an item `commandExecution` with `source: userShell`, `aggregatedOutput`, `exitCode`, a user message `<user_shell_command>…</user_shell_command>` in the thread, `turn/completed`, no model call), and the model sees it at the next prompt. Its cancel ends the direct child; descendants can outlive it (the TUI behaves the same).
- Fleet Deck drives Claude with `stream-json` (no shell input) and Codex with the app-server. Claude revert is refused on both hosts (DL-087); a Codex turn has a checkpoint and can be rolled back.

## 2. Design

### 2.0000000 Clarifications after the round-10 PASS (they win over all earlier change lists)

1. **Start-up keeps terminal results.** The start-up reconciliation (§2.000000.2) reclassifies only rows whose state is not terminal; a terminal row with `advance_pending = 1` keeps its state and result and only gets its advance. Every reclassification is a guarded update (`… WHERE effect_id = ? AND state = <the state read>`), so a durable-replay completion that lands first is never overwritten.
2. **Undispatched means settled, both providers.** Whenever a shell effect is retired as not dispatched (the dispatch policy recheck, a retirement before the task registers, any generic retirement path of an effect whose payload is a shell command), the same store transaction settles its `shell_runs` row `cancelled` with `advance_pending = 1` (Claude and Codex). The effect store does this in its retirement transition for shell payloads, not in the shell task.

### 2.000000 Changes in revision 10 (they win over all earlier change lists)

1. **Settled means settled and advanced.** `shell_runs` gets `advance_pending INTEGER NOT NULL DEFAULT 0`. Every settlement (by the task, by Codex completion, by proof, by `ReleaseShell`, by start-up recovery) sets the terminal state **and** `advance_pending = 1` in one transaction. A task (and the start-up step below) then takes the root fence and, in one transaction, increments the root's generation and sets `advance_pending = 0`. Everywhere the design says "unsettled" (the root barrier, the input and revert refusals, drain, the restart check), it means: state not terminal **or** `advance_pending = 1`. So no checkpoint can be taken at the old generation after a settlement, also across a crash.
2. **Start-up reconciliation before admission.** Before `shell_admission` opens and before any command is admitted, the station walks every row that is unsettled and settles it by its effect's recovered state:
   - the effect was never claimed or is retired as not dispatched (proved unsent) → `cancelled`;
   - a Claude row whose effect was claimed (the command may have started) → the marked-process kill (§2.3), then `unknown`;
   - a Codex row whose request may have been sent → `awaiting_proof` (released by Codex completion in the durable replay, by proof, or by `ReleaseShell`);
   - then it completes every pending advance (rule 1) under the root fences, before admission.
3. **`ReleaseShell`, concretely in any session state.** Its decide branch does not call `live_session`; it reads the session row (any state: live, lost, exited, hibernated), the shell row and the caller, and appends the events (the event store accepts events for a session that exists). It accepts a row that is `awaiting_proof`, or not terminal while its session is lost or exited (the same "the owner states the command no longer runs" meaning). It settles with `advance_pending = 1` (rule 1).

### 2.00000 Changes in revision 9 (they win over all earlier change lists)

1. **Owner release of a folder.** New `SessionCommand::ReleaseShell { session_id, effect_id }`, method `session.release_shell`, the same feature `shell_command.v1`. Deck: owner login only (members and tokens 403), scope `Admin`. Station decide: a paired deck caller with origin `Board`, no authority; the row exists, belongs to the session and is `awaiting_proof` (any other state: `InvalidState`, "This command has already settled."); the session may be in any state (live, lost, exited or hibernated). The decide transaction settles the row `unknown` ("released by the owner") and appends a `ToolCompleted { status: Failed, exit_code: None, provider_refs: { "shell": "released" } }` event for the shell node and an audit event `shell.released { effect_id, worktree_root }`; the generation advance happens under the root fence right after the commit (a task; until it ran, the barrier stays, so the release cannot open the root before the advance). This works on every host (direct Linux, shim Linux, macOS) and is the owner's statement that the command no longer runs; the board asks first: "Release this folder? Only do this when you are sure that `!<command>` is no longer running (for example after a reboot or after you checked the processes)."
2. **Where it is offered:** the board shows "Release the folder" on the shell card and in the folder's busy notice whenever a row is `awaiting_proof`. A proof-based settlement (§2.0000.1) still settles the row by itself when it arrives.

### 2.0000 Changes in revision 8 (they win over all earlier change lists)

1. **Codex settlement needs proof (replaces the "session ends" trigger of §2.000.1).** A Codex `shell_runs` row settles only on (a) the provider-confirmed completion of the shell turn (Codex's own `turn/completed` of that turn, any status including `interrupted`, observed from the app-server stream or its durable replay), or (b) the DL-182 proof that the provider process ended (a verified clean exit, a verified reboot, or the owner's reconciliation `session.reconcile_execution`). A synthetic turn end or a `lost` session from recovery does **not** settle it: the row stays unsettled (`awaiting_proof`), the root barrier and the refusals stay, and the board shows "A shell command may still be running in this folder; reconcile the session to release it." The settlement (with its generation advance) happens when the proof arrives, under the root fence.
2. **Shell admission closes before shutdown.** A durable station flag `shell_admission` (`open` | `closed`) in the store: shutdown and a planned restart set it to `closed` in one transaction before the cancel sweep of §2.000.3; the decide of `ShellCommand` reads it in its own transaction and refuses when closed ("Fleet Deck is stopping."); the station start sets it to `open` only after the start-up settlement of §2.3 and before admission opens. So no shell row can be created after the sweep.
3. **Codex non-delivery settles at once.** A dispatch refusal (the policy recheck, `Dispatch::NotDispatched`), an error response to `thread/shellCommand`, or a write that provably did not reach the provider settles the row `failed` (generation advance as usual) and clears the pending marker. A request with no response within 30 s becomes `awaiting_proof` (rule 1); the owner releases it with `Stop` of the session (which ends the provider process and yields the DL-182 proof) or with reconciliation.

### 2.000 Changes in revision 7 (they win over all earlier change lists)

1. **The whole folder is busy during any shell, both providers.** The root-wide refusal of §2.0.2 (`Prompt`, `FollowUp`, `Steer`, `ShellCommand`, `Start`, `Resume`, `Import`, and per §2.00.2 `Revert`, `RetryRestore`) applies while any `shell_runs` row of the root is unsettled, for Claude **and** Codex rows. A Codex row settles when its shell turn completes (or is interrupted, or the session ends). So no other session of the root can capture a start checkpoint while a shell may write. Inputs admitted before the shell's decide dispatch before it (queue order), so their start checkpoints precede the generation advance.
2. **The generation advances at the start and at the settlement.** Under the root fence, the generation is incremented just before the first possible write (as §2.00.1) and again when the row settles (`done`, `failed`, `cancelled`, `timeout`, `unknown`, including the start-up settlement), so no checkpoint can share a shell's generation and predate its writes.
3. **Durable cancel at shutdown.** Shutdown first sets `cancel_requested` on every unsettled `shell_runs` row in one transaction (queued, claimed or running), then signals the running tasks, then joins the worker. Every shell task checks `cancel_requested` in the same transaction that registers it as running and again right before the spawn; a set flag settles the row `cancelled` without running anything. This also covers effects that the worker dispatches during its join.

### 2.00 Changes in revision 6 (they win over §2.0 and §2.1–§2.5 where they differ)

1. **Folder generation instead of times (replaces the `started_at` rule of §2.0.1).** A durable per-root counter `worktree_generations (worktree_root PK, generation INTEGER)`. A shell run (Claude: the station task; Codex: the dispatch of `thread/shellCommand`) increments it under the root's turn fence, in one transaction that also stores the new value on the `shell_runs` row (`generation`), just before the first possible write (Claude: before the spawn; Codex: before the request is sent). Every turn start checkpoint (`manager/turns.rs`, created under the same fence) stores the root generation it was taken at. A revert, its dispatch, its recovery and every restore retry are refused when the target turn's stored generation is lower than the root's current generation ("A shell command ran in this folder after this turn; the files cannot be reverted across it."). A start checkpoint without a stored generation (older rows) counts as generation 0. A Codex rollback range that contains a shell turn stays refused.
2. **No revert while a shell is open.** While any `shell_runs` row of the root is not settled (decided, queued, or running), `Revert` and `RetryRestore` of every session of the root are refused at decide ("A shell command is running in this folder."), so no revert dispatch waits on the root's fence in the effect worker.
3. **Drain and shutdown.** The drain busy snapshot lists the sessions of every unsettled `shell_runs` row (decided, queued or running) until it settles, so `station drain` never reports idle while a command may run. Shutdown sends the cancel to every running Claude shell task before it joins the effect worker and before shim sessions detach, then waits (at most 10 s) for the rows to settle (`cancelled`, else `unknown`).

### 2.0 Changes in revision 5 (they win over §2.1–§2.5 where they differ)

1. **Folder barrier (both providers).** `shell_runs` also stores `worktree_root` (the canonical worktree root of the session, as the turn fences use it). The row is written at decide, before any execution. A revert in **any** session whose worktree root equals a row's root is refused when the row's `started_at` is after the start of the revert's target turn, whatever the row's state ("A shell command ran in this folder after this turn; the files cannot be reverted across it."). The check runs at revert admission, at revert dispatch, at revert recovery and at every restore retry. Codex: a revert whose rollback range contains a shell turn (also when the target is the shell turn) is refused; the shell-turn revert claim of §2.2 is dropped.
2. **No worker stall.** While a Claude `shell_runs` row of a root is `running`, the decide of `Prompt`, `FollowUp`, `Steer`, `ShellCommand`, `Start`, `Resume` and `Import` of every session of that root is refused ("A shell command is running in this folder."), so no dispatch in the effect worker waits on that root's fence. The shell task reads the cancel stored at decide on the row before it starts and while it runs.
3. **Restart.** `ShellCommand` counts as work in the submit counters (`drain::creates_work`, the restart gate's freeze handshake), queued shell effects count as claims, and the restart check lists open shell rows as work (consent refused while one is open). A shutdown cancels a running Claude shell run and settles it (`cancelled`); a Codex shell turn follows the existing turn rules.
4. **Every settled Claude result reaches the model.** `done`, `failed`, `cancelled`, `timeout` and `unknown` rows are all attachable: the blocks carry the command, the end state (`exit N`, "cancelled", "timed out", "outcome unknown: the station restarted during the command"), and the output the station captured (none for `unknown`).
5. **Byte budget at the station.** At decide, each attached result is serialized and escaped first; a result over 32 KiB has its streams cut from the front (with `[… N characters omitted]`) until it fits, keeping the command and the end state; the total per input is 64 KiB; results that do not fit stay pending for the next input (the board shows "N shell results wait for the next prompt").
6. **Claude transport, named.** `SessionCommand::Prompt`/`FollowUp` gain `input.shell_results: Vec<EffectId>`; the resolution to `InputRecord.shell_context: Vec<ShellContext>` happens in the one helper that builds an input record (`input_effect` and the resume-first path `resume_hibernated()` both call it); `host_input()` (`manager/durable.rs`) copies it to a new `HostInput.context: Vec<ShellContext>` (`providers/adapter.rs`); `ClaudeCore::prompt` passes it to `control::user_message`, which writes the blocks before the prompt text; the durable replay rebuilds `HostInput` from the stored effect, so live dispatch and replay carry the same snapshot. Codex ignores `HostInput.context` (Codex keeps native context).
7. **Cancel guarantee per provider.** Claude: a cancel and a timeout end the process group and marked processes (escaped descendants excepted). Codex: native semantics (Codex ends its direct child; descendants can outlive it, as in the Codex TUI).

### 2.1 Command, gates and threat model

- `SessionCommand::ShellCommand { session_id, command: String }`, method `session.shell_command`, link feature `shell_command.v1`.
- **Deck:** owner login only (`Principal::Owner`; members and tokens 403), the station's `allow_shell == Some(true)` (else 409 `shell_not_allowed`), scope `Admin`, no `.member()`.
- **Station decide:** the caller is a paired deck (`Caller::is_peer()`) with origin `Board`; no authority, not a coordinator origin; `policy.allow_shell()` (checked again at dispatch); `command` 1–4096 characters, no NUL; the session is live (a hibernated session: "Resume the session first.") and idle: no current run, no pending input, start or shell effect, no open shell run (else `InvalidState`, "Wait for the current turn or command to end."); a restart in progress refuses ("Fleet Deck is restarting.").
- Only the composer parses `!`; any other path's `!` text stays a prompt.
- **Threat model:** the owner runs the command with the station user's rights (like the Terminal and like `!` in the terminal UIs). Fleet Deck guarantees: only the owner login starts one; the gates above; the command runs in the session folder; a cancel and a timeout end its process group; the result reaches the model once, with the prompt that names it. Not guaranteed (as in the terminal UIs): descendants that leave the process group and clear the marker variable outlive a cancel or a station crash; the command's file changes are not part of a Claude turn. A shared session shares the conversation, so its members see the command and the output that the agent sees; the composer says so in `!` mode on a shared session.

### 2.2 Codex sessions: native

- The effect sends `thread/shellCommand { threadId, command, timeoutMs: 600000 }`. The ack confirms the dispatch. The Codex core keeps a pending-shell marker; the turn that Codex starts next is the shell run: `RunStarted { origin: Shell (new RunOrigin variant), input_summary: "!<command>" }` (not "provider turn"), and it ends with Codex's `turn/completed`.
- Decide refuses prompts while the shell effect is pending or its run is current (the existing busy rules plus the pending shell effect); an `Interrupt` of that session interrupts the shell turn with `turn/interrupt` (before `turn/started` the marker records the cancel and the core interrupts as soon as the turn id is known).
- The `commandExecution` item with `source: userShell` maps to `ToolStarted { tool_kind: Shell, title: "Shell command", input_summary: command }`, `ToolOutput` (the last 16 KiB of `aggregatedOutput`) and `ToolCompleted { exit_code }`.
- The shell turn is a turn like any other: its file changes are captured in its checkpoint, and its revert uses Codex rollback. Until a live test proves that `thread/rollback` counts a shell turn as one turn, a revert whose target is older than a Codex shell turn is refused ("A shell command ran after this turn.").
- Auto-continue never continues a session whose lost run has origin `Shell`.

### 2.3 Claude sessions: station-run, result with the next prompt

- **Run:** a station task (not the effect worker) runs `bash -c <command>` (`/bin/sh -c` when bash is missing) in the session's cwd (rechecked with `policy.workspace`), with `ChildEnv::from_process()` plus `FLEETDECK_SHELL_ID=<effect id>`, stdin null, a new process group, timeout 600 s. It first waits (at most 5 s) until the session's turn bookkeeping has no open or due capture, else the command fails with "The session is still saving its last turn; try again." It holds the session's turn fence while it runs. Cancel (`Interrupt` of that session, admitted while the shell row is open even with no provider run; a cancel stored on the row before the task starts is honored at start), timeout and the end send TERM then KILL to the group and to marked processes.
- **Row:** `shell_runs (effect_id PK, session_id, command, state running|done|failed|cancelled|timeout|unknown, exit_code NULL, stdout_tail, stderr_tail, cancel_requested, included_in NULL, started_at, ended_at)` written at decide with the effect. At station start, every `running` row is settled `unknown` after the marked-process kill (the setup-command pattern); queued shell effects of an earlier station generation are retired, not run.
- **Events:** `ToolStarted { node_id: "shell-<effect id>", tool_kind: Shell, title: "Shell command", input_summary: command }`, `ToolOutput` (the last 16 KiB of stdout, then stderr), `ToolCompleted { status: Ok | Failed, exit_code, provider_refs: { "shell": "done" | "cancelled" | "timeout" | "unknown" } }`. No run is created (the model is not called).
- **Result to the model (parity):** the board marks every Claude shell result of the session that is `done` or `failed` and not yet included as pending ("Includes the output of `!<command>`", a chip per result, removable by the owner). The next `Prompt` or `FollowUp` the owner sends carries `input.shell_results: [effect_id]` (the chips that remain). At decide, each id must name a `shell_runs` row of this session in state `done`/`failed` with `included_in` NULL (else the id is dropped and the board shows why); the station copies the command, the tails (the last 30,000 characters of each stream) and the exit code into the new `InputRecord` (new field `shell_context`, `#[serde(default, skip_serializing_if = "Vec::is_empty")]`) and sets `included_in = <input id>` in the same transaction. The context is then part of that input: durable in the effect, replayed with it on the shim host, delivered once with it, and gone with it when that input is discarded.
- **Format:** the Claude core writes, before the prompt text in the same user message, per result two text blocks in Claude Code's format (escaped, the empty-stdout text, a stream over 30,000 characters cut from the front with `[… N characters omitted]`), at most 64 KiB per input (the board refuses to attach more and says so). The run label and `input.text` stay the prompt.

### 2.4 Board

- Composer: a text that starts with `!` (owner, `allow_shell` not false) switches to "Shell command" mode: a chip `!`, the placeholder "Shell command (runs in the session folder)"; Enter sends `session.shell_command`; `\!` sends a literal `!` prompt; while the session works: "Wait for the turn to end." A running command shows "Running… Stop" (Interrupt).
- The chat shows the shell item: the command (mono), the output, the exit or the end state. Claude: the pending-result chips above the composer.

### 2.5 History import

- Claude `bash-input`/`bash-stdout`/`bash-stderr` records and Codex `<user_shell_command>` messages render as one shell item with the command and the output.

## 3. Tests

- Deck: owner allowed; member, token 403; `allow_shell` off or absent 409.
- Decide: refused from the local socket, a coordinator, an authority, `allow_shell` off (decide and dispatch), bad text, hibernated, busy, restarting; an `Interrupt` is admitted while a Claude shell row is open.
- Codex: a recorded app-server fixture of `!pwd` (live record, 0.162.1) gives one run with origin `Shell`, the shell item with output and exit code; a prompt during it is refused; an interrupt before `turn/started` interrupts once the turn is known; auto-continue skips a lost shell run; a revert across a shell turn is refused.
- Claude: the command runs in the cwd with the scrubbed env and the marker; cancel before and during the run; timeout; the capture wait; a crash leaves `running` → `unknown`; a queued effect of an old generation is retired; the next prompt with chips carries the blocks once (a second inclusion is dropped); a removed chip is not sent; a discarded input takes its context with it; the shim host replays the context with the input.
- Live smoke (architect run): Claude and Codex `!pwd`, then "which folder did the command print?" answers it without the owner pasting anything.

## 4. Out of scope

- `!` during a running turn (queueing). Members running commands. Interactive commands (stdin is null; use the Terminal).

## 5. Review map (revision 2 → revision 4)

| Finding (round 2) | Answer |
| --- | --- |
| BLOCKER the launcher can register its scope after a crash | no systemd scope; the setup-command pattern with the documented limit (§2.1 threat model) |
| BLOCKER root-wide cancel crosses sessions | cancel is session-scoped (§2.3) |
| BLOCKER work escapes into another user unit | not guaranteed, stated in the threat model (as in the terminal UIs) |
| MAJOR `systemd-run` expands arguments | no `systemd-run` |
| MAJOR queued effects survive a crash | §2.3 old-generation shell effects are retired at start |
| MAJOR the folder-idle check | session-scoped idle, as in the terminal UIs; Claude changes are not part of a turn (Claude revert is refused anyway); Codex shell runs are turns |
| MAJOR attached-unknown means zero deliveries | the context is part of the input that names it (§2.3): delivered with it or gone with it |
| MAJOR context through the Codex queue | Codex keeps its own context natively (§2.2) |
| MAJOR cancel admission and pre-task cancel | §2.3 Interrupt admitted while the row is open; the cancel is stored on the row |
| (round 1) redaction | the threat model states that a shared session shares the conversation (§2.1) |
| (round 1) delayed capture | §2.3 the capture wait before the run |
| (round 1) restart and local socket | §2.1 |

## 6. Review map (revision 4 → revision 5)

| Finding | Answer |
| --- | --- |
| BLOCKER revert protection stops at the session | §2.0.1 folder barrier at admission, dispatch, recovery and retry; Codex ranges with a shell turn refused |
| MAJOR a shell task stalls the effect worker | §2.0.2 every input of the root refused at decide while a Claude row runs; cancel from the row |
| MAJOR restart admission | §2.0.3 submit counters, claims, restart check, shutdown settlement |
| MAJOR cancelled and timed-out results never reach the model | §2.0.4 every settled result attachable with its end state |
| MAJOR one result can be too large | §2.0.5 per-result byte budget after escaping at the station |
| MAJOR a Codex shell turn has no start checkpoint | §2.0.1 the shell-turn revert claim is dropped; ranges with a shell turn are refused |
| UNVERIFIED Claude transport | §2.0.6 the named path through `input_effect`/`resume_hibernated`, `host_input`, `HostInput`, `ClaudeCore::prompt`, replay |
| UNVERIFIED the guard misses the shell turn itself | §2.0.1 a range that contains the shell turn is refused |
| MINOR cancel guarantee | §2.0.7 per provider |

## 7. Review map (revision 5 → revision 6)

| Finding | Answer |
| --- | --- |
| BLOCKER an admission-time `started_at` can allow a revert of shell writes | §2.00.1 a durable per-root generation advanced under the fence just before the first write; checkpoints store their generation; restores across a newer generation are refused at admission, dispatch, recovery and retry |
| MAJOR a permitted revert blocks the worker on the shell fence | §2.00.2 `Revert` and `RetryRestore` refused while any shell row of the root is unsettled |
| MAJOR drain does not wait for a Claude shell | §2.00.3 unsettled shell rows in the drain snapshot; shutdown cancels before the worker join and the shim detach |

## 8. Review map (revision 6 → revision 7)

| Finding | Answer |
| --- | --- |
| BLOCKER a checkpoint during a native Codex shell shares its generation | §2.000.1 the root-wide refusal covers Codex rows until the shell turn settles; §2.000.2 the generation advances again at the settlement |
| MAJOR a queued shell can start after the shutdown cancel sweep | §2.000.3 a durable cancel on every unsettled row before the sweep; checked at registration and before the spawn |

## 9. Review map (revision 7 → revision 8)

| Finding | Answer |
| --- | --- |
| BLOCKER a lost session settles a Codex shell while the command still runs | §2.0000.1 settlement only on provider-confirmed completion or DL-182 proof; otherwise `awaiting_proof` keeps the barrier |
| MAJOR the one-time cancel does not close admission | §2.0000.2 a durable `shell_admission` flag closed before the sweep and read in the decide transaction |
| MAJOR a refused Codex shell has no settlement | §2.0000.3 refusal, error response and proven non-delivery settle `failed`; no response in 30 s → `awaiting_proof`, released by Stop or reconciliation |

## 10. Review map (revision 8 → revision 9)

| Finding | Answer |
| --- | --- |
| BLOCKER a direct-host Codex shell can lock the root for ever | §2.00000 an owner-only, audited `session.release_shell` for an `awaiting_proof` row, in any session state and on every host; the generation advances under the fence before the root opens |
| (note) `turn/aborted` is not handled | §2.0000.1 now names `turn/completed` (any status) only |

## 11. Review map (revision 9 → revision 10)

| Finding | Answer |
| --- | --- |
| BLOCKER a queued shell row survives recovery outside the release state | §2.000000.2 start-up reconciliation of every unsettled row by its effect state before admission; §2.000000.3 release also accepts a non-terminal row of a lost or exited session |
| MAJOR no durable "generation advance pending" state | §2.000000.1 `advance_pending` set in the settlement transaction, counted as unsettled, cleared with the increment under the fence, completed at start before admission |
