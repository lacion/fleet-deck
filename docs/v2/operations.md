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
| `offline_approval_policy` | `wait` | What happens to an approval when no deck is reachable: `wait`, or `deny_after(<n>s)` (also `m` or `h`). |
| `listen` | (empty) | Link listen addresses when the deck dials this station. |
| `host` | `direct` | `direct`: the agents are children of the station and stop with it. `shim`: each agent runs under a `fleetdeck shim` process and survives a station restart or upgrade (ADR-005). |
| `shim_buffer_mib` | `16` | The shim output buffer, 1–256 MiB. |
| `hibernate_after_min` | `0` (off) | Minutes of idleness after which the station stops a live, idle session gracefully (`session.hibernated`). The board resumes it with the next input (R6.6, DL-169). Up to 10080. |
| `repo_groups` | (empty) | Repository path → group name, for the conflict radar. |
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
- With `host = "direct"` (default), the station ends its agents when the service stops. Set `host = "shim"` in `station.toml` to keep them running.
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

**Current limit:** the station gives the grant credential only to a provider version that the opt-in coordinator live smoke test (ADR-006 §8.5) verified (`fleetdeck_providers::coordinator::VERIFIED` in `crates/fleetdeck-providers/src/coordinator.rs`, DL-219). Claude 2.1.296 and later is verified. A Codex coordinator start fails with `handoff_unverified`, and so does a start on a station where any Claude instance has an older or unknown version.

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

- Auto-continue works only with `host = "direct"`. With `host = "shim"`, the station skips a lost session with the reason `unsupported`.
- The station checks every ADR-007 rule when it stores the continuation. Right before the dispatch, it checks again only these rules: identity, proven end of the old process, newer owner work, requested stop and the time window. It does not check again the preference, the turn state, the capability, the workspace or a free session slot.

### Resume on the shim host (DL-182)

With `host = "shim"` a resume starts a new shim and a new agent process. It is never a reattachment. The board shows `resumed`.

- Resume on the shim host works on Linux only. On another platform the station refuses `session.resume`, `session.import`, the input of a hibernated session and a handover with `unsupported` ("resume on the shim host needs Linux").
- The station resumes a session only when it can prove that the earlier agent run ended. The proof is one of these: a clean exit that the shim checked completely, a launch that never started, a verified reboot (the boot id differs from the one that the run stored), or your reconciliation.
- A reboot proof is checked again at the resume. If the boot id is the old one again (for example after a restore of the VM memory) or a process with the stored identity lives, the resume is refused.
- Reconciliation: for a run whose end is not proven, the station method `session.execution_check` lists the processes that still belong to it (the agent, its process group and its session). Stop them on the station. Then the command `session.reconcile_execution` marks the run as ended. The station checks the processes again and refuses while one lives or when the check is incomplete. The board page for this action comes in a later release; the station methods exist now.
- Detached hooks: the station cannot see a process that left the agent's process group and session (it called `setsid()`). A hook that detaches and keeps a writable descriptor on the transcript can write to it after a resume. The direct host has the same limit. The check lists such processes for information only.
- Rollback: a launch journal of this release has a `boot_id` field. A station of an earlier release reads that journal as unknown evidence and fails closed: the session stays `lost` there. A shim of this release speaks the shim protocol 2 and the previous version 1 (an unavailable process count is `u32::MAX` on version 1).
- Auto-continue stays off with `host = "shim"`.

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
