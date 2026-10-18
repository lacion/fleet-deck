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

Only you may write the file (no group or other write bit). When a `[[provider_instance]]` has a plain `env` value (for example an API key), only you may read the file too: run `chmod 600 station.toml`, or use a secret reference (`{ secret = "name" }`). Else the station does not start (DL-187, NV-08).

| Key | Default | Meaning |
| --- | --- | --- |
| `name` | the host name | The station name on the board. The board shows this name; a change applies at once and the deck renames the station (the name must be free on the deck). |
| `allowed_roots` | `["~"]` (the home directory) | The directories where sessions, worktrees and clones may run. An explicit list must have at least one entry. |
| `max_sessions` | `12` | The most sessions with a live process. A start or resume above the limit is refused. |
| `allow_setup_cmd` | `false` | Allow a session start to run the owner's setup command in its workspace (DL-148). |
| `allow_bypass_permissions` | `false` | Offer the mode that skips every permission check: Claude `bypassPermissions`, Codex `danger-full-access` (DL-177). Turning it on needs an elevation, and it applies after a station restart. |
| `allow_fleet_control` | `false` | Lets coordinator sessions with a fleet-control grant (scope `fleet`, or the classes `inspect`, `manage`, `answer`) act on the sessions of this station and read their files, diffs and requests (ADR-008). A danger setting: turning it on needs an elevation. It applies at once and is read live, only on this station: off stops the calls of its own coordinators at intake, and every action of any coordinator on its sessions at the decision and right before the provider write (`authority_lost`). It does not revoke a grant. Run an `answer` grant only on trusted work. |
| `allow_shell` / `max_shells` | `false` / `8` | The human shell (PLAN §6.8). |
| `allow_desktop` | `false` | Board label "Desktop control". Lets the owner control the desktop of this machine through the fleet: screens, the wall, notifications and previews. A danger setting: turning it on needs an elevation. It is `Live`: it applies at once, and off stops new actions at once. |
| `fleet_summary` | `false` | Board label "Fleet summary for the desktop shell". Sends a summary of the whole fleet (counts, the callsigns of sessions that need you, usage) to this station, for the Omarchy bar. Programs that run as you on this machine can read it. A danger setting: turning it on needs an elevation. It is `Live`. |
| `allow_desktop_capture` | `false` | Board label "Desktop screenshots". Lets the owner take a screenshot of the monitors of this machine from the board. The station keeps each screenshot as an artifact. A danger setting: turning it on needs an elevation. It is `Live`: off stops new screenshots at once. |
| `capture_monitors` | (empty) | Board label "Screenshot monitors". The monitors that a screenshot may capture, for example `DP-1`. An empty list allows every monitor. A danger setting: a change that widens the list (the list becomes empty, or it gets a new monitor) needs an elevation. A change that narrows the list does not. It is `Live`. |
| `offline_approval_policy` | `wait` | What happens to an approval when no deck is reachable: `wait`, or `deny_after(<n>s)` (also `m` or `h`). |
| `listen` | (empty) | Link listen addresses when the deck dials this station. |
| `host` | `shim` on Linux, `direct` elsewhere (DL-199) | `direct`: the agents are children of the station and stop with it. `shim`: each agent runs under a `fleetdeck shim` process and survives a station restart or upgrade (ADR-005). |
| `shim_buffer_mib` | `16` | The shim output buffer, 1–256 MiB. |
| `hibernate_after_min` | `0` (off) | Minutes of idleness after which the station stops a live, idle session gracefully (`session.hibernated`). The board resumes it with the next input (R6.6, DL-169). Up to 10080. |
| `repo_groups` | (empty) | Repository path → group name, for the conflict radar. |
| `clone_root`, `traits`, `allow_check_cmd`, `allow_local_remotes`, `[taildrive_mounts]` | see §13.2 | The Fleet Git keys. They apply after a restart. |
| `[[trackers]]` | (none) | A Jira or Linear tracker for the board's issue views (R3, DL-164). See "Trackers" below. |
| `[model_help]` | on | Small generated text from the station's own provider login (R10, DL-167): `enabled` (the off switch), `claude_model` (`claude-haiku-5-5`), `codex_model` (`gpt-5.6-luna`), `provider` (`claude` or `codex`; default the first signed-in instance), `per_call_max_output_tokens` (`400`), `daily_token_cap` (`200000`, one UTC day). The use shows on the usage panel as "Fleet Deck". |
| `tailscale_bin` | (none) | The absolute path of the `tailscale` program (§6.1). It overrides every automatic choice. Empty or missing: the deck looks for `tailscale` on the `PATH`, then (macOS) the app, then (WSL) Tailscale for Windows. A danger setting: any change needs an elevation. Applies at once. |
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

### Accounts: Linear sign-in with the built-in app (DL-185)

Open Settings, then Accounts, then Linear. Fleet Deck has a built-in Linear app. Use it when you set up no Linear OAuth app of your own.

Until the checks pass, `LINEAR_BUILTIN_VERIFIED` is false: the built-in app is off unless the station runs with `FLEETDECK_LINEAR_BUILTIN=1` (for the checks).

- The section shows "Uses the built-in Fleet Deck app." and the Connect button is on.
- The built-in app is a public client. The station holds a public client id and no client secret. The sign-in uses PKCE.
- The built-in app accepts two addresses only: `https://127.0.0.1:7420/oauth/callback` and `https://localhost:7420/oauth/callback`. Open the board at `https://127.0.0.1:7420` or `https://localhost:7420` on the deck machine, then press Connect.
- If you open the board at another address (a tailnet or LAN address, another port, `http`, or `[::1]`), the station refuses the sign-in with `builtin_needs_localhost`. The board shows a note next to the Connect button. Nothing goes to Linear.
- For other addresses, choose "Set up your own OAuth app…". An app of your own takes the place of the built-in app for new sign-ins.

Each Linear account remembers the app that made it. A refresh uses that app only.

- The same workspace that you connect with the built-in app and with your own app gives two accounts. Log out of the one that you do not want.
- An account that your own app made needs that app. If you replace your own app with another client id, the account shows the status "reconnect" (`needs_reconnect`) and the button "Connect again". Press it to sign in with the app that is set now. Accounts that the built-in app made keep working.
- An account from an earlier version has no record of its app. Fleet Deck tries one refresh with your own app, if you set one. If Linear accepts, the account is bound to that app. If not, or if you set no own app, the account shows "Connect again". Connecting again makes a new account. Log out of the old one.
- If the deck restarts during a sign-in, the board shows "The sign-in expired. Sign in again."

## 3. Deck configuration (`deck.toml`)

| Key | Default | Meaning |
| --- | --- | --- |
| `name` | the host name | The deck name. |
| `listen` | `127.0.0.1:7420` | The board HTTP and WebSocket addresses. Login is always required. |
| `link_listen` | `127.0.0.1:7421` | The link listener for stations that dial the deck. |
| `tailnet_hostname` | (none) | The tailnet DNS name of the deck, for the WebSocket `Origin` check. |
| `conflict_mail` | `true` | When two sessions change the same file within 30 minutes, the deck sends each session one follow-up mail about it (DL-145). Set `false` to turn this off. |
| `fleet_control` | `false` | Lets coordinator sessions with a fleet-control grant drive any session, read the fleet and answer approvals (ADR-008). Off stops every fleet-control action at once. Applies at once. Turning it on needs the owner's elevation; turning it off does not. See §7.3. |
| `clone_host` | `github.com` | The Git host for a repository that the board gets as `owner/name`. A lower-case host name, for example `gitlab.com`. Applies at once. |
| `clone_org` | (empty) | The owner or group for a repository that the board gets by name only. Use `acme` or a group path such as `acme/tools`. Applies at once. |
| `clone_transport` | `https` | `https` uses your Git credential helper. `ssh` uses your SSH keys. It decides the URL that the board builds from a short name. Applies at once. |
| `tailscale_bin` | (none) | The absolute path of the `tailscale` program (§6.1). It overrides every automatic choice. Empty or missing: the deck looks for `tailscale` on the `PATH`, then (macOS) the app, then (WSL) Tailscale for Windows. A danger setting: any change needs an elevation. Applies at once. |
| `tailnet_auto` | `true` | "Use Tailscale automatically" (§6.1, item 8). On when the key is missing. At each start the deck puts the tailnet addresses of this machine in `listen` and `link_listen`. Set `tailnet_auto = false` (or turn the toggle off) to stop it. Applies at the next start. A danger setting: a change from off to on, also a reset of a saved `false` to the default, needs an elevation; turning it off does not. |
| `tailnet_auto_listen`, `tailnet_auto_link_listen` | (empty) | The entries that the automatic mode added to `listen` and to `link_listen`. Only the deck writes them; the board does not show them. Do not edit them by hand. |

> **Caution:** use only loopback, private LAN or tailnet addresses. The deck refuses a public address. Do not forward a port from a router.

## 4. Start and stop

- One machine: `fleetdeck up` runs the deck and a local station in one process.
- More machines: `fleetdeck deck` on the deck machine, `fleetdeck station` on each station machine.
- As services: use the systemd user units or the launchd agents in `packaging/`. The station unit uses `KillMode=process` (launchd: `AbandonProcessGroup`). Keep this setting: it lets shim-hosted agents survive a station stop.
- Logs go to standard error. `--log-format json` gives JSON lines; `FLEETDECK_LOG=debug` gives more detail.

A station stop with `host = "direct"` stops every agent; the sessions become `lost` at the next start, or are continued when auto-continue is on and every recheck passes. With `host = "shim"`, the agents keep running and the next station start reattaches them.

### 4.1 Run as a service (DL-186)

Run `fleetdeck service install` to start Fleet Deck at boot or login. Use `both` (default, runs `fleetdeck up`), `deck` or `station`. You can install `deck` and `station` together; do not mix them with `both`.

- Linux and WSL: a systemd user service (`fleetdeck-up.service`, `fleetdeck-deck.service`, `fleetdeck-station.service`). Linger starts it at boot without a login. Logs: `journalctl --user -u <unit>`.
- WSL: systemd must be on (`[boot]` and `systemd=true` in `/etc/wsl.conf`). The command also adds a Windows logon task that starts the distro when you sign in to Windows.
- macOS: a launchd user agent (`dev.fleetdeck.<kind>`). It starts at login and ends at logout. Logs: `~/Library/Logs/fleetdeck/<up|deck|station>.log`.
- The service runs a copy of the program under `<data>/station/bin/current` (`deck`: `<data>/deck/bin/current`). After you build or install a new version, run `fleetdeck service start` (it compares the builds by commit time, falls back to file times for an older copy without build information, installs the newer program and restarts the service) or `fleetdeck service install`. `fleetdeck service status` says when the service runs an older copy.
- The service gets the safe folders of your shell `PATH`: absolute folders that only you or root can change (your own private group may write them). `install` prints one line with the counts; `--verbose` lists the folders that it leaves out. It warns when `claude`, `codex`, `git`, `gh`, `glab` or `curl` is only in a folder that it leaves out.
- With `host = "direct"`, the station ends its agents when the service stops. With `host = "shim"` (the default on Linux, DL-199), the agents keep running.
- `fleetdeck service install --dry-run` prints the files and the commands and changes nothing.
- `fleetdeck service start`, `stop` and `status [--json]` control and show the services. `fleetdeck service uninstall` removes them. Your data stays.
- If another Fleet Deck process runs, the command installs the service but does not start it. Stop that process, then run `fleetdeck service start`.

### 4.2 Versions (DL-192)

A release build reports `2.0.0`. Every other build reports `2.0.0-dev.<YYYYMMDD>+<commit>`: the UTC date and the short commit of the git commit that it was built from. A build without git reports `2.0.0-dev`. To build a release, run `make release-build` (it sets `FLEETDECK_RELEASE=1`). `fleetdeck --version`, the station and deck info and the service directory name show this version.

## 5. Board access

1. Set the owner password: `fleetdeck deck password` (from a prompt or `--stdin`). A new password ends every login.
2. Get a one-time login URL: `fleetdeck open`.
3. For scripts, make an API token: `fleetdeck deck token`. A token has a scope; keep it secret.
4. Read the audit log: `fleetdeck deck audit`.

### Settings, elevation and restart (DL-172)

- Edit the settings in the board (`#/settings`). The deck and the station write their own config file with the board; edit by hand only while Fleet Deck is stopped.
- A danger change (for example `allow_shell` on, a new allowed root, a `listen` address that is not loopback) and every restart need an elevation. In the board, give the owner password, or run `fleetdeck open --elevate` on the deck machine and type the code. The command prints one line: `Elevation code: XXXXX-XXXXX (one use, valid for 5 minutes). Type it in the board.` An elevation lives 10 minutes and only for the login that asked for it. Wrong tries count in the same limit as the login (5 in 1 minute, then HTTP 429).
- Restart from the board (`POST /api/settings/restart`): the process executes itself again with the argv, the environment and the executable path that it had at start. It first stops as for SIGTERM. `fleetdeck up` restarts as one process.
- **Exit code 75.** When the new `exec` fails after the process stopped, the process logs `restart failed: <errno>` and exits with code 75. The deck then shows as offline on the board. Read the log, repair the cause (for example a deleted or replaced binary), and start `fleetdeck` again.

### Settings from the shell (DL-172)

The deck and the station are the only writers of their config file while they run. To change a setting from a shell, use `fleetdeck settings`. It talks to the running process over its owner-only local socket (`deck.sock` or `station.sock`). Do not edit the file by hand while Fleet Deck runs. In `fleetdeck up` mode both sockets exist.

1. Show the settings of the deck: `fleetdeck settings get`. Add `--station` for the station of this machine, or `--json` for the full view. The table has the columns key, value and apply. A secret value shows `(set)`. `apply` is `live` (at once) or `restart` (at the next start).
2. Change one setting: `fleetdeck settings set <key> <value>` (add `--station` for the station). The value is read as JSON when it parses (`true`, `5`, `["a"]`, `null` removes the key). Else it is a plain string.
3. Read the answer. `Saved and applied.` means that the change is live. `Saved. Applies after a restart: <keys>.` means that you must restart the process. An error prints its code and message, and the exit code is 1.
4. Store a secret of the station: `fleetdeck settings secret <key> --station`. The value is the first line of stdin, or a prompt that does not show it. It is never an argument. The deck has no secret settings.

The local socket is the owner on this machine, so these commands need no elevation. A save reads the current revision first. When another change happens in between (`settings_changed`), the command reads again and tries once more. Each deck save writes an audit row with the actor `local` and the keys only.

### Limits

- **Board connections:** at most 256 open connections, and 32 from one IP address. The deck drops a connection over the limit at once. An IPv6 source counts by its /64 prefix.
- **Header timeout:** a board client has 10 seconds to send the request headers. The same limit ends an idle keep-alive connection. An open WebSocket is not limited by it.
- **Login:** each flow has its own limit: the password (also the elevation check), the login link, and the member join. One IP address can fail 5 times in 1 minute in one flow; all IP addresses together can fail 50 times. A refused attempt gets HTTP 429 `too_many_attempts` with the wait in seconds. A full flow does not block the other flows.
- **Password checks:** at most 2 run at the same time. A login that waits more than 2 seconds for a free slot gets HTTP 429 `busy`; this is not a failed attempt.
- **Link:** a deck or station listener accepts at most 64 pending handshakes, and 16 from one IP address. An established link does not count.

## 6. Pair a station

Station dials the deck (the station is behind NAT or on a laptop):

1. On the deck: `fleetdeck deck invite`. The invitation is single-use and a secret.
2. On the station: `fleetdeck station join`. Paste the invitation at the prompt, or give `--stdin` or `--invite-file <file>` (mode 0600). Never put an invitation on the command line.

Deck dials the station (the station has a fixed private address):

1. On the station: set `listen` in `station.toml`, then `fleetdeck station invite`.
2. On the deck: `fleetdeck deck add-station`.

To unpair: `fleetdeck deck remove-station <station>` on the deck, or `fleetdeck station forget-deck` on the station.

On a tailnet, a station can ask the deck to pair without an invitation (section 6.2).

### 6.1 Tailnet (DL-178)

Fleet Deck reads the `tailscale` CLI to show the tailnet. Tailscale is a display source only: its answers never authorize a connection, a pairing or a login. The link still needs the pinned keys.

1. **Program.** Fleet Deck runs `tailscale status --json`. It looks for the program in this order: the setting `tailscale_bin` (an absolute path), then `tailscale` on the `PATH`, then (macOS) `/Applications/Tailscale.app/Contents/MacOS/Tailscale`, then (WSL only) Tailscale for Windows (item 10). `GET /api/tailnet` has `program` (`configured`, `path`, `macos_app` or `windows`) when a program was chosen. The station expands a leading `~` in `tailscale_bin`; the deck does not. `tailscale_bin` exists in `deck.toml` and in `station.toml`. Each change, also an unset, is a danger change and needs an elevation.
2. **Limits.** Each call gets a minimal environment (`PATH`, `HOME`, `LANG`, and on macOS `TAILSCALE_BE_CLI=1`), no standard input, 3 seconds, at most 2 MiB of standard output and 64 KiB of standard error. A call over a limit is stopped. A duplicate JSON key, a missing required field or an address outside the Tailscale range makes the answer "not available" with a reason.
3. **Use tailnet.** `GET /api/tailnet` shows this machine (name, addresses, login), the `listen` and `link_listen` lists that "Use tailnet" would save, and the saved lists. `POST /api/tailnet/use` saves them as a normal elevated settings save (the board shows the changes first). The deck manages the tailnet-range listen addresses: in `listen` and in `link_listen` it removes every entry in the Tailscale range (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`) and adds `<tailnet address>:<port>` for each tailnet address of this machine, IPv4 and IPv6 (IPv6 in brackets). The port is the port of the first entry of the list; the defaults are 7420 and 7421. Every other entry (loopback, LAN) stays. The same save sets `tailnet_hostname` to the MagicDNS name of this machine. "Update" is the same action when the tailnet addresses changed. An address is added only when a local network interface has it.
4. **Restart.** `listen` and `link_listen` apply at the next start. After the save the answer says `restart_needed: true`; restart the deck from the board or with `fleetdeck`.
5. **Userspace networking.** When Tailscale runs with `--tun=userspace-networking`, its addresses are not on a local interface. The deck then offers no "Use tailnet" (`409 tailnet_userspace`). Set the listen addresses by hand, or run Tailscale with a TUN device.
6. **Devices.** `GET /api/tailnet/devices` lists the peers of `tailscale status` (online and offline) with host name, DNS name, addresses, OS, login, tags and a pairing state. `paired` means that a paired station is dialed at one of the addresses or at the DNS name of the device. Every other device is `none`. The deck sends no probe to any device. While the pairing window is open the `command` field holds the station command of section 6.2 (else `null`), and a device with a waiting request has the state `request waiting`. `tailscale status` is cached for 5 seconds.
7. **CLI.** `fleetdeck tailnet status` (add `--json`) reads `deck.toml` and `station.toml` and runs `tailscale` for each. It prints, for the deck and the local station, if Tailscale is available (with the reason when not), the DNS name, the addresses, if it runs in userspace networking, and the tailnet listen addresses of the deck.
8. **Use Tailscale automatically.** The toggle on the Tailnet card (or the key `tailnet_auto`) makes the deck keep the tailnet addresses current. **It is on by default** (the key is missing from `deck.toml`): at the first start where Tailscale runs and is not in userspace networking, the deck adds its tailnet addresses to `listen` and `link_listen`, so the board becomes reachable from the devices on your tailnet. Every request still needs the login, the CSRF token and TLS. To turn it off, turn the toggle off on the Tailnet card (no elevation needed) or set `tailnet_auto = false` in `deck.toml`; the deck then forgets which entries it added, so every entry becomes manual. A reset to the default (`null`) turns it on again and needs an elevation when the saved value is `false`. It applies at the next start, before any listener binds, in one absolute deadline of 3 seconds. The whole Tailscale call (the program choice with its file checks, the process start, the output and the cleanup of the child) runs on its own thread; at the deadline the start goes on without the answer, the child is killed, and a late answer saves nothing. For each list (`listen`, `link_listen`) the deck removes only the entries that it added before (`tailnet_auto_listen`, `tailnet_auto_link_listen`) and adds the current tailnet addresses that a local interface has. Every manual entry stays, also a manual entry in the tailnet range. An address that is already a manual entry of a list is not recorded as automatic there. The deck saves `listen`, `link_listen`, both lists and `tailnet_hostname` in one settings save without a new elevation (you gave it when you turned the mode on) and writes the audit row `tailnet.auto` with the changed keys. The mode saves nothing when Tailscale is not available, runs in userspace networking, does not answer in 3 seconds, or when `ip_nonlocal_bind` is `1` on the machine. The card then says "Not applied at start" and why. When the board certificate for the new addresses cannot be made, the deck starts with the manual entries and the existing certificate, and the card says "the board certificate could not be updated". A new certificate (the tailnet name and address) has a new fingerprint; the device-link dialog shows it. An automatic entry that is not on this machine, or that cannot be bound, is skipped with a warning and never stops the start; the card lists it. A manual entry that cannot be bound still stops the start. While the deck runs nothing changes; the card shows "Your tailnet address changed. Restart the deck to use it." when the addresses differ. A save of `listen` or `link_listen` by hand keeps only the automatic entries that you left in the list. Turning the mode off makes every entry manual.
9. **Local addresses.** An address counts as local only when the kernel has it on an interface of this machine (`/proc/net/fib_trie` for IPv4, `/proc/net/if_inet6` for IPv6). Where those files are not available, the deck tries to bind the address.
10. **WSL.** On WSL 2 with `networkingMode=mirrored`, the tailnet addresses of Tailscale for Windows are on the interfaces inside WSL, so "Use tailnet" and the automatic mode work. With the default NAT networking they are not, and the card says to set `networkingMode=mirrored` in `%UserProfile%\.wslconfig`, run `wsl --shutdown` and start Fleet Deck again, or to install Tailscale inside WSL. Fleet Deck detects WSL from the kernel text and the WSL interop file, never from an environment variable. When no other program is set or found, the deck uses Tailscale for Windows automatically, in the deck, the station and the CLI, after these checks: `/mnt/c` is the WSL mount of the root of the drive `C:` (read from `/proc/self/mountinfo`: exactly one entry for `/mnt/c`, root `/`, type `9p` or `drvfs`, source `C:\`, and for `9p` the option `path=C:\`); no mount is on `/mnt`, `/mnt/c/Program Files`, `/mnt/c/Program Files/Tailscale` or the program; `Program Files` and `Tailscale` are real directories and `tailscale.exe` is a regular file, none a symbolic link. When a check fails, or `mountinfo` cannot be read, no Windows program is used. The Tailnet card then says "Using Tailscale for Windows. It runs as your Windows user, with your Windows environment, the same as in a Windows terminal."; the minimal environment of item 2 covers the Linux side only. Fleet Deck does not defend against a Windows administrator who replaces the program. To use another program, or none, set `tailscale_bin`.
11. **Trusted certificate from Tailscale (DL-190).** By default the board uses a self-signed certificate, and a browser warns about it. Turn on the toggle "Trusted certificate from Tailscale" on the Tailnet card (or set `tailscale_cert = true`) to get a Let's Encrypt certificate for the tailnet name of the board (`tailnet_hostname`, for example `deck.tail1234.ts.net`). Turning it on is a danger change and needs an elevation; it applies at once. Before you turn it on, allow HTTPS certificates for the tailnet once: Tailscale admin console → DNS → HTTPS Certificates. The tailnet name becomes public in Certificate Transparency logs, and this cannot be undone. The deck renews the certificate by itself. The card shows `valid` with the end date, `renewing`, `expired`, `name_mismatch` (the tailnet name of this machine differs from `tailnet_hostname`) or `failed` with a reason (`GET /api/tailnet` has `certificate` with only a reason code, never text from Tailscale). Your own `board.crt` and `board.key` win for every name. The device link and `fleetdeck open` choose the address with a trusted certificate first. The addresses by IP keep the self-signed certificate.

### 6.2 Pair a station over the tailnet (DL-178)

This needs a deck with a link listener on a tailnet address ("Use tailnet", section 6.1, then a restart) and `fleetdeck station serve` running on the station. Nothing pairs by itself: the station owner runs one command, and the deck owner approves on the board.

1. **Open the window (deck).** `POST /api/pairing/window { "open": true, "elevation": "<elevation>" }` (owner login, `admin` scope, CSRF, elevation). The window lasts 10 minutes and applies to the tailnet link listeners only. A restart closes it. `GET /api/pairing/window` shows `open`, `generation` and `deadline_ms`. Close it with `{ "open": false }`; that cancels every waiting request.
2. **Get the command.** While the window is open, `GET /api/tailnet/devices` fills `command` for each device that is not paired: `fleetdeck station join --tailnet <deck DNS name>:<port> --deck-fingerprint <64 hex>`. The fingerprint is the deck link key fingerprint (not the HTTPS certificate hash).
3. **Run it (station).** `fleetdeck station join --tailnet <host>[:<port>] [--deck-fingerprint <64 hex>] [--scopes read,drive,approve,admin] [--name <name>]`. The default port is 7421, the default name is the station name, the default scopes are all four. The host must be a peer in the `tailscale status` of the station (a DNS name, a short host name or an address); the station dials exactly the tailnet addresses of that peer, IPv4 first. Without `--deck-fingerprint` the command only observes the deck key (a TLS connect that writes and sends nothing), prints all 64 digits and asks "Does this match the link key fingerprint on the board? [y/N]"; it needs a terminal for that. The command waits up to 11 minutes.
4. **Approve (deck).** `GET /api/pairing/requests` lists the waiting requests (requested name, full station fingerprint, offered scopes, source address, digest and the informational Tailscale identity). Compare the fingerprint with the one the station printed or knows, then `POST /api/pairing/requests/{request_id}/approve { "fingerprint": "<64 hex>", "digest": "<digest>", "elevation": "<elevation>" }`. `POST .../deny` refuses it (that key waits 10 minutes). Only the owner login can use these routes.
5. **Result.** The station prints `paired with <deck name>`, stores the deck key, its grants and the dial target in one transaction, and dials the deck. Exit code 0. Every refusal prints `<code>: <detail>` and exits with 1. Codes: `window_closed`, `busy`, `already_paired`, `request_id_conflict`, `denied`, `expired`, `cancelled`, `unknown_request`, `revoked`, `fingerprint_mismatch`, `unsupported`, and from the station `not_in_tailnet`, `tailscale_unavailable`, `pending`, `fingerprint_required`, `invalid_request`, `client_gone`, `not_accepted`. `not_accepted` means that the deck closed the connection before it answered (the window may be closed); the request is kept, so run the same command again.

If the answer is lost (a crash, a network cut), the station keeps `<station data>/pair-pending.json` (mode 0600). Run the same command again: it asks the deck for the stored approval (`PairResume`), or sends the saved request again when the deck holds none. A file for another deck or fingerprint blocks a new join: run `fleetdeck station join --tailnet --cancel`, which removes the file only.

The deck keeps one completion record for each approval (`pairing_completions` in `deck.db`, migration V22) until the station is removed. Audit rows: `pairing.window_open`, `pairing.window_close`, `pairing.approve`, `pairing.deny` (ids and fingerprints only).

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

**Current limit:** the station gives the grant credential only to a provider version that the opt-in coordinator live smoke test (ADR-006 §8.5) verified (`fleetdeck_providers::coordinator::VERIFIED` in `crates/fleetdeck-providers/src/coordinator.rs`, DL-219). Claude 2.1.296 and later and Codex 0.160.0 and later are verified. A start fails with `handoff_unverified` on a station where any instance of the chosen provider has an older or unknown version.

> **Caution:** A coordinator drives other sessions without a prompt from you. Revoke its grant when the work is done.

1. On the board, open "New session" and choose a station, a provider instance and a directory.
2. Select "Coordinator". The import field must be empty: an imported session cannot be a coordinator.
3. Give the first prompt and start the session.
4. Find the grant in the "Coordinator grants" list of the "Mail and tasks" panel on the fleet screen.
5. To stop the coordinator calls, select "Revoke" on its grant. A grant that expired after an idle time shows "Renew".

The deck creates the grant with the classes `read`, `mail` and `drive` and with its default expiry. It gives the session the control skill (`crates/fleetdeck-deck/src/control_skill.md`). The start fails for a resumed session, for a shim-hosted session and for a station without the coordinator endpoint (§11).

### 7.3 Fleet control (ADR-008)

A fleet-control grant lets a coordinator drive any session on its stations, start sessions in projects, read the fleet and answer approvals. A grant is a fleet-control grant when it has the fleet scope or one of the classes `inspect`, `manage` or `answer`.

> **Caution:** a coordinator reads third-party text (issues, pull requests, files, the output of other sessions). A prompt injection in that text can make it approve a tool call in another session. Give the `answer` class only for trusted work. A coordinator answers once, with "allow once" or "deny". It never edits the tool input, never chooses "always" or an amendment, and never starts, resumes or sets a session into a bypass mode.

- To create the grant, turn on "Fleet control" in the spawn form. The owner's elevation is in the same request. A member, a share guest or an API token cannot create one.
- The coordinator station and every station of the grant must run the link feature `coordinator.v2` on the current link. Otherwise the start is refused with `unsupported`.
- Two switches stop fleet control. The deck setting `fleet_control` is the switch for the whole deck. The station setting `allow_fleet_control` is the switch of one station. Both are off by default.
- A coordinator never acts for another coordinator, for itself, or for a shared session. Share guests and members never extend a coordinator.
- A fleet-control intent expires after 10 minutes. The deck checks the grant again before each send and each resend. When the grant is revoked, expired or changed, or a switch is off, the deck does not send the command and the item ends `dead` with the reason `authority_lost`.
- The answer cap of a grant is 500 by default (0 to 5000). The deck counts only an accepted answer.

Residual windows (ADR-008 §6.2). A revocation, or a switch that you turn off, stops every action whose provider write has not begun. Two short windows stay open. (a) Between a `valid` answer of `coordinator.authority_check` and the provider write inside the target station process: no wait for another process, a few milliseconds. (b) Between the `coordinator.policy_now` answer of the caller station and that `valid` answer: one deck step, at most the 5 s deadline of the check. No window comes from the propagation of a setting, because each switch is read live from its own process.

### 7.4 Desktop control and owner screenshots (R13)

To control the desktop of a station, turn on "Desktop control" in Settings, on the station tab of that station. The deck setting "Desktop control" must also be on.

#### Owner screenshots and artifact deletion (R13 P5a)

Take a screenshot:

1. Open Settings, then the station tab of the station.
2. Turn on "Desktop screenshots". The board asks for a recent sign-in. Sign in again when it asks.
3. Leave "Screenshot monitors" empty to allow every monitor. To limit it, enter the monitor names. Widening the list needs a recent sign-in, and narrowing it does not.
4. Open the station card and open the Desktop panel.
5. Press "Screenshot" next to a monitor.
6. If the button is off, read the reason line in the panel. The reason is one of these: the deck or the station does not support screenshots, `grim` is not installed, screenshots are off, or the monitor is not in the list.

The screenshot appears in the Desktop panel as an image, with the link "Open the screenshot". It is also an artifact of the kind image in the artifact list. Its title is `Desktop capture · <station name>/<monitor>`.

Limits:

- A screenshot is a PNG of at most 32 MiB.
- The station keeps at most 4 GiB of screenshots. Delete some screenshots when the station refuses a new one with the text "This capture would exceed the station's 4 GiB capture limit. Delete some captures first."
- The deck keeps at most 256 MiB of screenshots in its cache. It evicts the least recently used file first, and the artifact stays valid.
- The deck fetches a screenshot file for at most 60 seconds in total.

Delete an artifact:

1. Open the artifact in the viewer.
2. Press "Delete artifact". The board shows the confirmation and asks for a recent sign-in.
3. Confirm. The board shows "Deleted." when the station removed its copy.
4. The board can show "Hidden on this deck. Waiting for station deletion." The deck hides the artifact at once. The station deletes its copy when its link is up. The board then shows "Deleted."

"Deleted" means removed from the active Fleet Deck stores. It is not a secure erase of database pages or backups. Downloaded copies and provider conversations stay.

### 7.5 The Omarchy desktop integration (R13 P4)

This section covers the Omarchy integration of a station. The design is in `design/omarchy-p4.md`. The built version is recorded in DL-235. The live smoke on the owner's Omarchy machines is not done yet (`make desktop-smoke` prints the list).

What the integration gives you on the desktop of the station:

- **A bar widget and a panel.** The widget shows the number of working sessions and, when a session needs you, the number of those sessions with a warning sign. The widget is dim when the deck is down or sends no data. A left click opens the panel. A right click opens the board. A middle click shows the wall window.
- **Notifications.** A new request that needs you shows a notification with the title "<callsign> needs you". The text is "<Kind> on <station>", where Kind is Approval or Question. A click opens the board at that request. A notification never answers a request.
- **Hotkeys.** See the table below.
- **The screensaver.** The screensaver text shows the number of sessions that need you, the counts of the other states and the usage. It is off by default. The install records it only with the option `--screensaver`. This version has no board path that turns it on (open item).
- **The theme sync.** The station sends the colors of the Omarchy theme to the deck each time the theme changes.
- **The crash diagnosis.** The Omarchy menu has the rows "Fleet", "Open the board" and "Diagnose the last crash". The last row starts one supervised session that investigates the newest crash that systemd-coredump recorded.

The widget, the panel and the notifications show only counts, station names, callsigns and request kinds. They show no session title, no tool name and no chat text. The panel has no approval control. You answer a request on the board.

#### Turn on the fleet summary

The deck sends the fleet summary to a station only when you turn it on for that station.

1. Open Settings, then the station tab of the station.
2. Turn on "Fleet summary for the desktop shell". The board asks for a recent sign-in. Sign in again when it asks.
3. Wait a few seconds. The deck then sends the first summary, and the widget of the station shows the counts.

The station keeps the summary in memory only. A program that runs as you on the machine of the station can read it from the local socket. Turn the setting off to stop the summary. The deck then sends no new summary, and the widget shows no data.

The install also turns this setting on, but only when the station is running at that time. Otherwise the setting stays off. Turn it on on the board.

#### Choose the Omarchy theme on the board

1. Open the board preferences: the theme row at the foot of the sidebar. On a phone, open the More page.
2. Open the list "Use the Omarchy theme of". The list has "Off" and the stations that have sent a palette.
3. Select a station. The board shows the colors of the Omarchy theme of that station and reads them again every 60 seconds.
4. Select "Off" to return to the normal theme.

The list does not show while the choice is "Off" and no station has sent a palette. The choice is kept in this browser only. A wall display that is bound to a station uses the palette of that station by itself.

#### How the install happens

- For a station that P8 provisioned (section 15), the bootstrap runs the install at the first login of the user, with `fleetdeck omarchy install --yes`. The step has no question. You do nothing on the machine.
- **Open item (DL-235 item b): no board path installs the Omarchy integration on a station that P8 did not provision.** The command `fleetdeck omarchy install` does the install, but this version has no board button that starts it for an existing station. The gap is a product gap. It is not an instruction to log in to the machine.

The install writes only these files:

- The plugin folder `fleetdeck.fleet` in the Omarchy config folder.
- The three hook files `post-boot`, `post-update` and `theme-set`.
- The theme template.
- The file `hypr.lua` in the Fleet Deck config folder.
- The install record.
- The menu rows. The install writes them only when the Omarchy menu file does not exist. Otherwise it prints them.

The install also does these things:

- It adds one managed line to the Hyprland file `hyprland.lua`.
- It asks the Omarchy shell to rescan its plugins.
- It enables the plugin once, in the right section of the bar.
- It turns on the station setting "Fleet summary for the desktop shell".

The install follows no link. It keeps a file that a person changed. `fleetdeck omarchy uninstall` removes only what the install wrote. `fleetdeck omarchy status` reports each file as `ok`, `changed` or `missing`. It also reports "Hyprland line missing" when the managed line is not in place.

The install does not edit `hyprland.lua` when the file is a link, is not a regular file or belongs to another user. It prints the line instead.

#### The hotkeys

The installed file `hypr.lua` binds four keys. The binding runs with the session `PATH`, so `fleetdeck` must be on that `PATH`.

| Keys | Description in the Omarchy key list | Action |
| --- | --- | --- |
| `SUPER + CTRL + F` | Fleet Deck panel | Opens the panel of the plugin `fleetdeck.fleet`. |
| `SUPER + ALT + W` | Fleet Deck wall | `fleetdeck desktop wall show`: focuses the wall window. When the wall is already on the active workspace, it goes back to the previous workspace. |
| `SUPER + ALT + N` | Fleet Deck: next session that needs you on the wall | `fleetdeck desktop wall next`: shows the next session that needs you on the wall of this station. |
| `SUPER + ALT + M` | Fleet Deck: next wall mode | `fleetdeck desktop wall mode next`: changes the mode of the wall, in the order mission, focus on a request, ambient. |

`SUPER + ALT + N` and `SUPER + ALT + M` need the deck setting "Desktop control" on, and a wall display that is bound to this station. Without "Desktop control" the deck answers "Desktop control is off on the deck." Without a bound wall it answers "This station has no wall display bound to it." For the setting, see section 7.4. `SUPER + ALT + W` needs neither: it works on the window of this machine only. With no wall window it prints "No Fleet Deck wall window is open."

The panel keys are `j` and `k` or the arrow keys to move, `Enter` to open the board at the selected request, `w` to show the next session on the wall, and `Esc` to close.

#### What `fleetdeck doctor` reports

`fleetdeck doctor` has two Omarchy lines:

| Line | Meaning |
| --- | --- |
| `ok omarchy: no Omarchy on this machine` | The machine is not an Omarchy machine. |
| `ok omarchy: Omarchy <version>; the integration is not installed (run fleetdeck omarchy install)` | Omarchy is present and the install was not made. |
| `ok omarchy: installed, no drift` | Every installed file is as written. |
| `warn omarchy: <n> changed or missing files (run fleetdeck omarchy status)` | A person changed a file, or a file is gone. |
| `warn omarchy: the install record is not valid (run fleetdeck omarchy status)` | The install record cannot be read. |
| `warn omarchy-sudo: passwordless sudo is on for this user` | Omarchy has an active passwordless sudo grant for this user. Agents run as this user. |
| `ok omarchy-sudo: off` | No passwordless sudo grant is active. |
| `warn omarchy-sudo: could not check` | The check timed out (5 seconds) or did not start. |

There is no `omarchy-sudo` line when the Omarchy script `omarchy-sudo-passwordless` is not on the `PATH`. The flag `--brief` prints only the `warn` and `fail` lines and the final line. After an Omarchy update, the hook `post-update` runs `fleetdeck doctor --brief`. When the exit code is not 0, it shows the notification "Fleet Deck: doctor found a problem after the Omarchy update". This version has no board view of the doctor lines.

### 7.6 Agent desktop tools and screenshots (R13 P6)

This section covers the desktop tools of a coordinator session. The contract is `design/omarchy-p6.md`. The built version is recorded in DL-237. The PRE-PR review and the live smoke on the owner's Omarchy machines are not done yet (`make desktop-smoke` prints the list).

> **Caution:** a coordinator with these classes works on the desktop of a station without a prompt from you. A screenshot shows the whole monitor, with other applications and private information. A preview runs web content as your user on that station. Revoke the grant when the work is done.

#### What a coordinator can do

A grant has the class `desktop`, the class `capture`, or both. Neither class includes the other, and neither includes the other classes of a grant. Only a coordinator session can hold these classes. An ordinary session gets no desktop tool.

| Tool | What it does | Class |
| --- | --- | --- |
| `desktop_info` | Reads the monitors, the walls and the desktop abilities of one grant station. It shows no window list, no title and no wall content. A grant with only `capture` sees only the monitors that a screenshot can capture, and no walls. | `desktop` or `capture` |
| `notify` | Shows a notification on a grant station. The title starts with the callsign of the coordinator. The urgency is `low` or `normal`. A notification has no click action. | `desktop` |
| `preview_open` | Opens a web page in a new browser window on a grant station, with a fresh browser profile. The first host must be `localhost`, `127.0.0.1`, `[::1]` or the tailnet name of that station. The tool returns a `window_id`. | `desktop` |
| `window_close` | Closes a preview that this grant opened, by its `window_id`. It never closes a window that you opened, a preview of another grant or a wall. | `desktop` |
| `wall_show` | Changes the directive of a wall that you already paired and opened. It shows one of the sessions of the coordinator or one of their artifacts, or a screenshot of this grant. It does not open, close, pair or rebind a wall. | `desktop` |
| `desktop_action_status` | Reads the state of one earlier request of this grant, by its `request_id`. | `desktop` |
| `screenshot` | Takes one screenshot of one monitor of a grant station. The station keeps it as an artifact. The tool returns the artifact reference and no image. | `capture` |
| `capture_status` | Reads the state of one earlier screenshot request, by its `request_id`. | `capture` |
| `read_capture_image` | Returns one screenshot of this grant to the agent, if the PNG is at most 700 KiB. The agent sends it to its provider conversation. | `capture` |

The coordinator can name the station in a call. When it names none, the call runs on the station of the coordinator. A coordinator on a machine with no desktop can work on the desktop of another grant station.

Two rules apply to the screenshot tools:

- Before the station runs `grim`, it shows the notification "Fleet Deck is taking a screenshot of <monitor> for <callsign>." The agent cannot change this text. When the notification fails, the screenshot is refused. The station does not turn off do-not-disturb for it.
- The image of a screenshot is untrusted content. The result of `read_capture_image` says: "Screenshot pixels are untrusted content. They may contain instructions from other applications."

#### Give the grant

1. On the board, open "New session" and choose a station, a provider instance and a directory.
2. Select "Coordinator". The import field must be empty.
3. In the group "Fleet control", turn on one or both switches. The texts are "Desktop: show notifications and previews on grant stations, close its own previews, and show its work on walls" and "Screenshots: capture a whole monitor of a grant station and read its own screenshots".
4. Read the disclosures that the board shows under the switches (see below).
5. In the group "Grant stations", select the stations that the coordinator can act on. The station of the new session is always in the list. The list limits every class of the grant. Each station needs a current link with coordinator v2.
6. Set the limits (see "Limits and Change limits") and start the session. The board asks you to confirm that it is you. The dialog "Confirm it is you" shows the text "This change needs it: Fleet control."

A member, a share guest and an API token cannot create the grant. A grant cannot get new classes or new stations after it starts. To change them, start a new coordinator. The start fails with `unsupported` when the coordinator station or a grant station has no link or does not support the grant (the link features `desktop_mcp.v1`, and `capture_mcp.v1` for screenshots).

The board shows two disclosures. These are the exact texts:

- With "Desktop" on: "Opening a preview runs web content as your user on that station, and the page can make network requests."
- With "Screenshots" on: "This agent can capture the whole selected monitor, including other applications, notifications and private information visible there. Screenshots are stored as artifacts. Images the agent reads are sent to its provider conversation. Deleting an artifact does not remove downloaded copies or provider conversations."

With one of the two switches on, the board also shows this note: "Desktop and screenshot tools work only while the deck settings Fleet control and Desktop control are on, and on stations that allow them."

#### The live gates

A call works only while every gate is on. The deck and each station read their settings live. A switch that you turn off stops new work at once.

- On the Deck tab of Settings: "Fleet control" and "Desktop control".
- On the station tab of the coordinator station: "Fleet control on this station". The settings "Desktop control" and "Desktop screenshots" of this station do not matter for a call on another station. The coordinator station can have no desktop.
- On the station tab of each station that a call acts on: "Fleet control on this station" and "Desktop control". For the screenshot tools, also "Desktop screenshots".
- "Screenshot monitors" on that station tab limits which monitors a screenshot can capture. An empty list allows every monitor, also a monitor that you attach later. A new screenshot also needs that the monitor exists now.

The grant can exist while a gate is off. A call then fails with an error such as `fleet_control_off` ("Fleet control is off on this deck."), `desktop_control_off` ("Desktop control is off on this deck.") or `not_granted` ("This grant does not allow this desktop operation."). Turn the gate on from the board and ask the coordinator again. The board asks for a recent sign-in when you turn on one of these settings. Narrowing "Screenshot monitors" needs none.

#### Limits and Change limits

Each grant counts its use for its whole life. Renewal does not reset the counts. A count is not given back when a request fails, ends with an unknown result, or when you delete the artifact.

| Limit | Default | Range | What it counts |
| --- | --- | --- | --- |
| "Desktop action limit" | 300 | 0 to 5000 | Each accepted action, with each screenshot and each wall directive |
| "Screenshot limit" | 20 | 0 to 5000 | Each accepted screenshot (it also uses one desktop action) |
| "Image read budget (MiB)" | 64 | 0 to 1024 | Each image read charges 700 KiB (716,800 bytes), also when the PNG is smaller or the delivery fails |

A PNG that `read_capture_image` returns is at most 700 KiB. A larger screenshot stays a normal artifact that you open on the board. The tool then answers `image_too_large` ("The screenshot exceeds the 700 KiB MCP image limit. Open the artifact on the board."). A limit at 0 refuses every new action of that kind: the codes are `desktop_cap_reached`, `capture_cap_reached` and `image_budget_reached`.

A grant also has rates. A call that goes over a rate fails with `rate_limited` ("This grant's desktop rate limit has been reached. Try again later."). A call that finds no free slot fails with `busy`. The rates are 30 new actions in 60 seconds for each grant, 6 notifications in 60 seconds for each coordinator session, 2 new screenshots in 60 seconds, 60 status or information calls in 60 seconds, and 6 image reads in 60 seconds. Two actions and one image read can run at the same time for a grant. A grant accepts at most 10,000 different requests.

The grant row in the "Coordinator grants" list of the "Mail and tasks" panel shows the stations of the grant and the use of each limit, for example "desktop actions 12 of 300", "screenshots 2 of 20" and "image reads 1.4 of 64 MiB". To change the limits:

1. Select "Change limits" on the grant row. The button shows only for your owner login, and not for a revoked grant.
2. Change "Desktop action limit", "Screenshot limit" or "Image read budget (MiB)". The last two show only for a grant with the class `capture`.
3. Select "Save". A lower limit needs no confirmation. A higher limit asks you to confirm that it is you (the label is "Desktop limits"). When you cancel the dialog, nothing changes.

#### Desktop operations

Select "Desktop operations" on the grant row to see the 50 newest operations of the grant. Each line has the tool, the station name, the state and, when there is one, the code in brackets, for example "notify on <station>: failed (desktop_failed)". The states are `prepared`, `attempted`, `done`, `failed` and `unknown`.

A line that ends with "— unresolved" has the state `prepared`, `attempted` or `unknown`. The deck has no final answer for it. The action can have run, or not. Fleet Deck never sends the action again, and it never takes a recorded screenshot a second time. When the deck starts, it ends a `prepared` operation as failed without sending it, and it marks an `attempted` operation without a final answer as `unknown`. A lookup can later finish the record of a screenshot, but a lookup never starts new work. To find what happened, look at the station: the notification on its screen, or the screenshot in the artifact list on the board. The list holds no URL, no notification text and no image.

#### Stop it

- Select "Revoke" on the grant row. A revoked grant is final. The next call is refused, and an action whose last check has not finished does not start.
- Turn off "Fleet control" or "Desktop control" on the Deck tab, or a setting of a station (see "The live gates"). The effect on the next call is the same.

A short interval stays open after a revoke or a switch change. The check of the caller policy and the final answer of the deck take at most 5 seconds. After the final answer, the station does the effect with local work only. A process or a file operation that already started can finish. A revoke does not close a preview that is already open, does not delete a screenshot, does not undo a notification and does not take back an image that the provider already received. Remove a screenshot with "Delete artifact" in the artifact viewer. The Desktop panel of the station card closes only a window that you opened in that panel. This version has no board path to close a preview that the agent opened: this is an open item (no board path yet).

The tools do not isolate the agent from other programs. Agents and the station run as your user, and a screenshot of a monitor cannot leave out other applications.

#### What the agent never receives

- No display credential, pairing token, cookie, share link or other secret of the board.
- No wall data. `wall_show` returns only the generation of the new directive, and `desktop_info` returns no wall content.
- No data of a station outside the grant, and no data of another coordinator. This includes your own screenshots: a grant reads only the screenshots that it made itself.
- No window title, process list, window address, environment value or monitor serial.
- No URL of a preview, no browser profile and no file path.
- No image larger than 700 KiB.
- No error text with a value that the agent supplied, and no raw request ID. The deck and its audit store only identifiers and fixed codes.

### 7.7 Monitor recordings (R13 P5b)

This section covers the owner recording of a monitor. The contract is `design/omarchy-p5b.md`. The built version is recorded in DL-239. The PRE-PR review, the end-to-end proofs (task P1) and the live smoke on the owner's Omarchy machine are not done yet (`make desktop-smoke` prints the list). The real-recorder smoke is required before you rely on this feature. Agent recordings (P5b-2) are built (DL-241): see the part "Agent recordings (P5b-2)".

> **Caution:** a recording shows the whole monitor, with other applications, notifications and private information. The recording is an ordinary artifact until you delete it. Fleet Deck does not isolate the recording from other programs that run as your user on the station.

#### What a recording is

- One whole monitor. The tool is `wf-recorder`. There is no region, no window and no audio.
- The cursor appears as the recorder normally shows it.
- The file is an MP4 with one H.264 video track, at most 30 frames each second.
- The duration is a whole number from 1 to 120 seconds. The recorder needs a short time to start, so the saved clip can be shorter than the duration that you asked for.
- The Desktop panel shows this disclosure under every Record control: "Records the whole selected monitor, including other applications, notifications and private information visible there. No audio is recorded. The recording is stored as an ordinary artifact until deleted."
- A recording needs no confirmation and no recent sign-in. Only the setting that allows it needs one (see "Turn it on").

#### Turn it on

1. Open Settings, then the Deck tab. Turn on "Desktop control" if it is off.
2. Open the station tab of the station. Turn on "Desktop control" and "Desktop screenshots". The board asks for a recent sign-in. Sign in again when it asks.
3. Leave "Screenshot monitors" empty to allow every monitor, or enter the monitor names. One setting and one list cover screenshots and recordings. When "Desktop screenshots" is off, the panel line for a recording says "Desktop screenshots are off on this station. Recordings need them. Turn them on in Settings."
4. The station and the deck must both be a version that supports recordings. The recording tool `wf-recorder` must be on the `PATH` of the desktop. Installing `wf-recorder` on a station has no board path yet. This is an open item.

If Record is off, read the reason line in the Desktop panel. The reason is one of these texts:

- "This deck does not support monitor recordings."
- "This station does not support monitor recordings."
- "The recording tool wf-recorder is not installed."
- "Desktop screenshots are off on this station. Recordings need them. Turn them on in Settings."
- "This monitor is not allowed for recordings."

#### Record a monitor

1. Open the station card and open the Desktop panel.
2. Find the monitor. Each monitor has a field "Duration (seconds)" with the value 30, the button "Record" and the disclosure.
3. Enter a whole number from 1 to 120. For any other value the panel shows "Recording duration must be a whole number from 1 to 120 seconds." and it sends nothing.
4. Press "Record".
5. Read the phase in the panel. The phase text is one of these:
   - "Preparing recording": the station checks its settings and the monitor, reserves disk space and starts the tool.
   - "Recording": the tool records the monitor.
   - "Stopping recording": the duration ended or you pressed "Stop recording". The station waits at most 3 seconds for the tool to end.
   - "Saving recording": the station checks the file and saves it as an artifact.
6. When the recording is saved, the panel shows the link "Open the recording".

While a recording or a screenshot runs on the station, "Record" and "Screenshot" are off for every monitor of that station. The station runs one capture at a time. The board never sends the start again. It polls the result every 2 seconds for the duration plus 45 seconds. After that time the panel shows "The recording result is unknown. Check the artifact list before starting another recording." with the button "Check result". "Check result" asks the station one more time.

If you reload the board, or open the panel on a second tab, the panel asks the deck for the active recording and shows its phase again. Closing or reloading the board does not stop a recording. When the station saved the file and the deck is still updating its list, the panel shows "The recording was saved on the station. The artifact list is still updating."

#### Stop a recording

1. Press "Stop recording" in the Desktop panel. The button shows whenever the board knows the recording. It works also when the station setting is off.
2. The station keeps the part that it already recorded and saves it as a normal recording. The panel shows "Stopping recording", then "Saving recording".
3. If you press it before the tool starts, the station saves nothing. The panel shows "The recording was stopped before it started."

Another owner login can stop the recording, also from another tab.

#### Where the recording goes

- The recording is an artifact of the kind Video. Its title is `Desktop recording · <station name>/<monitor>`. It is in the artifact list.
- The viewer plays it with the native controls of the browser. It does not start by itself and it does not loop. You can seek: the browser asks the deck only for the part that it needs. Above the player the viewer shows `Desktop recording · <station>/<monitor> · <time>`. When the saved length and the requested length differ by more than 1 second, the line also shows both lengths.
- If the browser cannot play the file, the viewer shows "This browser cannot play this video." with "Download" and "Retry". "Retry" loads the bytes again and does nothing else. The "Download" action of the toolbar also works.
- To remove the recording, open it and press "Delete artifact" (see section 7.4).
- The wall does not play video. A wall that shows a recording shows the card "Open this artifact on your own device".
- The deck reads the video from the station in parts. It can keep a complete copy in its cache only when the file is 64 MiB or smaller. When the station is offline and the deck has no cached copy, the video cannot play.
- An agent cannot read a video artifact through its tools.

#### When a recording is discarded

The station discards the recording and keeps no file when a safety rule fails. The panel then shows a text from the table. Only the end of the duration and "Stop recording" save a recording. A rule that fails after you pressed "Stop recording" still discards the recording.

| What happened | What the panel shows |
| --- | --- |
| You turned off "Desktop control" or "Desktop screenshots" on the station, or the monitor is no longer in "Screenshot monitors". The station sees it within 1 second. | "The recording was interrupted before it was saved." |
| You turned off "Desktop control" on the Deck tab, or the sign-in that started the recording ended (sign-out, revoke or expiry). The station sees it with a check of the deck, in 6 seconds at most. | "The recording was interrupted before it was saved." |
| The screen locks, or the lock state is unknown. The station sees it within 2 seconds. | "The screen is locked or its lock state is unknown." |
| The monitor goes away, its size or rotation changes, or the desktop session restarts. The station sees it within 2 seconds. | "The target of the desktop action was not found." |
| The link between the deck and the station closes, the deck sends an answer that is not valid, or the station hears nothing from the deck for 6 seconds (a cut network, a stopped deck). | "The recording was interrupted before it was saved." |
| The station shuts down, or it stopped without warning and started again. The start of the station removes the files that the stop left. | "The recording was interrupted before it was saved." |
| You delete the artifact before the station saved it. | "The recording was interrupted before it was saved." |
| The file reaches 256 MiB. | "The recording exceeds 256 MiB." |
| The tool cannot write the file, or the tool ends by itself, also without an error. | "The recording tool failed." |
| The free space on the station falls below the reserve of 2 GiB. | "The station has too little free space for a capture." |
| The whole operation passes the duration plus 30 seconds. | "The station did not answer in time; the action may have run." |

Two more failures can come after a normal stop. The station checks the structure of the MP4 file before it saves it. A bad file gives "The recording file could not be verified." A failure to save the artifact gives "The recording could not be saved as an artifact." Neither leaves an artifact.

A discard removes the files in a durable way. The station removes each file and each emptied folder, and it syncs the parent folders. It counts the space as free only after that. The start of the station finds and removes files that no operation owns. The station never resumes or saves a recording after a restart.

When the board shows "The recording result is unknown", the recording can exist. Look in the artifact list before you start another one.

#### Limits

| Limit | Value |
| --- | --- |
| Duration | 1 to 120 seconds |
| File size | 256 MiB |
| Disk reservation for one recording | 512 MiB (the staging file and the copy that the station saves) |
| All captures of one station | 4 GiB, screenshots and recordings together. The station never deletes an artifact for you. It refuses a new capture with "This capture would exceed the station's 4 GiB capture limit. Delete some captures first." |
| Free space on the station | 2 GiB must stay free, plus the 512 MiB of the recording |
| Captures at the same time on one station | 1, shared by screenshots and recordings. The text is "A capture is already running." |
| Monitor size | Each side from 2 to 8192 pixels and even, at most 16,777,216 pixels. Another size gives "The monitor exceeds the recording size limit or has unsupported dimensions." |

#### What an older board or deck shows

- An older board does not declare the video feature. The deck then sends the recording as a file of the kind "File", with no recording line and no player. You can still download the MP4. An older board has no Record control.
- An older deck does not support recordings. The panel shows "This deck does not support monitor recordings." and sends no recording request.
- An older station gets no recording request. The panel shows "This station does not support monitor recordings." Its screenshots work as before.

#### Agent recordings (P5b-2)

This part covers the tool `record_monitor` of a coordinator session. The contract is section 13 of `design/omarchy-p5b.md`. The built version is recorded in DL-241. The end-to-end proofs (task Q-P1) pass. The PRE-PR review and the live smoke are not done yet (`make desktop-smoke` prints the list). The real-recorder smoke of section 7.7 is also required.

> **Caution:** an agent with this allowance records a whole monitor of a grant station without a prompt from you. A recording shows other applications, notifications and private information. The agent gets only a reference, but the video is an ordinary artifact that you and other artifact readers can open until you delete it. Revoke the grant when the work is done.

What it allows:

- One whole monitor of a grant station, with no audio, for 1 to 120 seconds each time. The file is the same kind of MP4 as an owner recording (see "What a recording is").
- Only a grant with the class `capture` and the explicit allowance can do it. The class `capture` alone does not allow it. A grant that exists before this version has no allowance and stays screenshot-only. Renewal never adds the allowance.
- The coordinator can name the station in a call. When it names none, the call runs on the station of the coordinator.
- The board shows this disclosure when you turn the allowance on: "This agent can record the whole selected monitor for up to 120 seconds at a time, including other applications, notifications and private information visible there. Recordings are stored as video artifacts. The agent gets only a reference to each recording, never the video."

Give the allowance:

1. On the board, open "New session". Choose a station, a provider instance and a directory. Select "Coordinator". The import field must be empty.
2. In the group "Fleet control", turn on the switch "Screenshots: capture a whole monitor of a grant station and read its own screenshots". The switch "Allow monitor recordings" shows only while the screenshots switch is on. When you turn screenshots off, the board turns the recordings switch off too.
3. Turn on "Allow monitor recordings".
4. Set the two limits that then show. "Recording limit" counts the recordings of the grant. The default is 5 and the range is 0 to 5000. "Recording time limit (seconds)" counts the requested seconds of all recordings of the grant. The default is 600 and the range is 0 to 600000. A value outside the range shows a message and blocks the start.
5. Read the disclosure under the switches. In the group "Grant stations", select the stations that the agent can record. Set the other limits (see "Limits and Change limits" in section 7.6).
6. Start the session. The board asks you to confirm that it is you. The dialog "Confirm it is you" shows the text "This change needs it: Fleet control." The elevated confirmation of the grant covers the allowance.

The start fails with `unsupported` when the coordinator station or a grant station has no link or lacks the feature `capture_recording_mcp.v1`. The deck, the station of the coordinator and every other grant station must be a version that supports it. An older station cannot be in a grant with the allowance. A call on a station that has a link but lacks the recording features gets `desktop_unsupported` ("This station's build has no desktop control."), and a call on a station with no link gets `station_unreachable` ("The station is not connected."). All the live gates of section 7.6 apply to a recording: "Fleet control" and "Desktop control" on the Deck tab, and, on the station that records, "Fleet control on this station", "Desktop control", "Desktop screenshots" and the list "Screenshot monitors".

What the station shows:

- Before the station starts the recorder, it shows this notification: "Fleet Deck is recording <monitor> for <callsign> for up to <seconds> seconds." The agent cannot change the text. When the notification fails, the station refuses the recording and no recorder starts. The station does not turn off do-not-disturb for it.
- The station records one capture at a time. While an agent recording runs, a second capture on that station is refused with "A capture is already running." When the Desktop panel of the station knows the recording, "Record" and "Screenshot" are off for every monitor of the station.

What the agent gets:

- The tool `record_monitor` is in the tool list of the agent only when the grant has the allowance and the station negotiated the feature. A screenshot-only session has no recording tool.
- A saved recording returns the request identity, the station, the reference to the video artifact, the monitor, the size and the hash, the start time, the requested and the saved length, and the fixed stop reason.
- The agent never gets the video bytes, a frame, a thumbnail, a file path, a share link or a token. `read_capture_image` refuses a recording with the answer for an unknown artifact. No tool of the agent reads a video or stops a recording.
- You play the video in the artifact viewer, as for an owner recording (see "Where the recording goes"). The deck builds the provenance of the artifact itself. It records the caller station, the caller session and the grant, and never the provenance of an owner recording.

The grant row in the "Coordinator grants" list shows the recording use only while "Recording limit" is above 0. For example: "recordings 1 of 5, recording time 10 s of 600 s". This version has no board form to change the recording limits of a grant that already exists, and the deck has no route for it yet. This is an open item (no board path yet). To get other limits, start a new coordinator. The "Change limits" form of section 7.6 changes only the three P6 limits.

Stop an agent recording:

- Open the Desktop panel of the station that records. The panel asks the deck for the active recording and shows its phase. Press "Stop recording". The station keeps the part that it already recorded and saves it as a normal recording. The agent then gets the artifact reference with the stop reason `owner_stop`.
- Select "Revoke" on the grant row. The station sees the revoke with the next check of the deck, in 6 seconds at most, discards the recording and keeps no file. The agent gets `authority_lost`. When the station already saved the file before the revoke, the artifact stays in the artifact list and the agent does not get its reference.
- Turn off "Fleet control" or "Desktop control" on the Deck tab, or a setting of the station (see "The live gates" in section 7.6 and "When a recording is discarded"). The recording is discarded and the agent gets `authority_lost`. When the deck cannot check its authority, the agent gets `authority_unknown`.
- Turn off "Desktop control" or "Desktop screenshots" on the station, or turn off "Fleet control on this station". The station discards the recording within 2 seconds.

The discard table of "When a recording is discarded" applies to an agent recording. The agent sees the fixed code and text of the matching row, for example `screen_locked` ("The screen is locked or its lock state is unknown."). The deck waits for the station at most the requested seconds plus 35 seconds. The call of the agent ends after the requested seconds plus 45 seconds.

The charges:

- At the moment the deck accepts the call, it charges in one step: one desktop action, one capture, one recording and the requested seconds.
- Fleet Deck gives no charge back. An early stop, a refusal by the station, a cancel, an artifact that you delete, a time out and an unknown result all keep the charge. Renewal and a restart of a deck or station do not reset a count.
- A screenshot and a recording use the same capture limit ("Screenshot limit"). They also share the rate of 2 new captures in 60 seconds for the grant.

The refusals, in your words (the agent sees the code and the fixed text):

| Code | Text | What it means |
| --- | --- | --- |
| `recording_not_allowed` | "This grant does not allow monitor recordings." | The grant has no recording allowance, or "Recording limit" is 0. |
| `recording_cap_reached` | "This grant has used its recording limit." | The grant made as many recordings as "Recording limit". |
| `recording_seconds_reached` | "This grant has used its recording time limit." | The requested seconds would go over "Recording time limit (seconds)". |
| `capture_cap_reached` | "This grant has used its capture limit." | "Screenshot limit" is used up. A recording needs one capture. |
| `desktop_cap_reached` | "This grant has used its desktop action limit." | "Desktop action limit" is used up. |
| `rate_limited` | "This grant's desktop rate limit has been reached. Try again later." | The grant made 2 captures in 60 seconds. |

A refused call charges nothing. A call that is accepted and then fails keeps its charge.

#### Open items

- The real-recorder smoke on the owner's Omarchy machine is not done (see `docs/v2/LIVE-SMOKE.md`). Do not rely on recordings before it passes.
- The deck does not end a video response when a client stops reading. The 180 second limit needs a connection write timeout (DL-239 item c).
- Installing `wf-recorder` has no board path yet.
- Agent recordings: the PRE-PR review and the live smoke are not done. The board has no form to change the recording limits of an existing grant (no board path yet).

## 8. Back up

- The station event log (`station.db`) is the source of truth for sessions. The deck database (`deck.db`) is the source of truth for mail, plans, tasks, routing and settings.
- Stop the node, then copy the database file with its `-wal` and `-shm` files. A copy of a running database without its WAL file can lose recent events.
- The identity keys are in the same directories. A lost station key needs a new pairing.
- Retention (DL-187): the station removes acknowledged events after 7 days and finished history (effects, requests, processes, shell effects, coordinator outbox rows, artifact suggestions) after 90 days, every hour. The deck removes audit rows after 90 days and keeps at most 200,000. Back up before that time if you need older rows.

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

- With `host = "shim"` (Linux), the station continues a lost session only when it holds the DL-182 proof that the old run ended (§10, "Resume on the shim host"). In practice this is a verified reboot. Without the proof, it skips the session with the reason `execution_unknown`, and you resume it yourself. Right before the dispatch, it checks the proof again like a manual resume (DL-188 step 3).
- The station checks every ADR-007 rule when it stores the continuation. Right before the dispatch, it checks again only these rules: identity, proven end of the old process, newer owner work, requested stop and the time window. It does not check again the preference, the turn state, the capability, the workspace or a free session slot.

### Resume on the shim host (DL-182)

With `host = "shim"` a resume starts a new shim and a new agent process. It is never a reattachment. The board shows `resumed`.

- Resume on the shim host works on Linux only. On another platform the station refuses `session.resume`, `session.import`, the input of a hibernated session and a handover with `unsupported` ("resume on the shim host needs Linux").
- The station resumes a session only when it can prove that the earlier agent run ended. The proof is one of these: a clean exit that the shim checked completely, a launch that never started, a verified reboot (the boot id differs from the one that the run stored), or your reconciliation.
- A reboot proof is checked again at the resume. If the boot id is the old one again (for example after a restore of the VM memory) or a process with the stored identity lives, the resume is refused.
- Reconciliation: for a run whose end is not proven, the station method `session.execution_check` lists the processes that still belong to it (the agent, its process group and its session). Stop them on the station. Then the command `session.reconcile_execution` marks the run as ended. The station checks the processes again and refuses while one lives or when the check is incomplete. The board page for this action comes in a later release; the station methods exist now.
- Detached hooks: the station cannot see a process that left the agent's process group and session (it called `setsid()`). A hook that detaches and keeps a writable descriptor on the transcript can write to it after a resume. The direct host has the same limit. The check lists such processes for information only.
- Rollback: a launch journal of this release has a `boot_id` field. A station of an earlier release reads that journal as unknown evidence and fails closed: the session stays `lost` there. A shim of this release speaks the shim protocol 2 and the previous version 1 (an unavailable process count is `u32::MAX` on version 1).
- Auto-continue on the shim host uses the same proof (DL-188 step 3): a lost session is continued only when its run is proven ended.

## 11. Troubleshooting

Run `fleetdeck doctor` first. It changes nothing. It prints one line for each check (`ok`, `warn` or `fail`) and then a summary, and its exit code is 1 when a check fails. It checks the data directory and its mode, `station.toml` and `deck.toml` (parse and mode), `git`, the `--version` of each provider binary (a `warn` when it is older than the version that Fleet Deck is tested with, a `fail` when it is known to break the protocol), `allow_shell`, the deck local socket, the board listener, and the v1 Fleet Deck Claude plugin. Remove the v1 plugin after the cutover: its hooks slow every stop of a v2 session.

| Symptom | What to do |
| --- | --- |
| A session is `lost` | The station restarted and could not prove what happened to the agent. Read the detail in the timeline. Resume the session if the provider supports resume. With `host = "shim"`, the resume needs proof that the agent ended (see "Resume on the shim host"). |
| A command is `outcome_unknown` | The station cannot prove whether the provider received it. It never resends it. Check the session and send a new command if needed. |
| The station does not start: "cannot be marked live" | An update record in `<data>/station/upgrades/` cannot be read or written. The station does not open its database, so a later rollback cannot restore an old snapshot over newer state (DL-187, NV-29). Fix or remove the named file, then start the station again. |
| The station does not start: "holds a plain `env` value" | `station.toml` has a plain provider `env` value and other users can read it (NV-08). Run `chmod 600` on the file, or use a secret reference. |
| `forget-deck` or remove-station says "the revocation was not stored" | The key is unpinned in memory, but the store write failed (NV-15). Fix the disk or database problem and run the command again before a restart. |
| A revert needs reconciliation | The station could not prove the state of the files or the conversation. Read the revert card and resolve it; input stays blocked until then. |
| The station is not on the board | Check the link: the station log, the `listen`/`link_listen` addresses, and the pairing. `fleetdeck deck stations` shows the link state. |
| Usage shows "unknown" | The provider did not report usage for that window. Fleet Deck never shows unknown usage as zero. |
| The board refuses a WebSocket | Set `tailnet_hostname` when you open the board by its tailnet name. |
| A link call fails with `feature_not_negotiated` | The deck and the station do not share the feature of that method (for example `coordinator.v1` for coordinator sessions). Upgrade the older side; an N−1 peer keeps the shared features. |
| A coordinator tool call fails `deck_unreachable` | The station has not linked to a deck since it started, so it knows no deck to send to. Check the link (row "The station is not on the board"). After the first link, the station keeps coordinator calls in its durable outbox and sends them when the deck is back, for up to 15 minutes (DL-218). |
| A start fails with `launch_dir_changed`: "The session folder changed: a link is in its path." | A link or a rename replaced a folder of the session path after the station checked it (B9, DL-200). Check the path for links, remove the link, and start again. |
| A start fails with `launch_dir_unavailable`: "The station cannot hold the session folder open (no /proc)." | The station needs `/proc` to hold the folder open. Mount `/proc` for the station process (for example in a container), then start again. |
| A start fails with `launch_dir_missing`: "The station did not check the session folder before the launch." | This is a station fault. Update the station, and report the fault when it stays. |
| A start fails with `launch_dir_no_policy`: "The station has no allowed roots for the session folder." | The station has no allowed roots. Set `allowed_roots` in `station.toml`, then start again. |
| The station log says "Landlock is not available: worktree and clone provisioning is checked after Git, not confined." | The kernel has no Landlock (Linux 5.13 or later). Provisioning still works and the station checks the new folder after Git. Update the kernel to confine Git. `fleetdeck doctor` shows the same warning. |
| A stopped or failed worktree or clone start says "The new folder stays at <path>: remove it after a check." | The station never deletes a folder after a failed start. Look at the folder, and remove it yourself when it is a partial result. The worktree list shows it. |
| A coordinator session does not start | The start fails with its reason. `a coordinator session starts only as a new direct child`: a resume cannot carry the grant credential; start a new session. `a coordinator session on the shim host needs a provider version whose bridge is verified`: update the provider (Claude 2.1.296, Codex 0.162.1 or later; DL-194). `the coordinator endpoint does not listen`: check the station coordinator MCP ingress. `handoff_unverified`: the provider version has no verified credential handoff. Claude 2.1.296 and later and Codex 0.160.0 and later are verified for the direct host (§7.2; DL-215, DL-219, DL-220). |

## 12. Owner steps that this repository cannot test

- A container image build (`docker build`) and a run of the image.
- A real tailnet, a macOS host (launchd) and a physical phone.
- The release download and signature check (cargo-dist).
- `dist generate` and the commit of its release workflow, and the Homebrew tap repository (7-5, `packaging/README.md`).
- The approval round trip from the board to an agent across a real LAN between two hosts; `make perf` measures loopback only (7-4).
- The live smoke tests against the real `claude` and `codex` CLIs: `make smoke-live` (opt-in, network, a small number of tokens; [LIVE-SMOKE.md](./LIVE-SMOKE.md)).
- A real GitHub Actions run of `.github/workflows/v2.yml` (it runs `make check`; this repository never pushes).
- The coordinator live smoke of ADR-006 §8.5: run it again for each new Claude or Codex version before you add that version to the verified list (§7.2).
- Fleet Git (§13) on real machines: a push and a land with the SSH agent or the credential helper of a service, a real Taildrive share and mount, and a look at the new board sections on a real screen (the end-to-end tests ran at the phone viewport only).

## 13. Fleet Git

Fleet Git makes one Git repository work across the stations of the fleet. The design is [design/fleet-git.md](./design/fleet-git.md). The link changes are in [ADR-009](./adr/ADR-009-fleet-git-link.md). This section tells the owner how to turn it on and how to run it.

What is built: fetch before a start (FG-1), push (FG-2), cleanup (FG-3), worktree bootstrap (FG-4), homes on many stations (FG-5), placement by traits (FG-6), the conflict radar (FG-7), land (FG-8), branches of other stations and the coordinator tools (FG-10), the config drift report (FG-11) and Taildrive artifacts (FG-12). **Move a session to another station (FG-9) is not built.** It waits for an owner decision (DL-227). Nothing in this section is available for it.

### 13.1 Rules that always hold

- **Opt-in.** Every behavior needs the project setting "Sync this project across stations". A project with sync off keeps every text and route of the earlier releases.
- **Git version.** Each station needs Git 2.45 or newer. An older Git gives the code `git_too_old`.
- **Both sides.** The deck and the station must both support the feature of an action. Else the board shows 409 `fleet_git_unsupported`: "This station does not support Fleet Git. Update it first." Update the older side.
- **No stored credential.** The deck never stores, sends or logs a Git credential. A remote URL with a password or a token is refused. A login name (`git@host:path`) is allowed.
- **Remote writes.** The only refs that Fleet Git pushes are the branches `fd/<station>/<callsign>` that a station made, and the result of a land (§13.11). Each push goes to one URL that the station checked right before the push.

### 13.2 Station keys (`station.toml`)

| Key | Default | Rule |
| --- | --- | --- |
| `clone_root` | the first allowed root + `/projects` | The folder where the station clones a new home (§13.8). It must be inside an allowed root (equal is allowed) and must not hold a `..` part. An empty value means the default. |
| `traits` | `[]` | The labels that a start can require (§13.9). At most 16. Each label is 1 to 32 characters of `a-z`, `0-9`, `_` or `-`. Each label at most once. |
| `allow_check_cmd` | `false` | Lets a project run its check command on this station before a land (§13.11). A danger setting: a change in the board needs an elevation. |
| `allow_local_remotes` | `false` | Lets a project use a `file://` URL or an absolute local path of a bare repository as a remote. For tests and for a repository on this machine. A danger setting. |
| `[taildrive_mounts]` | empty | A table `"<machine>/<share>" = "/abs/path"`: the folder where this station mounts a Taildrive share (§13.13). At most 16 entries. The key has the same form as `artifact_share`. The value is an absolute path without `..`. |

All five keys apply after a station restart. Edit them in the board (`#/settings`, the station section "Fleet Git") or with `fleetdeck settings set <key> <value> --station`. The labels in the board are "Clone folder", "Station labels", "Allow check commands", "Allow local remotes" and "Taildrive mounts". The station reports `clone_root`, `allow_check_cmd`, `allow_local_remotes` and its Git version to the deck.

```toml
clone_root = "~/projects"
traits = ["gpu", "always_on"]
allow_check_cmd = true

[taildrive_mounts]
"nas/artifacts" = "/mnt/artifacts"
```

### 13.3 Project Git settings

Open the project page, then the section "Git sync" of the project form. Only the owner can save. The save goes to `PUT /api/projects/{id}/git`. A refused value gives 400 `invalid_git_settings`, and the board shows the message under its field.

| Field in the board | Default | Rule |
| --- | --- | --- |
| Sync this project across stations | off | Needs the remote URL. Only one sync project can own a folder on a station, and a sync folder cannot be inside another one. |
| Remote URL | filled from the origin of the first home | An https, ssh or `user@host:path` URL without a password. Fleet Deck compares the exact form. A change to another repository needs a confirm (§13.8). |
| Push branches | Off | "After each turn" or "When the session ends". Needs sync. |
| Remote | `origin` | The remote name for fetch and push: 1 to 64 characters of `A-Za-z0-9._-`, not starting with `-` or `.`. |
| WIP remote URL (optional) | none | Pushes go here instead of the remote, for example a bare repository on an always-on server. |
| Default branch (optional) | the branch that the remote names | A branch name. |
| Clean up merged worktrees | Off | "When merged". Needs sync. |
| After (hours) | 24 | 1 to 720. |
| Copy ignored files into new worktrees | none | One relative path per line, at most 32. A `*` or `?` matches inside one name. No `..`, no `**`. |
| Environment for agents and setup | none | One `NAME=value` per line, at most 32. `PATH`, `HOME`, `LD_*`, `DYLD_*`, `GIT_*` and the provider and Fleet Deck variables are refused. |
| Artifact share (optional) | none | `<machine>/<share>`, for example `nas/artifacts` (§13.13). |

The settings land mode, check command and radar home are part of the same settings record. This release has no board field for them, and the board form sends them unchanged. To set them, send the full settings and the current `revision` in `PUT /api/projects/{id}/git` as the owner (`"land": "pr"` or `"merge"`, `"check_cmd": "<command>"`, `"radar_home": "<station id>"`).

- **Land mode.** Empty means `pr` when the project has a forge binding, else `merge`.
- **Check command.** 1 to 4096 characters. It runs only on a station with `allow_check_cmd = true`.
- **Radar home.** A station that has a home of the project. Empty means the first home on a station with the label `always_on`, else the first home. The deck works this out and sends the result to the stations.

A save sends the new settings to every home station. A station that is offline gets them at its next link.

### 13.4 Git hooks stay off (design §2.3)

Every Git command that a station runs for Fleet Git uses the hardened runner with `core.hooksPath=/dev/null`. This holds for fetch, push, clone, rebase, merge-tree and worktree add. A `pre-push` hook of a repository never runs, and no other hook runs. A project that needs a check before a push uses the check command of the land flow (§13.11).

### 13.5 Push credentials of a service, and Landlock

The station runs each network Git command with your user configuration: your SSH agent and your credential helper. Fleet Deck adds none.

- **Service.** A service does not get the environment of your shell. The Git commands of the service need credentials that the service user can reach: an SSH agent or a credential helper. Run `fleetdeck service install` from a terminal, because the service takes the safe folders of the `PATH` of the shell that runs it (§4.1). Test it as the service user: run `git ls-remote <remote url>` in a home.
- **Landlock.** On Linux 5.13 or later, the commands that write into a repository (fetch, push, clone, rebase, worktree add) run under `confine-exec`. They can write only inside the allowed roots (§11, "Landlock is not available"). A credential helper that writes outside the allowed roots fails there. Examples: `git-credential-store` with a file outside the roots, the credential cache socket, and a `known_hosts` update. Git ignores a failed `credential approve`, and SSH only warns about `known_hosts`. So a later push can ask for the credential again. Use an agent, or a helper that only reads. Connect once to each Git host from a terminal, so that `known_hosts` has its key.
- **Repository configuration.** A repository can still name programs in its own configuration (`core.sshCommand`, `credential.helper`). This is the DL-142 position: the agents run as the same user.

### 13.6 Pre-upgrade backup, restore and forward repair

Fleet Git adds tables to `deck.db` and `station.db` (internals §11). Each migration only adds. Before a process runs pending migrations on an existing database, it writes a consistent copy of that database.

- **Where.** `<data>/backups/<deck|station>-v<from>-to-v<to>-<unix ms>.db`, for example `~/.local/share/fleetdeck/backups/station-v27-to-v30-1760000000000.db`. The folder has mode 0700 and the files 0600. Fleet Deck keeps the newest 3 backups of each database.
- **No backup, no migration.** When the backup cannot be written, or the free space is less than twice the size of the database, the process does not migrate and stops with: "Fleet Deck cannot write the pre-upgrade backup <path>: <reason>. Free disk space, or start once with FLEETDECK_SKIP_UPGRADE_BACKUP=1 to upgrade without a backup." The value `1` skips the backup and logs a warning. Use it once, only when you accept the risk.
- **Older binaries refuse a newer database** (`DbError::TooNew`). Never lower `user_version` by hand to make an older binary start.

> **Caution:** a restore loses every deck or station write since the upgrade. This includes sessions, mail and tasks, not only Fleet Git data.

Restore (only when the new version has not written data that matters yet, and you must run the older binary):

1. Stop the service: `fleetdeck service stop`. Stop any other Fleet Deck process.
2. Find the backup with the `<from>` version of the older build: `ls ~/.local/share/fleetdeck/backups/`.
3. Move the live database and its `-wal` and `-shm` files to a safe folder. For the station these are `<data>/station/station.db*`. For the deck they are `<data>/deck/deck.db*`.
4. Copy the backup file to the live name (`station.db` or `deck.db`) and run `chmod 600` on it.
5. Start the older binary.

Forward repair: when the new version has written data that matters (homes, lands, pushes), do not restore. Install a fixed newer build. At each deck start, the deck compares the legacy station and path of each project with its first home, repairs a difference from the home and logs it (`projects_legacy_repaired`). The first home is authoritative.

### 13.7 Fetch, push, cleanup and bootstrap

For a project with sync on, a session that starts in a verified, ready home of the project gets these behaviors.

- **Fetch before a start (FG-1).** A new worktree start fetches the remote first, then makes the branch `fd/<station>/<callsign>` from the default branch (an owner base or branch wins). A failed fetch does not stop the start: the session starts from the local `HEAD` and shows the chip "Stale base" with the reason. A local branch `fd/<station>` blocks the new branch (`branch_namespace_blocked`): rename it.
- **Station name in branches.** The station part is the station name in lower case, with every character outside `a-z`, `0-9` and `-` changed to `-`. The deck reserves it for one station in a repository (`slug_taken`). A rename of the station does not free it. The project page shows "Git names".
- **Push (FG-2).** The station pushes only the branch of its own session, to the WIP remote when set, else to the remote. It pushes after each turn or when the session ends (the setting). The lease is the last commit that the station pushed, so a revert pushes with force only over its own commit. The station never pushes an owner branch, tags or other refs. A failed push tries again after 1, 2, 4, 8 and then every 10 minutes, with no last attempt, and survives a restart. The card shows "Not pushed" and a "Push now" button. A push that Fleet Deck stopped on purpose (for example after a land) shows "Push stopped" when commits are not pushed.
- **Cleanup (FG-3).** For "When merged", the station moves a worktree to the trash (restorable, never a branch delete) after all of these are true: the session ended at least the set hours ago; no land holds it; the worktree is on its own branch; a fetch worked; its last commit is on the default branch of the remote; the worktree is clean; and the last commit is pushed (when push is on). A squash merge or a rebase merge of a forge is not detected: trash that worktree yourself. The session menu shows why a worktree is kept.
- **Bootstrap (FG-4).** After a worktree start, the station copies the listed ignored files (each at most 1 MiB, 10 MiB in total; never a link, never over an existing file) and gives the environment to the agent and to the setup command. The chat shows "Not copied: <path> (<reason>)" for a skipped file. An in-place start in a home gets the environment only.

### 13.8 Homes on several stations

A project has one home on each station that works on it: a clone of the repository. The first home is the one that you made the project with. The project page section "Homes" lists them as "<station> · <path> · Ready", "Cloning into <path>…", "Not verified: <reason>" or "Failed: <reason>".

- **Add a home.** Needs the remote URL of the project. Select "Add home", pick a station, then confirm. The station clones the URL into `<clone_root>/<project name>` (characters outside `A-Za-z0-9._-` become `-`, at most 80 characters). The board shows "Cloning…" and reads the state again every 5 seconds. A clone has a limit of 15 minutes.
- **A folder that already exists.** The station uses it only when it is a complete clone with the right remote. A folder that holds an incomplete clone is never used and never deleted. Remove it yourself, then retry.
- **Retry.** A failed home has "Retry". After a station restart during a clone the home is failed ("The clone stopped when the station restarted. Remove <path>, then retry.").
- **Remove.** "Remove" is not on the last home. It is refused while a clone runs, while a land of the project uses that station, or while a session runs in the home. It deletes no file.
- **Rebind (change the remote).** Save a URL that names another repository: the board asks "Rebind the project to <repository>?". With sync on, every home is "Not verified" until its station checks the new remote. With sync off, the homes stay ready. A rebind is refused while a home is busy.
- **Not verified.** A home that does not match the project remote shows the reason: no remote of that name, a remote with more than one URL, a different repository, or a folder that another sync project owns. Fleet Deck never changes the configuration of a home. Fix the remote by hand.
- **Local remotes.** A local remote needs `allow_local_remotes = true` on every home station of the project. Else: "This station does not allow a local remote."
- **Start in a project.** The start dialog has a "Home" select. A start in a home that is not ready gives 409 `home_unverified` or `home_not_ready`.

### 13.9 Placement by traits

Each station reports its traits: the system (`os:linux`), the architecture (`arch:x86_64`), whether it runs on battery, and its `traits` labels. The Stations page shows one line, for example "linux · x86_64 · on battery · gpu, always_on".

In the spawn form, "Auto (best station)" appears when a station reports traits. Its fields:

- "Needs": the labels, `os:<os>` or `arch:<arch>` that the station must have. A station without traits matches no requirement.
- "Prefer": "Idle station", "Not on battery" and "Always-on station" (the label `always_on`).
- "Spread over stations" for a batch.

The deck ranks the stations that can start the session: first by the number of preferences met, then by free resources, then by the fewest live sessions, then by name. It never calls a model. A station in WSL always counts as not on battery. When no station matches: 409 `no_station`, "No station matches: <the first unmet requirement>." The same rules serve `POST /api/placement`, the batch start of a project with `home = "auto"` and `assign auto` with `requires` and `prefer`.

### 13.10 Conflict radar

Fleet Git adds a prediction to the radar of the fleet screen. The radar home of the project (§13.3) fetches the remote and tests each pair of pushed branches, and each branch against the default branch, with `git merge-tree`. It changes no ref and no file.

- The run starts every 5 minutes, and 30 seconds after a push of the project. It tests at most 50 pairs in 120 seconds.
- Only branches that the stations pushed take part. A session with push off shows only in the older "Conflict" rows (the same files changed).
- The panel groups the rows by project. A prediction shows "Will conflict" with the first 3 files and "+<n>" for more.
- The panel footer shows the reason when a run fails: the fetch failed, Git older than 2.45, or the radar station is offline (then the results are from an earlier time).

### 13.11 Land

"Land" rebases the branch of a session onto the default branch in a separate folder, checks it, and publishes it. The session folder is never changed.

Needs: a sync project, a session that Fleet Git started in a worktree, a ready home, a session that has ended or is idle, and a station with the land feature. Only the owner can land from the board. A coordinator with the `manage` class can request a land (§13.14).

1. Open the session, select "Land", and confirm the dialog "Land <branch>?".
2. The deck puts the land in a queue. One land of a project, and one land of a repository, runs at a time. The status line shows "Queued (<n> ahead)", then "Landing: <phase>". The project page section "Lands" shows the queue and the last 20 ends.
3. The station fetches, fixes the session commit and the default branch commit, and rebases the commit in a temporary folder under `<allowed root>/.fleetdeck-land/`. Git needs `user.name` and `user.email` on the station (`land_no_identity`).
4. When the project has a check command and the station has `allow_check_cmd = true`, the station runs it with `/bin/sh -c` (10 minutes, the project environment). The check must not change the tested commit or a tracked file (`check_mutated`). A failed check shows the last 50 lines. A station without `allow_check_cmd = true` refuses a land with a check command (`check_disabled`).
5. The station pushes the branch (to the WIP remote when set). In `pr` mode it then opens or updates a pull request. The branch must be on the repository of the forge, so a WIP remote that is not the forge gives `land_pr_needs_forge_branch`: choose `merge`, or clear the WIP remote. In `merge` mode the station pushes the result to the default branch, never with force.
6. When the default branch moved during the land, the station fetches, rebases and checks once more. A second move gives `remote_moved`: land again.

During a land the session is held: no prompt, resume, revert, shell, trash or push. After the branch push, the push of that session stops ("Push stopped"), unless the local branch already holds the published commit. So the old local commit is never pushed over the rebased branch.

What to do for each end:

| End | Action |
| --- | --- |
| "Landed <commit>", or "Pull request #<n>" | Nothing. |
| "Conflict in <n> files" | Select "Send to session". The agent gets a fixed message with the base commit and the files. When the session ended, the button reads "Resume the session first". |
| "The check command failed" | Read the output tail, fix, land again. |
| A queued land | "Cancel land" works only on a queued land. |
| "Checking the result of the land after an interruption…" | Wait. The station checks the remote. The queue waits for it. |
| "The station has no record of this land…" or the result is unclear | Look at the remote, then select "Stop checking". The queue is free only after the station confirms ("Stopping… waiting for <station> to confirm."). Fleet Deck does not know whether the land changed the remote. |
| The station was removed during the land | "Forget" frees the queue. A station that still runs can still push. |

### 13.12 Config drift report

The Stations page can compare the agent configuration of each station with one reference station. It reports four items: `~/.claude/CLAUDE.md`, `~/.claude/agents/`, `~/.claude/skills/` and `~/.codex/AGENTS.md`, and the Claude and Codex versions.

1. Open the Stations page.
2. In the menu of one station select "Use as config reference". The header shows "Config reference: <station>" with "Change".
3. Read the line of each station: "Config: Same", "Config: Differs: <labels>" (`CLAUDE.md`, `agents`, `skills`, `AGENTS.md`, `claude`, `codex`), "Config: Reference" or "Config: Unknown".

Each station computes the hashes at start and every 10 minutes, and never sends a file content. A hash does not use the absolute path. A limit (10000 entries, 4 MiB for a file, 64 MiB in total) gives `too_large`. Nothing is synchronized: fix a difference by hand.

### 13.13 Taildrive artifact share

Taildrive can give the sessions of a project one shared folder for outputs. **Policy:** Fleet Deck runs only `tailscale drive list` and `tailscale status --json`. It never runs `tailscale drive share`, `rename` or `unshare`, and never changes the tailnet policy. Taildrive is never used for code, worktrees, homes, remotes or config. A test makes sure that the source holds no other `drive` call.

Each station lists its shares on the Stations page ("Taildrive shares"), every 10 minutes. A share shows a warning when it exposes a project home, a session worktree, the Fleet Deck data folder, the Claude or Codex home, `~/.ssh`, `~/.gnupg`, `~/.aws` or a path that the member deny list hides. A share that Fleet Deck cannot check completely (more than 20000 entries, deeper than 8 folders, a link to a folder, a folder that it cannot read, or a path with trailing spaces) is not safe either. **An exposed or unverified share is never an artifact share.** Fleet Deck cannot stop your tailnet policy from exposing a folder.

Steps for an artifact share:

1. On the machine that holds the folder, make the share with the Tailscale CLI yourself. Taildrive must be on in your tailnet (the `drive:share` attribute). Choose a folder that holds none of the paths above.
2. On the Stations page, check that the share shows without a warning.
3. In the project Git settings, set "Artifact share (optional)" to `<machine>/<share>`. The machine is the tailnet machine name in lower case. The save is refused with `share_exposed` or `share_unverified` when the exporting station reports the share as exposed or unknown. This is only a hint at the save: every use is checked again.
4. Choose how a session station reaches the share.
   - **The share is on the session station.** Nothing more. The station uses the share path.
   - **The share is on another machine.** Mount it as a writable folder for the station user. Add the mount to `station.toml` as `"<machine>/<share>" = "/mount/path"` in `[taildrive_mounts]`, then restart the station. The folder must exist, be a folder and be writable, and must not be inside a protected path.
   - **No mount.** The agent gets only the address `FLEETDECK_ARTIFACTS_URL` (`http://100.100.100.100:8080/<tailnet>/<machine>/<share>/<project>/<callsign>/`). No folder is made, and the board shows the address with "Copy address".
   - **The path has trailing spaces.** The share is "ambiguous". Add it to `[taildrive_mounts]` with one exact path.
5. Start a session of the project. The station makes `<path>/<project folder>/<callsign>/` (mode 0700) and sets `FLEETDECK_ARTIFACTS` to it, with a final `/`. The session shows the "Artifacts" link, which opens the folder in the file browser. A member of a shared session sees it with the member deny list.

Fleet Deck asks for a fresh check at each start, each resume, each access and every 5 minutes. When the share is not verified any more, the chat shows "The artifact share is not verified now…" and the link stops. The running agent keeps `FLEETDECK_ARTIFACTS` until its session ends: an environment variable cannot be taken back. A machine that is not a Fleet Deck station (for example a NAS) cannot be checked: only the mount checks apply, and the board says so. When a start cannot use the share, the session starts without the variables and the chat says why (`share_exposed`, `share_unverified`, `mount_missing`, `mount_not_folder`, `mount_not_writable`, `tailnet_unknown`).

### 13.14 The coordinator across the fleet

The Branches view of a session of a sync project has an "Other stations" group. It lists the `fd/<station>/…` branches that the other stations pushed, with the session, "↑<ahead> ↓<behind>" and "Will conflict". "Fetch" runs one fetch on the radar home. A branch list that takes more than 18 seconds fails. This covers the backlog item B1.

A coordinator session gets three Fleet Git tools (ADR-009 §7):

| Tool | Class | What it does |
| --- | --- | --- |
| `project_branches` | `inspect` | The branches of the project on other stations. The commit subjects are fenced as untrusted text. |
| `project_fetch` | `inspect` | One fetch on the radar home. It changes only remote-tracking refs. |
| `land_session` | `manage` | Requests a land (§13.11). The deck stores the grant and checks it again at each step. A renewal, a revocation, an expiry (10 minutes) or the kill switch ends the land with `authority_lost`. |

The tools need the link features `coordinator.v2` and the Fleet Git feature. The kill switches `fleet_control` and `allow_fleet_control` (§7.3) apply. There is no `move_session` tool (FG-9).

> **Known gap:** the deck makes every coordinator grant with only the station of the coordinator session. ADR-006 says that the owner picks the target stations. So a coordinator reaches the stations of other homes only after a change of the grant data, and these tools see one station in practice. A coordinator can request a land, but no tool reads the progress of a land yet.

### 13.15 Troubleshooting Fleet Git

| Symptom | What to do |
| --- | --- |
| The card shows "Stale base" | The fetch before the start failed. The session started from the local `HEAD`. Check the credentials of the station (§13.5), then start a new session. |
| The card shows "Not pushed" | The title has the reason and the next try. Typical: `push_auth` (credentials), `push_rejected`, `remote_mismatch`. Fix it, then select "Push now". |
| A home is "Not verified: …" | Read the reason (§13.8). Fix the remote of that home by hand. A rebind makes every sync home wait for the check. |
| A home is "Failed: …" | `clone_failed`, `destination_reserved`, `incomplete_folder` or `clone_root_missing`. Fix the cause, remove an incomplete folder, then select "Retry". |
| A start fails with `branch_namespace_blocked` | A branch `fd/<station>` exists. Rename it, for example `git branch -m fd/<station> fd-<station>`. |
| A start fails with `slug_mismatch` | The station name changed. Try again in one minute. |
| 409 `slug_taken` | The Git name of this station is reserved for another station in this repository. Rename this station. |
| `git_too_old` | Update Git on the station to 2.45 or newer. |
| A land waits in "Queued" | Another land of the project or of the repository runs. A land that is uncertain keeps the queue until "Stop checking". |
| The station does not start: "Fleet Deck cannot write the pre-upgrade backup" | See §13.6. |
| Radar footer: "the fetch failed" | The radar station cannot reach the remote. Check its credentials. |

## 14. Provisioning media custody (P8)

This procedure belongs to the Omarchy provisioning of a new machine (`design/omarchy-p8.md` section 4; `design/omarchy.md` §9A, §9B). It is part of the P8 acceptance. "No Fleet Deck secret" on the medium closes the invitation custody. It does not close the LUKS and Tailscale custody. This section does.

### 14.1 Supported media

- Use an owner-controlled LUKS-encrypted POSIX removable filesystem, or a read-only `cidata` ISO whose host image is held on encrypted storage in a 0700 directory as a 0600 file.
- Do not require Unix mode preservation inside an ISO.
- FAT and exFAT media are not accepted for secret-bearing P8 media.

### 14.2 Private copy

- The installer copies only `fleetdeck/provision.toml` into `<config home>/fleetdeck/provision.toml`, and creates the destination privately as 0600.
- It does not keep source modes. It does not run source-media scripts.
- It rejects source symlinks, nonregular files and files above 16 KiB.
- It publishes the destination atomically. It does not replace a different existing file.

### 14.3 Detach

After the installation, detach or eject the medium. Remove the virtual CD attachment.

### 14.4 Retention and disposal

- Keep a source only in the protected encrypted location, with access limited to the owner.
- Disposal includes ISO copies, templates, snapshots and backups.
- File unlinking or a flash overwrite alone is not claimed as secure erasure.

### 14.5 Distinct credentials

Each machine gets a distinct Tailscale enrollment key and a distinct disk-encryption credential. Do not clone an initialized Fleet Deck identity, Tailscale state or bootstrap state.

### 14.6 Recovery after an exposure

The recovery depends on the credential. Do these four steps in order:

1. Revoke the exposed Tailscale auth key.
2. Inspect and remove unauthorized Tailscale nodes. Revoking the auth key does **not** deauthorize nodes that are already enrolled with it ([Tailscale auth-key documentation](https://tailscale.com/docs/features/access-control/auth-keys)).
3. Replace the exposed LUKS passphrase or keyslot credential. If an attacker also had a disk or header copy, a credential replacement alone does not protect that copy. Require a disk-key replacement or a reinstall.
4. If a wrong Fleet Deck fingerprint was approved, remove that station from the deck. Removing the medium or revoking a Tailscale auth key does not revoke the Fleet Deck enrollment.

## 15. Provision a new Omarchy station (P8)

This section gives the owner steps to add a new Omarchy machine as a station. The design is in `design/omarchy-p8.md`. The built version is recorded in DL-234.

> **Note:** the production use of this procedure waits for the release task of the owner. That task supplies the pinned `fleetdeck-bin` package, its SHA-256, the trusted manifest `/etc/fleetdeck-provision-release.json` and the installer image step. Until then, the bootstrap on a real machine stops with `release_untrusted`.

After the one first login, every step happens on the board or on your own deck machine. You do not log in to the new station again.

### 15.1 Steps

1. On the deck machine, make the provisioning file:

   ```sh
   fleetdeck deck provision-file <name> --deck <tailnet name>:7421 --out <private folder>/provision.toml
   ```

   - The folder must be yours and must have mode 0700. A folder below `/tmp` is refused.
   - The command prints "Provisioning file written." on stderr.
   - The command refuses an output path that already exists.
2. Put the file on a supported medium (section 14.1) as `fleetdeck/provision.toml`.
3. Install the machine with the trusted installer image of the owner's release task. The image installs the pinned package and the trusted manifest. The image also copies the file into the config folder of the user. It does this with the hidden command `fleetdeck provision-import`. The image runs this command, not you.
4. At the first login, the hook `/usr/lib/fleetdeck/provision-first-login` starts the bootstrap `fleetdeck provision-bootstrap`. A notification "Fleet Deck pairing" shows the text "Approve this station fingerprint on the board:" and the fingerprint of the new station. The fingerprint has 16 groups of four characters.
5. On the board, open Stations and select "Add station". Select "A computer on my tailnet". In the list "Can be added", select "Add" on the row of the new machine. The board asks for a recent sign-in for "Accept a new station for 10 minutes". The pairing window is then open.
   - Do not run the command that the board shows for the machine. The new station asks by itself.
   - The request card shows "<name> asks to join this deck". Compare all 16 groups with the fingerprint in the notification on the new machine.
   - Approve only if every group matches. Select "Yes, the codes match. Add <name>". The board asks for a recent sign-in for "Pair station <name>".
6. The bootstrap waits for the link. Then it checks the providers and installs the Omarchy integration. The station appears on the board.

### 15.2 A refused or cancelled pairing

- If you select "No, refuse", or the pairing is cancelled or revoked, the station asks again after 600 seconds. Open the pairing window again on the board (step 5). The new request card shows the same fingerprint.
- If the window is closed or ended, the station asks again after 30 seconds. Open the window on the board. The request arrives by itself.
- A request needs an open window and your approval of the exact fingerprint. A repeated request gives nothing to anyone else.

### 15.3 Terminal states

A terminal state stops the bootstrap. The bootstrap keeps the code and does not clear it at the next login. Each state needs a repair. This version has no board path for these repairs. This is an open item (DL-234). It is not an instruction to log in to the machine.

| Code | Cause |
| --- | --- |
| `paired_elsewhere` | The station is already paired with a deck that has another fingerprint. |
| `station_identity_changed` | The station key is not the key that the bootstrap saved first. |
| `config_changed` | The provisioning file changed after the pairing started. |
| `release_untrusted` | The trusted manifest or an installed hook file is missing, is not owned by root, or does not match its hash. |
| `build_mismatch` | The running build or the binary of the station does not match the trusted manifest. |
| `service_conflict` | The station service that is installed has another binary or another data folder. |
| `provider_probe_failed` | The station reports a configured provider as `broken` or `unknown`, or without a version. |
| `integration_failed` | The Omarchy integration install or its status check did not succeed. |

### 15.4 Cloned machines

Do not clone an initialized machine. Each machine needs its own medium credentials and its own provisioning (section 14.5). Two machines that start from one clean medium get two different fingerprints.
