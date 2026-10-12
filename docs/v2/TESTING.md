# Try Fleet Deck v2 on your machines

This guide is for the owner's first hands-on tests of the `v2` branch. It
uses the default directories of your user. For a throwaway run, add
`--data-dir <dir>` to every `fleetdeck` command (or set
`FLEETDECK_DATA_DIR`).

> **Caution:** Fleet Deck v1 (the npm package, port 4711) and v2 use
> different ports and directories, so both can run at the same time. Do not
> point v2 at the data of v1.

## 1. Build

You need Rust stable, `bun` and `git`. Sign in to `claude` and/or `codex`
first: the station uses the CLIs and their logins as they are.

```sh
git clone https://github.com/lacion/fleet-deck && cd fleet-deck
git switch v2
bun install --cwd web --frozen-lockfile
bun run --cwd web build          # the board files; the binary embeds them
cargo build --release -p fleetdeck-cli
export PATH="$PWD/target/release:$PATH"
fleetdeck --version
```

Build the board before the binary. A binary built without `web/dist`
shows "This deck has no board files" in the browser.

## 2. One machine

1. Start the deck and a local station in one process:

   ```sh
   fleetdeck up
   ```

   It prints `board at https://127.0.0.1:7420`. Keep it running.
2. In a second terminal, get a login link:

   ```sh
   fleetdeck open
   ```

   It prints the SHA-256 fingerprint of the self-signed board certificate
   and a single-use link that is valid for 5 minutes.
3. Open the link. The browser warns once about the certificate. Accept it
   only if the fingerprint matches.
4. Optional: set an owner password for later logins: `fleetdeck deck password`.

With no `station.toml`, the station may run sessions anywhere in your home
directory and offers the `claude` and `codex` instances. Configuration
files are in `~/.config/fleetdeck/` and must have mode 0600.

## 3. What to try on the board

| Flow | Steps |
| --- | --- |
| Spawn | **New session** → station, provider, browse to a directory, optional mode, model and first prompt → **Start session**. |
| Approval | Ask the agent to run a shell command. The request shows in the **Needs you** rail and in the session. Answer it there. |
| Steer and follow-up | While a turn runs, type a message and use **Steer now** or **Queue follow-up**. |
| Interrupt | **Interrupt** during a turn. The timeline shows the result. |
| Mode and model | The mode ladder and the model field in the session view. Only the controls that the provider supports show. |
| Subagents | Ask Claude to use a subagent. The session shows the subagent tree; the card shows the running-subagent count. |
| Deck restart | Stop `fleetdeck up` (Ctrl-C) and start it again. The board shows stale data, then reconnects without a new login. |
| Work without the deck | `fleetdeck station sessions`, `fleetdeck station run --provider claude --cwd <dir> "<prompt>"`, `fleetdeck station approve <request>`. These need only the station. |

## 4. Two machines

Machine A runs the deck (and its own station). Machine B runs a station
only. The machines must reach each other on a private LAN or on a tailnet.

> **Caution:** use only loopback, private LAN (10/8, 172.16/12, 192.168/16)
> or tailnet (100.64/10) addresses. The deck refuses a public address. Do
> not forward a port from a router.

### Machine A (deck)

1. Write `~/.config/fleetdeck/deck.toml` (mode 0600). Use the LAN or
   tailnet address of machine A:

   ```toml
   listen = ["127.0.0.1:7420", "192.168.1.10:7420"]   # board
   link_listen = ["192.168.1.10:7421"]                 # stations dial here
   # tailnet_hostname = "deck.tailnet-name.ts.net"    # if you open the board by that name
   ```

2. `chmod 600 ~/.config/fleetdeck/deck.toml`, then `fleetdeck up`.
3. In a second terminal: `fleetdeck deck invite`. Copy the invitation. It
   is single-use and secret.

### Machine B (station)

1. Optional: write `~/.config/fleetdeck/station.toml` (mode 0600), for
   example `name = "laptop"` and `allowed_roots = ["~/code"]`.
2. Start the station: `fleetdeck station serve`. Keep it running.
3. In a second terminal: `fleetdeck station join`, and paste the
   invitation at the prompt. Do not put it on the command line.
4. The station dials the deck. On the board, **Stations** shows it as
   **Link up** with its version, capacity and providers.

The pairing stays after restarts. To pair again: `fleetdeck station
forget-deck` on B, or `fleetdeck deck remove-station <name>` on A.

### Open the board from a phone or the other machine

Open `https://192.168.1.10:7420` (or the tailnet name). Run
`fleetdeck open` on machine A for a login link and change its host to the
address that you open, or log in with the owner password.

## 5. Optional checks

- Full test suite (offline, about 10 minutes): `make check`.
- Live smoke tests against the real CLIs (network, a few tokens):
  `make smoke-live`. See [LIVE-SMOKE.md](./LIVE-SMOKE.md).
- Logs: `FLEETDECK_LOG=debug fleetdeck up`.

## 6. Report problems

Note the time, the station, the session and what you did. Attach
`fleetdeck deck audit` output and the terminal log. Never paste an
invitation, a login link, a token or a password into a report.

More detail: the [operations guide](./operations.md).
