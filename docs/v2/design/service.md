# Design: `fleetdeck service` (start at login or boot)

Status: revision 2, built (DL-186). Revision 1 had an advisor review (Codex); §13 maps each point.

## 1. Goal

One command makes the deck, the station, or both a service of the operating system that starts automatically and restarts after a failure. The owner needs no knowledge of systemd, launchd or the Windows Task Scheduler.

Platforms (PLAN D12, A11): Linux and macOS. Windows means WSL2 (Linux); a native Windows build does not exist.

## 2. Facts (verified on the owner's machine, WSL 2.6.3, Ubuntu, 2026-10-14)

- systemd runs as PID 1 under WSL when `/etc/wsl.conf` has `[boot] systemd=true`; `systemctl --user` works; `loginctl` linger works.
- A systemd user service starts a Windows program: `systemd-run --user --wait --pipe "/mnt/c/Program Files/Tailscale/tailscale.exe" version` printed `1.102.4`, without `WSL_INTEROP` in the service environment.
- A systemd user service gets a minimal `PATH`, so `claude` and `codex` (for example in `~/.local/bin` or under nvm) are not found unless the unit sets `PATH`. A systemd user service also inherits the environment of the user manager; a unit cannot clear that.
- WSL stops a distro when no Windows client holds it. A working pattern on this machine (another product): a Windows logon task that runs `conhost.exe --headless wsl.exe -d Ubuntu --exec /bin/sleep infinity`.
- The v1 product owns `~/.config/systemd/user/fleetdeck.service` (active). v2 does not use that name.
- The board restart (DL-172) executes `/proc/self/exe` again in the same process (same PID). It restarts the running build; it does not switch to a new build.
- The station upgrade (DL-400) keeps releases under `<data>/station/bin/` with the links `current` and `previous` (`Launcher`), and "service units run `<root>/current`".
- The default provider host is `direct`. Agents survive a station stop only with `host = "shim"` (ADR-005).

## 3. Command

```
fleetdeck service install   [both|deck|station]   (default: both)  [--dry-run]
fleetdeck service uninstall [both|deck|station]   (default: every installed Fleet Deck v2 service)
fleetdeck service start     [both|deck|station]   (default: every installed one)
fleetdeck service stop      [both|deck|station]   (default: every installed one)
fleetdeck service status    [--json]
```

- `both` is one service that runs `fleetdeck up` (the deck and a local station in one process; the local station pairs over loopback by itself, DL-061). `deck` runs `fleetdeck deck serve`. `station` runs `fleetdeck station serve`. `deck` and `station` can be installed together.
- Conflict: `install both` when `deck` or `station` is installed, or `install deck|station` when `both` is installed, fails before any change: "The <kind> service is installed. Remove it first: `fleetdeck service uninstall <kind>`."
- One installation per OS user. Each service file records its data directory (§4). `install` of a kind that is installed for another data directory fails before any change: "The <kind> service runs Fleet Deck with the data directory <dir>. Remove it first: `fleetdeck service uninstall <kind>`."
- `install` of an installed kind (same data directory) is an update: the program (§4), the file and the environment are written again, and the service restarts.
- `--dry-run` prints the files and the commands and changes nothing (no file, no command, no program copy).
- When `--data-dir` (or `FLEETDECK_DATA_DIR`) is set, the service gets `--data-dir <absolute directory>`.

## 4. What the service runs

- Program: a managed copy, never the build directory. The launcher root is `<data>/station/bin` for `both` and `station` (the station upgrade launcher) and `<data>/deck/bin` for `deck`. The service file names the link `<root>/current` (the absolute path of the link; the link is not resolved in the file).
- `install` (not `--dry-run`): when `<root>/current` does not exist, or its target file is not byte-equal to the running binary (`std::env::current_exe()`), the running binary is copied as a new release `<root>/releases/<version>-svc-<ulid>/fleetdeck` (mode 0755) and `current` points at it (`previous` at the old target; the existing `Launcher` code). A broken `current` link (target missing) is replaced the same way. Then every release directory whose name contains `-svc-` and that is neither the target of `current` nor of `previous` is removed. Releases of the upgrade flow (`-upd-`) are never removed.
- So a new build reaches the service when the owner runs `fleetdeck service install` with that build. The output ends with: "Run `fleetdeck service install` again after you install a new Fleet Deck version or a provider CLI in a new folder."
- With `both`, `fleetdeck station upgrade` also changes the deck program; the deck migrates its database at its start (open item: a whole-node upgrade with a deck trial is not in this design).
- Arguments: `[--data-dir <dir>] up` | `deck serve` | `station serve`.
- Environment, copied at install time: `PATH`; `XDG_CONFIG_HOME` and `XDG_DATA_HOME` when set and absolute (a relative value fails the install); plus `FLEETDECK_LOG_FORMAT=json`. No other variable is written to a service file, so no secret enters a file. (systemd user services also inherit the user manager's environment; the file does not clear it.) Only the safe folders of `PATH` are written (operations guide §4.1); the output gives one `PATH:` count line (`--verbose` lists the left-out folders) and warns when `claude`, `codex`, `git`, `gh`, `glab` or `curl` is lost.
- A marker line in each service file records the data directory: systemd `# fleetdeck-data: <dir>`; launchd a comment `<!-- fleetdeck-data: <dir> -->`. `<dir>` is the resolved `NodeDirs.data`.
- Station and `both`: only the main process gets the stop signal (systemd `KillMode=process`, launchd `AbandonProcessGroup`). With `host = "shim"` the agents keep running through a stop or a restart; with `host = "direct"` (the default) the station ends its agents when it stops. The `install` output names the current host.

## 5. Linux (systemd user services)

- Check, in this order (each command with a 5 s limit):
  1. `/run/systemd/system` is a directory, else systemd is not the init system: under WSL (`/proc/sys/kernel/osrelease` contains `microsoft`, case-insensitive) the text is "systemd is off in this WSL distro. Add these two lines to /etc/wsl.conf (needs sudo): `[boot]` and `systemd=true`. Then run `wsl.exe --shutdown` in Windows and open the distro again."; else "This machine does not run systemd. Run `fleetdeck up` under your own process supervisor."
  2. `systemctl --user show --property=UnitPath --value` succeeds, else "The systemd user manager does not answer (<stderr, first line>). Log in to this user once (not with `su`), then run the command again."
  3. The unit directory is `<config home>/systemd/user`, where `<config home>` is `XDG_CONFIG_HOME` when set, else `~/.config`. It must be one of the space-separated entries of `UnitPath`; else "systemd does not read <dir>; set XDG_CONFIG_HOME the same for your shell and your login, or unset it."
- Files: `<unit dir>/fleetdeck-up.service` | `fleetdeck-deck.service` | `fleetdeck-station.service`, mode 0644, temp file + rename. Content (in this order):
  ```
  # Written by fleetdeck service install. fleetdeck service uninstall removes it.
  # fleetdeck-data: <dir>
  [Unit]
  Description=Fleet Deck <deck|station|deck and local station>
  Documentation=https://github.com/lacion/fleet-deck/blob/v2/docs/v2/operations.md
  After=network-online.target
  Wants=network-online.target

  [Service]
  Type=simple
  ExecStart=<program> <args>
  Environment=<NAME=value>        (one line for each copied variable, then FLEETDECK_LOG_FORMAT=json)
  KillMode=process                (station and both only)
  KillSignal=SIGTERM
  TimeoutStopSec=30
  Restart=on-failure
  RestartSec=2
  UMask=0077
  NoNewPrivileges=true            (deck only: provider CLIs under a station may need sudo or setuid tools)

  [Install]
  WantedBy=default.target
  ```
- Quoting (systemd.service and systemd.exec syntax):
  - Every text: a control character (below 0x20, or 0x7f) fails the install before any change.
  - `ExecStart` program: written in double quotes; `\` → `\\`, `"` → `\"`, `%` → `%%`; `$` stays.
  - `ExecStart` arguments: each in double quotes; `\` → `\\`, `"` → `\"`, `%` → `%%`, `$` → `$$`.
  - `Environment=`: the whole `NAME=value` in double quotes; `\` → `\\`, `"` → `\"`, `%` → `%%`; `$` stays.
- Commands: `systemctl --user daemon-reload`; `systemctl --user enable <unit>`; then the start step (§8) runs `systemctl --user restart <unit>`.
- Start at boot without a login (linger): `loginctl show-user <uid> --property=Linger --value`; when `no`, `loginctl --no-ask-password enable-linger <uid>` (10 s limit). The output says: "Linger is on for <user>: your user services start at boot, without a login." On a failure: "Your services start when <user> logs in. To start them at boot, run: `sudo loginctl enable-linger <user>`." `uninstall` does not change linger (other services can need it).
- Logs: `journalctl --user -u <unit>`.

## 6. WSL: start the distro at Windows logon

Only under WSL (`tailscale_wsl::is_wsl()`), after the systemd part succeeded. A failure in this section is a warning, never an error: "The services start when WSL starts. Fleet Deck could not add the Windows logon task (<reason>)."

- The distro is `WSL_DISTRO_NAME`, accepted only when it matches `^[A-Za-z0-9._-]{1,64}$`. Else the task is skipped with the warning (reason "the distro name has other characters").
- One Windows program: `/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe`, run only when:
  - `/proc/self/mountinfo` parses and passes the `/mnt/c` checks of `c_drive_mount_ok`, and no mount entry has a mount point equal to a component of the PowerShell path below `/mnt/c` (`/mnt/c/Windows`, `/mnt/c/Windows/System32`, `/mnt/c/Windows/System32/WindowsPowerShell`, `…/v1.0`, `…/powershell.exe`);
  - each of those components is a directory, and the last is a regular file, by `symlink_metadata` (no symbolic link).
- Arguments: `-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command -`. The script comes on stdin. The only variable part of any script is the checked distro name, written between single quotes. Every call has a 30 s limit.
- The task: path `\Fleet Deck\`, name `WSL <distro> <SID>`, where `<SID>` is `[Security.Principal.WindowsIdentity]::GetCurrent().User.Value` (computed inside the script). Install script (fixed text; `<distro>` replaced):
  ```powershell
  $ErrorActionPreference = 'Stop'
  $id = [Security.Principal.WindowsIdentity]::GetCurrent()
  $name = 'WSL <distro> ' + $id.User.Value
  $sys = $env:SystemRoot + '\System32'
  $a = New-ScheduledTaskAction -Execute ($sys + '\conhost.exe') -Argument ('--headless "' + $sys + '\wsl.exe" -d <distro> --exec /bin/sleep infinity')
  $logon = New-ScheduledTaskTrigger -AtLogOn -User $id.Name
  $again = New-ScheduledTaskTrigger -Once -At (Get-Date) -RepetitionInterval (New-TimeSpan -Minutes 5) -RepetitionDuration (New-TimeSpan -Days 3650)
  $p = New-ScheduledTaskPrincipal -UserId $id.User.Value -LogonType Interactive -RunLevel Limited
  $s = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -MultipleInstances IgnoreNew -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1)
  Register-ScheduledTask -TaskPath '\Fleet Deck\' -TaskName $name -Action $a -Trigger @($logon, $again) -Principal $p -Settings $s -Force | Out-Null
  Start-ScheduledTask -TaskPath '\Fleet Deck\' -TaskName $name
  'ok'
  ```
  Success: exit status 0 and the last output line is `ok`.
- Status script: prints `absent` when `Get-ScheduledTask -TaskPath '\Fleet Deck\' -TaskName $name -ErrorAction SilentlyContinue` returns nothing, else `state=<State>` (for example `state=Running`). A failed call is `unknown`, never `absent`.
- Remove script: `Unregister-ScheduledTask -TaskPath '\Fleet Deck\' -TaskName $name -Confirm:$false` when the task exists, then `'ok'`. `uninstall` runs it only when no Fleet Deck v2 service remains after the uninstall.

## 7. macOS (launchd user agents)

- Files: `~/Library/LaunchAgents/dev.fleetdeck.up.plist` | `dev.fleetdeck.deck.plist` | `dev.fleetdeck.station.plist`, mode 0644, temp file + rename. Keys: `Label`; `ProgramArguments` (the program link and its arguments, no shell); `EnvironmentVariables` (the copied variables and `FLEETDECK_LOG_FORMAT=json`); `StandardOutPath` and `StandardErrorPath` = `<home>/Library/Logs/fleetdeck/<up|deck|station>.log` (directory created 0700; the log file created empty with mode 0600 when absent); `RunAtLoad` true; `KeepAlive` {`SuccessfulExit` false}; `ExitTimeOut` 30; `Umask` 63; `ProcessType` `Background`; `AbandonProcessGroup` true (station and both only). Every text is XML-escaped (`&`, `<`, `>`, `"`, `'`); a control character fails the install.
- The domain is `gui/<uid>`. `launchctl print gui/<uid>` fails (no graphical login, for example over SSH): the file is written, nothing is loaded, and the output says "Installed. It starts at your next graphical login."
- Loaded check: `launchctl print gui/<uid>/<label>` exit 0 = loaded. Running: the output has a line `state = running`.
- `bootout` absent: exit status 3 or 113, or stderr containing "No such process" or "Could not find service", is "not loaded" and is ignored. Every other `bootout` failure fails the command.
- install: `launchctl bootout gui/<uid>/<label>` (when loaded), write the file, `launchctl enable gui/<uid>/<label>`, then the start step (§8): `launchctl bootstrap gui/<uid> <plist>`.
- start: `launchctl enable gui/<uid>/<label>`; not loaded → `launchctl bootstrap gui/<uid> <plist>`; loaded but not running → `launchctl kickstart gui/<uid>/<label>`.
- stop: `launchctl bootout gui/<uid>/<label>` (stops and unloads; the file stays, so it starts again at the next login).
- uninstall: bootout, then remove the file.
- A user agent starts at login and ends at logout. The output says so. Logs are not rotated (open item).

## 8. Another process and readiness

- Probe: the deck socket (`deck`, `both`) and the station socket (`station`, `both`) answer `LocalClient::connect` within 2 s. A refused connection, a missing socket file or a timeout is "not answering".
- `install` and `start` probe before any change (before `bootout`, `restart`, or a file write) and again just before the start step:
  - The service is active (systemd `is-active` = `active`; launchd running) → the socket belongs to the service; go on (install: restart).
  - The service is not active and a socket answers → another Fleet Deck process runs. `install` writes the files and enables the service (systemd) but does not start it; on macOS it does not bootstrap (RunAtLoad would start it). Output: "Installed for automatic start. Fleet Deck runs now in another process. Stop it (Ctrl-C in its terminal), then run `fleetdeck service start`." Exit code 0. `start` fails with the same text without the first sentence. Exit code 1.
- Readiness after the start step: within 20 s, the socket(s) of the kind answer. Else: "The service did not start. Look at its log: <log command or path>", exit code 1 (the files stay installed).

## 9. Status

For each kind: installed (file present), the data directory (marker line), the program and its target (the `current` link and where it points), active/running, enabled (systemd `is-enabled`); Linux: linger; WSL: the task state (`absent`, `state=…`, or `unknown`); the log command or path. `--json` prints one object: `{ "platform": "systemd"|"launchd", "services": [ { "kind", "installed", "data_dir", "program", "program_target", "active", "enabled" } ], "linger": bool|null, "wsl_task": "absent"|"unknown"|"<state>"|null, "logs": { "<kind>": "<command or path>" } }`.

## 10. Tests

No automated test runs `systemctl`, `loginctl`, `launchctl` or `powershell.exe`. The service code takes a runner: one call runs one program with arguments, optional stdin text and a time limit, and returns the exit status, stdout and stderr (or a time-out or start error). Tests use a recording runner with fixed answers, temporary directories for every path, and a fake socket probe.

- Rendering: systemd quoting of the program, arguments and environment (space, `"`, `\`, `%`, `$`); a control character is refused; `KillMode=process` for station and both only; `NoNewPrivileges` for deck only; plist escaping and keys; the PowerShell scripts (the distro name is the only variable part; a refused name runs nothing).
- Plans (the recorded commands, in order): install `both` (first install: copy, reload, enable, restart, linger); install again with the same binary (no copy, restart); a conflict (`both` over `deck`) and another data directory (no change, no command); another process answers (files written, enable, no start; macOS no bootstrap); readiness time-out; no systemd (the WSL and non-WSL texts); the user manager does not answer; the unit directory not in `UnitPath`; linger `no` → `enable-linger`, its failure is a warning; WSL distro name refused; the task script fails → warning, exit 0; uninstall of the last service removes the task, of one of two does not; launchd: bootout "not loaded" ignored, another bootout error fails, no GUI domain → "next graphical login", start of a loaded idle job → kickstart.
- Program: the first install copies and points `current`; the same binary again makes no copy; a new binary makes a new release and moves `previous`; a broken `current` is replaced; `-svc-` releases other than current and previous are removed; `-upd-` releases stay.
- `--dry-run` makes no file and runs no command.
- `packaging.rs`: the static files in `packaging/` stay as a manual reference and keep their checks. New check: the commands of the generated units (`up`, `deck serve`, `station serve`) are real commands (`assert_real_command`).
- Manual acceptance (owner, on real hosts; not automated): WSL (install, close every WSL window, `wsl.exe --shutdown`, sign out of Windows and in again: the board answers), a Linux host with systemd, and macOS (install, log out and in).

## 11. Documentation

docs/v2/operations.md: a "Run as a service" section (the command, the three kinds, what runs at boot or login on each platform, the copy of the program and when to install again, the PATH note, the host note, logs, uninstall). packaging/README.md: the command first; the manual steps stay below it.

## 12. Not in scope

Native Windows services; root or system-wide services (LaunchDaemons, system units); copying secrets or other environment variables; log rotation on macOS; a whole-node upgrade with a deck trial.

## 13. Advisor review of revision 1 (Codex)

| Point | Decision |
|---|---|
| Automatic replacement of conflicting services needs a rollback | Changed: a conflict fails before any change with the exact uninstall command (§3). |
| One installation per OS user; bind to the data directory | Accepted: marker line and refusal (§3, §4). |
| Agent survival needs `host = "shim"` | Accepted: stated, and named in the output (§4). |
| Adopt a managed binary; never the build directory; broken link fails | Accepted with one change: a broken `current` is replaced by the running binary instead of a failure, because `install` is the command that sets the program (§4). |
| Combined upgrade of deck and station | Open item (§4, §12). |
| Environment promise; no transient sockets | Accepted (§4). |
| systemd quoting rules differ for program, arguments and `Environment=` | Accepted (§5). |
| Unit directory from the running manager | Accepted: checked against `UnitPath` (§5). |
| Do not use `show-environment` (it can show secrets) | Accepted: `UnitPath` property instead (§5). |
| Missing systemd vs. user bus not reachable; WSL text without interop | Accepted (§5). |
| Linger without a password prompt; disclose; keep on uninstall | Accepted (§5). |
| No `NoNewPrivileges` for station and both | Accepted (§5). |
| WSL task: start it now; repeat trigger; SID; absolute `wsl.exe`; mount check of the PowerShell path; state query | Accepted (§6). |
| launchd lifecycle, absent-only bootout errors, GUI domain check, private logs | Accepted (§7). Log rotation: open item. |
| Probe before changes, again before start; `start` too; readiness | Accepted (§8). |
| Runner with stdin and time limit; manual acceptance on real hosts | Accepted (§10). |
