# Fleet Deck v2 packaging

Run `fleetdeck service install` (deck and local station), `fleetdeck service install deck` or `fleetdeck service install station`. It writes and starts the service for this platform (docs/v2/operations.md §4.1). The files below are for a manual setup.

This directory holds the service units and the container file of Fleet Deck v2 (PLAN Phase 7 "Packaging", DL-400). The release archives of `cargo-dist` (`dist-workspace.toml`) include these files. `make package-check` runs their static checks and the offline smoke test of the built binary (`crates/fleetdeck-cli/tests/packaging.rs`).

## Install the binary

- Shell installer (from a GitHub release): `curl --proto '=https' --tlsv1.2 -LsSf <release URL>/fleetdeck-cli-installer.sh | sh`. It puts `fleetdeck` into `~/.cargo/bin`.
- Homebrew: `brew install lacion/tap/fleetdeck`.
- From source: `bun install --cwd web --frozen-lockfile && bun run --cwd web build && cargo install --locked --path crates/fleetdeck-cli`.

The units below start `~/.cargo/bin/fleetdeck`. If you install it in a different directory, edit `ExecStart` (systemd) or the `ProgramArguments` string (launchd).

## Linux: systemd user units

1. Copy the units: `mkdir -p ~/.config/systemd/user && cp packaging/systemd/fleetdeck-*.service ~/.config/systemd/user/`.
2. Reload: `systemctl --user daemon-reload`.
3. Start the station: `systemctl --user enable --now fleetdeck-station`.
4. On the deck machine, start the deck: `systemctl --user enable --now fleetdeck-deck`.
5. Keep the services after logout: `loginctl enable-linger "$USER"`.

Logs: `journalctl --user -u fleetdeck-station` (JSON lines).

The station unit uses `KillMode=process`. A stop, a restart or an upgrade sends SIGTERM to the station only. The station detaches, and the provider shims and agents keep running (ADR-005 §7). Do not remove this line: the systemd default kills every agent of the unit.

## macOS: launchd user agents

1. Copy the agents: `cp packaging/launchd/dev.fleetdeck.*.plist ~/Library/LaunchAgents/`.
2. Load the station: `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.fleetdeck.station.plist`.
3. On the deck machine, load the deck: `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.fleetdeck.deck.plist`.

Logs: `~/Library/Logs/fleetdeck/station.log` and `deck.log`. The station agent sets `AbandonProcessGroup`, so a stop ends the station only (ADR-005 §7).

## Container: station image

`packaging/container/Containerfile` builds a station image from the repository root:

```sh
docker build -f packaging/container/Containerfile -t fleetdeck-station .
docker run -d --name fleetdeck-station -v fleetdeck-data:/data fleetdeck-station
```

- The image runs `fleetdeck station serve` as the non-root user `fleetdeck` (UID 10001), with `tini` as PID 1.
- The volume `/data` holds `station.toml`, the identity key and the database (`FLEETDECK_DATA_DIR=/data`).
- The image has no provider CLI. Add `claude` or `codex` in a derived image and sign them in as the `fleetdeck` user.
- A container stop ends every agent of the container, as a reboot does.

> **Caution:** a paired deck can run code as the station's OS user (PLAN §6.8). A container or a dedicated OS user limits what an agent can reach.

## Owner steps

These steps need tools or hosts that the build machine of M3 does not have (DL-400):

- Run `dist init` / `dist generate` with network access to write `.github/workflows/release.yml` from `dist-workspace.toml`, and create the tap repository `lacion/homebrew-tap`.
- Build the container image (no container runtime on the build machine).
- Install the launchd agents on a macOS host.
