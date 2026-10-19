# Omarchy management from the board: install, screensaver, provisioning repair

- **Status:** built on v2 (DL-243) and accepted after PRE-PR round 3 PASS (DL-244). Revision 5 (architect, 2026-10-18), accepted for the build: DESIGN REVIEW round 4 (the owner-approved extra round) has no BLOCKER and closed every round 3 finding; its three findings of medium severity are fixed in §3.7 without a further round (review rule: no new round for findings below high). Revision 4 (architect, 2026-10-18). DESIGN REVIEW round 3 FAIL (the last round; report `adv-omarchy-admin-design-r3-report.md`): every open finding was in the long-running screensaver job of a previous build. The owner approved a 4th round with a simpler model (2026-10-18): revision 4 replaces §3.4-§3.6 (the station runs the screensaver cycle; record version 2 stops old writers; the backup is never deleted; an outdated integration refuses screensaver and uninstall actions). Revision 3 (architect, 2026-10-18). DESIGN REVIEW round 2 FAIL: six of seven round 1 findings closed; the screensaver race stays open for a job of the previous build that runs during an upgrade (1 BLOCKER, 1 MAJOR with the same cause; report `adv-omarchy-admin-design-r2-report.md`); revision 3 adds §3.6. Revision 2 (architect, 2026-10-18). DESIGN REVIEW round 1 FAIL (1 BLOCKER, 3 MAJOR, 3 MINOR; report `adv-omarchy-admin-design-r1-report.md`): revision 2 answers each finding (§3.5 the Omarchy lock; §3.3 the retry of an abandoned bootstrap; §3.1 the child environment; §3.4 one screensaver job; §3.1 the diagnostic channel; §2.1 the presence of Omarchy; §3.2 the safe reads). Adversary DESIGN REVIEW required before code (high risk: the deck makes a station run a fixed program in the owner's account).
- **Parent:** [omarchy.md](./omarchy.md) §8 (the installer), [omarchy-p4.md](./omarchy-p4.md) §3.4 (task C, the installer) and §3.8 (task G, the screensaver), [omarchy-p8.md](./omarchy-p8.md) §3 (the bootstrap and its terminal states).
- **Gaps closed:** DL-235 open items b (install on a station that P8 did not provision) and f (turn the screensaver on); DL-234 open item d (repair a P8 terminal state). Owner instruction: every station action from the board ("the settings need to be in the UI, they are remote stations").
- **Follow-up:** installer conflict repair and the busy lock code are proposed in `board-gaps.md` (O12-25).

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
  "omarchy": { "present": true, "version": "3.4.1" },
  "install": {
    "installed": true,
    "files_ok": 9,
    "files_changed": 0,
    "files_missing": 0,
    "plugin_enabled": true,
    "hyprland_line": true
  },
  "screensaver": { "enabled": true, "file_restored": null },
  "provision": { "phase": "integration", "error": "integration_failed", "bootstrap_running": false, "last_retry": null },
  "running": null
}
```

- `omarchy`: `present` from the existing desktop detection (`desktop/detect.rs` `omarchy_root`); `version` is the detected version or `null` when the version file is missing or empty (Omarchy is still present). With `present: false`, `install` and `screensaver` are `null` and only `provision` can be set.
- `install`: from the install record and the status check of `fleetdeck omarchy status --json` (the station runs it as a child, §3.2). `installed: false` when no record exists.
- `screensaver`: `enabled` is the value of the screensaver part of the install record; `file_restored` as in §3.4. `install` also has `outdated` (§3.6).
- `provision`: `null` when `<station data>/provision-state.json` does not exist; else `phase` (`validated`, `service`, `paired`, `link`, `integration`, `complete`), `error` (one of the 21 fixed terminal codes of `omarchy-p8.md`, or `null`), `bootstrap_running` (true when the bootstrap holds `provision.lock`; probed with a non-blocking lock attempt that is released at once) and `last_retry` (the result of the last board retry since the station started: `null`, `"running"`, `"complete"`, `"terminal"`, `"timeout"`, `"failed"`). The station reads only `phase` and `error`; any other content or a read failure gives `phase: null, error: "unknown_error"` (§3.2).
- `running`: the action that runs now (§3.3), or `null`.

### 2.2 `omarchy.act`

Params `{ "action": "install" | "uninstall" | "screensaver_on" | "screensaver_off" | "screensaver_rebaseline" | "provision_retry", "elevated": true }` with `deny_unknown_fields`. The station refuses `elevated: false` with `elevation_required`. Result: `{ "outcome": "done" | "failed" | "started", "code": null | "<code>", "message": null | "<text>", "status": <the §2.1 object after the action> }`.

Wire errors: `elevation_required`, `busy` (an action runs, or the Omarchy lock of §3.5 is not free within 10 s), `omarchy_missing` (Omarchy not found, for every action except `provision_retry`), `not_provisioned` (`provision_retry` when §3.3 does not allow it), `invalid_request`.

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

- **Environment (answer to r1 MAJOR 2):** the child gets the station process environment unchanged (the environment that the service manager gave the station, so `PATH` stays the service `PATH`), plus only the three session variables of the found desktop: `XDG_RUNTIME_DIR`, `WAYLAND_DISPLAY`, `HYPRLAND_INSTANCE_SIGNATURE` (from the existing desktop detection; without a found desktop, none is added). The station never moves `PATH` for these children. The installer finds its own tools itself: `omarchy_env()` (the environment of the installer's `omarchy`, `omarchy-shell` children) puts `<omarchy root>/bin` in front of `PATH` for those children only. The bootstrap therefore inspects the service with the service `PATH`.
- The child runs in its own process group, with stdin `/dev/null`. Stdout is read up to 64 KiB and stderr up to 16 KiB, both at the same time. A longer output or the limit kills the group and gives `failed` with `code: "timeout"` or `"output_too_large"`.
- **The diagnostic (answer to r1 MINOR 1):** `message` is the stderr text of the child (the installer and the uninstaller print their conflicts and warnings on stderr), with every control character except the line feed removed, at most 2,000 characters. Stdout is used only by the status command. The message never goes into an audit row or a log line.
- `install` exit 0 → `done`; exit 1 → `failed` with `code: "install_refused"` and `message`.
- `uninstall` exit 0 → `done`; exit 1 → `failed`, `code: "uninstall_incomplete"`, `message`.
- `screensaver on|off` exit 0 → `done`; exit 1 → `failed`, `code: "screensaver_refused"`, `message`.
- Any other exit or a spawn failure → `failed`, `code: "failed"`, no message.
- `config home` is `$XDG_CONFIG_HOME` or `$HOME/.config` of the station process (the same rule as the installer).

### 3.2 Status

`omarchy.status` runs `omarchy status --json` only when Omarchy is detected and an install record exists; it parses only the fields of §2.1 (a parse failure gives `install: null`). It reads the record's screensaver flag and the provision state file directly (answer to r1 MINOR 3):
- open with `O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC`; `fstat` the descriptor: it must be a regular file, owned by the station user, not writable by group or others, at most 16 KiB; then read at most 16 KiB + 1 from that descriptor;
- a missing file is "absent"; any other refusal or a parse failure is "unreadable" (`install: null` for the record; `phase: null, error: "unknown_error"` for the state file). No read follows a link or blocks on a special file.

### 3.3 One action at a time, and the detached retry

- One mutex per station: a second `omarchy.act` while one runs answers `busy`. `omarchy.status` never waits for the mutex; it reports `running`.
- `provision_retry` answers `started` at once and runs the bootstrap as a child of the station with the 10 min limit; the mutex is held until it ends. The bootstrap takes its own lock (a second bootstrap answers `Busy` and exits 0). The board polls `omarchy.status` to see the new phase and error. A station restart ends the child; the bootstrap resumes from its state file at the next run (its existing rule).
- **When a retry is allowed (answer to r1 MAJOR 1):** the phase is `paired`, `link` or `integration` (an earlier phase cannot be repaired from the board, and `complete` needs no retry), and either the state has an `error`, or the state has no error and no bootstrap holds `provision.lock` (an abandoned checkpoint: a retry or a first-login run that a limit, a kill or a restart ended before `complete`). Otherwise `not_provisioned`. A board retry of an abandoned checkpoint runs the bootstrap without `--retry` (nothing to clear); with an error it passes `--retry`.
- The station keeps `last_retry` in memory: `running` while the child runs; then `complete` (exit 0 and the state phase `complete`), `terminal` (exit 2), `timeout` (the 10 min limit killed the group), or `failed` (any other end). A station restart loses it; the state file and `bootstrap_running` still show the truth, and the rule above allows another retry.
- The station is the child's parent; the bootstrap at phase `paired` or later does not install or replace the station service (that is phase `service`, before `paired`), so the retry never restarts the station that runs it.

### 3.4 The screensaver: one writer, run by the station (revision 4)

Revisions 2 and 3 kept the long-running screensaver job of P4 task G and tried to make old and new jobs safe with locks and process signals; DESIGN REVIEW round 3 showed that an old plugin can always relaunch an old, lockless job from `PATH` (an upgrade switches the station's own launcher, not the `fleetdeck` on `PATH`), and that PID-based stops race. Revision 4 (the owner approved a 4th round with this simpler model, 2026-10-18) removes the long-running job.

- **One writer:** the station service runs the screensaver cycle itself, as a short child of its own executable: every 60 s, while Omarchy is present and an install record exists, the station runs `<current_exe> [--data-dir D] omarchy screensaver-tick` with the child rules of §3.1 (limit 15 s). The station's own `current_exe` is always the running build, so `PATH` plays no part. The station never runs two ticks at once, and never a tick while an `omarchy.act` child runs (the action mutex of §3.3).
- **The tick** (new hidden CLI subcommand) does one cycle of the existing P4 logic: it takes the Omarchy lock (§3.5), reads the record, writes nothing when the record is absent, not version 2 (§3.6) or `enabled` is false, else renders the text (the fleet summary from the station's local socket and the banner, as the job does today) and applies the existing `write_if_changed` with its first-write and backup rules. It releases the lock before it exits. It never deletes the backup (§3.6).
- **The plugin of this build** starts no screensaver process (`Service.qml` loses the `screensaver` `Process` and its timer). The hidden `fleetdeck desktop screensaver-run` of this build only prints "The screensaver now runs in the station service." and exits 0 (an old plugin that still starts it gets no writer and no restart loop, because the exit code is 0).
- **`omarchy screensaver on`:** needs a version 2 record (else exit 1, "Install the Omarchy integration first."); sets `screensaver.enabled = true` and saves the record under the Omarchy lock. The next tick writes.
- **`omarchy screensaver off`:** under the Omarchy lock, sets `enabled = false`, and puts the original back when the current file is still the last written text: it copies the backup over the file, and **keeps the backup file** (it does not delete it); when there was no original, it removes the written file. It saves the record.
- **Status** (§2.1): `screensaver` becomes `{ "enabled": bool, "file_restored": bool | null }` (`job_running` is removed): `file_restored` is set after an `off` (true when the current file equals the backup, or is absent when there was no original); `null` while enabled.
- **Board:** "Screensaver: on" / "Screensaver: off" / "Screensaver: off. The screensaver file changed after Fleet Deck restored it; the original is kept as a backup.".

### 3.5 The Omarchy lock

Every writer of the install record, of the screensaver file or of its backup takes one exclusive advisory lock, `<config home>/fleetdeck/omarchy.lock` (`flock`, created 0600 if missing, opened with `O_NOFOLLOW`): `omarchy install`, `omarchy uninstall`, `omarchy screensaver on|off`, `omarchy screensaver-tick`. Each holds it for its whole run, waiting at most 10 s (then it exits 3 with "Another Omarchy change is running."; the station maps exit 3 to `busy`, `LOCK_BUSY_EXIT`, `board-gaps.md` §3.8; a tick that cannot take it does nothing and exits 0). Order: `provision.lock` (the bootstrap) before the Omarchy lock; no holder of the Omarchy lock takes `provision.lock`; the station's action mutex before both. Tests: the interleavings tick/`off`, tick/`uninstall`, `on`/`off`, install/tick, each with the lock held by one side.

### 3.6 Migration from a previous build (answer to the round 2 and round 3 findings)

The previous build's job (in a running process, or relaunched by its plugin from `PATH`) writes without the Omarchy lock. This build makes it harmless without finding or signalling any process:
- **Record version 2:** this build writes the install record with `v: 2` (the same fields; `omarchy install` converts a version 1 record when it runs, under the Omarchy lock). The previous build's `record::load` accepts only `v == 1` and fails otherwise, so a previous-build job that reads the record at its next cycle start gets an error, exits non-zero and writes nothing; the previous build's plugin relaunches it after 5 s, and it fails the same way, with no write. This build reads `v: 2` and, for the conversion only, `v: 1`.
- **The last old cycle:** a previous-build job that read the version 1 record just before the conversion can still write the screensaver file once and save its stale version 1 record over the new one. Then the record is version 1 again: this build treats the integration as outdated (below) until `omarchy install` runs again. Because no code of this build deletes the backup (§3.4), such a late write can never lose the original: the original stays in `screensaver.txt.fleetdeck-backup`.
- **Outdated integration:** `omarchy.status` reports `install.outdated: true` when the record is version 1, or when an installed plugin file differs from this build's embedded file (the plugin of a previous build), or when the record has been version 2 for less than 120 s (`record.converted_at`, set by the conversion; two job cycles, so that any last old cycle has ended). While outdated, the station refuses `screensaver_on`, `screensaver_off` and `uninstall` with `failed`, `code: "integration_outdated"`, and the board shows "Update the integration first: press Install, then wait two minutes." `install` and `provision_retry` stay allowed.
- **Uninstall:** under the Omarchy lock, after the existing steps, the screensaver part is put back as in `off` (the backup is kept, and its path is printed: "The original screensaver is kept in <path>."); the record is removed last.
- **Tests:** a version 1 record is converted by install and refused by `on`/`off`/`uninstall` until 120 s pass; a fake previous-build writer (lockless, `v == 1` only) that writes after `off` never removes the original (the backup still holds it, and `file_restored` turns false); the tick writes only with `v: 2` and `enabled`; the old `screensaver-run` entry prints its line and exits 0.

### 3.7 Recovery rules (revision 5, answers to the round 4 findings)

Round 4 (the owner-approved extra round) closed the round 3 findings and found three recovery defects (Codex severity medium; no data loss). The architect fixes them here without a further review round; the PRE-PR review checks the code.
- **Install adopts its own files (round 4 finding 1):** `omarchy install` classifies a file whose current bytes equal this build's embedded asset as `Adopt`, whatever hash the record holds, and writes the correct hash into the record. A file that differs from both the recorded hash and the embedded asset stays a conflict (unchanged rule). So a stale version 1 save of an old job is repaired by the next install, and uninstall later removes the plugin files normally.
- **Restore resets the write cache, and keeps its metadata on failure (round 4 finding 2):** `off` and the screensaver step of `uninstall` read the screensaver part of the record without clearing it first; only after the original is back (or the written file is removed) do they clear `last_written_sha256`, `backup_sha256` and `had_original`. A later `on` therefore goes through the first-write rules again. When the restore fails (a read or write error), the record keeps the screensaver part unchanged and the command exits 1 (`screensaver_refused`), so a second `off` can retry.
- **A refused first write is visible, with a recovery action (round 4 finding 3):** when the tick's first write is refused because a kept backup differs from the current file, the tick records `first_write_refused: true` in the screensaver part of the record (under the Omarchy lock) and writes nothing; a later successful first write clears it. `omarchy.status` reports `screensaver.blocked: "kept_original_differs"` (else `null`). The board shows "Screensaver: on, but blocked: the original screensaver that Fleet Deck kept differs from the current file." and the button **Use the current screensaver as the original**. This is a sixth action, `screensaver_rebaseline` (argv `omarchy screensaver rebaseline`, 15 s, elevated like every action): under the Omarchy lock it renames the kept backup to `screensaver.txt.fleetdeck-backup.<unix seconds>` (it never deletes it; the name is printed), clears `first_write_refused`, and leaves `enabled` as it is, so the next tick takes a new backup of the current file.
- **Tests:** a record with stale plugin hashes and byte-identical files is adopted and repaired by install; `off` then `on` with an unchanged rendering writes again; a failed restore keeps the record part and a second `off` restores; a differing kept backup gives `blocked` on the board, and `rebaseline` keeps the old backup under a new name and lets the next tick write.

## 4. Deck

- `GET /api/stations/{station}/omarchy` → `omarchy.status` (owner board login only; 15 s wait; 404 `unknown_station`; 409 `station_unreachable`; 501 `unsupported` when the link lacks `omarchy_admin.v1`).
- `POST /api/stations/{station}/omarchy` body `{ "action": … }` (`deny_unknown_fields`) with the header `x-fleetdeck-elevation`: the deck checks the elevation of this owner login first (400 `elevation_required` without a valid one) and then sends `omarchy.act` with `elevated: true`. Wait: 75 s (the longest child limit plus 15 s); `provision_retry` answers at once.
- Audit `station.omarchy` with the action and the outcome code; never the message or a file path.
- Method rows: board API `station.omarchy_status` and `station.omarchy_act`, scope `admin`.

## 5. Board

The station tab of Settings shows the **Omarchy** card when the station is reachable:
- Without the feature: "This station's build cannot manage Omarchy from the board."
- `omarchy.present: false`: "Omarchy was not found on this station." Only the provisioning line and **Retry provisioning** can show. `present: true` with `version: null`: "Omarchy (version unknown)".
- The lines: "Omarchy <version>"; "Integration: installed" / "Integration: not installed" / "Integration: installed, <n> files changed or missing"; "Plugin: on/off"; "Hyprland line: present/missing"; "Screensaver: on" / "Screensaver: off" / "Screensaver: off. The screensaver file changed after Fleet Deck restored it; the original is kept as a backup."; "Update the integration first: press Install, then wait two minutes." while `install.outdated`; "Provisioning: <phase>" and, with an error, "Provisioning stopped: <code>" with the fixed owner text of `operations.md` §15.3 for that code; while `bootstrap_running`, "Provisioning is running"; an abandoned checkpoint (no error, not running, phase before `complete`): "Provisioning did not finish".
- Buttons: **Install** (when not installed, or installed with changed files), **Uninstall** (when installed), **Turn the screensaver on** / **Turn the screensaver off** (when installed), **Retry provisioning** (when §3.3 allows a retry). Each asks the elevation with the label `Omarchy integration`. While an action runs, every button is off and the card shows "Working…"; after `started` the card polls the status every 5 s for at most 10 min.
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
