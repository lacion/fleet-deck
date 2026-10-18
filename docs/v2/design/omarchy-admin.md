# Omarchy management from the board: install, screensaver, provisioning repair

- **Status:** proposed (architect, 2026-10-18). Adversary DESIGN REVIEW required before code (high risk: the deck makes a station run a fixed program in the owner's account).
- **Parent:** [omarchy.md](./omarchy.md) §8 (the installer), [omarchy-p4.md](./omarchy-p4.md) §3.4 (task C, the installer) and §3.8 (task G, the screensaver), [omarchy-p8.md](./omarchy-p8.md) §3 (the bootstrap and its terminal states).
- **Gaps closed:** DL-235 open items b (install on a station that P8 did not provision) and f (turn the screensaver on); DL-234 open item d (repair a P8 terminal state). Owner instruction: every station action from the board ("the settings need to be in the UI, they are remote stations").

## 1. Scope

The station tab of Settings gets an **Omarchy** card. It shows the state of the Omarchy integration of that station and has five actions: **Install**, **Uninstall**, **Turn the screensaver on**, **Turn the screensaver off**, **Retry provisioning**. Each action runs one fixed `fleetdeck` subcommand on the station, as the station's own user, with a fixed argument list. The board sends only the action name.

Not in scope:
- A P8 terminal state that happens before the station is linked to this deck (for example `file_invalid`, `release_untrusted`, `paired_elsewhere`, `fingerprint_mismatch`). The board cannot reach such a station. Its repair is a new provisioning file (the P8 media path), which is part of the first setup of a new machine. `operations.md` §15.3 keeps the list.
- Any free argument, path, command or environment from the board.
- The Hyprland and menu files that the owner changed: the installer keeps its existing conflict rules.

## 2. Wire

Feature `omarchy_admin.v1` (station). Two link methods, deck to station, scope `admin`, not receipt-mutating:

### 2.1 `omarchy.status`

Params `{}`. Result:

```json
{
  "omarchy": "3.4.1",
  "install": {
    "installed": true,
    "files_ok": 9,
    "files_changed": 0,
    "files_missing": 0,
    "plugin_enabled": true,
    "hyprland_line": true
  },
  "screensaver": true,
  "provision": { "phase": "integration", "error": "integration_failed" },
  "running": null
}
```

- `omarchy`: the detected Omarchy version (the existing desktop detection), or `null` when Omarchy is not found. With `null`, `install` and `screensaver` are `null` and only `provision` can be set.
- `install`: from the install record and the status check of `fleetdeck omarchy status --json` (the station runs it as a child, §3.2). `installed: false` when no record exists.
- `screensaver`: the `enabled` value of the screensaver part of the install record.
- `provision`: `null` when `<station data>/provision-state.json` does not exist; else `phase` (`validated`, `service`, `paired`, `link`, `integration`, `complete`) and `error` (one of the 21 fixed terminal codes of `omarchy-p8.md`, or `null`). The station reads only these two fields; any other content or a read failure gives `{ "phase": null, "error": "unknown_error" }`.
- `running`: the action that runs now (§3.3), or `null`.

### 2.2 `omarchy.act`

Params `{ "action": "install" | "uninstall" | "screensaver_on" | "screensaver_off" | "provision_retry", "elevated": true }` with `deny_unknown_fields`. The station refuses `elevated: false` with `elevation_required`. Result: `{ "outcome": "done" | "failed" | "started", "code": null | "<code>", "message": null | "<text>", "status": <the §2.1 object after the action> }`.

Wire errors: `elevation_required`, `busy` (an action runs), `omarchy_missing` (Omarchy not found, for every action except `provision_retry`), `not_provisioned` (`provision_retry` without a state file, or a state without an error), `invalid_request`.

## 3. Station

### 3.1 The fixed commands

| Action | argv (after the station's own `current_exe` and its `--data-dir` when the station has one) | Limit |
|---|---|---|
| `install` | `omarchy install --yes` | 60 s |
| `uninstall` | `omarchy uninstall` | 60 s |
| `screensaver_on` | `omarchy screensaver on` (new, §3.4) | 15 s |
| `screensaver_off` | `omarchy screensaver off` (new, §3.4) | 15 s |
| `provision_retry` | `provision-bootstrap --file <config home>/fleetdeck/provision.toml --retry` | 10 min, detached (§3.3) |
| (status) | `omarchy status --json` | 15 s |

- The child runs as the station user, with the desktop environment of the station (`desktop_env`: the Hyprland variables and `$OMARCHY_PATH/bin` on `PATH`), stdin `/dev/null`, stderr discarded, stdout read up to 64 KiB. A longer output or the limit kills the child (its process group) and gives `failed` with `code: "timeout"` or `"output_too_large"`.
- `install` exit 0 → `done`; exit 1 → `failed` with `code: "install_refused"` and `message` = the stdout text (the installer prints the conflicting files; control characters removed, at most 2,000 characters).
- `uninstall` exit 0 → `done`; exit 1 → `failed`, `code: "uninstall_incomplete"`, `message` as above.
- Any other exit or a spawn failure → `failed`, `code: "failed"`, no message.
- `config home` is `$XDG_CONFIG_HOME` or `$HOME/.config` of the station process (the same rule as the installer).

### 3.2 Status

`omarchy.status` runs `omarchy status --json` only when Omarchy is detected and an install record exists; it parses only the fields of §2.1 (a parse failure gives `install: null`). It reads the record's screensaver flag and the provision state file directly (read-only, at most 16 KiB each, no link following).

### 3.3 One action at a time, and the detached retry

- One mutex per station: a second `omarchy.act` while one runs answers `busy`. `omarchy.status` never waits for the mutex; it reports `running`.
- `provision_retry` answers `started` at once and runs the bootstrap as a child of the station with the 10 min limit; the mutex is held until it ends. The bootstrap takes its own lock (a second bootstrap answers `Busy` and exits 0). The board polls `omarchy.status` to see the new phase and error. A station restart ends the child; the bootstrap resumes from its state file at the next run (its existing rule).
- Before it starts `provision_retry`, the station checks that the state file has an `error` (else `not_provisioned`) and that the phase is `paired` or later (an earlier phase cannot be repaired from the board: `not_provisioned`).

### 3.4 Screensaver on and off (CLI)

New subcommand `fleetdeck omarchy screensaver on|off`:
- `on`: needs an install record (else exit 1, "Install the Omarchy integration first."); sets `screensaver.enabled = true` and saves the record (0600). The existing first-write and backup rules of P4 task G apply when the job writes.
- `off`: runs the existing `restore` of the screensaver part (the same code as `uninstall` uses: the original comes back when the current file is still the last written text) and leaves `enabled = false`.
- The job `fleetdeck desktop screensaver-run` changes: while the record exists and `enabled` is false, the job sleeps one cycle (60 s) and reads the record again, instead of ending. It still ends when the record is gone (uninstall) or on the old-backup refusal. So a board toggle takes effect within 60 s without a plugin reload.

## 4. Deck

- `GET /api/stations/{station}/omarchy` → `omarchy.status` (owner board login only; 15 s wait; 404 `unknown_station`; 409 `station_unreachable`; 501 `unsupported` when the link lacks `omarchy_admin.v1`).
- `POST /api/stations/{station}/omarchy` body `{ "action": … }` (`deny_unknown_fields`) with the header `x-fleetdeck-elevation`: the deck checks the elevation of this owner login first (400 `elevation_required` without a valid one) and then sends `omarchy.act` with `elevated: true`. Wait: 75 s (the longest child limit plus 15 s); `provision_retry` answers at once.
- Audit `station.omarchy` with the action and the outcome code; never the message or a file path.
- Method rows: board API `station.omarchy_status` and `station.omarchy_act`, scope `admin`.

## 5. Board

The station tab of Settings shows the **Omarchy** card when the station is reachable:
- Without the feature: "This station's build cannot manage Omarchy from the board."
- `omarchy: null`: "Omarchy was not found on this station." Only the provisioning line and **Retry provisioning** can show.
- The lines: "Omarchy <version>"; "Integration: installed" / "Integration: not installed" / "Integration: installed, <n> files changed or missing"; "Plugin: on/off"; "Hyprland line: present/missing"; "Screensaver: on/off"; "Provisioning: <phase>" and, with an error, "Provisioning stopped: <code>" with the fixed owner text of `operations.md` §15.3 for that code.
- Buttons: **Install** (when not installed, or installed with changed files), **Uninstall** (when installed), **Turn the screensaver on** / **Turn the screensaver off** (when installed), **Retry provisioning** (when `provision.error` is set and the phase is `paired` or later). Each asks the elevation with the label `Omarchy integration`. While an action runs, every button is off and the card shows "Working…"; after `started` the card polls the status every 5 s for at most 10 min.
- A `failed` answer shows its code text and the message (in a `<pre>` block, as plain text).

## 6. Version

- A station without `omarchy_admin.v1` never gets the methods; the deck answers 501 `unsupported`, and the board shows the line of §5.
- An older deck: the board does not show the card when `GET /api/stations/{station}/omarchy` answers 404 (no route).

## 7. Security

- The deck: owner board login, elevation checked on the deck for every action, scope `admin` on the link, audit without free text.
- The station: refuses `elevated: false`; runs only the argv of §3.1; no input from the wire reaches the argv, the environment or a path; one action at a time; output bounded; children killed at their limit.
- The installer, the screensaver and the bootstrap keep their own rules (the conflict rules, the record hashes, the backup rule, the bootstrap lock and identity checks). The board path adds no new file effect.
- Residual: an elevated owner (or a stolen elevated board session within its 10 min elevation) can install or remove the integration and retry provisioning on any linked station. That is the same power as the station settings route.

## 8. Tests (four proofs)

- Behavior: each action runs the exact argv with the exact environment and limit (a fake `current_exe` child); status fields from a fake record, a fake status output and a fake state file; `screensaver on/off` and the job that waits while off and writes after on; the board card lines and buttons for each state.
- Failure: a child that hangs past its limit is killed (group) and gives `timeout`; output over 64 KiB gives `output_too_large`; exit 1 gives the mapped code with the cleaned message; `busy` while an action runs; a station restart during `provision_retry`.
- Exposure: `elevated: false` refused on the station; the deck refuses without a valid elevation and for a non-owner; no wire field reaches the argv; the audit row has no message; the message has no control characters.
- Version: a station without the feature gets no method (501 on the deck, the board line); an older deck without the route hides the card.

## 9. Task split

1. **A (CLI):** `omarchy screensaver on|off`, the job change, tests.
2. **B (proto, link, station):** the feature, the two methods, the station service (§3), tests with a fake child.
3. **C (deck):** the two routes, elevation, audit, method rows, tests.
4. **D (board):** the card, tests.
5. **E (docs):** `operations.md` (the card; §15.3 repair from the board), DL entry, parity, security, smoke items.
