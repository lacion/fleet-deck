# Board paths for the remaining owner actions

- **Review state (2026-10-19):** DESIGN REVIEW used all 3 rounds. Round 3: R1-2 and R1-4 FIXED; one new BLOCKER in BG-1 (the station user writes the `pacman.conf` that root reads, so code of that user can change it while sudo waits; report `adv-board-gaps-design-r3-report.md`). **BG-1 waits for the owner's decision** (recommendation: the root-owned system `/etc/pacman.conf` and repository-qualified targets `core/<name>`, `extra/<name>`; the residual of code that runs as the user stays the P8-17 boundary). BG-2, BG-3 and BG-4 have no open finding and are accepted for the build.
- **Revision history:** Revision 2 (2026-10-19): DESIGN REVIEW round 1 FAIL (1 BLOCKER, 4 MAJOR, 1 UNVERIFIED; report `adv-board-gaps-design-r1-codex.md`); fixed by the architect decisions O12-26. Revision 3 (2026-10-19): DESIGN REVIEW round 2: R1-2 and R1-4 open (2 MAJOR; report `adv-board-gaps-design-r2-report.md`); fixed by O12-27.
- **Build status (2026-10-19):** BG-2, BG-3 and BG-4 built on v2 (DL-249); PRE-PR reviews pending. BG-1 waits for the owner.
- **Status:** Proposed design (architect, 2026-10-19), from the advisor answer `advisor-board-gaps-answer.md` against v2 HEAD `fbce0923`. The architect accepts BG-1 to BG-4 as architect decisions (O12-25; the owner may change any of them). DESIGN REVIEW required before implementation; PRE-PR required before merge; live smoke required for the real sudo/pacman, compositor tools and process trees.
- **Scope:** four gaps that still need an action on the station itself: the desktop tools (`operations.md` lines about 642, 799, 925 and 930); the installer conflict repair and the busy lock code (DL-243 items b and e); a P8 failure before the station is linked (DL-243 item c and `operations.md` §15.3); stopping the processes of a lost run (`operations.md` about line 978).
- **Out of scope:** the residual product gaps of §1 (unsupported package delivery, trust or configuration repairs before the link, legacy process attribution without a trustworthy witness).

This annex is proposed contract content for adversary review. It does not claim that it closes every owner action. Package availability, failures before a trusted reporting endpoint exists, recorded owner edits, and lost executions without reliable membership evidence each need explicit treatment. Each section states what it does not close.

## 1. Summary and residual product gaps

The advisor recommends four options. The architect accepts them as BG-1 to BG-4.

| ID | Gap | Decision |
|---|---|---|
| **BG-1** | Desktop tools | Option 1a. Keep the interactive sudo in the board terminal, but start it in a dedicated maintenance terminal (§2). |
| **BG-2** | Installer repair | Option 2a. A reviewed replacement plan, with a separate selection for recorded edits (§3). |
| **BG-3** | P8 before the link | Option 3a. A limited reporting and retry facility, with a separate supervisor, an owner-opened reporting window and one-shot retries that never override P8 checks (§4). |
| **BG-4** | Lost executions | Option 4a. A new stop plan with retained pidfds and a separate authorization plan (§5). |

Every section uses independent optional features. The common authorization, version and proof requirements are in §6. The build split is in §7.

### 1.1 Residual product gaps

Three limits stay open. The design does not hide them. None of them becomes an instruction to the owner to log in to a station.

1. **Unsupported package delivery.** Arch documents `wlrctl` as an AUR package. This design does not bootstrap repository trust, does not build AUR packages and does not use an AUR helper. `wlrctl` stays "unavailable" until the supported distribution contract provides a reviewed binary package through an already trusted package source.
2. **Trust or configuration repairs before the link.** A report can request another attempt. It cannot repair trust. `release_untrusted`, `config_conflict`, `service_conflict`, `paired_elsewhere` and the other codes of §4.5 with a disabled retry stay without a board repair. Also open: no valid destination or pin, no usable identity, no running supervisor or network.
3. **Legacy process attribution without a trustworthy witness.** Safe stopping of every existing legacy run cannot be retrofitted from the current read model. A legacy run whose provider is gone and that has no reliable witness gets `identity_unproven` and no signal.

Other limits that the sections state again:
- A report channel is a lossy diagnostic channel. It is not attestation (§4.7).
- A recorded integration file that the owner edited is kept unless the owner selects it (§3).
- The stop of a run contains the specifically authorized task identities. It does not contain adversarial processes of the same user (§5.4).

## 2. BG-1: desktop tools

### 2.1 What is wrong with "typed but not sent"

The owner authenticates through the board terminal, and the station continues to run as the owner user. This privilege model is sound. Passwordless sudo is neither required nor enabled. The problems are elsewhere:

- **"Typed but not sent" is not a reliable PTY safety boundary.** Bytes sent to a PTY are input to the program that owns it at that time. A shell, a provider TUI, a password prompt or a shell binding can interpret them in different ways. The preview stays entirely in browser state.
- **Executable names are not a package contract.** Arch documents `wlrctl` as an AUR package. Do not assume that `pacman -S wlrctl` works on the supported Omarchy image. Do not silently substitute an AUR helper. Reference: [ArchWiki: Wayland automation](https://wiki.archlinux.org/title/wayland).
- **Installing a package does not prove desktop PATH visibility or compositor compatibility.** The station refreshes the facts afterward.
- **Versions need tool-specific handling.** Do not try speculative `--version` flags on input or recording tools.
- **Package installation and system upgrade are different owner decisions.** `-S --needed` does not refresh the package databases. Do not silently add `-Sy`. A separate full-upgrade action must disclose its scope. Pacman supports a system upgrade together with explicit package targets. Reference: [Pacman manual](https://man.archlinux.org/man/pacman.8).
- **Do not apply the existing short-child timeout policy to pacman.** Stopping an active package transaction on a network timeout or a board disconnect can damage the station.

The existing shell deliberately survives a board disconnect and does not audit keystrokes. This design keeps both properties. Reference: `docs/v2/PLAN.md` §6.8.

### 2.2 Wire contract

Two independent optional features:

| Feature | Methods | Authority |
|---|---|---|
| `station_tools.v1` | `station.tools` | Owner board, link scope `admin`, read-only |
| `desktop_tool_install.v1` | `station.desktop_tools_plan`, `shell.open_maintenance` | Owner board; elevation for opening the maintenance terminal |

There is no automatic package-install method.

### 2.3 `station.tools`

Params: `{}`. Unknown fields are rejected. Answer:

```json
{
  "sampled_at": 1792400000000,
  "desktop": "omarchy",
  "tools": [
    {
      "id": "wtype",
      "present": true,
      "version": "0.4",
      "version_source": "package",
      "package_matches_executable": true,
      "probe": "ok"
    }
  ],
  "keyboard": {
    "status": "ok",
    "layouts": ["us"]
  },
  "plugin": {
    "status": "valid"
  },
  "passwordless_sudo": "off"
}
```

The fixed tool IDs are `wtype`, `wf_recorder`, `wlrctl`, `grim`, `busctl`, `jq`, `hyprland`, `chromium` and `notify_send`. `notify_send` is included so that an existing diagnostic is not dropped.

Definitions:

| Field | Definition |
|---|---|
| `present` | Resolution that uses the same desktop environment and the same executable resolver as the actual desktop operation. |
| `version` | Bounded version text, or `null`. |
| `version_source` | `executable`, `package` or `none`. |
| `package_matches_executable` | Whether package ownership identifies the resolved executable. `null` when unknown. |
| `probe` | `ok`, `missing`, `unknown`, `timeout` or `failed`. |
| `plugin.status` | `valid`, `invalid`, `unavailable` or `unknown`. |
| `passwordless_sudo` | `on`, `off` or `unknown`. |

Rules:
- A package version is labelled as a package version. A same-named user wrapper must not inherit the version of the system package.
- The answer has parsed facts only. It has no doctor stdout, no raw `hyprctl` JSON, no validation output, no environment values and no executable paths. The keyboard diagnostics must not return device identifiers.
- Every probe uses a fixed, reviewed argv. Version collection never types, clicks, starts a recording, opens Chromium or changes plugin state.
- Limits: four children at the same time; five seconds for each child; twenty seconds in total; 16 KiB for each captured stream; 32 KiB for the answer.
- A failed probe leaves the other facts usable.

### 2.4 `station.desktop_tools_plan`

Params: `{}`. Answer:

```json
{
  "plan_id": "<opaque>",
  "expires_at": 1792400120000,
  "installable": ["wtype", "wf_recorder"],
  "unavailable": ["wlrctl"],
  "already_present": ["grim"],
  "command_display": "sudo pacman -S --needed --noconfirm ...",
  "system_upgrade": false
}
```

- The station chooses the packages from a compiled allowlist. No package, repository, URL, path, option or command string that the browser supplies reaches execution.
- **The policy covers the complete package transaction, not only the targets. The enforced policy is the repository set, not the exact versions of the dry run (revision 3).** The station writes a pacman configuration file `<station data>/maintenance/pacman.conf` (mode 0600). It has the `[options]` that pacman needs, copied from the system file (`HoldPkg`, `Architecture = auto`, `SigLevel` as in the system file; no change to `IgnorePkg`), and exactly two repositories, `[core]` and `[extra]`, each with `Include = /etc/pacman.d/mirrorlist`. It has no other repository, no `Server` line of its own, no `XferCommand` and no hook directory option. The station writes the file again before each plan.
- Before the terminal opens, the station runs a dry run without root with the fixed argv `pacman --config <that file> -Sp --needed --print-format '%r %n %v' <targets>`. It keeps the resolved list (repository, name and version of every package) in the plan as the expected transaction.
- The plan is valid only when both conditions are true: every resolved package comes from the repository `core` or `extra`, and the list holds every allowlisted target that is missing. Otherwise the plan answers `transaction_refused` with the names of the packages that are not allowed. Nothing is installed.
- The terminal command is `sudo pacman --config <that file> -S --needed --noconfirm <targets>`. A transaction therefore resolves only from `core` and `extra`, whenever it resolves. A conflict prompt or a removal prompt gets the answer no from `--noconfirm`, so a transaction that would remove or replace a package stops before any change. There is no `-y`.
- **Accepted residual (revision 3).** Another program can refresh the package databases between the dry run and the installation, for example while sudo waits for the password. Then the versions and the provider choices can differ from the dry run. The transaction still stays inside `core` and `extra` and still cannot remove or replace a package. The post-check after the exit compares the installed set with the plan and reports `transaction_changed`.
- Before the terminal opens, the station compares the configuration file with the one that the plan used. A difference refuses with `transaction_refused`.
- **Accepted authority.** The scriptlets and the hooks of the packages from `core` and `extra` run as root during the transaction. The system pacman hooks in `/usr/share/libalpm/hooks` and `/etc/pacman.d/hooks` run as for any pacman transaction. The owner authorizes that with the sudo password.
- `wlrctl` becomes installable only after the supported distribution contract provides a reviewed binary package through an already trusted package source. This design does not bootstrap repository trust and does not build AUR packages.
- `system_upgrade` is `false`. A full system upgrade is never part of this plan. A separate full-upgrade action must disclose its scope. This design does not add that action.
- A plan lasts two minutes. There is one plan for each station and owner login.

### 2.5 `ShellChange::OpenMaintenance`

`ShellChange::OpenMaintenance` is gated by `desktop_tool_install.v1`:

```json
{
  "shell_id": "<id>",
  "purpose": "desktop_tools",
  "plan_id": "<opaque>",
  "elevated": true,
  "cols": 100,
  "rows": 30
}
```

It creates a **fresh maintenance PTY**. The PTY belongs to a separate runner process (§2.6) that executes the fixed absolute sudo/pacman argv directly. It does not reuse a shell, does not run shell startup files and does not attach to a provider TUI. The browser displays the command before it requests this action.

It reuses the shell receipts and the data transport. Rules:
- A maintenance terminal cannot be handed to a provider.
- The action respects `allow_shell`. It never bypasses it.
- Opening a maintenance terminal needs an elevation with the label **Install desktop tools**.
- **The PTY baseline has no echo.** The runner clears the terminal attributes `ECHO` and `ECHONL` on the slave side before it spawns the child and before it accepts any input. sudo saves the attributes that it finds and restores them, so the baseline stays echo-off after the authentication.
- The runner accepts input only while the child runs, and only after it has set the PTY baseline. Input after the exit is refused with `maintenance_ended`.
- The output that the station keeps is the output of the PTY only. With echo off, the typed input is not in that output.

### 2.6 Maintenance terminal lifecycle

Before the start, the station revalidates the plan, the dry run, the tool facts, the package mapping, the station identity and the shell policy.

**Runner.** The transaction runs in a separate runner process. The runner is the own binary of the station, `fleetdeck maintenance-run --id <id>`, started with `systemd-run --user --collect --unit=fleetdeck-maintenance-<id>`. The unit is a transient user unit that is independent of the station unit. The runner:
- Owns the PTY and spawns sudo.
- Keeps a bounded output ring of 256 KiB.
- Writes a state file `<station data>/maintenance/<id>.json` with the mode 0600. Its state is `running`, `exited` with the exit code, or `lost`.
- Serves one unix socket `<station data>/maintenance/<id>.sock` with the mode 0600 and an owner-checked peer. The socket carries the attach and the input.

After the start:
- A board disconnect does not terminate the transaction.
- There is no automatic resend, restart, system upgrade, lock deletion or `--overwrite`.
- Closing the panel detaches it. A running package transaction must not inherit a generic "close terminal and kill child" control.
- The station never kills the runner. `shells.close_all()` does not touch it. A station restart closes only the socket connection of the station.
- After a station restart, the station scans the state files and offers **Reattach** for a running transaction. A reconnect of the board reattaches through the existing shell ownership checks.
- A runner whose unit is gone while its state file says `running` becomes `lost`. The board shows "The installation runner ended without a result. Check the desktop tools again."
- The password travels only as terminal input. Never put it in a request body, a command receipt, an audit row, an analytics event or an input recording.
- Exit code zero triggers fresh diagnostics. It does not by itself mean that the tools are usable.
- After the exit, the station compares the installed package versions with the plan. A difference is reported as `transaction_changed`.

Tests:
- A password that is pasted before the sudo prompt, a failed authentication, a reconnect, and an input after the exit. Each test asserts that the typed bytes are in no stored output and in no retransmitted output.
- A station restart during an active transaction; the reattach; the discovery of a completion after the restart.
- A dry run with a package from another repository, with a missing target and with a conflict prompt: each gives `transaction_refused` or a stopped transaction.

### 2.7 Board texts

| Condition or control | Text |
|---|---|
| Card | **Desktop tools** |
| Read action | **Refresh diagnostics** |
| Install action | **Install desktop tools…** |
| Confirmation | "This opens a board terminal to install the listed packages as root. Review the transaction and enter your station password there if sudo asks." |
| Start control | **Open installation terminal** |
| Shell disabled | "Board terminals are disabled on this station. Enable them in station settings to install desktop tools." |
| Unsupported package | "Fleet Deck has no supported package source for wlrctl on this station." |
| Missing after installation | "The package transaction finished, but wtype is still missing from the desktop PATH." |
| Unknown compatibility | "Installed. Desktop compatibility has not been verified." |
| Passwordless sudo | "Passwordless sudo is enabled on this station." |
| `transaction_refused` | "The package transaction needs packages that Fleet Deck does not allow. Nothing was installed." |
| `transaction_changed` | "The installed packages differ from the reviewed plan." |
| `maintenance_ended` | "The installation has ended. Input is not sent." |
| Runner lost | "The installation runner ended without a result. Check the desktop tools again." |
| Reconnect control | **Reattach** |

### 2.8 Audit

Audit the creation and the completion of the terminal with the compiled package IDs, the station, the owner login, the request ID and a fixed outcome. Do not audit the terminal contents.

## 3. BG-2: installer repair

### 3.1 `Conflict` and a recorded edit

The distinction is essential. The current classifier has two cases:

- `Conflict`: an unrecorded file differs from the asset.
- `Keep`: a recorded file differs from its recorded hash and from the current asset.

The installer also deliberately skips an existing owner menu. Reference: `crates/fleetdeck-cli/src/omarchy/install.rs` (line 24).

Therefore **`--replace-conflicts` alone does not repair recorded owner edits**. This design does not widen its meaning silently.

Other problems of a simple "rename and run the installer" option:
- Backup names that use only seconds collide.
- A normal rename can overwrite an earlier backup.
- A preview becomes stale before the execution.
- Renaming several files and then running the normal installer is not an atomic transaction.
- A crash can leave a backup and an absent target. The recovery must recognize that state.
- Backups need a board recovery path if the board presents them as recoverable copies.
- Replacement must never override a symlink, an unsafe owner, a directory, an invalid record or an unrelated configuration file.

Decision: a reviewed plan replaces all blocking unrecorded conflicts. A separate explicit selection covers the recorded integration-file edits.

### 3.2 Wire contract

Feature `omarchy_repair.v1`. It requires the existing `omarchy_admin.v1`.

Add the read-only method `omarchy.install_plan`. Params: `{}`. Answer:

```json
{
  "plan_id": "<opaque>",
  "expires_at": 1792400120000,
  "files": [
    {
      "asset_id": "fleet_plugin",
      "path": "/home/owner/...",
      "classification": "unrecorded_conflict",
      "replaceable": true
    },
    {
      "asset_id": "fleet_bindings",
      "path": "/home/owner/...",
      "classification": "recorded_edit",
      "replaceable": true
    }
  ],
  "blocked": null
}
```

`asset_id` is a finite protocol enum that maps to the embedded installer assets. The station never accepts a path from the browser.

Wire additions of the feature `omarchy_repair.v1` for the recovery:

| Method or action | Params | Answer and rules |
|---|---|---|
| read `omarchy.repair_status` | `{}` | The last 8 journals. For each: `repair_id`, `state` (`complete`, `incomplete` or `restored`) and, for each file, `asset_id`, `state` and `backup_path`. |
| `omarchy.act` action `install_resume` | `{repair_id}` | Allowed only when the build hash of the journal equals the running build. Otherwise `build_mismatch`: "This repair was started by another build. Restore the kept copies instead." |
| read `omarchy.restore_plan` | `{repair_id}` | A restore plan for the kept copies. |
| `omarchy.act` action `restore` | `{repair_id, plan_id, asset_ids}` | Restores the selected assets (§3.6). |

A lost action answer is recovered with `omarchy.repair_status`.

Extend `omarchy.act` with this action:

```json
{
  "action": "install_replace",
  "plan_id": "<opaque>",
  "replace_recorded_edits": ["fleet_bindings"],
  "request_id": "<id>",
  "elevated": true
}
```

- All blocking unrecorded conflicts of the reviewed plan are included.
- Recorded edits are unchecked by default. They need an explicit selection.
- Outside the replacement set: the existing owner menus, whole Hyprland configuration files, the screensaver data, the install record and all unrelated files.

### 3.3 Plans

The plan binds:
- The embedded asset and build hashes.
- The content of the install record.
- The identity, the ownership, the mode, the size and the content hash of each target.
- The selected classifications.
- The station identity and the requesting deck. The deck also binds the plan to the owner login.

The station keeps at most four plans for each station. A plan lasts two minutes. The station rechecks the plan under the existing Omarchy lock. Any changed premise gives `plan_changed` before any intentional replacement.

### 3.4 CLI

The CLI needs a bounded structured repair input. It does not parse stderr and does not accept arbitrary paths. The fixed argv is:

```text
omarchy install --yes --replace-conflicts --plan-stdin
```

Only this action gets a bounded structured stdin. The other admin children keep `/dev/null`. The selections of recorded edits are explicit entries in that input. They are not a side effect of `--replace-conflicts`.

Answer of the action:

```json
{
  "outcome": "done",
  "code": null,
  "repair_id": "<id>",
  "files": [
    {
      "asset_id": "fleet_plugin",
      "path": "/home/owner/...",
      "backup_path": "/home/owner/....fleetdeck-conflict-1792400000-<random>",
      "state": "replaced"
    }
  ],
  "status": {}
}
```

### 3.5 Filesystem and recovery rules

1. Hold the existing Omarchy lock for the planning revalidation, the backup, the publishing and the record updates. Do not release and reacquire it between the backup and the install.
2. Accept only bounded regular files that the station user owns. Reject hard links and unsafe permissions or ancestry.
3. Walk through directory descriptors without following links. Open reads nonblocking before the check of the file type.
4. Use same-directory backup names with seconds **and random suffixes**, and `RENAME_NOREPLACE`. The existing filesystem helper already provides a no-replace rename.
5. Journal the repair and the planned backup names durably before any mutation. Record each completed step. The journal and its recovery are in §3.5.1.
6. Verify the moved object against the approved identity and content. A concurrent edit stops the operation. Retain every displaced object and restore it without clobbering when possible.
7. Publish the assets without overwriting an unexpected target. Record each successful publication. The publication protocol is in §3.5.1.
8. Keep the backups indefinitely. There is no automatic cleanup.
9. On an interruption, report the actual state of each file. Do not claim "nothing changed" after the first mutation.
10. Reopening the card shows **Resume repair** or **Restore kept copies…**, derived from the journal. Neither of them repeats the whole replacement operation without a check.

#### 3.5.1 Publication, journal and recovery

**Publication.** Each asset is written to a synced staging file in the target directory, with the name `.<name>.fleetdeck-stage-<random>`. The station publishes it with `renameat2(RENAME_NOREPLACE)` to the empty target name, after the backup moved the old file away. The station syncs the directory after each rename.

**Journal (write-ahead, revision 3).** The file is `<config home>/fleetdeck/omarchy-repair/<repair_id>.json`, with the mode 0600, synced, with at most 64 files. Before any file operation, the journal records for each file, and syncs: `asset_id`, the target path, `original_hash` (the reviewed hash of the target content, or `none` for a target that did not exist), `asset_hash`, the backup name and the staging name. Before each step, the journal records the intent of that step (`backing_up`, `staging` or `publishing`) and syncs. After the step and its directory sync, it records the completion (`backed_up`, `staged` or `published`). A completion record is a hint, never proof.

**Recovery by the files.** Resume, restore and every `omarchy.repair_status` observe, for each file: the target (absent, or the hash of its content: `original_hash`, `asset_hash` or another hash), the named backup (absent, present with `original_hash`, or present with another hash) and the named staging file (absent or present). The decision comes from this table, not from the journal state alone:

| Target | Backup | Staging | Meaning | Resume | Restore |
|---|---|---|---|---|---|
| `original_hash` | absent | any | Nothing moved. | Remove the staging file. Back up, then publish. | Nothing to restore. Remove the staging file. |
| absent | `original_hash` | any | Backed up, not published. | Remove the staging file. Stage and publish. | Copy the backup to the target (no replace). The backup stays. Remove the staging file. |
| `asset_hash` | `original_hash` | any | Published. | Remove the staging file. Done. | Only after a new restore plan: move the target away to a new kept name, then copy the backup to the target. The backup stays. Never overwrite. |
| `original_hash` | `original_hash` | any | Restored (revision 4, O12-28): the target holds the original again and the backup stays. | Skip the file. | Nothing to restore. |
| `asset_hash` | absent | any | Published, and `original_hash` is `none`. | Remove the staging file. Done. | Remove the target only when its hash is `asset_hash`. Done. |
| `asset_hash` | absent | any | `original_hash` is not `none`: the backup is gone. | Stop: `repair_incomplete`. | Stop: `repair_incomplete`. |
| absent | absent | any | `original_hash` is `none`: nothing is published yet. | Stage and publish. | Nothing to restore. |
| absent | absent | any | `original_hash` is not `none`. | Stop: `repair_incomplete`. | Stop: `repair_incomplete`. |
| another hash | any | any | A later change by someone. | Stop for this file: `restore_conflict`. | Stop for this file: `restore_conflict`. |
| any | another hash | any | The backup changed. | Stop for this file: `repair_incomplete`. | Stop for this file: `repair_incomplete`. |

Every action in the table is idempotent. If a crash comes before the completion record of an action, the next run finds the same row or the next row. The station removes a staging file only when its name is the recorded staging name.

Tests: a crash after each rename and before its completion record, after each staging write and after each directory sync, each followed by resume twice and by restore twice, with a check of the files after each run; a crash at each journal state; a target that a later owner edit created; a lost action answer that is recovered with `omarchy.repair_status`; a resume on another build (`build_mismatch`).

### 3.6 Restore

Restoration is elevated and plan-based. It accepts a repair ID and asset IDs. It never accepts a path. It refuses a current target that changed unexpectedly. It does not overwrite a later owner edit. **A restore copies the bytes of the backup through a staging file and publishes them with `RENAME_NOREPLACE` to the absent target (revision 4, O12-28).** A rename of the backup cannot keep the backup, so a restore never renames it. The backup stays after the restoration. The restore plan names its actions `copy_backup`, `move_aside_then_copy_backup`, `remove_published`, `nothing`, `stop_restore_conflict` and `stop_repair_incomplete`.

### 3.7 Board and error texts

| Code or control | Text |
|---|---|
| Action | **Review files to replace…** |
| Recorded edit | "You edited this recorded integration file. Fleet Deck will keep it unless you select it." |
| Confirmation | "Replace the listed integration files? Fleet Deck will keep a separate copy of each current file." |
| Confirm | **Keep copies and replace** |
| `plan_changed` | "The files changed since this review. Review the replacement plan again." |
| `unsafe_path` | "Fleet Deck cannot safely replace one of these paths." |
| `backup_failed` | "Fleet Deck could not keep a copy of a file. The repair stopped." |
| `repair_incomplete` | "The repair stopped after changing some files. Review the kept copies and repair status." |
| `restore_conflict` | "A current file changed after the repair. Fleet Deck kept both versions and did not overwrite it." |
| `build_mismatch` | "This repair was started by another build. Restore the kept copies instead." |
| `busy` | "Another Omarchy change is running. Try again shortly." |

### 3.8 Exit 3 for the busy lock

Fix DL-243 item b on its own. Reserve **CLI exit 3 for the contention of the Omarchy lock**, in the same way for all the relevant commands, and map it to the wire error `busy`. Do not identify contention by a match on human stderr. Exit 1 keeps its ordinary refusal meaning.

### 3.9 Audit

Audit `station.omarchy_repair` with: the owner, the station, the request or repair ID, the selected asset IDs, a fixed outcome and the counts. The audit has no paths, no hashes, no file contents and no child diagnostics.

## 4. BG-3: provisioning reports before the link

### 4.1 What a report proves and does not prove

The report channel is feasible. It does not close the whole gap.

Mutual Auth proves control of a key. **It does not prove that an unpaired key belongs to the intended physical station of the owner, that its name is truthful, or that the reported failure occurred.** The deck pin authenticates the deck to the reporter. It does not enroll the reporter.

Limits of the channel:
- A terminal bootstrap exits. A polling supervisor is a new lifecycle. It is not a small addition to the final error branch of the bootstrap.
- Early failures can occur before a usable station key exists.
- On an initial invalid provisioning file, the current code fails before it creates a provisioning state. "Every terminal code is retained" is too broad. Reference: `crates/fleetdeck-cli/src/provision/run.rs` (line 211).
- `denied`, `cancelled` and `revoked` are normally retried after 600 seconds by the existing P8 policy. That these codes are in the terminal-code enum does not mean that each occurrence stops the bootstrap.
- A wrong pin cannot authorize a report to the wrong peer. Never fall back to an unpinned connection.
- Clearing `release_untrusted`, `config_conflict`, `service_conflict` or `paired_elsewhere` does not repair their cause.
- A key change must not migrate a retry permission.
- Per-key limits do not stop flooding. An attacker can generate keys.

Decision: a separate supervisor, an owner-opened reporting window, and one-shot retries that never override P8 checks. The remaining cases are product gaps (§4.8).

### 4.2 Report protocol

Optional feature `provision_report.v1`, offered by both endpoints. It must not become a required feature for ordinary pairing.

After the existing Pair-channel mutual Auth, choose exactly one protocol branch: the existing pairing, or the provisioning report. Do not mix report frames with `PairRequest`, `PairResume` or `PairProof`.

There is one request and one response for each connection.

Request:

```json
{
  "type": "provision_report",
  "terminal_id": "<128-bit random id>",
  "state_digest": "<sha256>",
  "code": "service_conflict",
  "phase": "validated",
  "name": "desk-west"
}
```

The authenticated peer key supplies the fingerprint. There is no field for a station ID or a claimed fingerprint that the client selects.

`terminal_id` identifies one observed terminal condition. The digest binds the validated provisioning state, the configuration, the destination pin and the station identity. The station stores this binding privately. A changed binding creates a new terminal ID and invalidates any old retry.

Response:

```json
{
  "type": "provision_report_reply",
  "status": "stored",
  "retry_after_seconds": 600
}
```

Statuses: `stored`, `disabled`, `capacity`, `rate_limited` or `retry`.

A `retry` response also contains `retry_id`, `terminal_id` and `state_digest`. The station accepts it only on a fresh authenticated connection to the pinned deck, and only for the exact current terminal binding.

No command, path, replacement configuration, new pin, key, package or free-text instruction is carried.

### 4.3 Supervisor

Add a root-packaged, fixed executable entry point, for example `provision-supervise`, that runs as the owner user. The first-login hook can invoke it. The hook still installs no packages.

The supervisor:
- Uses a separate private nonblocking lock. It never holds the bootstrap lock while it waits.
- Runs the existing bootstrap with its existing exit meanings.
- On a terminal exit, reports only when the provisioning file passes its private-file and parsing rules, the target and the pin are usable, and an existing station identity can be loaded safely.
- Never creates a replacement identity, never starts a mismatched service and never weakens the release validation to send a report.
- Reports at once, then every 600 seconds with bounded jitter and connection deadlines.
- Revalidates the file, the destination, the identity and the retained condition before every report and every retry.
- Stops reporting after completion. A successfully linked station uses the existing board management path.
- Treats a reporting failure as a diagnostic failure. It does not replace the retained provisioning error.

**Retry authorization.** The supervisor does not pass a boolean retry flag for a report retry. It writes a structured authorization `<station data>/provision-retry.json` with the mode 0600 and the fields `retry_id`, `terminal_id` and `state_digest`. It runs `provision-bootstrap --file <file> --retry-authorization <path>`.

Under `provision.lock`, the bootstrap:
1. Computes the current terminal binding: the terminal ID and the digest of the retained condition, the configuration, the destination pin and the station identity.
2. Compares the binding with the authorization.
3. Only when they are equal, writes in one atomic state-file write the cleared error and `consumed_retry_id`.

A mismatch keeps the retained condition, leaves the authorization unused and exits with `report_changed`. A `retry_id` that is already in `consumed_retry_id` is refused: there is no replay. A crash before the atomic write leaves the condition and the authorization unchanged. A crash after it cannot replay the permission, because `consumed_retry_id` is in the same write as the cleared error.

A retry clears only the terminal error that the existing retry semantics permit. All the P8 checks (configuration, release, identity, service and pairing) run again after the clear, as before. `--retry` without an authorization keeps its existing meaning for a linked station (DL-243 **Retry provisioning**).

Tests: a binding that changes between the supervisor check and the bootstrap lock (mismatch, `report_changed`, condition kept); a replay of a consumed `retry_id`; a crash on each side of the atomic write.

### 4.4 Deck and board routes

Owner-only board routes:

| Route | Purpose |
|---|---|
| `GET /api/provision-reports` | Bounded report list and the status of the reporting window |
| `POST /api/provision-reports/window` | Open or close the reporting; elevation required |
| `POST /api/provision-reports/{id}/retry` | Queue one retry for the current terminal binding; elevation required |
| `DELETE /api/provision-reports/{id}` | Dismiss a report |

The reporting window is separate from the pairing window. It lasts one hour and is off by default. Opening it must not open the pairing window.

Retry body:

```json
{
  "terminal_id": "<id>",
  "state_digest": "<sha256>",
  "request_id": "<id>"
}
```

A retry permission expires after twenty minutes and is single-use. A changed report, a dismissal, a revocation policy or the expiration of the report invalidates it. Repeated clicks with the same request ID do not create more than one permission.

The pairing page shows **Setup reports from unpaired keys**. It does not state "stations that could not finish setup".

Every row shows this text:

> "This key has not been paired. Its name and setup status are self-reported."

Each row shows the full grouped fingerprint, a labelled reported name, the fixed code text, the phase, the receipt time of the deck and the last contact. Never merge rows by name, address or claimed old identity. A report must not create a station row, a paired badge, a grant, an invitation or a control connection.

Controls:
- **Allow one setup retry**
- **Cancel retry**
- **Dismiss report**

Confirmation:

> "Allow this key to try setup once more? This does not pair it, change its trusted deck, replace its identity, or override setup checks."

Queued status:

> "Retry allowed. The setup supervisor will check again within about ten minutes while it is running."

Do not show "retry started" only because a permission is queued.

### 4.5 Provisioning texts

These are fixed catalogue strings. Peer text never supplies them.

| Code | Owner text | Report retry |
|---|---|---|
| `file_invalid` | "The provisioning file is missing, unsafe, or invalid." | No usable reporting path guaranteed |
| `release_untrusted` | "The trusted manifest or an installed hook file is missing, is not owned by root, or does not match its hash." | Disabled |
| `build_mismatch` | "The running build or the binary of the station does not match the trusted manifest." | Disabled |
| `config_conflict` | "An existing station configuration conflicts with this setup." | Disabled |
| `config_changed` | "The provisioning file changed after the pairing started." | Disabled |
| `service_conflict` | "The station service that is installed has another binary or another data folder." | Disabled |
| `station_identity_changed` | "The station key is not the key that the bootstrap saved first." | Disabled |
| `paired_elsewhere` | "The station is already paired with a deck that has another fingerprint." | Disabled |
| `denied` | "The pairing request was refused." | Allowed; normal pairing rules still apply |
| `cancelled` | "The pairing request was cancelled." | Allowed |
| `revoked` | "The pairing request or authorization was revoked." | Allowed; never clears revocation |
| `fingerprint_mismatch` | "The peer did not match the configured deck fingerprint." | Disabled |
| `request_id_conflict` | "The pairing request identifier conflicts with an earlier request." | Disabled |
| `pending` | "An earlier pairing request is still pending." | Allowed; never cancels another request |
| `unsupported` | "A required setup operation is not supported by this build." | Disabled |
| `invalid_request` | "A setup request was rejected as invalid." | Disabled |
| `retry_file` | "Fleet Deck could not safely use the saved pairing recovery file." | Disabled |
| `pairing_failed` | "Pairing did not finish." | Allowed |
| `provider_probe_failed` | "A configured provider is broken, unknown, or has no readable version." | Allowed |
| `integration_failed` | "The Omarchy integration install or its status check did not succeed." | Allowed |
| `unknown_error` | "Setup stopped for an unrecognized reason." | Allowed; checks remain binding |

For a disabled retry, the board shows:

> "Retrying cannot repair this condition. Fleet Deck has no board repair for it yet."

For a stale binding, the error is `report_changed`:

> "The reported setup condition changed. Review the latest report."

### 4.6 Abuse and exposure limits

These are explicit limits. There is no unspecified `N`.

- One latest report for each authenticated key. 64 reports in total. 24 hours of retention.
- Maximum report frame: 4 KiB. Stored row: 1 KiB.
- Name: 64 characters, stripped of controls and bidirectional formatting, rendered as plain text.
- Fixed allowlists for the code and the phase. An unknown code becomes `unknown_error`. A malformed frame is rejected.
- No stderr, file paths, configuration contents, credentials, build logs or environment fields.
- The existing bounded TLS and Pair admission applies before Auth. Add bounded global and per-source handshake rates and limits on concurrent connections.
- After Auth: at most one accepted update for each key in five minutes, and global update limits. Duplicate polls do not append history or audit rows.
- When the capacity is exhausted, the station refuses new rows. It never evicts station identities or pairing records.
- Reporting uses bounded work and storage that are separate from the normal paired control traffic.
- Audit the window changes and the owner retry decisions. Aggregate the rejected and flooded traffic. Never create an unlimited stream of audit rows.

A malicious tailnet peer can still fabricate reports and fill the report quota while a window is open. This is an accepted **lossy diagnostic-channel risk**. It is not proof of a station failure. If reliable admission is required, provision a separate enrollment capability through trusted setup media. Possession of a key alone cannot provide it.

### 4.7 P8-17

P8-17 still applies: code that already runs as the station user can impersonate its reports. The report channel is not attestation. Reference: `docs/v2/design/omarchy-p8.md` (the P8 trust boundary, line 291).

### 4.8 Residuals

The residual is larger than `file_invalid`. Each of these can stay unreachable or unrepairable:
- No valid destination or pin.
- No usable identity.
- No running supervisor or network.
- Trust, configuration and service repairs.

The annex records each of them explicitly. It provides no instruction to log in to a station.

## 5. BG-4: stopping a lost run

### 5.1 Why numeric kills are unsafe

A numeric group kill is unsafe, also with start-time checks. There are two separate problems:

1. A process can exit between the check of its start time and the signal to its numeric PID.
2. **The correct current start time of a PID does not prove that the PID belongs to the old run.** The existing inspection infers the membership from the recorded provider PID that it uses as PGID and SID. Those numbers can describe a later, unrelated execution.

The current `ExecutionCheck.complete` means that the inspection obtained enough evidence for its read-side decision. It is not a complete and stable termination inventory. An inspection can finish early when it finds a live member.

The current wire list has no start times and no ownership evidence. Reference: `crates/fleetdeck-proto/src/execution.rs` (line 20).

Decision: a new stop plan, retained pidfds, and stricter attribution checks. Never use `kill(-pgid, …)` in this operation.

A pidfd is a stable process reference for signalling. A check followed by the use of a numeric PID is not. The existing `rustix` already has the required operations. Reference: [Linux pidfd documentation](https://www.man7.org/linux/man-pages/man2/pidfd_open.2.html).

### 5.2 Wire contract

Feature `execution_stop.v1`, together with the existing `shim.resume.v1`.

Add the read-only method `session.execution_stop_plan`. Params:

```json
{
  "session_id": "<id>",
  "shim_id": "<id>"
}
```

Answer:

```json
{
  "plan_id": "<opaque>",
  "expires_at": 1792400030000,
  "eligible": true,
  "code": null,
  "targets": [
    { "pid": 1234, "command": "provider" },
    { "pid": 1235, "command": "worker" }
  ],
  "escaped": [],
  "check": {}
}
```

Start times, UID evidence and pidfds stay inside the station. Commands are bounded, cleaned `comm` values. They are never arguments.

Add the receipt-mutating `SessionCommand::StopExecution`:

```json
{
  "session_id": "<id>",
  "shim_id": "<id>",
  "plan_id": "<opaque>"
}
```

- It travels through the command envelope and the existing durable effect and receipt machinery.
- **Board origin only.** The deck checks the owner login and an elevation with the label **Stop previous run processes**.
- The command never accepts an origin, a PID list, a process group, a UID, a signal or a timeout from the caller.

Answer:

```json
{
  "outcome": "done",
  "code": null,
  "term_sent": 2,
  "kill_sent": 1,
  "gone": 2,
  "skipped": 0,
  "check": {}
}
```

An accepted command means that the operation was admitted. Only its completed effect establishes its outcome. The station persists the authorization before the signals. A duplicate command ID never signals again. After a station crash, an attempted effect becomes `outcome_unknown`. The recovery performs a fresh read and needs a fresh authorization for any further signal.

### 5.3 Attribution and signalling rules

1. Serialize the operation against resume, import, reconcile and other stop operations of that session.
2. Require the exact unresolved shim row, the database incarnation and the boot. Refuse an active managed run, a changed row, an unknown identity or an unsupported platform.
3. Obtain a fresh full inventory for the stop plan. Do not reuse the read-side check as a termination census.
4. Establish continuity with the original execution through the witness rules below.
5. **An equal old PGID or SID alone is never such a witness.** If the provider is gone and no reliable witness exists, return `identity_unproven` and send no signal.
6. Build the deduplicated union of the provider and the verified group and session members. Include the provider explicitly, because the existing member lists omit its leader.
7. Require station-user ownership. Exclude PID 1, the station, its supervisor and the protected infrastructure. Unexpected ownership or a protected member refuses the plan.
8. Open the pidfds. Then revalidate the identity, the ownership, the membership and the attribution around the acquisition. A reused PID must not become an authorized target.
9. Hold those pidfds in a short-lived plan: 30 seconds, at most 128 targets, at most eight plans in the whole station, one for each session. A plan is never truncated silently.
10. At the dispatch, revalidate the row, the boot, the plan and the targets. A new member or a changed target set before the signalling gives `plan_changed`.
11. Send SIGTERM to each target individually through its pidfd. Wait five seconds with monotonic time. Send SIGKILL only through the same pidfds, only to the surviving authorized targets.
12. Finish with a fresh execution check. Bound the operation to fifteen seconds. Survivors that are unreadable or uninterruptible stay visible.

**Witness.** When the shim spawns the provider in a new session, the station records durably the leader identity (the PID, the start time in clock ticks since boot, and the boot ID) with the run. The authorization of a domain (the session and the group of that PID) follows these rules:
- **(a) The leader is alive.** The leader with the recorded PID, start time and boot ID is alive, and it is still the leader of that session and of that group. Then every current member of that session or group that the station user owns is a target.
- **(b) The leader is gone.** A member is a target only when the station recorded it as a member witness (PID, start time and boot ID) while the leader was alive, and it is still in that session or group now.
- A witness authorizes only itself. It never authorizes other members.
- A witness that left the domain is not a target.
- A legacy run without a recorded leader identity gives `identity_unproven`.

Test: an escaped witness and a recycled domain number both refuse with `identity_unproven`.

Do not add newly forked processes to the authorized set automatically. Return `processes_remain` and require another reviewed plan. Otherwise a reviewed finite list becomes an open-ended kill authority.

### 5.4 Escaped-process semantics

Escaped-process semantics must be precise.

- A process that the plan shows as escaped is never a target.
- Skip a target that was observed to have left the allowed domain before the signalling.
- Userspace cannot test the session membership and signal a task in one atomic step. A target can move between the two operations. A pidfd prevents PID reuse. It does not prevent membership changes.

If "never signal a process after it escapes" is an absolute requirement, this design cannot promise it. Stronger containment and synchronization are required. The accepted boundary: this design stops the specifically authorized task identities, with membership rechecks. It does not contain adversarial processes of the same user.

The old-run attribution limit also stays real: **safe stopping of every existing legacy run cannot be retrofitted from the current read model**. Future containment for each run, or durable membership evidence, can improve the coverage. Do not turn an ambiguous observation into a kill target.

### 5.5 Board texts

| Code or control | Text |
|---|---|
| Action | **Stop these processes…** |
| Confirmation | "Stop the listed processes from this previous run? Fleet Deck will ask them to exit, then force-stop surviving targets after five seconds. Unsaved work may be lost." |
| Escaped note | "Processes outside the run's group and session are listed for information and will not be stopped." |
| `identity_unproven` | "Fleet Deck cannot safely prove that these processes belong to the previous run. No processes were stopped." |
| `inspection_incomplete` | "Fleet Deck could not complete the safety check. No processes were stopped." |
| `plan_changed` | "The process list changed. Review it again before stopping processes." |
| `permission_denied` | "Fleet Deck could not stop one or more processes as the station user." |
| `processes_remain` | "Some processes remain. Review the updated list." |
| `outcome_unknown` | "The stop result is unknown. Refresh the process check before taking another action." |

Do not **Mark as ended** automatically after a stop. Keep the existing separate action. It is enabled only after its own complete fresh check permits it. Escaped processes keep their existing informational, nonblocking semantics.

### 5.6 Audit

Audit `session.execution_stop` with: the owner, the station, the session, the shim, the command ID, the counts and a fixed outcome. The audit has no command names, no arguments and no raw `/proc` output.

## 6. Common authorization, version and proof requirements

All new mutation routes need an owner board login, the existing CSRF and origin protection, and a recent elevation. Scope `admin` alone must not make API tokens, MCP callers, coordinators, displays or provider TUIs eligible.

Feature support describes the protocol implementation. It does not attest installed packages, successful probes, safe process attribution or a completed live smoke.

Use independent feature gates. An older peer receives no unknown method and no unknown enum variant. The deck returns `unsupported`, and the board explains the missing capability. Report frames are sent only after bilateral negotiation. A station restart invalidates the plans in memory and the retained pidfds. An unknown outcome is never replayed automatically.

The four proofs are acceptance criteria:

| Surface | Behavior proof | Failure proof | Exposure proof | Version proof |
|---|---|---|---|---|
| Tools | Fixed facts use the desktop environment; exact compiled package argv; fresh facts after the transaction | Missing package, stale plan, denied sudo, shell disabled, disconnect during the transaction, station restart during the transaction, package succeeds but PATH stays wrong, a dry run with a package that is not allowed | No command interpolation; no password or terminal input in any stored or retransmitted output (early paste, failed authentication, reconnect, input after the exit), audit or storage; no input or recording side effects from probes | Independent diagnostics and install features; old station and old deck; installed package with an unknown executable version |
| Replacement | Unrecorded conflicts are replaced; recorded edits need a selection; backups and restore work | Backup collision, concurrent edit, full disk, failure or crash at every journal state, lost action answer, lock contention, changed restore target | No symlink, special file or path escape; no backup overwrite; no file contents in diagnostics or audit | An old station never gets the new action; CLI exit mapping; a repair journal or build mismatch invalidates the plan |
| Provision reports | A report reaches the unpaired-key list; the exact one-shot retry reruns the P8 checks | Invalid file or pin, absent key, offline supervisor, changed state, crash on each side of the atomic consumption, binding change before the bootstrap lock, full report capacity | Forged name or code, replay, cross-key retry, key rotation, Sybil flooding, bounded audit and storage, no enrollment side effect | Bilateral feature negotiation; old hook or package; ordinary pairing unchanged |
| Execution stop | The provider is included; the union is deduplicated; TERM then a five-second KILL through the retained pidfds | PID reuse at every seam, recycled PGID or SID, escaped witness, absent anchor, forks, escapes, zombies, permission failure, restart and duplicate command | Board-only admission; protected and foreign processes refused; no numeric group kill; no arguments exposed | Legacy rows without witnesses refuse safely; unsupported pidfds or platform; an old peer never receives the command |

Use fake binaries, fake process inspectors and deterministic crash seams for the automated proofs. The live smoke must exercise the real sudo and pacman, the supported compositor and tool versions, and controlled disposable process trees through the board. The automated tests must not install packages and must not signal unrelated host processes.

## 7. Build split and review order

| Task | Deliverable |
|---|---|
| **A: Protocol contracts** | Feature constants, strict schemas, generated TS, method scopes, error catalogue, independent version gates |
| **B: Tool diagnostics** | Shared desktop resolver, bounded probes, version provenance, keyboard, plugin and sudo facts |
| **C: Maintenance terminal** | Fixed package mapping, plans, owner elevation, dry-run transaction check, echo-off PTY baseline, separate runner process with state file and socket, reattach after a station restart; keep the no-install packaging test |
| **D: Installer repair** | CLI exit 3, structured plans, the distinction between conflict and edit, no-clobber backups, staged no-replace publication, journal states, recovery table, repair status, resume and restore |
| **E: Provision supervisor** | Packaged lifecycle, safe state and identity loading, terminal binding, polling and the structured retry authorization that the bootstrap consumes atomically under `provision.lock` |
| **F: Report listener and store** | Pair-channel branch, admission and resource limits, bounded report storage, owner routes and audit |
| **G: Execution safety** | Strong inventory, leader identity and member witnesses, pidfd plans, durable effects, session serialization, fresh final checks |
| **H: Board** | Diagnostics, maintenance preview and terminal, repair review and recovery, unpaired report list, stop confirmation and the exact texts |
| **I: Proofs and documents** | End-to-end tests; `PARITY.md`, `SECURITY-CHECKLIST.md`, operations, smoke and decision-log updates |

Review order:
1. Resolve the package sourcing, the repair recovery, the report admission and lifecycle, and the process-attribution semantics in the adversary DESIGN REVIEW **before their workers build**.
2. The PRE-PR review inspects the same boundaries and the crash and replay cases.
3. Run focused crate checks through the exact heavy-command queue of the repository. The coordinator owns `make check`.

The annex preserves three unresolved limits (§1.1): **unsupported package delivery, pre-link trust or configuration repairs, and legacy process attribution without a trustworthy witness**. They stay product gaps. None of them becomes an instruction for the owner to log in to a station.
