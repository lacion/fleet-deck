# M2 owner demo checklist (MVP)

Status: **NOT YET PERFORMED.** The owner has not run any step of this checklist. No step has a result.

This checklist is the owner part of the M2 gate ([GATE-M2.md](./GATE-M2.md) §7). It tests the PLAN Phase 4 exit on real hardware: from a phone, the owner spawns sessions on two different machines, answers approvals, steers and interrupts them, and the demo survives a deck restart and a network drop.

The automated M2 evidence ran only on one machine, on loopback, with Playwright at 390×844. This demo is the first test with two machines, a real phone, and a real private network.

## How to use this checklist

1. Do the steps in sequence.
2. Write the result in the "Owner result" column: `pass`, `fail`, or `skipped`, and a short note.
3. For each `fail`, keep the log lines of the deck and the station (`FLEETDECK_LOG=debug` gives more detail).
4. Give the completed file to the architect. The architect records each failure as a defect for M3.

Names in this checklist:

- **Machine A**: the deck machine. It runs `fleetdeck up` (the deck and a local station in one process).
- **Machine B**: the second machine. It runs `fleetdeck station serve`.
- **Phone**: the owner's phone, on the same private LAN or on the same tailnet.
- `<A-addr>`: the private LAN address or the tailnet address of machine A (for example `192.168.1.20` or `100.64.0.5`).
- `<B-addr>`: the private LAN address or the tailnet address of machine B.

All commands come from the repository and the `fleetdeck --help` output at `548d947`. The binary path is `target/release/fleetdeck`. Use the same `--data-dir` (or `FLEETDECK_DATA_DIR`) for every command on one machine if you do not use the default directories.

## 1. Prerequisites

| # | Step | Expected result | Owner result |
|---|---|---|---|
| 1.1 | Get Linux or macOS on machines A and B (A11). Install Rust (stable), `bun`, and `git`. | The `cargo --version`, `bun --version`, and `git --version` commands each print a version. | |
| 1.2 | On each machine, clone the repository and check out the `v2` branch at `548d947` or later. | `git log -1 --oneline` shows the expected commit. | |
| 1.3 | Optional: on machine A, run `make check`. | The last line is `check: all 14 commands passed`. | |

> **CAUTION — cost.** Step 1.4 and every step from section 7 on use real provider accounts. A login and each session or turn can use paid quota or tokens. Use short prompts and a test repository.

| # | Step | Expected result | Owner result |
|---|---|---|---|
| 1.4 | On each machine that runs real sessions, install and log in to the provider CLIs (`claude`, `codex`). Use the versions in [LIVE-SMOKE.md](./LIVE-SMOKE.md) (Claude 2.1.289, Codex 0.160.0) or later. | `claude --version` and `codex --version` print a version. Each CLI can start a session in a terminal. | |

## 2. Build

| # | Step | Expected result | Owner result |
|---|---|---|---|
| 2.1 | On each machine, install the web dependencies: `bun install --cwd web --frozen-lockfile`. | The command completes without an error. | |
| 2.2 | Build the board: `bun run --cwd web build`. The deck embeds `web/dist/` in the binary. | `web/dist/index.html` exists. | |
| 2.3 | Build the binary: `cargo build --release -p fleetdeck-cli`. | `target/release/fleetdeck --version` prints a version. | |

## 3. Configure the two machines

> **CAUTION — network exposure (D13).** Use only loopback, private LAN, or tailnet addresses. Never use a public address and never forward a port from a router. The deck refuses a public listen address; do not try to go around this check.

| # | Step | Expected result | Owner result |
|---|---|---|---|
| 3.1 | On machine A, write `~/.config/fleetdeck/deck.toml` (mode 0600; the file must not be group- or world-writable) with: `listen = ["<A-addr>:7420"]` and `link_listen = ["<A-addr>:7421"]`. For a tailnet, also add `tailnet_hostname = "<A tailnet DNS name>"`. | The file exists with mode 0600. | |
| 3.2 | On machine A, write `~/.config/fleetdeck/station.toml` (mode 0600) with `name = "machine-a"` and `allowed_roots` set to one test directory. Keep `allow_shell` and the provider instances at their defaults. | The file exists with mode 0600. | |
| 3.3 | On machine B, write `~/.config/fleetdeck/station.toml` (mode 0600) with `name = "machine-b"` and `allowed_roots` set to one test directory. | The file exists with mode 0600. | |
| 3.4 | On each machine, make a small Git test repository inside `allowed_roots`. Do not use a repository with important work. | `git status` in the test repository works. | |

## 4. Start the deck on machine A

| # | Step | Expected result | Owner result |
|---|---|---|---|
| 4.1 | On machine A, run `target/release/fleetdeck up`. Keep the terminal open. | The deck starts. At the first start, the local station pairs with the deck through the deck's link listener (`link_listen` of step 3.1, here `<A-addr>:7421`). The invitation of this pairing stays inside the process. | |
| 4.2 | On machine A, in a second terminal, set the owner password: `target/release/fleetdeck deck password`. Type the password at the prompt. Do not put the password in a command argument. | The command confirms that the password is set. | |
| 4.3 | On machine A, run `target/release/fleetdeck open`. | The command prints a one-time login URL and the SHA-256 fingerprint of the board certificate. | |

## 5. Pair machine B as a station

Use option A (the station dials the deck) or option B (the deck dials the station). Do not put an invitation in a command argument. Use the prompt, standard input, or a 0600 file. An invitation is single-use and is valid for 15 minutes.

| # | Step | Expected result | Owner result |
|---|---|---|---|
| 5.1 | On machine B, run `target/release/fleetdeck station serve`. Keep the terminal open. | The station starts. It has no deck yet. | |
| 5.2 | Option A: on machine A, run `target/release/fleetdeck deck invite`. | The command prints a single-use invitation. | |
| 5.3 | Option A: move the invitation to machine B in a private way. On machine B, run `target/release/fleetdeck station join` and paste the invitation at the prompt (or use `--stdin`, or `--invite-file <file>` with a 0600 file). Delete the file after use. | The station pairs with the deck. | |
| 5.4 | Option B (instead of 5.2–5.3): on machine B, run `target/release/fleetdeck station invite --listen <B-addr>:<port>`. On machine A, run `target/release/fleetdeck deck add-station` and paste the invitation at the prompt (or use `--stdin` or `--invite-file`). | The deck prints `paired with station machine-b (...)`; the deck dials it now. | |
| 5.5 | On machine A, run `target/release/fleetdeck deck stations`. | The list shows `machine-a` and `machine-b`, both with the link up. | |

## 6. Log in from the phone

> **CAUTION — network exposure (D13).** Connect the phone only to the private LAN or the tailnet. Do not open the board on a public network path.

| # | Step | Expected result | Owner result |
|---|---|---|---|
| 6.1 | On the phone, open `https://<A-addr>:7420/` (or the tailnet name). The board uses a self-signed certificate if you did not give `board.crt`/`board.key`. Compare the certificate SHA-256 with the output of step 4.3 before you accept it. | The fingerprints are the same. The login screen shows. | |
| 6.2 | Log in with the owner password. As an alternative, open the one-time URL of step 4.3 (it works once and expires after 5 minutes). | The fleet board shows. The stations panel shows `machine-a` and `machine-b` with the link up. | |
| 6.3 | On the phone, reload the page. | The login stays valid. The board shows the same data. | |

## 7. Spawn sessions

> **CAUTION — cost.** The steps from here on use real provider accounts. Each session and each turn can use paid quota or tokens. Use short prompts and a test repository. Stop each session at the end of the demo.

| # | Step | Expected result | Owner result |
|---|---|---|---|
| 7.1 | On the phone, open "New session". Select station `machine-a`, provider `claude`, and the test directory. Send a short prompt. | A session card shows on the board. The timeline shows the turn and the answer of Claude. | |
| 7.2 | Spawn a second session: station `machine-b`, provider `codex`, the test directory, a short prompt. | A second card shows, on `machine-b`. The timeline shows the Codex turn. | |
| 7.3 | Spawn a third session: station `machine-b`, provider `claude`. | The Claude session runs on machine B. The board shows sessions on two different machines. | |

## 8. Approvals and questions

| # | Step | Expected result | Owner result |
|---|---|---|---|
| 8.1 | In a Claude session with a permission mode that asks, tell the agent to run a shell command that changes a file (for example, create `demo.txt`). | An approval shows in the "Needs you" rail and in the session view. | |
| 8.2 | Answer the approval with "Allow once" on the phone. | The agent continues. The timeline shows the tool call and its result. The approval leaves the rail. | |
| 8.3 | Make a second approval and answer "Deny". | The agent does not run the command. The timeline shows the denial. | |
| 8.4 | Tell an agent to ask you a question with options before it continues (Claude `AskUserQuestion` or a Codex question). | The question shows in the "Needs you" rail with its options. | |
| 8.5 | Answer the question on the phone ("Send answer"). | The agent receives the answer and continues. A second answer to the same question is not possible. | |

## 9. Steer and interrupt

| # | Step | Expected result | Owner result |
|---|---|---|---|
| 9.1 | Give a session a long task. While the turn runs, type a message in the composer and use "Steer now". | The message goes into the running turn. The agent changes its work. | |
| 9.2 | While a turn runs, type a message and queue it as a follow-up. | The message shows as "Queued". It starts after the current turn ends. | |
| 9.3 | While a turn runs, use "Interrupt". | The turn stops. The card leaves the working state. The timeline shows that the turn was interrupted. | |

## 10. Subagent tree

| # | Step | Expected result | Owner result |
|---|---|---|---|
| 10.1 | Tell a Claude session to use a subagent for a small task (for example, "use a subagent to list the files"). | The session view shows a subagent tree. The card shows the count of running subagents. | |
| 10.2 | Open the subagent in the tree. | The child has its own timeline and status (running, done, or failed). | |
| 10.3 | Wait for the subagent to end. | The child shows done (or failed). The count of running subagents goes to zero. | |

## 11. Network cut

| # | Step | Expected result | Owner result |
|---|---|---|---|
| 11.1 | Start a long turn on machine B. Then disconnect machine B from the network (or stop the tailnet on machine B) for about 2 minutes. | The board shows that the link to `machine-b` is down. The cards of `machine-b` stay readable and show stale data. | |
| 11.2 | On machine B, run `target/release/fleetdeck station sessions`. | The session continues on machine B while the network is down (A10: a network failure never stops a session). | |
| 11.3 | Connect machine B to the network again. | The link comes back without a new pairing. The board receives the events of the down time. No event shows twice. | |
| 11.4 | Disconnect the phone from the network for about 1 minute, then connect it again. | The board reconnects without a new login and shows the current data. | |

## 12. Deck restart

`fleetdeck up` runs the deck and the local station of machine A in one process. A restart of `up` also restarts the station of machine A. A10 applies to the sessions of machine A: a station restart can stop them; recovery uses the native resume of the provider and can lose in-flight work and open approvals.

| # | Step | Expected result | Owner result |
|---|---|---|---|
| 12.1 | Start a long turn on machine B. On machine A, stop `fleetdeck up` (Ctrl-C). | The phone shows that the deck is not connected. The session on machine B continues. | |
| 12.2 | On machine A, start `target/release/fleetdeck up` again. | The deck starts with the same pairings. `machine-b` links again without a new pairing. | |
| 12.3 | On the phone, look at the board. | The board reconnects. The session on machine B shows the events of the down time with no duplicates. | |
| 12.4 | Look at the sessions of machine A. | Each session shows an explicit state: resumed, lost, or not resumable. The board never shows "reattached" for these sessions in the MVP. Open approvals of a dead process show as expired. | |
| 12.5 | Interrupt or end every demo session. | No session stays in the working state. | |

## 13. Clean up

| # | Step | Expected result | Owner result |
|---|---|---|---|
| 13.1 | Stop `fleetdeck station serve` on machine B and `fleetdeck up` on machine A. | Both processes end. | |
| 13.2 | Delete every invitation file that you made. | No invitation file stays on disk. | |
| 13.3 | Record the provider CLI versions, the operating systems, and the network type (LAN or tailnet) at the top of this file. | The architect can compare the result with the automated evidence. | |
