# Fleet Deck v2 operations guide

This guide tells the owner how to configure, run, pair, back up, drain, upgrade and repair a Fleet Deck v2 deck and its stations. For the install and the service units, see [packaging/README.md](../../packaging/README.md). For the move from v1, see [cutover.md](./cutover.md).

## 1. Directories

| What | Default | Override |
| --- | --- | --- |
| Configuration (`station.toml`, `deck.toml`) | `~/.config/fleetdeck/` (`$XDG_CONFIG_HOME/fleetdeck`) | `--data-dir` or `FLEETDECK_DATA_DIR` (then configuration and data share one directory) |
| Station data: identity key, `station.db`, local socket, shim sockets, `upgrades/`, `bin/` | `~/.local/share/fleetdeck/station/` | same |
| Deck data: identity key, `deck.db`, owner socket | `~/.local/share/fleetdeck/deck/` | same |

The data directories have mode 0700. The configuration files must have mode 0600 and must not be group- or world-writable; the binary refuses a file that is.

## 2. Station configuration (`station.toml`)

A missing file gives the defaults. An unknown key is an error.

| Key | Default | Meaning |
| --- | --- | --- |
| `name` | the host name | The station name on the board. |
| `allowed_roots` | `["~"]` (the home directory) | The directories where sessions, worktrees and clones may run. An explicit list must have at least one entry. |
| `max_sessions` | `12` | The most sessions with a live process. A start or resume above the limit is refused. |
| `allow_setup_cmd` | `false` | Allow a session start to run the owner's setup command in its workspace (DL-148). |
| `allow_shell` / `max_shells` | `false` / `8` | The human shell (PLAN §6.8). |
| `offline_approval_policy` | `wait` | What happens to an approval when no deck is reachable: `wait`, or `deny_after(<n>s)` (also `m` or `h`). |
| `listen` | (empty) | Link listen addresses when the deck dials this station. |
| `host` | `direct` | `direct`: the agents are children of the station and stop with it. `shim`: each agent runs under a `fleetdeck shim` process and survives a station restart or upgrade (ADR-005). |
| `shim_buffer_mib` | `16` | The shim output buffer, 1–256 MiB. |
| `repo_groups` | (empty) | Repository path → group name, for the conflict radar. |
| `[[trackers]]` | (none) | A Jira or Linear tracker for the board's issue views (R3, DL-164). See "Trackers" below. |
| `[[provider_instance]]` | `claude` and `codex` | `id`, `driver` (`claude` or `codex`), `binary`, `config_home`, `env` (values are secrets; they never leave the station). |

Example:

```toml
name = "laptop"
allowed_roots = ["~/code"]
max_sessions = 8
offline_approval_policy = "deny_after(10m)"
host = "shim"

[[provider_instance]]
id = "claude"
driver = "claude"

[[provider_instance]]
id = "codex-work"
driver = "codex"
config_home = "~/.codex-work"
```

### Trackers (`[[trackers]]`)

GitHub and GitLab need no entry. The station uses the signed-in `gh` and `glab` of its own user (run `gh auth login` and `glab auth login` as that user). Jira Cloud and Linear need one `[[trackers]]` entry each:

| Key | Meaning |
| --- | --- |
| `id` | A short name (1-32 of `a-z`, `0-9`, `-`, `_`). A project binding on the board names this id. |
| `kind` | `jira` or `linear`. |
| `site` | Jira only: `https://acme.atlassian.net`, without a path. |
| `email` | Jira only: the account e-mail that owns the API token. |
| `token_env` | The name of an environment variable of the station process that holds the API token. |
| `token_file` | A file that holds the API token. It must have mode 0600. |

Give exactly one of `token_env` and `token_file`. The token is read for each request and is never stored, logged, sent to the deck or put in the argument list or the environment of a child process (the station sends it to `curl` on stdin). A Jira API token comes from the Atlassian account page; a Linear personal API key comes from the Linear settings (API).

```toml
[[trackers]]
id = "acme"
kind = "jira"
site = "https://acme.atlassian.net"
email = "ana@acme.test"
token_env = "JIRA_API_TOKEN"

[[trackers]]
id = "lin"
kind = "linear"
token_file = "~/.config/fleetdeck/linear.token"
```

Check the setup from the board (project preflight) or with the `integrations.status` link method. `gh`, `glab` and `curl` must be on the `PATH` of the station.

## 3. Deck configuration (`deck.toml`)

| Key | Default | Meaning |
| --- | --- | --- |
| `name` | the host name | The deck name. |
| `listen` | `127.0.0.1:7420` | The board HTTP and WebSocket addresses. Login is always required. |
| `link_listen` | `127.0.0.1:7421` | The link listener for stations that dial the deck. |
| `tailnet_hostname` | (none) | The tailnet DNS name of the deck, for the WebSocket `Origin` check. |
| `conflict_mail` | `true` | When two sessions change the same file within 30 minutes, the deck sends each session one follow-up mail about it (DL-145). Set `false` to turn this off. |

> **Caution:** use only loopback, private LAN or tailnet addresses. The deck refuses a public address. Do not forward a port from a router.

## 4. Start and stop

- One machine: `fleetdeck up` runs the deck and a local station in one process.
- More machines: `fleetdeck deck` on the deck machine, `fleetdeck station` on each station machine.
- As services: use the systemd user units or the launchd agents in `packaging/`. The station unit uses `KillMode=process` (launchd: `AbandonProcessGroup`). Keep this setting: it lets shim-hosted agents survive a station stop.
- Logs go to standard error. `--log-format json` gives JSON lines; `FLEETDECK_LOG=debug` gives more detail.

A station stop with `host = "direct"` stops every agent; the sessions become `lost` at the next start, or are continued when auto-continue is on and every recheck passes. With `host = "shim"`, the agents keep running and the next station start reattaches them.

## 5. Board access

1. Set the owner password: `fleetdeck deck password` (from a prompt or `--stdin`). A new password ends every login.
2. Get a one-time login URL: `fleetdeck open`.
3. For scripts, make an API token: `fleetdeck deck token`. A token has a scope; keep it secret.
4. Read the audit log: `fleetdeck deck audit`.

## 6. Pair a station

Station dials the deck (the station is behind NAT or on a laptop):

1. On the deck: `fleetdeck deck invite`. The invitation is single-use and a secret.
2. On the station: `fleetdeck station join`. Paste the invitation at the prompt, or give `--stdin` or `--invite-file <file>` (mode 0600). Never put an invitation on the command line.

Deck dials the station (the station has a fixed private address):

1. On the station: set `listen` in `station.toml`, then `fleetdeck station invite`.
2. On the deck: `fleetdeck deck add-station`.

To unpair: `fleetdeck deck remove-station <station>` on the deck, or `fleetdeck station forget-deck` on the station.

## 7. Work without the deck

The station keeps working when the deck is down. On the station machine:

- `fleetdeck station sessions [--json]` lists the sessions.
- `fleetdeck station run --provider claude --cwd <dir> "<prompt>"` starts a session.
- `fleetdeck station approve <request> [--option <id>]` answers an approval.
- `fleetdeck station interrupt <session>` interrupts a turn.

Approvals that wait for the deck follow `offline_approval_policy`.

### 7.1 Import a CLI session

You can continue a Claude Code or Codex conversation that you started in a terminal as a driven v2 session:

1. End the terminal CLI. One native session must have only one driver.
2. On the board, open "New session", choose the station, the provider instance and the directory of that conversation.
   > **Caution:** Do not import a native session id of an earlier Fleet Deck session that is `lost` with a shim host, until the timeline of that session shows its exit. The station refuses only an id that another session drives now. It does not check a lost shim process, so that conversation can run twice.

3. Put the native session id into "Import CLI session id (optional)". The id has 1 to 128 letters, digits, `-`, `_` or `.`, and starts with a letter or a digit.
4. Optionally give a first prompt, then select "Import session".

The station resumes the native session with the permission mode that you select in the spawn form. A provider instance stores no permission mode. The import ignores the workspace and batch choices of the form. It refuses an id that another session of the station already drives. Fleet Deck does not read the terminal and does not import v1 data.

### 7.2 Start a coordinator session

A coordinator session can read the fleet, send mail and drive the sessions that it starts (ADR-006). The station keeps coordinator calls in a durable outbox (DL-218).

**Current limit:** the station gives the grant credential only to a provider version that the opt-in coordinator live smoke test (ADR-006 §8.5) verified (`fleetdeck_providers::coordinator::VERIFIED` in `crates/fleetdeck-providers/src/coordinator.rs`, DL-219). Claude 2.1.296 and later is verified. A Codex coordinator start fails with `handoff_unverified`, and so does a start on a station where any Claude instance has an older or unknown version.

> **Caution:** A coordinator drives other sessions without a prompt from you. Revoke its grant when the work is done.

1. On the board, open "New session" and choose a station, a provider instance and a directory.
2. Select "Coordinator". The import field must be empty: an imported session cannot be a coordinator.
3. Give the first prompt and start the session.
4. Find the grant in the "Coordinator grants" list of the "Mail and tasks" panel on the fleet screen.
5. To stop the coordinator calls, select "Revoke" on its grant. A grant that expired after an idle time shows "Renew".

The deck creates the grant with the classes `read`, `mail` and `drive` and with its default expiry. It gives the session the control skill (`crates/fleetdeck-deck/src/control_skill.md`). The start fails for a resumed session, for a shim-hosted session and for a station without the coordinator endpoint (§11).

## 8. Back up

- The station event log (`station.db`) is the source of truth for sessions. The deck database (`deck.db`) is the source of truth for mail, plans, tasks, routing and settings.
- Stop the node, then copy the database file with its `-wal` and `-shm` files. A copy of a running database without its WAL file can lose recent events.
- The identity keys are in the same directories. A lost station key needs a new pairing.

## 9. Drain and upgrade a station

> **Caution:** do not copy a new binary over the running one. Use the steps below; they keep a snapshot and a rollback path.

1. Drain the station: `fleetdeck station drain [--timeout-secs 600]`. The station refuses new work (start, resume, import, fork, prompt, follow-up, steer, revert) and waits until no session has an open turn. Answers, interrupts and stops still work. The command fails and names the busy sessions when the time ends.
2. Stop the station service.
3. Prepare the update: `fleetdeck station upgrade prepare --binary <new fleetdeck> --to-version <version>`. This step:
   - takes a snapshot of `station.db` with its WAL and SHM files,
   - runs the new binary's trial on a separate copy (migrations, shim protocol versions),
   - installs the release under `<station>/bin/releases/`,
   - prints the update id (`upd-...`).
   A failed trial leaves the old build active.
4. Activate it: `fleetdeck station upgrade activate <update id>`. The stable launcher `<station>/bin/current` now points at the new release. Make the service start `<station>/bin/current` (edit `ExecStart` or `ProgramArguments` once).
5. Start the station service.
6. If the new build fails, stop the service and run `fleetdeck station upgrade rollback <update id>`:
   - When the new station never opened the live database and no shim is live, the rollback restores the snapshot. The restored database gets a new incarnation, so the deck reloads the station's state.
   - Otherwise the rollback restores only the previous binary and keeps the live database. If the old build cannot open the migrated database, it says so; use the new build and report the defect.

Shim-hosted agents keep running through the whole sequence; the new station reattaches them when it speaks their shim protocol. An upgrade that cannot speak a running shim's protocol fails its trial.

## 10. Auto-continue

Auto-continue is off for each session by default. Turn it on in the session view ("Continue after a station restart"). Only the owner on the board can turn it on; a coordinator or the local socket cannot. After a station restart, the station continues a lost session only when every rule of ADR-007 holds. Otherwise the session stays `lost`, and the timeline shows "Auto-continue skipped: &lt;reason&gt;".

Limits of this release (DL-127, follow-ups in `docs/v2/progress/m3-durable.md`):

- Auto-continue works only with `host = "direct"`. With `host = "shim"`, the station skips a lost session with the reason `unsupported`.
- The station checks every ADR-007 rule when it stores the continuation. Right before the dispatch, it checks again only these rules: identity, proven end of the old process, newer owner work, requested stop and the time window. It does not check again the preference, the turn state, the capability, the workspace or a free session slot.

## 11. Troubleshooting

Run `fleetdeck doctor` first. It changes nothing. It prints one line for each check (`ok`, `warn` or `fail`) and then a summary, and its exit code is 1 when a check fails. It checks the data directory and its mode, `station.toml` and `deck.toml` (parse and mode), `git`, the `--version` of each provider binary, `allow_shell`, the deck local socket, the board listener, and the v1 Fleet Deck Claude plugin. Remove the v1 plugin after the cutover: its hooks slow every stop of a v2 session.

| Symptom | What to do |
| --- | --- |
| A session is `lost` | The station restarted and could not prove what happened to the agent. Read the detail in the timeline. Resume the session if the provider supports resume. |
| A command is `outcome_unknown` | The station cannot prove whether the provider received it. It never resends it. Check the session and send a new command if needed. |
| A revert needs reconciliation | The station could not prove the state of the files or the conversation. Read the revert card and resolve it; input stays blocked until then. |
| The station is not on the board | Check the link: the station log, the `listen`/`link_listen` addresses, and the pairing. `fleetdeck deck stations` shows the link state. |
| Usage shows "unknown" | The provider did not report usage for that window. Fleet Deck never shows unknown usage as zero. |
| The board refuses a WebSocket | Set `tailnet_hostname` when you open the board by its tailnet name. |
| A link call fails with `feature_not_negotiated` | The deck and the station do not share the feature of that method (for example `coordinator.v1` for coordinator sessions). Upgrade the older side; an N−1 peer keeps the shared features. |
| A coordinator tool call fails `deck_unreachable` | The station has not linked to a deck since it started, so it knows no deck to send to. Check the link (row "The station is not on the board"). After the first link, the station keeps coordinator calls in its durable outbox and sends them when the deck is back, for up to 15 minutes (DL-218). |
| A coordinator session does not start | The start fails with its reason. `a coordinator session starts only as a new direct child`: start a new session with `host = "direct"`; a resume or a shim-hosted start cannot carry the grant credential. `the coordinator endpoint does not listen`: check the station coordinator MCP ingress. `handoff_unverified`: the provider version has no verified credential handoff. Claude 2.1.296 and later is verified; Codex is not, so a Codex coordinator start fails with this reason (§7.2; DL-215, DL-219). |

## 12. Owner steps that this repository cannot test

- A container image build (`docker build`) and a run of the image.
- A real tailnet, a macOS host (launchd) and a physical phone.
- The release download and signature check (cargo-dist).
- `dist generate` and the commit of its release workflow, and the Homebrew tap repository (7-5, `packaging/README.md`).
- The approval round trip from the board to an agent across a real LAN between two hosts; `make perf` measures loopback only (7-4).
- The live smoke tests against the real `claude` and `codex` CLIs: `make smoke-live` (opt-in, network, a small number of tokens; [LIVE-SMOKE.md](./LIVE-SMOKE.md)).
- A real GitHub Actions run of `.github/workflows/v2.yml` (it runs `make check`; this repository never pushes).
- The coordinator live smoke of ADR-006 §8.5 for Codex: until it records a verified context handoff, a Codex coordinator start fails with `handoff_unverified` (§7.2). Run it again for each new Claude version.
