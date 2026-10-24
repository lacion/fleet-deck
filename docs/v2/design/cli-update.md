# The station updates Claude Code and Codex by itself (B28)

- **Status:** Draft for the architect, revision 2 (2026-10-24), after the adversary DESIGN REVIEW (`/home/luismorales/fleetdeck-briefs/backlog2/cli-update/adv-design.md`, FAIL, 1 blocker and 8 major findings). The section "Revision 2 (design review)" at the end lists each finding and the change. Risk: **high** (§6): the station runs code from the internet as the owner, on a timer. The adversary does a RE-REVIEW of this revision before the design is frozen and a PRE-PR REVIEW before the merge.
- **Backlog:** `docs/v2/BACKLOG.md` item B28 (lines 359-382).
- **Code base:** branch `v2`, HEAD `d1fa4205`. All file:line references are on this commit.
- **Evidence:** `/home/luismorales/fleetdeck-briefs/backlog2/cli-update/evidence-lighting.txt` (Lighting, read only; the vendor channels, read as data). The release key that §3.5 pins: `/home/luismorales/fleetdeck-briefs/backlog2/cli-update/claude-code.asc`.
- **Research limit:** the architect's fleet grant was refused ("The session token is not valid", `fleet_view` and `fleet://guide`). So I started no session on dev1, thunder or mac-m4, and §1.6 has facts of Lighting only. The design reports every layout that it does not know (§3.3), and the first merge step runs a read-only check on every station (§7, "At the merge").

## 1. Verified facts

### 1.1 What the code does with provider versions today

1. Nothing installs or updates a provider CLI (search on HEAD for `install`, `update`, `DISABLE_AUTOUPDATER`, `autoUpdat`: no hit in `crates/`).
2. The version gate: `tested_min_version`, `known_broken`, `version_status`, `classify` (`crates/fleetdeck-providers/src/version.rs:9-52`). An older CLI is `OlderThanTested` (a warning); a version on the deny list is `Broken`; no version or an unparsable one is `Unknown`. The constants: Claude `TESTED_MIN_VERSION = "2.1.289"`, `KNOWN_BROKEN = &[]` (`crates/fleetdeck-providers/src/claude/mod.rs:5-9`); Codex `"0.160.0"`, `&[]` (`crates/fleetdeck-providers/src/codex/mod.rs:75-77`).
3. The probe: Claude runs `--version` and `auth status` (`crates/fleetdeck-providers/src/claude/driver.rs:291-303`, `run_probe` at `:370-387`, both with the instance environment and a time limit). Codex runs `--version`, then `initialize` + `account/read` in a short `codex app-server` (`crates/fleetdeck-providers/src/codex/driver.rs:327-343`, `crates/fleetdeck-providers/src/codex/probe.rs:50-78`). So the Codex probe already speaks the protocol; the Claude probe does not.
4. The station probes **once per process**: `SessionManager::providers` fills a `tokio::sync::OnceCell` (`crates/fleetdeck-station/src/manager/manager_providers.rs:47-79`, field at `manager/mod.rs:125-126`, DL-059). The same call marks a `Broken` instance (`mark_broken`, `manager/target_base.rs:20-32`) and records the version for the handoff check (`record_version`, `coordinator/bindings_handoff.rs:39-49`). Nothing resets the cell. **Consequence:** a CLI that changes while the station runs keeps its old version on the board, in `station.info` and in the drift report (`drift.rs:200-219` reads `providers()`), until a station restart; a new version on the deny list is not refused until then.
5. `record_version` only appends (`bindings_handoff.rs:45-48`), and `handoff(provider, None)` checks every recorded version (`:26-36`). After a version change both versions stay in the list.
6. A start of a `Broken` instance fails at dispatch with `provider_broken` (`manager/target.rs:284-287`, text at `target_base.rs:35-40`).
7. The handoff minimums: `VERIFIED` Claude 2.1.296, Codex 0.160.0 (`crates/fleetdeck-providers/src/coordinator.rs:197-200`); `BRIDGE_VERIFIED` Claude 2.1.296, Codex 0.162.1 (`:227-231`); `handoff` and `bridge_handoff` (`:254-262`).
8. The binary of an instance is `binary` in `[[provider_instance]]`: a bare name is looked up on `PATH` at launch, a path is absolute (`crates/fleetdeck-core/src/config.rs:187-206`). The adapters get it as `program` (`manager/adapters.rs:22-54`), with `ChildEnv::for_instance(ChildEnv::from_process(), instance)`.
9. `ChildEnv` removes `ANTHROPIC_*`, `CLAUDE_*`, `OPENAI_*`, `CODEX_*`, `FLEETDECK_*` (`crates/fleetdeck-core/src/childenv.rs:26-42`). `DISABLE_AUTOUPDATER` is not removed and not set: **every Claude process that the station starts can run Claude's own background updater today** (§1.5).
10. The station upgrade (`crates/fleetdeck-station/src/upgrade.rs:1-20`, `docs/v2/operations.md:1378-1390`) replaces the Fleet Deck binary only.

### 1.2 Start admission and the restart freeze

1. The restart freeze (`crates/fleetdeck-station/src/manager/restart_gate.rs:1-25`): while `restarting`, the decision refuses new sessions (`guard`, `:194-200`) and the effect worker holds queued starts and resumes (`begin_claim`, `:117-123`, `hold_starts`). A claim counts itself before it reads the flag; the admission sets the flag before it reads the counts.
2. The worker keeps the claim guard only for `Start` and `Resume` effects, until their dispatch returns (`crates/fleetdeck-station/src/worker.rs:93-104`).
3. `settle_starts(bound)` waits until no such submit or claim is in flight (`restart_gate.rs:237-250`).
4. Live provider processes: `SessionTarget::live_pids` (`manager/target.rs:240-256`); a `Live` entry holds a direct session or a shim-hosted (`durable`) one (`target.rs:45-54`).

### 1.3 Settings, the link and the audit

1. Station settings: one `FieldMeta` per parser key (`crates/fleetdeck-core/src/settings/fields.rs:66-348`); a test asserts that the keys equal the parser keys, with dotted keys for the tables (`settings/tests/fields.rs:53-72`). `[model_help]` is the pattern of a table (`crates/fleetdeck-core/src/config_model_help.rs`, `config_raw.rs:23-58`, view values at `settings/holder.rs:169-180`).
2. The board's Settings sections are a list of keys (`web/src/settings/model.ts:9-80`). The station tab places cards in a section slot (`web/src/settings/SettingsTab.tsx:446-457`). The elevation dialog is `useElevation` (`web/src/settings/elevation.tsx:16-63`).
3. `station.info` is asked when a link comes up and every 60 s (`crates/fleetdeck-deck/src/link/info.rs:17-115`). `StationInfo` (`crates/fleetdeck-proto/src/link.rs:242-312`, file at 751 lines) has only optional additions since P4-2. The member view and the wall view drop `info` (`crates/fleetdeck-deck/src/http/read_routes.rs:17-30`, `http/display_view.rs` `display_station`).
4. The deck audit: `NewAudit`, `DeckStore::audit` (`crates/fleetdeck-deck/src/store/audit.rs:40-128`). The station has no audit log.
5. An owner station action: `station_call_wait` (`crates/fleetdeck-deck/src/http/settings_routes.rs:223-267`) and the restart route with the owner check, the elevation and the audit (`settings_routes.rs:657-690`).
6. HTTPS from the station: `curl` with the request as a config on stdin, `-q` first, `proto =https`, `no-location`, a fixed environment (`crates/fleetdeck-station/src/integrations/http.rs:51-176`, `integrations/exec.rs:131-150`).
7. Feature flags: `feature::ALL` (`crates/fleetdeck-proto/src/version.rs`, last entry `WALL_APPS`); link rows with `p5a_row` (Admin, a feature, not mutating at the link: "the deck keeps its own durable record", `crates/fleetdeck-link/src/methods_table_tail.rs:38-43`, `TAIL` has 68 rows).
8. The deck has no time zone database; schedules use `UTC` or a fixed offset (`crates/fleetdeck-deck/src/scheduler/cron.rs:13-15`, `parse_offset` `:181-204`).
9. Migrations: station at V46 (`crates/fleetdeck-station/src/store/migrations.rs`, last `schema_v46`); deck at V50 (`crates/fleetdeck-deck/src/store/schema.rs:790-793`). `schema.rs` has **799 lines**.

### 1.4 File sizes that limit the plan (rule: under 800 lines)

| File | Lines |
| --- | --- |
| `crates/fleetdeck-deck/src/store/schema.rs` | 799 |
| `crates/fleetdeck-station/src/link/runtime.rs` | 785 |
| `crates/fleetdeck-station/src/link/intake.rs` | 781 |
| `crates/fleetdeck-deck/src/store/mod.rs` | 780 |
| `crates/fleetdeck-station/src/manager/mod.rs` | 779 |
| `crates/fleetdeck-station/src/manager/target.rs` | 771 |
| `crates/fleetdeck-deck/src/http/settings_routes.rs` | 757 |
| `crates/fleetdeck-deck/src/http/mod.rs` | 754 |
| `crates/fleetdeck-proto/src/link.rs` | 751 |
| `crates/fleetdeck-providers/src/claude/driver.rs` | 740 |
| `crates/fleetdeck-station/src/store/mod.rs` | 734 |

New code goes into new files. The edits in these files are the line counts that §7 names.

### 1.5 The vendors (documents and channels, 2026-10-24)

Claude Code (`https://code.claude.com/docs/en/setup`):

1. Install methods: the native installer (recommended), Homebrew casks `claude-code` (stable) and `claude-code@latest`, WinGet, signed apt/dnf/apk repositories, and npm `@anthropic-ai/claude-code`.
2. "Native installations automatically update in the background": a check at the start and periodically while it runs. `DISABLE_AUTOUPDATER=1` stops the background check only; `claude update` and `claude install` still work. `DISABLE_UPDATES` blocks all of them.
3. `claude update` follows `autoUpdatesChannel` (`latest`, the default, or `stable`: "about one week old, skipping releases with major regressions"). `claude install <target>` takes `stable`, `latest` or a version number. `minimumVersion` is a floor for updates.
4. The native launcher `~/.local/bin/claude` is a symlink into `~/.local/share/claude/versions/`. The installer keeps the launcher's version, every version that a session on the same machine runs, and the two newest; it deletes the rest.
5. "Each release publishes a `manifest.json` containing SHA256 checksums for every platform binary. The manifest is signed with an Anthropic GPG key" (from 2.1.89). The key: `https://downloads.claude.ai/keys/claude-code.asc`, fingerprint `31DD DE24 DDFA B679 F42D 7BD2 BAA9 29FF 1A7E CACE`. Linux binaries have no code signature; macOS binaries are signed by "Anthropic PBC" and notarized.
6. npm: `npm install -g @anthropic-ai/claude-code@latest` upgrades; the package has a `postinstall` (`node install.cjs`) and per-platform optional dependencies; it has no provenance attestation (evidence).
7. The channel pointers (data, read by the vendor's own `bootstrap.sh`): `https://downloads.claude.ai/claude-code-releases/latest` → `2.1.310`, `/stable` → `2.1.308`; `/<version>/manifest.json` and `/<version>/manifest.json.sig`. The manifest has `version`, `buildDate`, `platforms.<os>-<arch>[-musl].checksum`.

Codex (source `openai/codex`, `codex-rs/tui/src/update_action.rs`, main):

8. `codex update` runs, by install method: `npm install -g @openai/codex`, `bun install -g @openai/codex`, `pnpm add -g …`, `vp install -g …`, `brew upgrade --cask codex`, or for the standalone installer `sh -c "curl -fsSL https://chatgpt.com/codex/install.sh | CODEX_NON_INTERACTIVE=1 sh"`. The method comes from `CODEX_MANAGED_BY_NPM` / `_BUN` / `_PNPM` / `_VITE_PLUS`, which the `codex.js` launcher sets.
9. npm: `@openai/codex` has one release dist-tag, `latest` (0.164.0), plus `alpha` tags. Each platform build is a version of the same package (`0.163.0-linux-x64`). Each version has registry signatures (key `SHA256:DhQ8wR5APBvFHLF/+Tc+AYvPOdTpcIDqOhxsBHRwC7U`, ECDSA P-256) and an SLSA provenance attestation. The package has no install script. I verified one signature over `@openai/codex@0.163.0:<integrity>` with that key (`openssl … Verified OK`, evidence).
10. The full npm document of `@openai/codex` is 18 MB (abbreviated 5 MB), so the station must not read it every night. The per-version document and `/-/package/<name>/dist-tags` are small. The tarball answers `Last-Modified`.

### 1.6 Lighting (the only station that I could read)

1. Claude: native. `~/.local/bin/claude` → `~/.local/share/claude/versions/2.1.309`. Four versions are on disk (2.1.306 to 2.1.309, about 291 MB each), installed on 21, 22, 22 and 24 October, two of them after midnight. `~/.claude.json` says `autoUpdates: false`, `autoUpdatesProtectedForNative: true`: the native updater ran anyway. The SHA-256 of `versions/2.1.309` equals the signed manifest checksum of `linux-x64`.
2. Codex: Bun global, 0.163.0 (`~/.bun/install/global/node_modules/@openai/codex`). **A second, older Codex is installed with npm** in the nvm prefix (0.146.1); it is shadowed because `~/.bun/bin` is first on the `PATH` of the station process. An updater that asks "is npm on PATH?" would update the wrong copy.
3. Tools: `gpgv` from Homebrew on Linux (`/home/linuxbrew/.linuxbrew/bin/gpgv`); `gpgv` with the pinned key verified the manifest of 2.1.309 (`VALIDSIG 31DDDE24…CACE`).

### 1.7 Corrections to the backlog

1. "Nothing in the code installs or updates a provider CLI": true for Fleet Deck code, but the native Claude updates itself today from inside station sessions (§1.1 item 9, §1.6 item 1). So the change of behavior that B28 makes is mostly a change of *who* updates Claude, and with which checks.
2. "The station only probes the installed version" — once per process (§1.1 item 4). A change that the station does not make is invisible until a restart. §3.9 fixes it for every change.
3. The handoff check of the backlog (`bindings_handoff.rs`) keeps every version that it ever saw (§1.1 item 5). §3.8 replaces the list after a probe.

## 2. Answers to the questions of the backlog

**Q1. Which install methods does the station detect, and which does it update?** It detects native (Claude), npm global, Bun global, Homebrew, a system package and unknown (§3.3). It updates two layouts, and in both it first stages the new version, verifies the bytes against the vendor signature and probes them, and only then activates them (§3.6, revision 2):
- native Claude with the installer-managed launcher `~/.local/bin/claude`: the station downloads the platform binary as the vendor's `bootstrap.sh` does, checks it against the signed manifest, and swaps the launcher link;
- Codex as an npm or Bun global: the package manager stages `@openai/codex@<version>` into a private folder with scripts off, the station checks every staged package against the signed registry integrity, and moves the verified folders into place.

Claude installed with npm or Bun (its `postinstall` runs code that the station cannot bind to a signature), Homebrew, system packages, the Codex standalone installer (a `curl | sh` at run time), a custom launcher link and unknown layouts are **report only** (owner questions 6 and 9).

**Q2. Schedule.** A nightly window on the station's own clock, default 03:00 to 05:00 local time, **off by default** (owner question 1). The owner can also click "Update now" or "Check" per station and per provider at any time. No update runs at the station start (a restart storm must not become an update storm); the start only finishes an interrupted attempt (§3.7).

**Q3. Pin or follow.** Follow a vendor channel, with a pin per provider as the override. Claude: `stable` (default) or `latest`. Codex: `latest` (its only channel). A new version installs only when it is at least `min_age_hours` old (default 48). The fixed bounds stay: never a `KNOWN_BROKEN` version, never below `TESTED_MIN_VERSION`, never below the handoff minimum. No upper cap (owner question 8).

**Q4. Gate.** Before any byte of the new version runs: the version gate (§3.4), the vendor signature (§3.5), and the binding of the staged bytes to the signed digests (§3.6). Then, still before the activation: the version and a protocol handshake of the staged copy with the adapter's real launch arguments. After the activation: the launcher resolves to the new copy and reports its version. A failure after the activation goes back to a local copy of the previous version, with no network (§3.6.2).

**Q5. Running sessions.** The activation starts only when no session of that provider has a live process, and it holds new starts and resumes until it ends (§3.7). The download and the staging run before the hold, so the hold lasts seconds. The owner can force an update while sessions run only for native Claude, where the running version stays on disk (owner question 4).

**Q6. Settings.** `[provider_updates]` in `station.toml`, shown in Settings → the station tab → "Provider updates" (§3.10).

**Q7. Audit.** Every attempt is a row in `station.db`; the deck copies every finished attempt into its audit log, and the owner's click is audited at the route (§3.11).

## 3. Design

### 3.1 Words

- **Install:** one provider CLI on one station, as the first provider instance of that kind runs it (the drift report uses the same "first instance", `drift.rs:200-207`). Another instance of the same kind with another resolved binary is reported as `second_install` and not updated.
- **Attempt:** one run of §3.7 for one provider: scheduled, by the owner, a recovery, or an external change that the station saw.
- **Target:** the version that an attempt wants to install.

### 3.2 Wire types (`crates/fleetdeck-proto/src/cli_update.rs`, new)

```rust
//! Provider CLI updates by the station (B28, design `cli-update.md`).
//! No type here holds a path, a URL or command output.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::SessionId;
use crate::event::ProviderKind;

/// How a provider CLI was installed (§3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "cli_update/")]
pub enum InstallMethod {
    /// Claude's native installer (`~/.local/share/claude/versions`).
    Native,
    Npm,
    Bun,
    Homebrew,
    SystemPackage,
    #[serde(other)]
    Unknown,
}

/// What started an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "cli_update/")]
pub enum UpdateTrigger {
    Schedule,
    Owner,
    /// The station start found an attempt that a stop interrupted.
    Recovery,
    /// The station saw a version change that it did not make.
    External,
    #[serde(other)]
    Unknown,
}

/// The result of an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "cli_update/")]
pub enum UpdateOutcome {
    Running,
    Updated,
    UpToDate,
    /// Sessions of the provider ran during the whole window.
    SkippedBusy,
    /// A rule or a check refused the target before any change.
    Refused,
    /// The install failed and the previous version is in place.
    Failed,
    /// The new version failed a check; the previous version is back.
    RolledBack,
    /// The new version failed and the way back failed too.
    RollbackFailed,
    /// The method is not one that the station updates.
    ReportOnly,
    /// The station stopped during the attempt (§3.7 recovery).
    Interrupted,
    /// A quarantined CLI passed the checks of §3.8.1 again; its starts are
    /// allowed again (revision 2).
    Verified,
    #[serde(other)]
    Unknown,
}

/// One attempt, as `station.db` keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "cli_update/")]
pub struct CliUpdateAttempt {
    pub seq: u64,
    pub attempt_id: String,
    pub provider: ProviderKind,
    pub trigger: UpdateTrigger,
    pub method: InstallMethod,
    /// `stable`, `latest` or `pin`.
    pub channel: String,
    pub from_version: Option<String>,
    pub target_version: Option<String>,
    /// The version after the attempt (the previous one after a rollback).
    pub to_version: Option<String>,
    pub outcome: UpdateOutcome,
    /// A code of [`reason`]; `None` for a plain `updated` or `up_to_date`.
    pub reason: Option<String>,
    /// Unix ms, station clock.
    pub started_at: i64,
    pub ended_at: Option<i64>,
}

/// The update state of one provider of a station.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "cli_update/")]
pub struct ProviderUpdateState {
    pub provider: ProviderKind,
    pub method: InstallMethod,
    /// The station can update this install now: a supported method and
    /// its verify tool are present.
    pub updatable: bool,
    /// Why not, when `updatable` is false: a code of [`reason`].
    pub blocked: Option<String>,
    /// `stable`, `latest` or `pin`.
    pub channel: String,
    pub installed: Option<String>,
    /// The target of the last check when it is newer than `installed`.
    pub available: Option<String>,
    /// Unix ms of the last check (station clock).
    pub checked_at: Option<i64>,
    pub last: Option<CliUpdateAttempt>,
    /// The starts of this provider are refused after a failed update or an
    /// unfinished recovery (§3.8.1): a code of [`reason`]
    /// (`rollback_failed`, `recovery_pending`, `process_left`).
    pub quarantined: Option<String>,
}

/// `cli_update.status`, and `StationInfo::cli_update`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "cli_update/")]
pub struct CliUpdateReport {
    /// `provider_updates.enabled` (the nightly window).
    pub enabled: bool,
    /// The window as text, for example `03:00-05:00 local`.
    pub window: String,
    pub providers: Vec<ProviderUpdateState>,
    /// The attempt that runs now.
    pub running: Option<String>,
    /// The highest `seq` of a finished attempt (the deck's audit import).
    pub last_seq: u64,
}

/// `cli_update.run` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "cli_update/")]
pub struct CliUpdateRun {
    pub provider: ProviderKind,
    /// Only resolve and verify the target; change nothing; no attempt row.
    #[serde(default)]
    pub check_only: bool,
    /// Update while sessions of the provider run (native Claude only).
    #[serde(default)]
    pub force: bool,
}

/// The answer of `cli_update.run`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "cli_update/")]
pub struct CliUpdateRunAnswer {
    pub state: ProviderUpdateState,
    /// The attempt that runs now (an update that started).
    pub attempt_id: Option<String>,
    /// The update did not start: a code of [`reason`].
    pub refused: Option<String>,
    /// The live sessions of the provider, with `refused = sessions_running`.
    #[serde(default)]
    pub busy: Vec<SessionId>,
}

/// `cli_update.attempts` params: finished attempts after `after_seq`,
/// oldest first, at most `limit` (1 to 100).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "cli_update/")]
pub struct CliUpdateAttemptsQuery {
    pub after_seq: u64,
    pub limit: u32,
}

/// The answer of `cli_update.attempts` (revision 2): the rows carry the
/// incarnation of the database that they come from, so the deck never files
/// rows of a restored database under the old cursor (§3.11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "cli_update/")]
pub struct CliUpdateAttemptsPage {
    pub incarnation: crate::ids::IncarnationId,
    pub rows: Vec<CliUpdateAttempt>,
}

/// The reason codes of attempts and refusals. The board has a text for
/// each (§3.12); an unknown code shows as "not updated".
pub mod reason {
    pub const REPORT_ONLY_METHOD: &str = "report_only_method";
    pub const SECOND_INSTALL: &str = "second_install";
    pub const NOT_FOUND: &str = "not_found";
    pub const GPGV_MISSING: &str = "gpgv_missing";
    pub const TOOL_MISSING: &str = "tool_missing";
    pub const DISK_FULL: &str = "disk_full";
    pub const CHANNEL_UNREACHABLE: &str = "channel_unreachable";
    pub const NOT_A_VERSION: &str = "not_a_version";
    pub const SIGNATURE_INVALID: &str = "signature_invalid";
    pub const SIGNATURE_UNKNOWN_KEY: &str = "signature_unknown_key";
    pub const PROVENANCE_MISSING: &str = "provenance_missing";
    pub const KNOWN_BROKEN: &str = "known_broken";
    pub const BELOW_TESTED: &str = "below_tested";
    pub const BELOW_HANDOFF: &str = "below_handoff";
    pub const TOO_NEW: &str = "too_new";
    pub const PUBLISH_TIME_UNKNOWN: &str = "publish_time_unknown";
    pub const STATION_RESTARTING: &str = "station_restarting";
    pub const STARTS_IN_FLIGHT: &str = "starts_in_flight";
    pub const SESSIONS_RUNNING: &str = "sessions_running";
    pub const ANOTHER_UPDATE: &str = "another_update";
    pub const INSTALL_FAILED: &str = "install_failed";
    pub const TIMEOUT: &str = "timeout";
    pub const VERSION_MISMATCH: &str = "version_mismatch";
    pub const CHECKSUM_MISMATCH: &str = "checksum_mismatch";
    pub const HANDSHAKE_FAILED: &str = "handshake_failed";
    pub const OUTSIDE_FLEET_DECK: &str = "outside_fleet_deck";
    // Revision 2.
    /// A launcher link that the vendor installer does not manage (§3.3).
    pub const CUSTOM_LAUNCHER: &str = "custom_launcher";
    /// The stage and the destination are not on one file system, or the
    /// new package changes its `bin` map (§3.6).
    pub const UNSUPPORTED_LAYOUT: &str = "unsupported_layout";
    /// npm before 7 (no hidden lockfile) or Bun without a text `bun.lock`.
    pub const TOOL_TOO_OLD: &str = "tool_too_old";
    /// The stage holds a package that is not in the verified set.
    pub const UNVERIFIED_PACKAGE: &str = "unverified_package";
    /// A staged package's integrity differs from the signed one.
    pub const INTEGRITY_MISMATCH: &str = "integrity_mismatch";
    /// A process of the attempt survived TERM and KILL (§3.6.3).
    pub const PROCESS_LEFT: &str = "process_left";
    /// A journal row of an interrupted attempt waits for the station start.
    pub const RECOVERY_PENDING: &str = "recovery_pending";
    /// The way back failed; the provider is quarantined.
    pub const ROLLBACK_FAILED: &str = "rollback_failed";
    /// The provider is quarantined; only §3.8.1 or a passing update clears it.
    pub const QUARANTINED: &str = "quarantined";
}
```

`StationInfo` (`link.rs`, after `fleet_summary`):

```rust
    /// The provider CLI updates of the station (B28). Absent from an older
    /// station, or when the station has no update service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cli_update: Option<crate::cli_update::CliUpdateReport>,
```

`cli_update.attempts` answers a `CliUpdateAttemptsPage` (revision 2), not a bare list.

Feature `feature::CLI_UPDATE = "cli_update.v1"` in `version.rs`, appended to `ALL` after `WALL_APPS`, not in `REQUIRED`. Link rows (append to `TAIL`, 68 → 71):

```rust
    // B28: provider CLI updates. Admin; the deck keeps its own audit.
    p5a_row("cli_update.status", feature::CLI_UPDATE),
    p5a_row("cli_update.run", feature::CLI_UPDATE),
    p5a_row("cli_update.attempts", feature::CLI_UPDATE),
```

### 3.3 Detection of the install method (`cli_update/detect.rs`)

Input: the first instance of the kind (`policy.instances()` order), its `binary`, and its child environment (`ChildEnv::for_instance`). Output:

```rust
pub struct Install {
    pub kind: ProviderKind,
    pub method: InstallMethod,
    /// The path that the launch runs (the PATH hit, or the absolute `binary`).
    pub launcher: PathBuf,
    /// The link chain of `launcher`: `launcher` first, then each hop of
    /// `read_link` made absolute, at most 8 hops; the last entry is `real`.
    pub chain: Vec<PathBuf>,
    /// `launcher` with every link resolved.
    pub real: PathBuf,
    pub tool: Tool,
    /// Set when the method is report only or a tool is missing.
    pub blocked: Option<&'static str>,
}

pub enum Tool {
    /// `link` is `home/.local/bin/claude` (the launcher link that the vendor
    /// installer manages); `versions` is `home/.local/share/claude/versions`;
    /// `gpgv` is the verifier.
    Native { link: PathBuf, versions: PathBuf, gpgv: Option<PathBuf> },
    /// Codex only. `modules` is `<prefix>/lib/node_modules`, `bin_link` is
    /// `<prefix>/bin/codex` (a member of `chain`), `npm` is `<prefix>/bin/npm`.
    Npm { prefix: PathBuf, npm: PathBuf, modules: PathBuf, bin_link: PathBuf },
    /// Codex only. `modules` is `<bun_install>/install/global/node_modules`,
    /// `bin_link` is `<bun_install>/bin/codex` (a member of `chain`), `bun`
    /// is `<bun_install>/bin/bun`.
    Bun { bun_install: PathBuf, bun: PathBuf, modules: PathBuf, bin_link: PathBuf },
    None,
}

/// Pure: classifies the link chain with `home`. `read_link` answers one
/// hop (made absolute) or `None` for a file that is not a link; `exists`
/// answers for a path.
pub fn classify(kind: ProviderKind, chain: &[PathBuf], home: Option<&Path>,
                read_link: &dyn Fn(&Path) -> Option<PathBuf>,
                exists: &dyn Fn(&Path) -> bool) -> (InstallMethod, Tool, Option<&'static str>);

/// Resolves the launcher on the instance PATH (same rule as the launch),
/// builds `chain` (more than 8 hops → `Err(not_found)`) and calls `classify`.
pub fn detect(kind: ProviderKind, binary: &Path, env: &ChildEnv) -> Result<Install, &'static str>;
```

Rules of `classify`, in this order (paths are compared by components, never by substring; `real` is the last entry of `chain`; `V` is `home/.local/share/claude/versions`):

| # | Condition | Method | Tool | `blocked` |
| --- | --- | --- | --- | --- |
| 1 | Claude; `chain` contains `L = home/.local/bin/claude`; `read_link(L)` (one hop) is `V/<v>` with `<v>` matching `^\d{1,6}\.\d{1,6}\.\d{1,6}$`; `real == V/<v>` | `Native` | `Native { link: L, versions: V, gpgv }` | `gpgv_missing` when no `gpgv` |
| 2 | Claude; `real` under `V` but rule 1 fails (a custom link such as `/custom/bin/claude → V/<v>`, a configured absolute path to one version file, or `L` that does not point straight into `V`) | `Unknown` | `None` | `custom_launcher` |
| 3 | Claude; `real` has the components `<prefix>/lib/node_modules/@anthropic-ai/claude-code/…` or `<bun_install>/install/global/node_modules/@anthropic-ai/claude-code/…` | `Npm` / `Bun` | `None` | `report_only_method` (revision 2: its `postinstall` cannot be bound to a signature) |
| 4 | Codex; `real` has the components `<prefix>/lib/node_modules/@openai/codex/…`; `chain` contains `<prefix>/bin/codex`; `<prefix>/bin/npm` exists | `Npm` | `Npm { prefix, npm, modules, bin_link }` | — |
| 5 | Codex; `real` is under `<bun_install>/install/global/node_modules/@openai/codex/` (`<bun_install>` is taken from the components of `real`, never from the environment); `chain` contains `<bun_install>/bin/codex`; `<bun_install>/bin/bun` exists | `Bun` | `Bun { bun_install, bun, modules, bin_link }` | — |
| 6 | Codex; rule 4 or 5 matches without its `bin/codex` in `chain` | `Npm` / `Bun` | `None` | `custom_launcher` |
| 7 | Codex; rule 4 without `<prefix>/bin/npm`, or rule 5 without `<bun_install>/bin/bun` | `Npm` / `Bun` | `None` | `tool_missing` |
| 8 | `real` starts with `/opt/homebrew/`, `/usr/local/Caskroom/`, `/usr/local/Cellar/` or `/home/linuxbrew/.linuxbrew/` | `Homebrew` | `None` | `report_only_method` |
| 9 | `real` starts with `/usr/` or `/opt/` (and not rule 8) | `SystemPackage` | `None` | `report_only_method` |
| 10 | anything else (the Codex standalone installer, a custom script) | `Unknown` | `None` | `report_only_method` |

- `gpgv` lookup: the first executable of `gpgv` on the instance PATH, then `/usr/bin/gpgv`, `/opt/homebrew/bin/gpgv`, `/usr/local/bin/gpgv`, `/home/linuxbrew/.linuxbrew/bin/gpgv`.
- `detect` returns `Err(reason::NOT_FOUND)` when the launcher is not found.
- A chain such as `/usr/local/bin/claude → home/.local/bin/claude → V/2.1.309` is native (rule 1): the station swaps `L`, and every link before it follows. A link straight into `V` that skips `L` is `custom_launcher`: the station cannot activate a new version through it without taking over a link that the vendor does not manage (finding 9).
- Lighting: Claude → rule 1 (native, `gpgv` found); Codex → rule 5 (Bun). The shadowed npm copy (§1.6) is never seen, because detection starts from the launcher that the station runs.

### 3.4 The gate (`cli_update/gate.rs`, pure)

```rust
pub struct GateInput<'a> {
    pub kind: ProviderKind,
    pub installed: Option<&'a str>,
    pub target: &'a str,
    pub pinned: bool,
    /// The station hosts sessions in a shim (`host = "shim"`).
    pub shim_host: bool,
    /// Unix ms when the target was published; `None` when unknown.
    pub published_at: Option<i64>,
    pub now_ms: i64,
    pub min_age_hours: u16,
}

pub enum GateVerdict { Install, UpToDate }

/// `Err` holds a code of `reason`.
pub fn gate(input: &GateInput<'_>) -> Result<GateVerdict, &'static str>;

/// A release version: `^\d{1,6}\.\d{1,6}\.\d{1,6}$` (no pre-release, no build).
pub fn is_release(version: &str) -> bool;
```

Checks, in order:

1. `!is_release(target)` → `not_a_version`. (This check also keeps a value that looks like an option out of every argv, §6 T3.)
2. `version_status(kind, Some(target))`: `Broken` → `known_broken`; `OlderThanTested` or `Unknown` → `below_tested`.
3. `coordinator::handoff(kind, Some(target))` is `Err` → `below_handoff`; with `shim_host`, `bridge_handoff(kind, Some(target))` is `Err` → `below_handoff`.
4. Up to date: not `pinned` and `installed` parses and `installed >= target` (numeric compare of the three parts) → `UpToDate`. `pinned` and `installed == target` → `UpToDate`. (No downgrade without a pin.)
5. Soak: not `pinned`, `min_age_hours > 0`: `published_at` is `None` → `publish_time_unknown`; `now_ms - published_at < min_age_hours * 3_600_000` → `too_new`.
6. Else `Install`.

### 3.5 The target and its verification (`cli_update/channel.rs`, `cli_update/verify.rs`)

All fetches use `curl` like the integrations (`-q -sS --fail -K -`, config on stdin with `proto = "=https"`, `max-time = 20`, `no-location`, `max-filesize`), with `curl_env()`. The hosts are constants; no host or path comes from the wire or from a response.

```rust
pub const CLAUDE_RELEASES: &str = "https://downloads.claude.ai/claude-code-releases";
pub const NPM_REGISTRY: &str = "https://registry.npmjs.org";
/// The fingerprint of the Claude Code release signing key (vendor docs).
pub const CLAUDE_KEY_FPR: &str = "31DDDE24DDFAB679F42D7BD2BAA929FF1A7ECACE";
/// The same key, dearmored (`gpg --dearmor` of `claude-code.asc`).
pub const CLAUDE_KEYRING: &[u8] = include_bytes!("keys/claude-code.gpg");
/// The npm registry signing keys (`/-/npm/v1/keys`), keyid → SPKI DER (base64).
pub const NPM_KEYS: &[(&str, &str)] = &[(
    "SHA256:DhQ8wR5APBvFHLF/+Tc+AYvPOdTpcIDqOhxsBHRwC7U",
    "MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEY6Ya7W++7aUPzvMTrezH6Ycx3c+HOKYCcNGybJZSCJq/fd7Qa8uuAKtdIkUQtQiEKERhAmE5lMMJhP8OkDOa2g==",
)];
```

**Claude (native only, revision 2).**

1. Target: the pin, or the text of `CLAUDE_RELEASES/<channel>` (`stable` or `latest`; at most 64 bytes, trimmed). It must pass `is_release`.
2. Manifest: `CLAUDE_RELEASES/<target>/manifest.json` (at most 256 KiB) and `…/manifest.json.sig` (at most 16 KiB).
3. Signature (`verify_claude_manifest`): in a new private temp directory (0700) under `<station dir>/cli-update/tmp`, write `claude-code.gpg` (`CLAUDE_KEYRING`), `manifest.json`, `manifest.json.sig` and an empty `gnupg` directory (0700). Run `gpgv --homedir <tmp>/gnupg --keyring <tmp>/claude-code.gpg --status-fd 1 <tmp>/manifest.json.sig <tmp>/manifest.json` with the environment `PATH`, `LANG=C` only, time limit 20 s. It passes only when gpgv exits 0 **and** a stdout line starts with `[GNUPG:] VALIDSIG ` and its last field equals `CLAUDE_KEY_FPR`. A `VALIDSIG` of another key → `signature_unknown_key`; anything else → `signature_invalid`. The temp directory is removed after.
4. The manifest must parse, its `version` must equal the target, and `buildDate` (RFC 3339 UTC) gives `published_at`.
5. The platform (revision 2: one key, one checksum): `platform = <os>-<arch>` (`os`: `linux` or `darwin`; `arch`: `x64` or `arm64`, from `std::env::consts`), plus `-musl` on Linux when `/lib/ld-musl-x86_64.so.1` or `/lib/ld-musl-aarch64.so.1` exists. `expected_sha256 = platforms.<platform>.checksum` (64 lower-case hex digits). No such key → `signature_invalid`. Every later byte check of this attempt compares with this one value.

**npm and Bun (`@openai/codex` only, revision 2).**

6. Target: the pin, or `dist-tags.latest`, from `NPM_REGISTRY/-/package/@openai%2fcodex/dist-tags` (at most 64 KiB).
7. The version document `NPM_REGISTRY/<name>/<target>` (at most 1 MiB): `name` and `version` must match; `dist.integrity` starts with `sha512-`; at least one `dist.signatures[]` entry whose `keyid` is in `NPM_KEYS` verifies (`verify_npm_signature`); else `signature_unknown_key` (a keyid that is not pinned) or `signature_invalid`.
8. `verify_npm_signature(name, version, integrity, sig_b64, spki_b64)`: the SPKI DER must be 91 bytes with the prefix `3059301306072a8648ce3d020106082a8648ce3d030107034200`; the last 65 bytes are the point. `ring::signature::UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, point).verify(format!("{name}@{version}:{integrity}").as_bytes(), &sig_der)`.
9. Platform packages: for each `optionalDependencies` entry whose key ends with `-<os>-<arch>` or `-<os>-<arch>-musl`: the spec `npm:<n>@<v>` names `<n>@<v>`; a plain spec `<v>` names `<key>@<v>`; `<v>` must be `<target>` or `<target>-<suffix>` with a suffix of `[a-z0-9-]{1,32}`. Each is fetched and checked as in step 7.
10. Provenance: the main version document must have `dist.attestations.provenance.predicateType == "https://slsa.dev/provenance/v1"`; else `provenance_missing`. (The station checks the presence, not the Sigstore bundle; §6 T5.)
11. `published_at`: `Last-Modified` of `HEAD NPM_REGISTRY/<name>/-/<basename>-<target>.tgz` (curl config `head`, `write-out = "%header{last-modified}"`), parsed as an RFC 1123 GMT date. No header → `None` (§3.4 step 5).
12. The result is the **verified set** `VS`: one entry per package of steps 7-9, `{ name, version, integrity }` (`name` is the real package name of the version document, also for an alias key). §3.6 S3 binds every staged package to `VS`.

```rust
/// The result of §3.5 (revision 2). No path, no URL from a response.
pub enum Verified {
    Native { target: String, published_at: Option<i64>, platform: &'static str, expected_sha256: String },
    Npm { target: String, published_at: Option<i64>, set: Vec<VerifiedPackage> },
}
pub struct VerifiedPackage { pub name: String, pub version: String, pub integrity: String }
```

A check (`check_only`) runs §3.3, steps 1-12 and §3.4, and remembers `available` and `checked_at` in memory. It writes no attempt row and changes nothing (one exception: §3.8.1 for a quarantined provider).

### 3.6 Stage, bind, activate and the way back (`cli_update/stage.rs`, `cli_update/activate.rs`, `cli_update/fence.rs`; revision 2)

**The rule (finding 1):** no byte of the new version runs before the station has bound it to the signed digest of §3.5. The order is always:

1. **Stage:** download or extract into a private folder. No script runs.
2. **Bind:** compare the staged bytes with `Verified` (§3.5).
3. **Probe the staged copy** (§3.6.1), at its staged path.
4. **Hold** (§3.7.1).
5. **Rollback copy:** a complete local copy of the previous version, checked, before any change.
6. **Activate:** renames on one file system.
7. **Verify the active copy** through the launcher.

A failure in steps 1 to 5 changes nothing in the install. A failure in steps 6 and 7 restores the local rollback copy, with no network (§3.6.2).

**Children.** Every child of an attempt (curl, npm, bun, gpgv, the probes) runs without a shell, with a fixed argv, stdin closed and a time limit. It runs in its own process group (`process_group(0)`), with the marker `FLEETDECK_CLI_UPDATE_ID=<marker>` in its environment. `marker` is the attempt id of the first attempt of the journal row (§3.7.2); it does not change when a recovery attempt takes the row over. Its stdout and stderr are appended to `<station dir>/cli-update/logs/<attempt_id>.log` (mode 0600, at most 256 KiB, the newest 20 files kept). No child output goes into the database, the wire or the station log. After each child, the fence of §3.6.3 runs.

**Environments:**

| Child | Environment |
| --- | --- |
| curl, gpgv | as §3.5 (`curl_env()`; gpgv: `PATH`, `LANG=C`), plus the marker |
| npm, bun (staging) | **Only these names** (finding 7): `PATH` = `<tool folder>:` + the instance `PATH`; `HOME=<R>/home`; `XDG_CONFIG_HOME=<R>/xdg`; `XDG_CACHE_HOME=<R>/cache`; `LANG=C`; the proxy and CA names of `curl_env()` and `NODE_EXTRA_CA_CERTS` when the station process has them; the marker; for bun also the four `BUN_INSTALL*` names of S2. No `npm_config_*` or `NPM_CONFIG_*`, no other `BUN_*`, no `NODE_OPTIONS`, no instance `env` table. `<R>/home`, `<R>/xdg`, `<R>/cache` and `<R>/cwd` are new empty 0700 folders; the cwd of the child is `<R>/cwd`. |
| probes (§3.6.1) | `ChildEnv::for_instance` of the instance, plus `DISABLE_AUTOUPDATER=1` and the marker |

#### Native Claude

The station does what the vendor's `bootstrap.sh` does for the download and the checksum, and then activates the verified file itself. It does not run `claude install` (that command downloads by itself, and nothing binds those bytes before they run).

| Step | Action | Failure |
| --- | --- | --- |
| S1 | Free space of the file system of `versions` is at least 1 GiB (`rustix::fs::statvfs`). Download `CLAUDE_RELEASES/<target>/<platform>/claude` (the URL form of `bootstrap.sh`; T5 keeps a copy of the script in `testdata`, and a test asserts the form) with the curl config `output = "<versions>/.fd-stage-<attempt_id>"`, `create-file-mode = 0600`, `max-filesize` 512 MiB, time limit 10 min. | `disk_full` (refused); `channel_unreachable` (failed) |
| S2 | **Bind:** the SHA-256 of the staged file (streamed, `ring::digest`) equals `expected_sha256`. Nothing has run the file. | `checksum_mismatch` (refused); the file is removed |
| S3 | `chmod 0755`. When `versions/<target>` exists, its SHA-256 must equal `expected_sha256`: then the staged file is removed and the existing file is used. Else rename the staged file to `versions/<target>` with no replace (Linux: `renameat_with(…, RenameFlags::NOREPLACE)`; macOS: `link` then `unlink` of the stage). | `checksum_mismatch` (refused) |
| S4 | §3.6.1 on `versions/<target>` itself: `version == target` and the handshake passes. The launcher still points to `from`. | `version_mismatch`, `handshake_failed` (refused); `versions/<target>` is removed when S3 created it |
| H | The hold (§3.7.1). | as §3.7 step 13 |
| A1 | **Rollback copy:** `<station dir>/cli-update/rollback/<attempt_id>/claude` is a hard link of `versions/<from>` (`linkat`), or a copy when the link fails with `EXDEV` (free space checked first). Its SHA-256 must equal `rollback_sha256` (the SHA-256 of `real`, taken at the attempt start). Then the journal row gets phase `activating`, `rollback_path` and `rollback_sha256`, committed before A2. | `disk_full`, `checksum_mismatch` (refused; nothing activated) |
| A2 | **Activate:** create the link `<L>.fd-new-<attempt_id>` → `versions/<target>` (an absolute target, the form of the vendor's link on Lighting) and rename it over `L`. | → §3.6.2 |
| V | `canonicalize(launcher) == versions/<target>`, and `<launcher> --version` parses to `target`. | `version_mismatch` → §3.6.2 |

The rollback copy is outside `versions/`, so the cleanup of the vendor installer (it keeps the launcher's version, the running versions and the two newest) cannot remove it (finding 2).

#### Codex by npm or Bun

`R` is the stage root `<parent of modules>/.fd-cli-update/<attempt_id>`: `<prefix>/lib/.fd-cli-update/<id>` for npm, `<bun_install>/install/global/.fd-cli-update/<id>` for Bun. It is created 0700. `st_dev(R)` must equal `st_dev(modules)`, else `unsupported_layout` (refused): the activation is a rename. `SM` is the staged global folder: `<R>/p/lib/node_modules` (npm) or `<R>/b/install/global/node_modules` (Bun).

| Step | Action | Failure |
| --- | --- | --- |
| S1 | Free space of the file system of `modules` is at least 1 GiB. | `disk_full` (refused) |
| S2 | **Stage** with the package manager, scripts off, into `R` only. Every destination is explicit, so no inherited setting can choose another install (finding 7). npm: `<npm> install --global --prefix=<R>/p --userconfig=<R>/empty.npmrc --globalconfig=<R>/empty.npmrc --cache=<R>/cache --registry=https://registry.npmjs.org/ --@openai:registry=https://registry.npmjs.org/ --ignore-scripts --no-fund --no-audit --no-update-notifier @openai/codex@<target>` (`empty.npmrc` is an empty 0600 file). Bun: `<bun> add --global --ignore-scripts --registry=https://registry.npmjs.org/ @openai/codex@<target>` with `BUN_INSTALL=<R>/b`, `BUN_INSTALL_GLOBAL_DIR=<R>/b/install/global`, `BUN_INSTALL_BIN=<R>/b/bin`, `BUN_INSTALL_CACHE_DIR=<R>/cache`. Time limit 10 min. | `install_failed`, `timeout` (failed; nothing changed) |
| S3 | **Bind** (`verify_staged`, pure over a file tree; finding 1). The lockfile is `<R>/p/lib/node_modules/.package-lock.json` (npm 7 or later) or `<R>/b/install/global/bun.lock` (text; only a `bun.lockb` → `tool_too_old`). (a) Every package entry of the lockfile (npm: each key of `packages` except `""`; Bun: each value of `packages`, `["<name>@<version>", …, "<integrity>"]`) names a `name@version` of `VS`, and its `integrity` equals the integrity of that `VS` entry. (b) Every package folder on disk under `SM` (also in nested `node_modules`; scoped names; dot entries skipped) has a `package.json` whose `name@version` is in `VS` and in the lockfile. (c) The `bin` map of `SM/@openai/codex/package.json` equals the `bin` map of `modules/@openai/codex/package.json`. The package manager checked each tarball against the integrity that it recorded (npm: ssri; Bun: its integrity check), so (a) binds the extracted bytes to the signed integrity. No staged file has run (scripts off). | `tool_too_old`, `unverified_package`, `integrity_mismatch`, `unsupported_layout` (refused) |
| S4 | §3.6.1 on `SM/@openai/codex/bin/codex.js`: `version == target` and the handshake passes. | `version_mismatch`, `handshake_failed` (refused) |
| H | The hold (§3.7.1). | as §3.7 step 13 |
| A1 | **Rollback copy:** `moved` is the list of the top-level packages of `SM`, sorted (a scope folder counts per package: `@openai/codex`, and on Bun also `@openai/codex-linux-x64`). For each `t` of `moved`, `digest(t)` is the tree digest of `modules/<t>`, or `null` when `modules/<t>` does not exist. Then the journal row gets phase `activating`, `moved` (JSON `[{"name": t, "digest": …}]`) and `rollback_path = <R>/old`, committed before A2. The originals stay in place until A2; A2 only renames them, so the rollback copy is the complete previous tree on the same file system. | — |
| A2 | **Activate:** for each `t` of `moved`: rename `modules/<t>` → `<R>/old/<t>` (when it exists), then `SM/<t>` → `modules/<t>`. `bin_link` keeps its path (S3 (c)). | → §3.6.2 |
| V | The `chain` of the launcher ends inside `modules/@openai/codex/`, and `<launcher> --version` parses to `target`. | `version_mismatch` → §3.6.2 |

- The tree digest (`cli_update/tree_digest.rs`, pure over a file tree): SHA-256 over the sorted list of `(relative path, kind, mode, SHA-256 of the content or the link text)` of every entry of the folder. Links are not followed.
- The package managers' own records do not change. npm discards the hidden lockfile of the real prefix when a folder is newer than it. Bun's global `package.json` and `bun.lock` still name the old version, so a later `bun add -g` or `bun update -g` by the owner can put the old version back; the station then records an `external` change (§3.9). Accepted (§6 T13).

**Clean-up.** The rollback copy (native) and `R` (npm, Bun) are removed when the attempt ends `Updated`, `RolledBack`, `Refused`, `Failed` or `SkippedBusy`. They stay with `RollbackFailed`, for the owner. At each attempt start and in the start recovery (§3.7.2), every `.fd-cli-update/<id>` folder, `versions/.fd-stage-<id>` file and `rollback/<id>` folder whose `<id>` is not the `attempt_id` or the `marker` of a journal row, and is not the folder of a `rollback_failed` quarantine row, is removed.

#### 3.6.1 The probe (`crates/fleetdeck-providers/src/update_probe.rs`, new)

```rust
/// The result of the update probe of one CLI (B28 §3.6.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateProbe {
    pub version: Option<String>,
    /// `Ok` when the CLI answered the protocol handshake with the adapter's
    /// launch arguments; `Err` with a short fixed reason.
    pub handshake: Result<(), &'static str>,
}

/// `--version`, then the handshake. No model call, no thread, no prompt.
/// `cwd` is an empty private folder. Each step is bounded by `limit`; the
/// child is killed at the end.
pub async fn check(
    kind: ProviderKind,
    program: &Path,
    env: &ChildEnv,
    cwd: &Path,
    limit: Duration,
) -> UpdateProbe;
```

- Claude: spawn `program` with `claude::args::cli_args(&Launch::default(), &claude::capabilities(false))`, write `claude::control::initialize("probe-init")` and a newline, read stdout lines until a JSON line with `type == "control_response"` and `response.request_id == "probe-init"`. Pass: `response.subtype == "success"` and `response.response.models` is an array (the adapter needs it, trap 1). Then close stdin and kill. Fail reasons: `no_start`, `no_answer`, `error_answer`, `no_models`.
- Codex: the frames of `codex::probe::account` (`initialize`, `initialized`, `account/read`) on `app_server_args()`; pass when the `initialize` answer has a `result` and the `account/read` answer is a `result` (any account state). Fail reasons: `no_start`, `no_answer`, `error_answer`.
- `limit` is 30 s in production.
- Revision 2 (finding 5): each child runs in its own process group (`process_group(0)`); the caller puts the marker of §3.6.3 into `env`. When `check` ends (pass, fail or limit), it sends SIGKILL to the group (`rustix::process::kill_process_group`) and waits for the child. The caller then runs the fence.
- `program` is the staged copy (§3.6 S4), the launcher (§3.6 V, §3.6.2, §3.8.1) or the launcher for the version only (§3.7 step 3).

#### 3.6.2 The way back (local, no network; finding 2)

`restore(record: &RecoveryRecord)` uses only local files and is idempotent: the start recovery can run it again after a second crash.

| Method | Restore | Verify |
| --- | --- | --- |
| Native | When `versions/<from>` exists and its SHA-256 equals `rollback_sha256`, use it. Else copy `rollback_path` to `versions/.fd-stage-restore-<attempt_id>` (0755), check its SHA-256 against `rollback_sha256`, and rename it over `versions/<from>`. Then create `<L>.fd-new-<attempt_id>` → `versions/<from>` and rename it over `L`. | `canonicalize(launcher) == versions/<from>`; its SHA-256 equals `rollback_sha256`; §3.6.1 on the launcher passes with `version == from` |
| Npm, Bun | For each entry `t` of `moved`, in reverse order. When `digest(t)` is not `null` and `<R>/old/<t>` exists: rename `modules/<t>` (when it exists) to `<R>/failed/<k>-<t>` (`k` is the next free number), then rename `<R>/old/<t>` → `modules/<t>`. When `digest(t)` is not `null` and `<R>/old/<t>` does not exist: nothing (never moved, or already back). When `digest(t)` is `null` and `modules/<t>` exists: rename it to `<R>/failed/<k>-<t>`. | the tree digest of each `modules/<t>` equals `digest(t)`; §3.6.1 on the launcher passes with `version == from` |

The verify passes → `RolledBack` (with the reason of the first failure). Else → `RollbackFailed`, and the provider is quarantined (§3.8.1) with the text `"{name} failed after an update and the way back failed. Reinstall it on this station: {hint}"`, where `hint` is `claude install <from>` (native), `npm install -g @openai/codex@<from>` (npm) or `bun add -g @openai/codex@<from>` (Bun).

#### 3.6.3 The fence (`cli_update/fence.rs`; finding 5)

```rust
/// The marker of every child of an attempt.
pub const MARKER_VAR: &str = "FLEETDECK_CLI_UPDATE_ID";

/// Stops every process of `marker`: SIGTERM to the group `pgid`, a wait of
/// at most 5 s, SIGKILL to the group, then
/// `setup_cleanup::kill_marked_in(MARKER_VAR, marker, pgid, 5 s)`.
/// `Ok` only when the group is gone (`test_kill_process_group` answers
/// `ESRCH`) and the marker scan finds no process. Blocking: call it in
/// `spawn_blocking`.
pub fn fence(marker: &str, pgid: Option<i32>) -> Result<(), &'static str /* process_left */>;
```

- The fence runs after each child of the attempt ends (exit, error or time limit), before the next step. It reuses the setup cleanup (DL-148): a group is signalled only when every process in it carries the marker, so a pgid that the system reused after a reboot is never signalled. On macOS the marker scan uses `ps`, as the setup cleanup does.
- The pgid of the S1 download and of the S2 staging child is written to the journal row (`installer_pgid`) right after the spawn.
- No rollback, no release of the hold, and no end of the row before the fence passes. A failed fence ends the attempt `RollbackFailed (process_left)` in one transaction that also writes the quarantine (`process_left`) and **keeps** the journal row. Then the hold is released (the quarantine refuses the starts of this provider). The next station start runs the fence again before its recovery (§3.7.2).
- §3.6.1 kills its own process group when it ends; the fence after it also finds a child that left the group (`setsid`) by the marker.

### 3.7 One attempt (`cli_update/run.rs`)

```text
run_attempt(kind, trigger, check_only, force) -> CliUpdateRunAnswer
 1. Single flight: try_lock(self.running). Held → refused another_update.
 2. A journal row of `kind` exists → refused recovery_pending (only the station
    start finishes it, §3.7.2).
 3. install = detect(kind)                      (Err → refused not_found)
    installed = §3.6.1 version only, on the launcher
 4. `kind` is quarantined and check_only → verify_current (§3.8.1); answer; end.
 5. blocked (report_only_method, custom_launcher, second_install, tool_missing,
    gpgv_missing)
      → check_only: answer the state; else record ReportOnly (report_only_method,
        custom_launcher, second_install) or Refused (tool_missing, gpgv_missing); end.
 6. verified = §3.5
      Err(channel_unreachable) → record Failed; other Err(r) → record Refused(r); end.
 7. verdict = gate(§3.4)  UpToDate → record UpToDate; Err(r) → record Refused(r); end.
    ("record" = one row inserted already ended, `cli_update_insert(a, Some(end))`;
     with check_only no row is written in steps 5-7, the answer carries the result.)
 8. check_only → state.available = target, checked_at = now; answer; end (no row).
 9. Busy precheck, no hold yet (it saves a download):
      live = manager.live_sessions_of(kind)
      live not empty and not (force and method == Native)
         → owner: answer busy = live, no row; schedule: kept in memory (§3.9); end.
10. One transaction (`cli_update_begin`): the attempt row (outcome running,
    from_version, target) and the journal row (phase staging, marker = attempt_id,
    the paths of §3.6, rollback_sha256 = SHA-256 of `real` for native).
11. Answer the caller now: attempt_id, started. The rest runs in a task.
12. §3.6 S1-S4 (stage, fence, bind, probe the staged copy). A failure → remove
    the stage; one transaction: end the row Failed (install_failed, timeout,
    channel_unreachable) or Refused(r), and delete the journal row; end.
13. Hold: manager.begin_provider_update(kind):
      restarting → end Refused(station_restarting)
      settle_starts(10 s) false → release; end SkippedBusy(starts_in_flight)
      live not empty and not (force and method == Native)
         → release; end SkippedBusy(sessions_running)
    (each end: one transaction with the delete of the journal row; then the
     stage is removed)
14. §3.6 A1 (rollback copy, journal phase activating), A2 (activate), V.
15. V passes → one transaction: end Updated (to = target), delete the journal
    row, delete the quarantine row of `kind`. Then release_quarantine(kind),
    refresh_providers(), release the hold, remove the rollback copy and R.
16. A2 or V fails → fence → restore (§3.6.2) → its verify.
      pass → one transaction: end RolledBack, delete the journal row.
      fail → one transaction: end RollbackFailed, delete the journal row,
             write the quarantine row (rollback_failed, hint).
    Then the quarantine mark (when written), refresh_providers(), release the hold.
17. Limits: step 12 at most 12 min; steps 13-16 (the hold) at most 3 min.
    Past a limit → SIGKILL to the group, the fence, then step 12's failure
    (before A2) or step 16 (from A2 on), with reason timeout.
```

- `Failed` versus `RolledBack`: `Failed` means that nothing in the install changed (a failure in step 12). `RolledBack` and `RollbackFailed` come only after A2.
- A scheduled busy result writes no row at step 9; the schedule (§3.9) writes one `SkippedBusy` row per window when the provider stayed busy in all of it. An owner attempt that is busy at step 9 writes no row either: the answer carries `busy`, and the route audits the refusal (§3.11). A busy result at step 13 (a session started between steps 9 and 13) ends the row `SkippedBusy`.
- Every row end that also changes the journal or the quarantine is **one** transaction (`cli_update_finish`, §3.11).

#### 3.7.1 The hold (`manager/provider_update.rs`, new; `restart_gate.rs`)

`RestartState` gets one field and four methods:

```rust
    /// The provider kind under an update (B28): 0 none, 1 Claude, 2 Codex.
    updating: AtomicU8,

    /// Starts the update hold of `kind`. False when the station restarts or
    /// another update holds: then nothing changed.
    pub(super) fn begin_update(&self, kind: ProviderKind) -> bool {
        let code = kind_code(kind);
        if self.restarting() || self.updating.compare_exchange(0, code, SeqCst, SeqCst).is_err() {
            return false;
        }
        // The restart admission sets `restarting` before it reads `updating`.
        if self.restarting() {
            self.updating.store(0, SeqCst);
            return false;
        }
        true
    }
    pub(super) fn end_update(&self) { self.updating.store(0, SeqCst); self.changed.notify_waiters(); }
    #[must_use] pub fn updating(&self) -> Option<ProviderKind> { /* decode */ }
```

- `begin_claim`: `hold_starts: self.restarting() || self.updating().is_some()`. Queued starts and resumes of **every** provider wait while an update holds. Revision 2: the hold covers only §3.6 H to V and the way back (at most 3 min, usually seconds); the download and the staging run before it. They are not failed.
- The restart admission (`settings_svc.rs`, the `station.restart` handler) refuses with `invalid_state` "a provider update runs on this station; restart in a few minutes" when `updating()` is `Some` after it set `restarting` (it then clears `restarting`).
- The dispatch backstop (`target.rs`, after the `broken_reason` check at `:284-287`): when `updating() == Some(kind of the instance)`, the start fails with the text "the station was updating {claude|codex}; start the session again" and the code `provider_updating`. Only a deferred setup start can reach it.
- `UpdateHold` is an RAII guard: drop calls `end_update()` and `store.nudge_worker()`.

```rust
impl SessionManager {
    /// The sessions with a live process whose instance is of `kind`.
    pub async fn live_sessions_of(&self, kind: ProviderKind) -> Vec<SessionId>;
    /// §3.7.1. `Err(reason)` with `station_restarting`, `another_update` or
    /// `starts_in_flight` (after `settle_starts(10 s)`).
    pub async fn begin_provider_update(&self, kind: ProviderKind) -> Result<UpdateHold, &'static str>;
}
```

#### 3.7.2 Recovery at the station start (`cli_update/recover.rs`; findings 3 and 4)

`recover(store, config, station_dir)` runs in `serve.rs` **after `Store::open` and before `SessionManager::start`**. At that point no effect worker, link, assist service or board terminal exists, so nothing can start a provider while the recovery runs (finding 4). It needs only the store, the configured instances (`ChildEnv::for_instance`), detection and §3.6.1. It runs also when updates are off, and it does not wait for a deck. Its whole limit is 5 min.

```text
recover:
 0. store.cli_update_interrupt_orphans(): a `running` row without a journal row
    ends `interrupted` (it cannot exist by construction; a defense).
 1. For each journal row J (Claude first, then Codex):
    a. fence(J.marker, J.installer_pgid) (§3.6.3).
       Fail → one transaction: end J's attempt RollbackFailed(process_left)
       when it is running; quarantine (process_left); keep J. Next row.
    b. J.phase == staging (nothing was activated): remove the stage; one
       transaction: end J's attempt Interrupted (to = from) and delete J. Next row.
    c. J.phase == activating. First one transaction (`cli_update_recover_begin`),
       before any change on disk (finding 3):
         end J's attempt Interrupted when it is still running;
         insert the successor attempt S (trigger recovery, outcome running,
           from = J.target_version, target = J.from_version);
         J.attempt_id = S.attempt_id (J.marker stays);
         upsert the quarantine row (recovery_pending).
       It returns the `RecoveryRecord` of J (§3.11).
    d. restore (§3.6.2) with the record, then its verify.
       pass → one transaction: end S RolledBack, delete J, delete the quarantine row.
       fail → one transaction: end S RollbackFailed, delete J, quarantine
              (rollback_failed, hint).
 2. Past the 5 min limit: the rows that are not done keep J and the quarantine row
    (recovery_pending). The station goes on with its start; the next start tries
    again. A second crash at any point leaves J, so the next start finds it.
 3. The stage clean-up of §3.6.
```

Then `SessionManager::start` loads the quarantine rows into the broken marks before its effect worker starts (§3.8.1), so a provider with an unfinished recovery cannot start a session.

### 3.8 The provider report follows the install (`manager_providers.rs`)

1. `providers: tokio::sync::OnceCell<…>` becomes `providers: tokio::sync::Mutex<Option<Vec<StationProvider>>>` (`manager/mod.rs:126`, `:396`).
2. `providers()` keeps its contract (the first call probes; later calls reuse) under the mutex.
3. New `refresh_providers()`: probes now under the same mutex, replaces the cache, and applies the effects of the new result: `target.set_denied(instance, kind, status == Broken)` (it sets or clears a deny mark only), and `coordinator.replace_versions(kind, versions of that kind)`.
4. The value of `SessionTarget.broken` (`target.rs:100`) becomes a mark with two kinds (revision 2):

   ```rust
   /// Why the starts of an instance are refused.
   pub(super) enum BrokenMark {
       /// The probed version is on the deny list (R8.2, DL-162). A refresh sets and clears it.
       Denied(&'static str),
       /// A failed update or an unfinished recovery (B28 §3.8.1). Only
       /// `release_quarantine` clears it.
       Quarantined(Arc<str>),
   }
   ```

   `mark_broken` stays as `set_denied(…, true)`. `set_denied(…, false)` removes only a `Denied` mark. `quarantine(kind, text)` sets `Quarantined` on every instance of the kind (it replaces a `Denied` mark). `release_quarantine(kind)` removes only `Quarantined` marks; the next refresh sets `Denied` again when it applies. The dispatch check (`target.rs:284-287`) and `broken_reason` (`target_base.rs:35-40`) give the text of either kind.
5. `Bindings::replace_versions(provider, Vec<String>)` (`bindings_handoff.rs`).
6. `refresh_providers()` runs after every attempt that changed or may have changed an install, and after an external change (§3.9).

#### 3.8.1 The quarantine (finding 8)

- **Durable.** The table `cli_update_quarantine` (§3.11) holds one row per quarantined provider. It is written in the transaction that ends an attempt `RollbackFailed`, in the first transaction of a recovery (§3.7.2 c) and with a failed fence.
- **Loaded before dispatch.** `SessionManager::start_inner` reads `store.cli_update_quarantines()` and calls `target.quarantine(kind, text)` for each row **before** `EffectWorker::register` (`manager/mod.rs:357`), next to the setup cleanup (`:318`). No queued start can dispatch first.
- **Texts:**
  - `rollback_failed`: the text of §3.6.2, with the hint.
  - `recovery_pending`: `"the station could not finish an interrupted {name} update. Restart the station; if this stays, reinstall {name}: {hint}"`.
  - `process_left`: `"a process of a {name} update did not stop. Restart the station."`
- **Cleared only by** an attempt that ends `Updated` (§3.7 step 15), or by `verify_current(kind)`. Each clears it in the transaction that records its result. `refresh_providers()` never clears a quarantine, and a version string alone never clears it.
- `verify_current(kind)`, in this order:
  1. No journal row of `kind` (else `recovery_pending`).
  2. Detect (§3.3); `<launcher> --version` parses; `version_status` is not `Broken`.
  3. Native with `gpgv`: §3.5 steps 2 to 5 for the installed version, and the SHA-256 of `real` equals its `expected_sha256` (else `checksum_mismatch`). Other methods have no integrity check here: the station did not stage the owner's reinstall (§6 T14).
  4. §3.6.1 on the launcher passes (else `handshake_failed`).
  5. Pass → one transaction: insert an attempt row (trigger `owner` or `external`, outcome `verified`, `to_version` = the version) and delete the quarantine row; then `release_quarantine(kind)` and `refresh_providers()`. Fail → no row; the answer carries the reason, and the quarantine stays.
- `verify_current` runs for the owner's `Check` on a quarantined provider (`check_only: true`, the one write of a check), and on the schedule tick when the `--version` of a quarantined provider changed (the owner reinstalled it, §3.9).
- The schedule does not update a quarantined provider. The owner can (`Update now…`); an update that passes clears the quarantine.

### 3.9 The schedule (`cli_update/schedule.rs`)

- A task started by `serve.rs` with the shutdown channel. First tick 2 min after the start (the recovery of §3.7.2 ended before the manager started), then every 15 min.
- **External change, every tick, also when updates are off:** for each provider, `<launcher> --version` (10 s, §3.6.1 version only, fenced). When it differs from the cached `providers()` version and no attempt runs: `refresh_providers()`, and one attempt row with trigger `external`, outcome `updated`, from → to, reason `outside_fleet_deck`. The audit log then also shows Claude's own updates. When the provider is quarantined, `verify_current` (§3.8.1) runs instead, with the trigger `external`; its row replaces the `updated` row when it passes, and a failure writes no row (one try per changed version).
- **The window**, when `provider_updates.enabled`: `offset` is the fixed offset of `timezone`, or for `local` the output of `date +%z` (10 s; on failure the tick is skipped). The window is `[window_start, window_start + window_hours)` in that offset; it may cross midnight. A station starts its first try at `window_start + jitter`, `jitter = (first byte of the SHA-256 of the station name) % min(60, window_minutes / 2)` minutes, so the fleet does not update at one moment.
- In the window, for Claude then Codex, skipping a provider that is quarantined or has a journal row: when no attempt of that provider with the trigger `schedule` ended in this window (except a busy one), run `run_attempt(kind, Schedule, false, false)`. A busy result is kept in memory; at the first tick after the window, a provider that was busy at every try gets one `SkippedBusy` row (reason `sessions_running`).
- No update runs at the station start, and none outside the window without the owner.

### 3.10 Settings (`[provider_updates]` in `station.toml`)

```rust
/// The `[provider_updates]` table of `station.toml` (B28).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderUpdatesConfig {
    /// The nightly window. A restart value: it also sets
    /// `DISABLE_AUTOUPDATER=1` for the Claude processes of the station.
    pub enabled: bool,
    /// Minutes after 00:00 (from `HH:MM`).
    pub window_start_min: u16,
    /// 1 to 12.
    pub window_hours: u8,
    pub timezone: UpdateZone,
    pub claude_channel: ClaudeChannel,
    pub claude_pin: Option<String>,
    pub codex_pin: Option<String>,
    /// 0 to 336.
    pub min_age_hours: u16,
}

pub enum UpdateZone { Local, Fixed(i32 /* minutes east of UTC */) }
pub enum ClaudeChannel { Stable, Latest }
```

| Key | Kind, apply | Default | Label | Help |
| --- | --- | --- | --- | --- |
| `provider_updates.enabled` | Bool, Restart, danger | `false` | Update Claude and Codex at night | The station installs new Claude and Codex versions in the night window, only from the vendor, and goes back to the old version when a check fails. Claude sessions that the station starts then no longer update Claude by themselves. Takes effect after a station restart. |
| `provider_updates.window_start` | Text, Live | `"03:00"` | Night window start | The start of the update window, HH:MM in the time zone below. |
| `provider_updates.window_hours` | Int 1-12, Live | `2` | Night window length (hours) | How long the station tries; it waits while sessions run. |
| `provider_updates.timezone` | Text, Live | `"local"` | Window time zone | local (the clock of this machine), UTC, or an offset like +02:00. |
| `provider_updates.claude_channel` | Enum `stable`/`latest`, Live | `stable` | Claude channel | stable: a version about one week old that skips releases with major regressions. latest: every release. Codex has one channel. |
| `provider_updates.claude_pin` | Text, Live, danger | `""` | Claude version pin | An exact version, for example 2.1.305. The station installs it and stays on it, also when it is older. Leave it empty to follow the channel. |
| `provider_updates.codex_pin` | Text, Live, danger | `""` | Codex version pin | An exact version, for example 0.163.0. The station installs it and stays on it, also when it is older. Leave it empty to follow the latest release. |
| `provider_updates.min_age_hours` | Int 0-336, Live, danger | `48` | Wait for new versions (hours) | A new version installs only after it has been public this long, so that a bad release is found first. A pin does not wait. |

Validation errors (in `RawProviderUpdates::validate`): `window_start` "use HH:MM from 00:00 to 23:59"; `window_hours` "use a value from 1 to 12"; `timezone` "use local, UTC or an offset like +02:00 or -05:30" (offset at most 14:00); pins "use an exact version like 2.1.305, or leave it empty" (`is_release` rule); `min_age_hours` "use a value from 0 to 336".

`DISABLE_AUTOUPDATER=1` (when `enabled` at the start): set in the child environment of every Claude instance in `adapters_from_config` (`manager/adapters.rs:34-41`) and in `assist_instances` (`serve.rs:562`), unless the instance `env` table sets the name. Board terminals are not changed.

### 3.11 The deck: routes and the audit

**Routes** (`crates/fleetdeck-deck/src/http/cli_update_routes.rs`, new):

| Method, path | Who | Station call | Answer |
| --- | --- | --- | --- |
| `GET /api/stations/{station}/cli-update` | owner (scope admin, board login) | `cli_update.status`, wait 10 s | `CliUpdateReport` |
| `POST /api/stations/{station}/cli-update` `{provider, check_only, force, elevation}` | owner; an elevation unless `check_only` | `cli_update.run`, wait 90 s | `CliUpdateRunAnswer` |

- Errors: 400 `invalid_body`, `elevation_required` (`keys: ["update"]`); 403 `owner_only`; 404 `unknown_station`; 409 `station_unreachable`; 501 `cli_update_unsupported` (the link did not negotiate `cli_update.v1`, or the station answers `unsupported` / `unknown_method`); 502 `station_error`; 504 `timeout`.
- Audit of the POST: actor `owner`, action `cli_update.check` or `cli_update.update`, target `<station name>/<provider>`, result `ok` when the answer has no `refused`, else `refused` with detail = the reason code; detail `force` when `force` is true.
- `methods_table.rs`: two rows, `station.cli_update_status` (GET, `Scope::Admin`, not mutating) and `station.cli_update_run` (POST, `Scope::Admin`, mutating).

**The audit import** (`link/cli_update_audit.rs`, `store/cli_update_audit.rs`, new; revision 2 order, finding 6):

1. After each `station.info` answer with `cli_update: Some(report)`, on a link with `cli_update.v1`, the deck reads the stored cursor `(incarnation, seq)` of the station **first**. `after = stored.seq` when `stored.incarnation == info.incarnation`, else `after = 0` (a new incarnation, for example after a snapshot restore, `upgrade.rs:317-319`; or no cursor yet).
2. When `report.last_seq > after`, a task asks `cli_update.attempts { after_seq: after, limit: 100 }`. The answer is a `CliUpdateAttemptsPage`. When `page.incarnation != info.incarnation` (the station changed between the two calls), the task drops the page and starts again at step 1 with the page's incarnation and `after = 0`, at most once per `station.info`. Else it imports the page (step 3) and asks the next page with `after_seq` = the last row's `seq`, until a page has fewer than 100 rows.
3. `DeckStore::import_cli_update_attempts(station_id, station_name, incarnation, rows)` runs one transaction. It reads the cursor row again; when its `incarnation` differs from `incarnation`, the cursor counts as 0. For each row with `seq` above the cursor: `INSERT OR IGNORE` into `cli_update_seen (station_id, attempt_id)`; only when that insert added a row, one audit row is written. This keeps a row that the deck imported before a snapshot restore from showing twice. Then it stores `(incarnation, MAX(seq))` and prunes `cli_update_seen` to the newest 2000 rows of the station.
4. The audit row: actor `station:<station name>`, action `cli_update.attempt`, target `<station name>/<provider>`, result `ok` for `updated`, `up_to_date`, `report_only`, `skipped_busy`, `verified`; `refused` for `refused`; `failed` for `failed`, `rolled_back`, `rollback_failed`, `interrupted`, `unknown`. Detail: `"<trigger> <method> <from or -> -> <to or target or ->: <outcome>"` plus `" (<reason>)"` when there is a reason; versions pass `is_release` or are written as `?`; at most 300 characters. `at` is the deck clock at the import.

Deck migration **V53** (`crates/fleetdeck-deck/src/store/cli_update_cursor_schema.rs`, new):

```sql
CREATE TABLE cli_update_cursor (
    station_id  TEXT    PRIMARY KEY,
    incarnation TEXT    NOT NULL,
    seq         INTEGER NOT NULL
);
-- Revision 2: the attempts that the deck has imported, so a station
-- restored from a snapshot does not show an attempt twice.
CREATE TABLE cli_update_seen (
    station_id  TEXT    NOT NULL,
    attempt_id  TEXT    NOT NULL,
    imported_at INTEGER NOT NULL,
    PRIMARY KEY (station_id, attempt_id)
);
```

Station migration **V49** (`crates/fleetdeck-station/src/store/schema_v49.rs`, new):

```sql
CREATE TABLE cli_update_attempts (
    seq             INTEGER PRIMARY KEY AUTOINCREMENT,
    attempt_id      TEXT    NOT NULL UNIQUE,
    provider        TEXT    NOT NULL CHECK (provider IN ('claude', 'codex')),
    trigger         TEXT    NOT NULL,
    method          TEXT    NOT NULL,
    channel         TEXT    NOT NULL,
    from_version    TEXT,
    target_version  TEXT,
    to_version      TEXT,
    outcome         TEXT    NOT NULL,
    reason          TEXT,
    started_at      INTEGER NOT NULL,
    ended_at        INTEGER
);
CREATE INDEX cli_update_attempts_provider ON cli_update_attempts (provider, seq);

-- Revision 2 (finding 3): the durable record of an attempt that may change
-- an install. It lives from §3.7 step 10 until the transaction that records
-- the final result; the start recovery reads it. Paths are local to the
-- station and never leave it.
CREATE TABLE cli_update_journal (
    provider        TEXT    PRIMARY KEY CHECK (provider IN ('claude', 'codex')),
    attempt_id      TEXT    NOT NULL REFERENCES cli_update_attempts (attempt_id),
    marker          TEXT    NOT NULL,
    method          TEXT    NOT NULL CHECK (method IN ('native', 'npm', 'bun')),
    phase           TEXT    NOT NULL CHECK (phase IN ('staging', 'activating')),
    from_version    TEXT    NOT NULL,
    target_version  TEXT    NOT NULL,
    launcher        TEXT    NOT NULL,
    dest            TEXT    NOT NULL,   -- native: the link L; npm, Bun: `modules`
    stage_root      TEXT    NOT NULL,   -- native: versions/.fd-stage-<id>; npm, Bun: R
    rollback_path   TEXT,               -- native: the rollback copy; npm, Bun: R/old
    rollback_sha256 TEXT,               -- native only
    moved           TEXT,               -- npm, Bun: JSON [{"name", "digest"}]
    installer_pgid  INTEGER,
    updated_at      INTEGER NOT NULL
);

-- Revision 2 (finding 8): the providers whose starts are refused until
-- §3.8.1 clears them.
CREATE TABLE cli_update_quarantine (
    provider    TEXT    PRIMARY KEY CHECK (provider IN ('claude', 'codex')),
    attempt_id  TEXT    NOT NULL,
    reason      TEXT    NOT NULL CHECK (reason IN ('rollback_failed', 'recovery_pending', 'process_left')),
    text        TEXT    NOT NULL,
    since       INTEGER NOT NULL
);
```

`rollback_sha256` moved from the attempts table to the journal (revision 2): only the recovery needs it.

Station store API (`crates/fleetdeck-station/src/store/cli_updates.rs` and `store/cli_update_journal.rs`, new):

```rust
pub struct NewAttempt { pub attempt_id: String, pub provider: ProviderKind, pub trigger: UpdateTrigger,
    pub method: InstallMethod, pub channel: String, pub from_version: Option<String>,
    pub target_version: Option<String> }
pub struct AttemptEnd { pub outcome: UpdateOutcome, pub reason: Option<String>,
    pub to_version: Option<String>, pub target_version: Option<String> }

pub enum JournalPhase { Staging, Activating }
pub struct NewJournal { pub provider: ProviderKind, pub method: InstallMethod,
    pub from_version: String, pub target_version: String, pub launcher: PathBuf,
    pub dest: PathBuf, pub stage_root: PathBuf, pub rollback_sha256: Option<String> }
pub struct MovedEntry { pub name: String, pub digest: Option<String> }
/// What A1 writes before A2 (§3.6).
pub struct Activation { pub rollback_path: PathBuf, pub rollback_sha256: Option<String>,
    pub moved: Vec<MovedEntry> }

/// One journal row, for the run and the recovery (internal; never on the wire).
pub struct RecoveryRecord { pub provider: ProviderKind, pub attempt_id: String,
    pub marker: String, pub method: InstallMethod, pub phase: JournalPhase,
    pub from_version: String, pub target_version: String, pub launcher: PathBuf,
    pub dest: PathBuf, pub stage_root: PathBuf, pub rollback_path: Option<PathBuf>,
    pub rollback_sha256: Option<String>, pub moved: Vec<MovedEntry>,
    pub installer_pgid: Option<i32> }

pub enum JournalAction { Delete, Keep }
pub enum QuarantineAction { Clear, Set { reason: &'static str, text: String }, Keep }
pub struct Quarantine { pub provider: ProviderKind, pub attempt_id: String,
    pub reason: String, pub text: String, pub since: i64 }

impl Store {
    /// Inserts a row (outcome `running`, or the final outcome when `end` is given); returns `seq`.
    pub async fn cli_update_insert(&self, a: NewAttempt, end: Option<AttemptEnd>) -> Result<u64>;
    /// §3.7 step 10: the attempt row and the journal row (phase `staging`,
    /// `marker` = the attempt id) in one transaction. A journal row of the
    /// provider exists → `Err` (the unique key), nothing written.
    pub async fn cli_update_begin(&self, a: NewAttempt, j: NewJournal) -> Result<u64>;
    pub async fn cli_update_journal_pgid(&self, provider: ProviderKind, pgid: i32) -> Result<()>;
    /// §3.6 A1: phase `activating` and the rollback data, committed before A2.
    pub async fn cli_update_journal_activate(&self, provider: ProviderKind, act: Activation) -> Result<()>;
    pub async fn cli_update_journals(&self) -> Result<Vec<RecoveryRecord>>;
    /// §3.7.2 c, one transaction: ends the journal's attempt `interrupted` when
    /// it is running, inserts `successor`, points the journal row at it (the
    /// marker stays), and upserts the quarantine row `recovery_pending` with
    /// `text`. Returns the updated record.
    pub async fn cli_update_recover_begin(&self, provider: ProviderKind,
        successor: NewAttempt, text: String) -> Result<RecoveryRecord>;
    /// One transaction: ends `attempt_id`, then applies `journal` and `quarantine`.
    pub async fn cli_update_finish(&self, attempt_id: String, end: AttemptEnd,
        journal: JournalAction, quarantine: QuarantineAction) -> Result<()>;
    /// §3.8.1 step 5, one transaction: the `verified` row and the delete of the quarantine row.
    pub async fn cli_update_verified(&self, a: NewAttempt, end: AttemptEnd) -> Result<u64>;
    pub async fn cli_update_quarantines(&self) -> Result<Vec<Quarantine>>;
    /// Finished rows after `after_seq`, oldest first, stopping before the first `running` row.
    pub async fn cli_update_attempts(&self, after_seq: u64, limit: u32) -> Result<Vec<CliUpdateAttempt>>;
    pub async fn cli_update_last(&self, provider: ProviderKind) -> Result<Option<CliUpdateAttempt>>;
    /// The highest `seq` of a finished row; 0 when none.
    pub async fn cli_update_last_seq(&self) -> Result<u64>;
    /// §3.7.2 step 0: marks every `running` row without a journal row `interrupted`; returns the count.
    pub async fn cli_update_interrupt_orphans(&self) -> Result<usize>;
    /// Keeps the newest `keep` rows (500 in production), never a row that a
    /// journal or a quarantine row names; returns the count deleted.
    pub async fn cli_update_prune(&self, keep: u32) -> Result<usize>;
}
```

`cli_update_prune(500)` runs in the existing history prune task (`serve.rs:406`).

### 3.12 The board

**Station card** (`web/src/board/StationCliUpdate.tsx`, new; one line in `StationsPanel.tsx` after the providers list, `:271-320`). Shown only when `info.cli_update` exists. Under the provider list, one line per provider of `cli_update.providers`, then the schedule line:

| State | Text | Tone |
| --- | --- | --- |
| `quarantined` is set (revision 2; it wins over every row below) | `Claude: starts blocked after a failed update. Reinstall Claude, then press Check in Settings.` (`recovery_pending`: `Claude: an interrupted update is not finished. Restart the station.`; `process_left`: `Claude: an update process did not stop. Restart the station.`) | hazard |
| `running` is this provider's last attempt | `Claude: updating to 2.1.310…` | normal |
| last `updated`, trigger `schedule`/`owner`/`recovery` | `Claude: updated 2.1.308 → 2.1.310 · 3 h ago` | quiet |
| last `updated`, trigger `external` | `Claude: changed outside Fleet Deck, 2.1.308 → 2.1.309 · 3 h ago` | quiet |
| last `up_to_date` | `Claude: up to date · checked 3 h ago` | quiet |
| last `skipped_busy` | `Claude: not updated, sessions ran · 3 h ago` | quiet |
| last `refused` | `Claude: not updated, {reason text} · 3 h ago` | quiet |
| last `report_only` or `blocked == report_only_method` | `Claude: update by hand ({method text})` | quiet |
| `blocked == custom_launcher` | `Claude: update by hand (a custom launcher link)` | quiet |
| last `failed` | `Claude: update failed, kept 2.1.309 · 3 h ago` | hazard |
| last `rolled_back`, trigger `recovery` | `Claude: an interrupted update was undone, back on 2.1.309 · 3 h ago` | hazard |
| last `rolled_back`, other triggers | `Claude: 2.1.310 failed the check, back on 2.1.309 · 3 h ago` | hazard |
| last `verified` | `Claude: checked again, starts allowed · 3 h ago` | quiet |
| last `rollback_failed` | `Claude: update and way back failed. Reinstall Claude on this station.` | hazard |
| last `interrupted` | `Claude: update interrupted · 3 h ago` | hazard |
| `available` set and nothing runs | append ` · 2.1.310 available` | — |
| schedule line | `Updates: off` or `Updates: nightly 03:00-05:00 local` | quiet |

- Method texts: native "native installer", npm "npm", bun "Bun", homebrew "Homebrew", system_package "a system package", unknown "an unknown method".
- Reason texts: `gpgv_missing` "install GnuPG (gpgv) to verify Claude updates"; `tool_missing` "the package manager was not found"; `disk_full` "less than 1 GB free"; `channel_unreachable` "the vendor did not answer"; `signature_invalid` "the release signature did not verify"; `signature_unknown_key` "the release has an unknown signing key"; `provenance_missing` "the package has no provenance"; `known_broken` "this version is known to break Fleet Deck"; `below_tested` "this version is older than Fleet Deck supports"; `below_handoff` "this version cannot run fleet tools"; `too_new` "the new version is still in its wait time"; `publish_time_unknown` "the release time is unknown"; `station_restarting` "the station was restarting"; `starts_in_flight` "sessions were starting"; `sessions_running` "sessions ran"; `another_update` "another update ran"; `second_install` "a second install, update it by hand"; `not_found` "the program was not found"; `not_a_version` "the vendor gave no release version"; revision 2: `custom_launcher` "a custom launcher link, update it by hand"; `unsupported_layout` "the install layout is not one the station can update"; `tool_too_old` "the package manager is too old (npm 7 or Bun 1.2 needed)"; `unverified_package` "the download held a package that was not signed"; `integrity_mismatch` "the download did not match the signed release"; `checksum_mismatch` "the download did not match the signed release"; `version_mismatch` "the new version reported a different version"; `handshake_failed` "Fleet Deck could not drive the new version"; `install_failed` "the download or the install failed"; `timeout` "the update took too long"; `process_left` "an update process did not stop"; `recovery_pending` "an interrupted update waits for a station restart"; `quarantined` "starts are blocked after a failed update"; others "see the station". The age uses `agoText` (as `Last seen`).
- For the owner (`actions.canAdmin`), the line ends with a link `Updates…` to Settings → the station tab (`formatHash({name: 'settings', tab: 'station', station: station.station_id, …})`), like "Sign in…" (`StationsPanel.tsx:306-318`).
- No new token or primitive: quiet text uses `fd-note`; the hazard tone uses the existing hazard attribute of the provider line. The worker applies the `fleet-design-system` skill.

**Settings** (Settings → station tab):

- `model.ts`: a section `{ title: 'Provider updates', keys: ['provider_updates.enabled', 'provider_updates.window_start', 'provider_updates.window_hours', 'provider_updates.timezone', 'provider_updates.claude_channel', 'provider_updates.claude_pin', 'provider_updates.codex_pin', 'provider_updates.min_age_hours'] }` after "Providers".
- `CliUpdateCard` (`web/src/settings/CliUpdateCard.tsx`, new) in the slot `'Provider updates': { after: card }` (`SettingsTab.tsx`), only for a live station with `cli_update.v1` (the GET answers; a 501 shows "This station cannot update its providers yet. Update the station first.").
- Card title: `Update now`. One row per provider: `Claude 2.1.309 · native installer · stable channel` (Codex: `· latest channel`; a pin: `· pinned to 2.1.305`), the last-attempt text of the card table, and two buttons `Check` and `Update now…`. A provider that is not `updatable` shows the blocked text and no `Update now…`.
- `Check`: POST with `check_only: true` (no elevation). Result line: `2.1.310 is available.` / `Up to date.` / `Not available: {reason text}.` For a quarantined provider (revision 2, §3.8.1) the button text stays `Check`, and the result line is `Claude passed the checks. Sessions can start again.` or `Still blocked: {reason text}.`
- `Update now…`: asks the elevation (`ask(['update'])`), then opens the dialog:
  - Title: `Update Claude on {station}?` / `Update Codex on {station}?`
  - Claude text: `The station downloads Claude {target} from Anthropic ({channel} channel), checks it against the signed release and checks that Fleet Deck can drive it before it switches. If a check fails after the switch, it goes back to {installed} from a local copy. New sessions wait a few seconds while it switches.`
  - Codex text: `The station downloads Codex {target} from the npm registry with {npm|Bun} into a separate folder, checks every package against the registry signature and checks that Fleet Deck can drive it before it switches. If a check fails after the switch, it puts {installed} back from a local copy. New sessions wait a few seconds while it switches.`
  - Buttons `Update` and `Cancel`. `{target}` is `available` of the last check, or "the newest version" when no check ran.
- Busy answer (`refused == sessions_running`): native: `{n} Claude sessions run on this station. They keep their version; new sessions use the new one.` with a check box `Update while they run` (sends `force: true`). Npm and Bun: `{n} Codex sessions run on this station. An update replaces the files in place: stop these sessions first.` and no check box.
- `refused == recovery_pending`: `An interrupted update of {name} waits for the station restart. Restart the station first.`
- Running: `Updating Claude to {target}…`; the card polls the GET every 3 s until `running` is `null`, then shows the last-attempt text.
- Other refusals: `Not updated: {reason text}.`

### 3.13 Compatibility

1. **Older station, newer deck:** no `cli_update.v1` → no `cli_update` in `station.info`, no card line, the Settings card shows the 501 text, no audit import.
2. **Newer station, older deck:** the station runs the schedule from its own `station.toml` and keeps the attempts (pruned at 500). A newer deck imports them later. An older deck ignores the unknown `StationInfo` field (serde default, `#[serde(other)]` in every enum).
3. **Settings:** every key of `[provider_updates]` is optional; an older `station.toml` parses unchanged. An older board shows the new keys under "Other" (the section model puts unknown keys there, `model.ts:141`).
4. **Behavior after the upgrade with defaults:** no change: updates are off, no `DISABLE_AUTOUPDATER`, Claude's own updater keeps running as today. New at once: the station sees version changes (§3.9) and refreshes its report; the audit log records them.
5. **Migrations:** station V49 (`cli_update_attempts`, `cli_update_journal`, `cli_update_quarantine`) and deck V53 (`cli_update_cursor`, `cli_update_seen`); reserved numbers, the merge renumbers. Both only create tables; to reverse, drop the tables.
7. **The start order (revision 2):** the update recovery runs between `Store::open` and `SessionManager::start` (§3.7.2). A station with no journal row spends one query there.
6. **The restart freeze:** the hold reuses `hold_starts`; a restart and an update exclude each other (§3.7.1).

### 3.14 Errors and edge cases

| Case | Result |
| --- | --- |
| The deck is offline at night | The station updates; the audit import runs at the next link. |
| The vendor channel is down | The row is `Failed` with `channel_unreachable`; nothing changed; the next window tries again. |
| Claude's own updater changes the version in a terminal of the owner | §3.9 records `external`; the gate is not applied to it; a `KNOWN_BROKEN` result is marked broken by the refresh (§3.8). |
| The owner pins an older Claude | The station stages, binds and activates it as any target (revision 2: no `claude install`, so `minimumVersion` does not apply). Claude's own updater in an owner terminal can move the link forward again; §3.9 records it as `external`. |
| Two instances of Claude with one binary | One install; the busy check counts the sessions of both. |
| Two instances with two binaries | The second reports `second_install`. |
| A pre-release on `latest` | `not_a_version`, refused. |
| A failed staging (npm or Bun half done) | Only `R` holds files; the install is untouched; `Failed (install_failed)`; `R` is removed. |
| The new version fails after the activation | The way back renames the local rollback copy into place, with no network (§3.6.2). When that fails too: `RollbackFailed`, the quarantine (durable) refuses the starts with the reinstall hint. |
| The station restarts while a provider is quarantined | The quarantine row is loaded before the effect worker starts; the starts stay refused until §3.8.1 passes. |
| A second crash during the start recovery | The journal row stays (the successor attempt is linked in the first transaction); the next start fences and restores again. |
| A postinstall or downloader child outlives its parent | The fence (§3.6.3) kills the group and the marked processes before the next step; a survivor quarantines the provider and keeps the journal row for the next start. |
| `npm_config_prefix`, `BUN_INSTALL_GLOBAL_DIR` or an `.npmrc` with `prefix` in the owner's setup | Not seen: the staging child has a fixed environment, its own `HOME`, and explicit destinations; the activation renames into the detected `modules` only. |
| A custom link to a Claude version file | `custom_launcher`, report only. |
| A station restored from a snapshot | The deck sees a new incarnation, imports from `seq` 0, and skips the attempts that it saw before (`cli_update_seen`). |
| The station stops during an attempt | Recovery at the next start, before the manager starts (§3.7.2). |
| A session starts while the update runs | Before the hold (download, staging): it starts normally. During the hold: it waits in the queue (`hold_starts`) and starts after it. |
| A model help call runs during the activation | It can fail once (npm, Bun: the folders are renamed) and is not retried; native is not affected. Accepted. |
| `date +%z` fails | The tick is skipped; the window is not missed for more than 15 min. |
| No `gpgv` on a native Claude station | `blocked = gpgv_missing`, never updated; the card says so. |
| The npm registry rotates its key | `signature_unknown_key` until a Fleet Deck release pins the new key. |
| A board terminal runs `claude` or `codex` | Not a session: not counted. Native keeps its version file. A Codex terminal keeps its open files; a new start in it uses the new version. |
| Bun's records still name the old Codex | A later `bun add -g`/`bun update -g` by the owner can put it back; §3.9 records `external`; the next window updates again (§6 T13). |

### 3.15 Out of scope

- Homebrew, apt/dnf/apk, pacman, WinGet, pnpm, Vite+ and the Codex standalone installer updates (report only).
- A deny list that the deck pushes to older stations (the deny list stays in the station build).
- Full Sigstore verification of npm provenance (the presence is checked).
- A Fleet Deck self-update (the station upgrade stays manual).
- Writing to the owner's `~/.claude/settings.json` (owner question 7).

## 4. Errors and limits (summary)

- Time limits: a fetch 20 s; gpgv 20 s; the native download 10 min; the staging 10 min; the steps before the hold 12 min; a probe step 30 s; the hold with the activation and the way back 3 min; the hold settle 10 s; a fence 10 s (5 s group, 5 s marker scan); the start recovery 5 min.
- Sizes: a pointer 64 bytes; a manifest 256 KiB; a signature 16 KiB; dist-tags 64 KiB; a version document 1 MiB; the native binary 512 MiB; a log file 256 KiB, 20 files.
- Rows: 500 attempts per station (journal and quarantine rows keep their attempts); 2000 seen attempt ids per station on the deck.

## 5. Decisions made in this draft

1. **Stage, bind, then activate, with an exact version (revision 2; replaces "the vendor's own commands only").** Native Claude: the station downloads the platform binary as `bootstrap.sh` does, binds it to the signed manifest checksum before it runs, probes it at its staged path and swaps the managed launcher link. Codex by npm or Bun: the package manager stages `@openai/codex@<version>` into a private folder with scripts off and a fixed environment, the station binds every staged package to the signed registry integrity, probes the staged copy and renames the verified folders into place. Rejected: `claude install <version>` and `npm|bun install -g` into the live install (they download and run bytes that nothing binds before they run: finding 1); `claude update` / `codex update` (they pick the version after the gate); `curl | sh` of the Codex standalone installer. The cost: the station re-implements the download and the activation of `bootstrap.sh`, and the package managers' own records are not updated (§6 T13).
2. **Detection from the launcher that the station runs**, never from the tools on PATH (§1.6 item 2).
3. **Report only** for Homebrew, system packages, the Codex standalone installer, unknown layouts, a custom launcher link (revision 2) and Claude by npm or Bun (revision 2: its `postinstall` runs code that the station cannot bind).
4. **Signature checks fail closed:** Claude by `gpgv` with a pinned keyring and a fingerprint check on `VALIDSIG`; npm by the registry ECDSA signature with a pinned key (`ring`, already a dependency); Codex needs a provenance attestation. Rejected: a new OpenPGP crate (a large new dependency); a checksum-only check for Claude (the manifest comes from the same host).
5. **No downgrade without a pin; a soak of 48 h; Claude `stable` by default.**
6. **The gate adds the handoff minimum** (and the bridge minimum on a shim host), so an update never removes the fleet tools.
7. **The probe uses the adapter's real launch arguments** and the protocol handshake (`initialize`), so a removed flag or a protocol change fails before a session meets it. No model call.
8. **The hold holds every queued start and resume** (one flag, the existing `hold_starts`), not one provider. Rejected: a per-provider hold in the claim SQL (more code in a reviewed path for a few minutes per night).
9. **Busy = a live process of an instance of the kind.** Force only for native Claude.
10. **Off by default; a nightly window with jitter; no update at the station start.**
11. **`enabled` is a restart value** because it sets `DISABLE_AUTOUPDATER=1` in the Claude children (the adapters are built at the start).
12. **External changes are recorded** every 15 min and refresh the provider report (fixes §1.1 item 4 for every station).
13. **Attempts live in `station.db`; the deck imports finished ones** with a cursor per station and incarnation in one transaction (no duplicate, no loss across a snapshot restore).
14. **No child output leaves the station:** a private log file per attempt; the wire and the database hold fixed codes.
15. **The buttons are in Settings** (the elevation lives there); the card shows the state and links to it.
16. **New code in new files;** the large files get only the lines named in §7.
17. **A local rollback copy exists and is checked before any change** (revision 2): native by a hard link or a copy outside `versions/`, checked by SHA-256; npm and Bun by renaming the previous folders aside, checked by a tree digest. The way back uses no network.
18. **A durable journal row per changing attempt** (revision 2): written with the attempt row, before any change; removed only in the transaction that records the final result. The start recovery links its successor attempt in its first transaction.
19. **The recovery runs before the session manager starts** (revision 2), so no start, resume, assist call or terminal can meet a half-installed CLI.
20. **Every child of an attempt is fenced** (revision 2): own process group, a marker in the environment, TERM then KILL, the marker scan of the setup cleanup; nothing goes on before the fence passes.
21. **The quarantine is durable** (revision 2) and is cleared only by a passing update or by `verify_current` (integrity where the method allows it, and the handshake), never by a version string.
22. **The deck reads the incarnation before it builds the cursor** (revision 2) and keeps the imported attempt ids, so a restored station is imported from 0 with no duplicate.
23. **The hold covers only the activation** (revision 2): the download and the staging run before it, so the start hold lasts seconds.

## 6. Threat notes and risk class

| # | Threat | Mitigation |
| --- | --- | --- |
| T1 | A compromised vendor channel or registry ships a malicious version, and many stations install it at night without a human. | Vendor signature (Claude GPG key pinned in the binary; npm registry key pinned); Codex provenance required; 48 h soak; Claude `stable` channel by default; off by default; jitter; every attempt audited. Revision 2: the **bytes that run** are bound to the signed digest before they run (§3.6 S2, S3): native by the SHA-256 of the staged file; npm and Bun by the lockfile integrity of every staged package against the signed integrity, with scripts off and no package outside the verified set. Residual: a release that the vendor signs and that passes the soak runs as the owner — the same trust as `claude update` by hand. |
| T2 | A man in the middle or a compromised mirror serves good signed metadata, then other bytes for the package or the binary (finding 1). | HTTPS only (`proto =https`, no redirects); the pointer only names a version; the signed manifest or the registry signature decides what is accepted; the staged bytes must match the signed digest before anything runs them, and only the staged, bound copy is activated. |
| T3 | Argument injection through a version string or a path. | Every version in an argv passes `is_release` (digits and dots only). Paths come from the station's own file system (detection), never from the wire. No shell. |
| T4 | The board or a stolen owner session triggers updates or pins an old version. | Owner only, with an elevation for an update; audited. A pin cannot go below `TESTED_MIN_VERSION`, the deny list or the handoff minimum. A pin is a `danger` setting (its own elevation). |
| T5 | npm provenance is only checked for presence. The station trusts the local package manager's tarball check (npm ssri, Bun) for the bytes that it extracted. | Documented residual; the registry signature binds the tarball integrity, and S3 compares the integrity that the package manager recorded with it. A later change can add Sigstore verification, or a station-side extraction. |
| T6 | An update breaks a running session (in-place files, a changed protocol). | No update while sessions of the provider run (force only for native, where versions sit side by side); starts are held; the handshake probe before release; the way back. |
| T7 | A failed update leaves no working CLI. | Revision 2: a local rollback copy is made and checked before any change; the way back needs no network, no old package and no old key (finding 2). `RollbackFailed` writes a durable quarantine that refuses starts with a reinstall hint, also after a restart (finding 8). |
| T8 | Secrets in output (an `.npmrc` token in an npm error). | Output goes only to a 0600 log file on the station; the wire, the database and the station log hold fixed codes. |
| T9 | The station updates the wrong copy (the shadowed npm Codex on Lighting; an inherited `npm_config_prefix`, `.npmrc` or `BUN_INSTALL_GLOBAL_DIR`; a custom launcher link). | Detection from the launcher chain; the staging child has a fixed environment, its own `HOME` and explicit destinations; the activation renames only into the detected `modules` or swaps only the managed link `~/.local/bin/claude`; a custom link is report only (findings 7 and 9). |
| T10 | Disk fills (291 MB per Claude version). | 1 GiB free space check; the native installer prunes old versions. |
| T11 | `DISABLE_AUTOUPDATER` hides security fixes. | Only when the owner turns the station updater on; the nightly update replaces it. |
| T12 | An installer descendant (a downloader, a script) survives a timeout or a crash and changes files during the way back or after the hold ends (finding 5). | Every child in its own process group with a marker; the fence (TERM, KILL, marker scan) before any next step; a survivor quarantines the provider and keeps the journal row; the next start fences first. Residual: a process that clears its environment and leaves the group is not found (the documented limit of the setup cleanup, DL-148). |
| T13 | The package managers' records disagree with the activated Codex (Bun's global `package.json` and `bun.lock`). | The station records the next owner change as `external`; the next window updates again. Accepted. |
| T14 | A crash leaves a half-activated CLI, and a start runs it (findings 3, 4). | The journal row is durable before any change; the recovery runs before the session manager starts; an unfinished recovery quarantines the provider; `verify_current` has no integrity check for an npm or Bun reinstall by the owner (only the version gate and the handshake). |

**Risk class: high.** The change runs code from an external source as the owner on a timer, adds a supply-chain surface, changes the start admission (the restart freeze), and adds an owner action with an elevation. Adversary DESIGN REVIEW before the build; PRE-PR REVIEW of the final branch; `SECURITY-CHECKLIST.md` gets a row (T9 of the plan).

## 7. Build plan

Workers reset their worktree to `v2` first. Every heavy command goes through `umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh <command>`. Workers compile only (`cargo check -p <crate> --tests`; web: `bun run typecheck`, `bun run lint`); they write the tests and do not run them. Every file stays under 800 lines (`wc -l`). At most two workers run at the same time (machine rule).

Order (revision 2: eleven tasks; at most two workers at a time; no two tasks of one wave own the same file):

| Wave | Tasks | Needs |
| --- | --- | --- |
| 1 | T1 ∥ T2 | — |
| 2 | T3 ∥ T5 | T1 |
| 3 | T4 ∥ T6 | T3 (T4); T5 (T6) |
| 4 | T7 ∥ T9 | T2, T3, T5, T6 (T7); T1 (T9) |
| 5 | T8 ∥ T10 | T4, T7 (T8); T1 (T10) |
| 6 | T11 | all |

`crates/fleetdeck-station/src/cli_update/mod.rs` (module lines only) is created by T5 and gets its module lines from T6, T7 and T8, one wave after the other.

### T1 (backend): wire types, feature, settings (≈ 600 lines)

- **Owned files:**
  - `crates/fleetdeck-proto/src/cli_update.rs` (new), `crates/fleetdeck-proto/src/cli_update_tests.rs` (new), `crates/fleetdeck-proto/src/lib.rs` (`pub mod cli_update;`)
  - `crates/fleetdeck-proto/src/link.rs` (the `cli_update` field, and `cli_update: None` in its two literals)
  - `crates/fleetdeck-proto/src/version.rs` (feature, `ALL`, one test)
  - `crates/fleetdeck-link/src/methods_table_tail.rs` (three rows, `TAIL` 68 → 71)
  - `crates/fleetdeck-core/src/config_provider_updates.rs` (new), `crates/fleetdeck-core/src/config_provider_updates_tests.rs` (new), `crates/fleetdeck-core/src/config.rs`, `crates/fleetdeck-core/src/config_raw.rs`
  - `crates/fleetdeck-core/src/settings/fields_updates.rs` (new), `settings/fields.rs` (one `.chain`), `settings/mod.rs` (one `mod`), `settings/holder.rs` (the view values), `settings/tests/fields.rs` (the table list)
  - every file with a `StationInfo {` literal (add `cli_update: None,`): the 22 files of `grep -rln "StationInfo {" crates` on `d1fa4205`
  - `web/src/gen/**` (generated only, `make ts-types`)
- **Changes:** §3.2 (all types, with revision 2: `UpdateOutcome::Verified`, `ProviderUpdateState::quarantined`, `CliUpdateAttemptsPage`, the new reason codes; the field, the feature, the rows); §3.10 (`ProviderUpdatesConfig`, `UpdateZone`, `ClaudeChannel`, `RawProviderUpdates` with `deny_unknown_fields`, `RAW_PROVIDER_UPDATES_KEYS`, `provider_updates` in `RAW_STATION_KEYS` and `RawStation`, `StationConfig.provider_updates`, the eight fields with their labels and helps, the view values: `window_start` as `HH:MM`, `timezone` as `local`/`UTC`/`±HH:MM`, pins as `""` when `None`). `is_release` lives in `fleetdeck-proto/src/cli_update.rs` as `pub fn is_release(&str) -> bool` (the gate and the config use it).
- **Tests:**
  - `cli_update_tests.rs`: each enum round-trips (also `verified`) and an unknown string decodes to `Unknown`; `CliUpdateRun` refuses an unknown field and defaults `check_only`/`force` to false; `CliUpdateAttemptsPage` round-trips with its incarnation; `is_release` accepts `2.1.310`, `0.164.0` and refuses `2.1.310-beta`, `--help`, `2.1`, `1.2.3.4`, `1234567.0.0`, `""`.
  - `version.rs`: `the_cli_update_feature_has_its_name_follows_wall_apps_and_is_not_required`.
  - `config_provider_updates_tests.rs`: defaults (off, 03:00, 2, local, stable, no pins, 48); a full table; each bad value with its field in the error (window `24:00`, `3:00`, hours 0 and 13, timezone `+15:00` and `CET`, pin `2.1`, min age 337); an unknown key is refused.
  - `settings/tests/fields.rs`: the existing key-equality test passes with `("provider_updates", RAW_PROVIDER_UPDATES_KEYS)`.
- **Acceptance at the merge:** `cargo nextest run -p fleetdeck-proto -p fleetdeck-core -p fleetdeck-link`; `make ts-types` gives no diff.

### T2 (backend): the update probe (≈ 420 lines)

- **Owned files:** `crates/fleetdeck-providers/src/update_probe.rs` (new), `crates/fleetdeck-providers/src/update_probe_tests.rs` (new), `crates/fleetdeck-providers/src/lib.rs` (`pub mod update_probe;`).
- **Changes:** §3.6.1 exactly, with the process group of revision 2. Reuse `claude::args::{cli_args, Launch, parse_version, capabilities}`, `claude::control::initialize`, `codex::probe::parse_version`, `codex::app_server_args`, `codex::rpc`. Spawn with `env_clear().envs(env.iter())`, `process_group(0)`, `kill_on_drop(true)`, stderr null; at the end SIGKILL to the group (`rustix::process::kill_process_group`) and `wait`.
- **Tests** (`update_probe_tests.rs`, fake CLIs as `#!/bin/sh` scripts in a temp dir, as other provider tests do):
  - `a_claude_that_answers_initialize_with_models_passes`;
  - `a_claude_error_answer_fails_with_error_answer`; `a_claude_answer_without_models_fails_with_no_models`; `a_silent_claude_fails_with_no_answer_within_the_limit` (limit 300 ms);
  - `a_missing_program_fails_with_no_start_and_no_version`;
  - `the_claude_probe_passes_the_adapter_launch_arguments` (the script writes its argv to a file; the test compares with `cli_args(&Launch::default(), &capabilities(false))`);
  - `a_codex_that_answers_initialize_and_account_read_passes`; `a_codex_error_on_initialize_fails`;
  - `a_background_child_of_the_probe_is_killed_with_its_group` (Linux; the script starts `sleep 60 &` and writes its pid; after `check` the pid is gone).
- **Acceptance:** `cargo nextest run -p fleetdeck-providers -E 'test(update_probe)'`.

### T3 (backend): station store: attempts, journal, quarantine (≈ 650 lines)

- **Owned files:** `crates/fleetdeck-station/src/store/schema_v49.rs` (new), `store/cli_updates.rs` (new), `store/cli_update_journal.rs` (new), `store/cli_updates_tests.rs` (new), `store/cli_update_journal_tests.rs` (new), `store/migrations.rs` (one `M::up`), `store/mod.rs` (three `mod` lines).
- **Changes:** §3.11 station part: schema V49 with the three tables, the whole store API (every write that §3.7, §3.7.2 and §3.8.1 call "one transaction" is one SQLite transaction), the ordering and the prune rule.
- **Tests:**
  - `cli_updates_tests.rs`: insert/end/last; `attempts` stops before a running row and pages by `after_seq`; `last_seq` ignores running rows; `prune(2)` keeps the two newest and never a row that a journal or quarantine row names; the CHECK refuses `provider = 'pi'`; `interrupt_orphans` ends a running row without a journal row and leaves one with a journal row.
  - `cli_update_journal_tests.rs`: `begin` writes the attempt and the journal in one transaction, and a second `begin` for the provider fails and writes nothing; `journal_activate` stores phase, `moved` and the rollback data, and `journals()` reads them back after the store is reopened; `recover_begin` ends the old attempt `interrupted`, inserts the successor, points the journal at it with the same marker and writes `recovery_pending`, all visible after a reopen; a second `recover_begin` on the same row ends the first successor `interrupted` and links a new one; `finish` with `Keep`/`Delete` and `Set`/`Clear`/`Keep`; `verified` writes the row and deletes the quarantine row together; quarantine rows survive a reopen.
- **Acceptance:** `cargo nextest run -p fleetdeck-station -E 'test(cli_update)'` plus the module-size and layering guards.

### T4 (backend): the hold, the provider refresh and the durable quarantine marks (≈ 450 lines)

- **Owned files:** `crates/fleetdeck-station/src/manager/restart_gate.rs`, `manager/provider_update.rs` (new), `manager/provider_update_tests.rs` (new), `manager/manager_providers.rs`, `manager/target_base.rs`, `manager/target.rs` (the `BrokenMark` field type at `:100` and the backstop; at most 12 changed lines), `manager/mod.rs` (the field type at `:126` and `:396`, `mod provider_update;`, and one call `provider_update::load_quarantine(&store, &target).await?` before `EffectWorker::register`; at most 6 added lines; the file stays under 800), `crates/fleetdeck-station/src/coordinator/bindings_handoff.rs`, `crates/fleetdeck-station/src/settings_svc.rs` (the restart refusal, at most 10 lines).
- **Changes:** §3.7.1; §3.8 items 1 to 6; §3.8.1 "Loaded before dispatch" (`load_quarantine`, `quarantine`, `release_quarantine`, `set_denied`, the texts).
- **Tests** (`provider_update_tests.rs` and `manager_providers.rs` tests):
  - `begin_update_fails_while_restarting`; `a_restart_admission_after_begin_update_is_refused_and_clears_restarting`; `a_queued_start_is_held_during_an_update_and_runs_after_the_hold_drops`; `live_sessions_of_lists_only_the_kind`; `a_deferred_start_of_the_updating_kind_fails_with_provider_updating`; `begin_provider_update_reports_starts_in_flight_after_the_settle_bound` (bound 50 ms).
  - `a_quarantine_row_refuses_a_queued_start_after_a_restart`: a store with a quarantine row of Claude and a queued Claude start effect; the manager starts; the start fails with the quarantine text and is never dispatched to the adapter (finding 8).
  - `refresh_providers_replaces_the_cache_and_clears_a_deny_mark`; `refresh_providers_does_not_clear_a_quarantine`; `release_quarantine_keeps_a_deny_mark`; `refresh_providers_replaces_the_handoff_versions`.
- **Acceptance:** `cargo nextest run -p fleetdeck-station -E 'test(provider_update) | test(refresh_providers) | test(restart) | test(quarantine)'` plus the module-size and layering guards.

### T5 (backend): detection, gate, channel and signature verification (≈ 650 lines)

- **Owned files:** `crates/fleetdeck-station/src/cli_update/mod.rs` (new: module declarations and re-exports only), `cli_update/detect.rs`, `cli_update/gate.rs`, `cli_update/verify.rs`, `cli_update/channel.rs`, `cli_update/keys/claude-code.gpg` (new: `gpg --dearmor` of `/home/luismorales/fleetdeck-briefs/backlog2/cli-update/claude-code.asc`, fingerprint checked), `cli_update/detect_tests.rs`, `cli_update/gate_tests.rs`, `cli_update/verify_tests.rs`, `cli_update/channel_tests.rs`, `cli_update/testdata/*` (new; also a copy of the vendor's `bootstrap.sh`, read as data), `crates/fleetdeck-station/src/lib.rs` (`pub mod cli_update;`).
- **Changes:** §3.3 (revision 2 rules with the link chain), §3.4, §3.5 (steps 1 to 12, `Verified`, `VerifiedPackage`, the platform with musl). `channel.rs` builds its curl configs with `crate::integrations::http::config` / `exec::run_for` (no new HTTP client) and has the native download URL builder. The RFC 3339 and RFC 1123 date parsers are small pure functions in `channel.rs`.
- **Tests:**
  - `detect_tests.rs` (temp trees): the Lighting layouts (native link; Bun global; a shadowed npm copy that is not chosen); `/usr/local/bin/claude → ~/.local/bin/claude → V/x` is native; `/custom/bin/claude → V/x` is `custom_launcher`; an absolute version file is `custom_launcher`; `~/.local/bin/claude` → another link → `V/x` is `custom_launcher`; Claude under npm and Bun is `report_only_method`; Codex npm with and without `bin/npm`, and without its `bin/codex` in the chain (`custom_launcher`); Bun with `BUN_INSTALL` set to another folder still takes `bun_install` from `real`; Homebrew, `/usr/bin`, unknown; `gpgv` present and missing; a chain of 9 links is `not_found`.
  - `gate_tests.rs`: every row of §3.4 in order (use a target below `TESTED_MIN_VERSION` for `below_tested`, below `VERIFIED` for `below_handoff`, a `shim_host` case below `BRIDGE_VERIFIED` for Codex 0.162.0).
  - `verify_tests.rs`: the recorded `@openai/codex@0.163.0` document (testdata) verifies and gives the verified set with the platform packages of linux-x64 (glibc and musl) only; one changed byte in `integrity` fails `signature_invalid`; an unknown keyid fails `signature_unknown_key`; a bad SPKI prefix fails; provenance missing fails; the platform key selection (musl file present and absent) gives one checksum; `VALIDSIG` parsing of the recorded status lines (pass, other key, no line). A gpgv run test that is skipped when `gpgv` is absent: the recorded 2.1.309 manifest and signature pass; a changed manifest fails.
  - `channel_tests.rs`: pointer text trimming and refusal of a pre-release; dist-tags parsing; RFC 3339 and RFC 1123 parsing; the curl config holds `proto = "=https"`, `no-location` and no user value; the native download URL equals the form in the recorded `bootstrap.sh`.
- **Acceptance:** `cargo nextest run -p fleetdeck-station -E 'test(cli_update)'`.

### T6 (backend): the binding of staged packages and the tree digest (≈ 450 lines, pure)

- **Owned files:** `cli_update/staged.rs` (new), `cli_update/staged_tests.rs` (new), `cli_update/tree_digest.rs` (new), `cli_update/tree_digest_tests.rs` (new), `cli_update/testdata/staged/*` (new fixture trees), `cli_update/mod.rs` (two module lines).
- **Changes:** §3.6 Codex S3 `verify_staged(sm: &Path, lockfile: Lockfile, set: &[VerifiedPackage], current_bin: &serde_json::Value) -> Result<Vec<String> /* moved */, &'static str>` with `enum Lockfile { Npm(PathBuf), Bun(PathBuf) }`; the Bun `bun.lock` reader accepts trailing commas (a string-aware pass that removes a comma before `}` or `]`, then `serde_json`); the tree digest of §3.6.
- **Tests:** an npm fixture (hidden lockfile, nested platform package) passes and returns `["@openai/codex"]`; a Bun fixture (flat, `bun.lock` with trailing commas) passes and returns both packages; one changed integrity → `integrity_mismatch`; an extra package folder → `unverified_package`; a lockfile entry outside the set → `unverified_package`; only `bun.lockb` → `tool_too_old`; a changed `bin` map → `unsupported_layout`; the tree digest is stable for one tree and changes with content, mode and link text, and does not follow links.
- **Acceptance:** `cargo nextest run -p fleetdeck-station -E 'test(staged) | test(tree_digest)'`.

### T7 (backend): stage, activate, the way back and the fence (≈ 700 lines)

- **Owned files:** `cli_update/stage.rs`, `cli_update/activate.rs`, `cli_update/fence.rs` (new), their `_tests.rs` files, `cli_update/mod.rs` (three module lines), `crates/fleetdeck-station/src/workspace/setup_cleanup.rs` (`kill_marked_in` becomes `pub(crate)`; one word).
- **Changes:** §3.6 (children, environments, native S1 to V, Codex S1 to V except S3, which calls T6), §3.6.2, §3.6.3. An `Fs` seam for `linkat` `EXDEV` in tests only.
- **Tests:**
  - `stage_tests.rs`: the exact npm and Bun argv; the staging env holds exactly the names of §3.6 (a station env with `npm_config_prefix`, `NPM_CONFIG_GLOBALCONFIG`, `BUN_INSTALL_GLOBAL_DIR`, `NODE_OPTIONS` and an instance `env` value does not reach the child); `HOME` is `<R>/home`; the stage and the destination on two devices → `unsupported_layout`; the log file is 0600 and capped.
  - `activate_tests.rs`: native: a staged "binary" that writes a marker file when it runs, with a wrong checksum → `checksum_mismatch`, the marker file does not exist, nothing in `versions` or `L` changed (finding 1); S3 refuses to replace a different `versions/<target>`; A1 hard link, and a copy on `EXDEV`; the swap is a rename; npm and Bun: A2 then V failure → restore → the tree digest equals the original; restore is idempotent at each crash point (original moved but stage not moved; both moved; one entry restored) and gives the original tree; restore uses no network (the test channel fails on any call).
  - `fence_tests.rs` (Linux): a group child and a grandchild in the group are gone after the fence; a marked child that left the group (`setsid`) is gone; a group with an unmarked process is not signalled; a process that ignores TERM is killed by KILL.
- **Acceptance:** `cargo nextest run -p fleetdeck-station -E 'test(cli_update) | test(setup_cleanup)'`.

### T8 (backend): attempts, recovery, schedule and wiring (≈ 750 lines)

- **Owned files:** `cli_update/run.rs`, `cli_update/recover.rs`, `cli_update/schedule.rs`, `cli_update/service.rs` (new: `CliUpdates`, `call` for the three link methods, `report()`, `verify_current`), their `_tests.rs` files, `cli_update/mod.rs` (four module lines), `crates/fleetdeck-station/src/serve.rs` (the recovery between `Store::open` and `SessionManager::start`; the service, the schedule task, the prune call, `assist_instances` env), `crates/fleetdeck-station/src/link/intake.rs` (one dispatch arm, the `cli_update` value in `station.info`; at most 8 lines, stays under 800), `crates/fleetdeck-station/src/manager/adapters.rs` (`DISABLE_AUTOUPDATER`), `crates/fleetdeck-station/src/manager/mod.rs` (the service holder: one field, one init line), `manager/provider_update.rs` (`set_cli_update` / `cli_update` / `cli_update_report`).
- **Changes:** §3.7 (steps 1 to 17), §3.7.2, §3.8.1 (`verify_current`), §3.9, §3.10 (env), the link methods (`cli_update.status` → `CliUpdateReport` with `quarantined`; `cli_update.run` → `CliUpdateRunAnswer`, `invalid_params` for bad params, `unsupported` without a service; `cli_update.attempts` → `CliUpdateAttemptsPage` with the store's incarnation, `limit` clamped to 1-100).
- **Tests** (fakes: an `Installer` trait over T7 and a `Verifier`/`Channel` trait over T5, so no network and no real CLI):
  - `run_tests.rs`: updated; up to date writes a row; refused gate writes a row; busy owner attempt at step 9 writes no row and returns `busy`; busy at step 13 ends `SkippedBusy`; force on native proceeds with live sessions; force on npm is still busy; the journal row exists before the first child runs; a staging failure → `Failed`, journal deleted, install untouched; a probe failure of the staged copy → `Refused(handshake_failed)`, nothing activated, the hold never taken; a failure after A2 → `RolledBack` with no channel call after A2 (finding 2); a failed restore → `RollbackFailed` and a quarantine row; a failed fence → `RollbackFailed(process_left)`, the journal row kept; `recovery_pending` while a journal row exists; `check_only` writes no row and sets `available`; `check_only` on a quarantined provider runs `verify_current` and clears it on a pass, keeps it on a failed handshake; a second concurrent attempt → `another_update`.
  - `recover_tests.rs`: phase `staging` → `Interrupted`, stage removed, journal deleted; phase `activating` → successor linked and `recovery_pending` written before the restore (a restore hook that stops the run leaves the journal, the running successor and the quarantine; a second `recover` ends it `interrupted`, links a new successor and restores → `RolledBack`, quarantine cleared) (finding 3); a recovery past its limit leaves the journal and the quarantine; a fence failure keeps the journal.
  - `schedule_tests.rs`: the window with a fixed offset, across midnight, the jitter bound; one scheduled attempt per window; a busy window writes one `SkippedBusy` row after the window; a quarantined provider or one with a journal row is skipped; an external version change writes one `external` row and refreshes; an external change of a quarantined provider runs `verify_current`; off → no attempt but external changes are still recorded.
  - `adapters.rs` test: a Claude adapter env has `DISABLE_AUTOUPDATER=1` only when `provider_updates.enabled`; an instance `env` value wins.
  - `serve.rs`: the recovery call is placed before `SessionManager::start` (finding 4); the PRE-PR review checks the order, and `recover_tests.rs` checks that `recover` needs no manager.
- **Acceptance:** `cargo nextest run -p fleetdeck-station -E 'test(cli_update) | test(adapters)'`; `cargo check -p fleetdeck-station --tests`.

### T9 (backend): deck routes and the audit import (≈ 600 lines)

- **Owned files:** `crates/fleetdeck-deck/src/http/cli_update_routes.rs` (new), `http/cli_update_routes_tests.rs` (new), `http/mod.rs` (two routes, one `mod`), `http/methods_table.rs` (two rows), `crates/fleetdeck-deck/src/store/cli_update_audit.rs` (new), `store/cli_update_audit_tests.rs` (new), `store/cli_update_cursor_schema.rs` (new), `store/schema.rs`, `store/mod.rs` (two `mod` lines), `crates/fleetdeck-deck/src/link/cli_update_audit.rs` (new), `link/info.rs` (the trigger, at most 10 lines), `link/mod.rs` (one `mod`), `crates/fleetdeck-deck/tests/cli_update.rs` (new).
- **Changes:** §3.11 (routes, the revision 2 import order, `cli_update_seen`). `schema.rs` stays at 799 lines: merge the two comment lines `// V46 (MCP P4b): …` and `// V47 (Fleet Sync FS-3): secret_files.` into one line `// V46 (MCP P4b): session_owner_grant, required_classes; V47 (Fleet Sync FS-3): secret_files.`, and change `// V48 to V50: …` to end with `; V53 (B28): the audit cursor of provider updates.` before the new `M::up(super::cli_update_cursor_schema::SQL),`.
- **Tests:**
  - `cli_update_audit_tests.rs`: import writes one audit row per new attempt with the actor, action, target, result and detail of §3.11; a second import of the same rows writes nothing; a new incarnation restarts at 0 and skips the attempt ids it saw (`cli_update_seen`); the cursor, the seen rows and the audit rows commit together (a failing insert leaves the cursor); a version that fails `is_release` shows `?`; the seen prune keeps 2000 per station.
  - `cli_update_routes_tests.rs`: owner only (403 for a token without the board login); `elevation_required` for an update and not for a check; 501 without the feature; the POST audit row for ok and for refused; `unknown_station`, `station_unreachable`.
  - `tests/cli_update.rs` (deck with a fake station link): an `info` answer with `last_seq = 2` leads to a `cli_update.attempts` call and two audit rows; **a restored station**: cursor `(A, 10)`, `info` with incarnation `B` and `last_seq = 3` → the call has `after_seq = 0`, the new rows are imported, the cursor becomes `(B, 3)` (finding 6); a page whose incarnation differs from `info` makes the task start again once with `after_seq = 0`.
- **Acceptance:** `cargo nextest run -p fleetdeck-deck -E 'test(cli_update)'`; the migration tests of the deck.

### T10 (web): the card line and the Settings card (≈ 620 lines)

- **Owned files:** `web/src/board/StationCliUpdate.tsx` (new), `web/src/board/stationCliUpdate.test.tsx` (new), `web/src/board/StationsPanel.tsx` (one element), `web/src/settings/CliUpdateCard.tsx` (new), `web/src/settings/cliUpdate.ts` (new: the texts, the calls, the poll), `web/src/settings/cliUpdate.test.tsx` (new), `web/src/settings/model.ts` (the section), `web/src/settings/SettingsTab.tsx` (the slot, at most 10 lines).
- **Changes:** §3.12 exactly (with the revision 2 rows and texts), with the generated types of T1. The worker applies the `fleet-design-system` skill; no new token, class or primitive.
- **Tests:** each row of the card table (text and tone), the quarantine row wins over the last attempt, the recovery `rolled_back` text, the method and reason texts, an unknown reason; the `Updates…` link only for `canAdmin`; no line without `info.cli_update`. Settings: the section order; `Check` sends `check_only: true` without an elevation, and on a quarantined provider shows the pass and the still-blocked texts; `Update now…` asks the elevation, shows the Claude and Codex dialog texts; the busy texts with and without the check box (`force` sent only from the check box); the `recovery_pending` text; the 501 text; the poll stops when `running` is `null`.
- **Acceptance:** `bun run typecheck`, `bun run lint`, `bun test web/src/board/stationCliUpdate.test.tsx web/src/settings/cliUpdate.test.tsx` (through `heavy.sh`).

### T11 (docs): operations and the security checklist (≈ 180 lines)

- **Owned files:** `docs/v2/operations.md` (a section "Provider updates" after the station upgrade section), `docs/v2/SECURITY-CHECKLIST.md` (one row), `docs/v2/DECISIONS-LOG.md` (the entries below), `docs/v2/BACKLOG.md` (B28 status).
- **Changes:** how to turn updates on (Settings path), the window, channels and pins, what "report only" and "custom launcher" mean and how to update by hand per method, the log files, the rollback copies and the stage folders (where they are, when they go), the quarantine and how to clear it (reinstall, then `Check`), the reinstall hints, `gpgv` on macOS (`brew install gnupg`).
- **Acceptance:** `make check` (docs lint).

### At the merge (coordinator)

1. The tests of every task, the module-size and layering guards, `make check`.
2. The adversary PRE-PR REVIEW of the final branch.
3. Live, read only first: on every station, Settings → station → Provider updates → `Check` for Claude and Codex. Record the method, `updatable`, `blocked` and the target per station in the merge note (this replaces the research that the refused grant stopped; see "Research limit" at the top).
4. Live, one write: with the owner present, `Update now…` for Codex on Lighting (Bun) and for Claude on one native station; then check the audit log, the card line and a new session. Check also that the stage folders and the rollback copies are gone after `Updated`.

## 8. Owner questions (the build proceeds with the defaults)

1. **Nightly updates on or off by default?** Default: **off**; when the owner turns them on, the window is 03:00 to 05:00 station local time. Reason: the update changes who updates Claude (§1.7), so it is the owner's choice per station.
2. **Claude channel?** Default: **stable** (about one week old). `latest` gets fixes sooner and also regressions sooner.
3. **Wait time for a new version?** Default: **48 hours** (`min_age_hours`), for Claude and Codex.
4. **Update while sessions run?** Default: **only for native Claude, after the owner ticks "Update while they run"**. Never for npm and Bun (files change in place). Never on the schedule.
5. **No `gpgv`, no Claude update?** Default: **yes, fail closed** (the card says "install GnuPG (gpgv)"). The other option is a checksum-only check, which trusts the same host for the file and its checksum.
6. **Homebrew?** Default: **report only**. The other option runs `brew upgrade --cask claude-code` / `codex`, where the station cannot choose or check the version before the change.
7. **Also stop Claude's own updater in the owner's terminals?** Default: **no**: only the Claude processes of the station get `DISABLE_AUTOUPDATER=1`; the station records the other changes as "changed outside Fleet Deck".
8. **An upper bound ("tested range" maximum)?** Default: **no upper bound**; the deny list and the tested minimum of the station build stay the fixed bounds. An upper bound in the build would stop every update until the next Fleet Deck release.
9. **Claude installed with npm or Bun?** Default (revision 2): **report only**. Its `postinstall` (`node install.cjs`) runs code that the station cannot bind to a signature before it runs. The vendor recommends the native installer; no known station uses npm for Claude (Lighting is native). The other option stages it with scripts off and runs the verified `install.cjs` after the binding; that needs a study of what `install.cjs` downloads.
10. **Native Claude: the station activates the version itself, not `claude install`?** Default (revision 2): **yes**. The station does the download and the checksum as `bootstrap.sh` does and swaps `~/.local/bin/claude`. The vendor's own updater keeps working with that layout. The cost: if the vendor changes the native layout, the station reports `custom_launcher` until a Fleet Deck release follows.

## Decision log entries (to append)

## DL-550: B28 the station stages, binds and then activates Claude and Codex, at an exact version (design agent, 2026-10-24, revision 2)

- Context: B28; design `docs/v2/design/cli-update.md` §3.3, §3.6. Today nothing in Fleet Deck updates a provider CLI, but native Claude updates itself from inside station sessions.
- Decision (revision 2): the station detects the install from the launcher chain that it runs. It stages the new version, binds the staged bytes to the signed digests, probes the staged copy, and only then activates it. Native Claude (only with the managed link `~/.local/bin/claude`): the download of `bootstrap.sh`, the SHA-256 against the signed manifest, a rename into `versions/`, a swap of the link. Codex by npm or Bun: a staging install into a private folder with scripts off, a fixed environment and explicit destinations; every staged package's lockfile integrity equals the signed integrity; the verified folders are renamed into the detected global `node_modules`. Claude by npm or Bun, Homebrew, system packages, the Codex standalone installer, custom launcher links and unknown layouts are report only. No shell; every version in an argv is digits and dots.
- Rejected: `claude install <version>` and `npm|bun install -g` into the live install (bytes run before anything binds them); `claude update` / `codex update` (the version is chosen after the gate); `curl | sh`.

## DL-551: B28 vendor signatures are checked and fail closed (design agent, 2026-10-24)

- Decision: Claude: the release `manifest.json` is verified with `gpgv`, a keyring pinned in the station binary, and the `VALIDSIG` fingerprint `31DDDE24DDFAB679F42D7BD2BAA929FF1A7ECACE`; the staged file's SHA-256 must equal the manifest checksum of the station's platform before the file runs (revision 2). Npm: at least one registry ECDSA P-256 signature with a pinned key over `<name>@<version>:<integrity>`, for the package and its platform packages, and every staged package's recorded integrity equals that signed integrity before anything in the stage runs (revision 2); Codex needs an SLSA provenance attestation (presence). No `gpgv`: no native Claude update. An unknown key: refused until a release pins it.
- Rejected: a new OpenPGP crate; a checksum-only check.

## DL-552: B28 the gate and the way back (design agent, 2026-10-24)

- Decision: a target must be a release version, not `KNOWN_BROKEN`, not below `TESTED_MIN_VERSION`, not below the handoff minimum (and the bridge minimum on a shim host), newer than the installed version unless pinned, and at least `min_age_hours` old (default 48) unless pinned. Before the activation (revision 2): the staged copy reports the target and passes a handshake with the adapter's launch arguments (Claude `initialize` with models; Codex `initialize` and `account/read`). After it: the launcher resolves to the new copy and reports the target. Before any change, a local rollback copy is made and checked (native: a hard link or a copy outside `versions/`, SHA-256; npm, Bun: the previous folders renamed aside, tree digest). A failure goes back from that copy with no network. A failed way back writes a durable quarantine with a reinstall hint.

## DL-553: B28 updates run only when no session of the provider runs, and they hold new starts (design agent, 2026-10-24)

- Decision: `RestartState` gets `updating`; `hold_starts` is `restarting || updating`, so queued starts and resumes of every provider wait. An update and a restart exclude each other. The update starts only after `settle_starts(10 s)` and when no session of an instance of the kind has a live process. The owner can force it for native Claude only. A deferred setup start of the updating kind fails with `provider_updating`.
- Rejected: a per-provider hold in the claim query; updating while npm or Bun sessions run.

## DL-554: B28 the schedule is off by default; `[provider_updates]` holds the settings (design agent, 2026-10-24)

- Decision: `[provider_updates]` with `enabled` (restart value, default false), `window_start` (03:00), `window_hours` (2), `timezone` (`local` via `date +%z`, `UTC` or a fixed offset), `claude_channel` (`stable`), `claude_pin`, `codex_pin`, `min_age_hours` (48). A window try every 15 min with a jitter per station; no update at the station start. With `enabled`, the Claude processes of the station get `DISABLE_AUTOUPDATER=1`.

## DL-555: B28 the station refreshes its provider report after a change, and records changes that it did not make (design agent, 2026-10-24)

- Decision: the provider report becomes refreshable (`refresh_providers`), which also sets or clears the broken mark and replaces the handoff versions. Every 15 min, also with updates off, the station compares `--version` with its report; a difference refreshes it and records an attempt with the trigger `external`. This amends DL-059 (one probe per process).

## DL-556: B28 attempts are kept by the station and copied into the deck audit log (design agent, 2026-10-24)

- Decision: station V49 `cli_update_attempts` (500 rows kept). `station.info` carries `cli_update` with `last_seq`; the deck asks `cli_update.attempts` and writes one audit row per finished attempt (actor `station:<name>`, action `cli_update.attempt`), with a cursor per station and incarnation (deck V53 `cli_update_cursor`) in the same transaction. The owner's `Check` and `Update now` are audited at the route. No child output leaves the station; it goes to a 0600 log file.

## DL-557: B28 the board: state on the station card, actions in Settings (design agent, 2026-10-24)

- Decision: the station card shows one update line per provider and the schedule, with a link to Settings for the owner. Settings → station → "Provider updates" holds the fields and a card with `Check` (no elevation) and `Update now…` (owner, elevation, a dialog that names the source, the checks and the way back). Routes `GET|POST /api/stations/{station}/cli-update`. Link feature `cli_update.v1`, methods `cli_update.status`, `cli_update.run`, `cli_update.attempts` (admin, not mutating at the link).

## DL-558: B28 is high risk: adversary design review and pre-PR review (design agent, 2026-10-24)

- Decision: B28 runs code from an external source as the owner on a timer and changes the start admission. It gets the adversary DESIGN REVIEW before T1-T8 and the PRE-PR REVIEW of the final branch. `SECURITY-CHECKLIST.md` gets a row.

## DL-559: B28 owner questions answered by default (design agent, 2026-10-24)

- Decision, until the owner answers: updates off by default; Claude `stable`; 48 h wait; force only for native Claude; no `gpgv` means no native Claude update; Homebrew report only; the owner's own Claude keeps its updater; no upper version bound; Claude by npm or Bun report only; native activation by the station (revision 2).

## DL-560: B28 a durable journal, recovery before the manager, and a durable quarantine (design agent, 2026-10-24, revision 2)

- Context: adversary DESIGN REVIEW findings 3, 4 and 8.
- Decision: an attempt that may change an install writes a journal row with its attempt row, before any change, and removes it only in the transaction that records the result. The start recovery runs between `Store::open` and `SessionManager::start`; for an activated attempt it links its successor attempt and writes the quarantine in its first transaction, then restores. Quarantine rows (`rollback_failed`, `recovery_pending`, `process_left`) are loaded into the broken marks before the effect worker starts; only a passing update or `verify_current` (integrity where the method allows it, the version gate and the handshake) clears them; a provider refresh never does.
- Rejected: marking running rows `interrupted` first and recovering after (a second crash loses them); a memory-only quarantine.

## DL-561: B28 every child of an attempt is fenced (design agent, 2026-10-24, revision 2)

- Context: adversary DESIGN REVIEW finding 5.
- Decision: each child runs in its own process group with `FLEETDECK_CLI_UPDATE_ID=<marker>`; after it ends, TERM and KILL to the group and the marker scan of the setup cleanup (DL-148) must find no process before the attempt goes on. A survivor quarantines the provider and keeps the journal row; the next start fences first.

## DL-562: B28 the deck audit import reads the incarnation first (design agent, 2026-10-24, revision 2)

- Context: adversary DESIGN REVIEW finding 6.
- Decision: the deck compares the stored incarnation with `station.info` before it builds `after_seq` (0 for a new incarnation); `cli_update.attempts` answers its incarnation with the rows; `cli_update_seen` keeps the imported attempt ids so a restored station shows no attempt twice.

## Revision 2 (design review)

The adversary DESIGN REVIEW of revision 1 (`/home/luismorales/fleetdeck-briefs/backlog2/cli-update/adv-design.md`, base `d1fa4205`, head `fda3932b`) gave FAIL. I checked each finding against the code on `d1fa4205`. All nine are correct. The table gives the finding, the check and the change.

| # | Severity | Finding | Checked in the code | Change in revision 2 |
| --- | --- | --- | --- | --- |
| 1 | BLOCKER | The signature checks covered the metadata, not the bytes that run: npm/Bun fetched the packages again at install, and native ran `--version` before the checksum. | True: revision 1 §3.5 checked documents only; §3.6 ran `npm|bun install -g` and `claude install` into the live install, and its step 2 ran the new binary before step 3 checked it. | §3.6 is now stage → bind → probe the staged copy → hold → rollback copy → activate → verify. Native: the station downloads the binary and checks its SHA-256 against the signed manifest before it runs (S1-S2). Codex: the staging runs with scripts off, and `verify_staged` binds every staged package to the signed integrity (S3, T6). Only the bound copy is activated. Claude by npm or Bun is report only (its `postinstall` cannot be bound). §3.5 step 12, §5 decision 1, §6 T1/T2, DL-550, DL-551. |
| 2 | MAJOR | The way back needed the network after the working install was overwritten. | True: revision 1 §3.6.2 fetched and verified `from_version` again for npm/Bun after a failure; native relied on `versions/<from>`, which the vendor cleanup can remove. | §3.6 A1 makes a local rollback copy before any change and checks it (native: hard link or copy outside `versions/`, SHA-256; npm/Bun: the previous folders renamed aside, tree digest). §3.6.2 restores from it with no network and is idempotent. §5 decision 17, §6 T7. |
| 3 | MAJOR | Crash recovery ended its journal before the recovery was durable, and its return type had no `rollback_sha256`. | True: revision 1 `cli_update_interrupt_running` ended the rows first and returned `CliUpdateAttempt`. | New table `cli_update_journal` (§3.11), written with the attempt row before any change and removed only in the final transaction. §3.7.2 c links the successor attempt, keeps the marker and writes the quarantine in its first transaction. `RecoveryRecord` carries the rollback data and the install identity. `cli_update_interrupt_running` is replaced by `cli_update_recover_begin`, `cli_update_finish` and `cli_update_interrupt_orphans`. DL-560. |
| 4 | MAJOR | The start recovery came after the effect worker started. | True: `SessionManager::start_inner` spawns the worker (`manager/mod.rs:357-365`); revision 1 put the recovery in `serve.rs` after it. | §3.7.2 runs between `Store::open` and `SessionManager::start` (`serve.rs:314-366`), where no worker, link, assist or terminal exists. `start_inner` loads the quarantine before `EffectWorker::register`. §5 decision 19, §3.13 item 7. |
| 5 | MAJOR | Killing one child does not stop installer descendants. | True: `exec::run_for` uses `kill_on_drop` on the direct child only (`integrations/exec.rs:178-221`); no process identity was stored. | §3.6.3 the fence: own process group, `FLEETDECK_CLI_UPDATE_ID` marker, TERM/KILL, the setup cleanup's marker scan (`workspace/setup_cleanup.rs`, DL-148); the pgid goes into the journal; nothing goes on before the fence passes; a survivor quarantines the provider and the next start fences first. The probe kills its group too (§3.6.1). §6 T12, DL-561. |
| 6 | MAJOR | The audit import built `after_seq` from the cursor before it compared incarnations. | True: revision 1 step 1 used the cursor; the reset was only in step 2. `upgrade::restore` writes a new incarnation (`upgrade.rs:317-319`). | §3.11 steps 1-3: the incarnation is compared first (`after = 0` for a new one); the page carries its incarnation (`CliUpdateAttemptsPage`); `cli_update_seen` avoids duplicates after a restore. T9 tests a restored station with a lower `seq`. DL-562. |
| 7 | MAJOR | Choosing the package manager did not choose the destination. | True: `ChildEnv` removes only `ANTHROPIC_*`, `CLAUDE_*`, `OPENAI_*`, `CODEX_*`, `FLEETDECK_*` (`childenv.rs:26-31`), so `npm_config_*` and `BUN_*` reached the child; npm had no `--prefix`. | The staging child gets a fixed environment (§3.6 "Environments"): its own `HOME`, empty npm configs, explicit `--prefix` and the four `BUN_INSTALL*` names, all inside the private stage. The activation renames only into the `modules` folder that detection derived from the launcher; `bun_install` comes from the components of `real`, never from the environment. §6 T9. |
| 8 | MAJOR | A failed-update block disappeared at a restart. | True: `broken` is a memory map (`manager/target.rs:100`), and revision 1 cleared marks by the `VersionStatus` of the version string (`manager_providers.rs:148-149`). | §3.8 item 4 `BrokenMark { Denied, Quarantined }`; §3.8.1 the durable `cli_update_quarantine`, loaded before dispatch, cleared only by a passing update or `verify_current` (integrity where possible, the version gate, the handshake). `refresh_providers` never clears it. `ProviderUpdateState::quarantined`, `UpdateOutcome::Verified`. DL-560. |
| 9 | MAJOR | Native detection accepted a launcher alias that the installer does not manage. | True: revision 1 rule 1 accepted any link into `versions/`, and `claude install` changes only `~/.local/bin/claude`. | §3.3 rule 1 needs the managed link `~/.local/bin/claude` in the launcher chain, pointing straight into `versions/`; the station swaps that link itself and checks `canonicalize(launcher)` after (§3.6 V). Other aliases are `custom_launcher` (report only). |

Other changes that follow from these:

- The hold now covers only the activation and the way back (at most 3 min, usually seconds); the download and the staging run before it, after a busy precheck (§3.7 steps 9 and 13; §5 decision 23).
- New reason codes (`custom_launcher`, `unsupported_layout`, `tool_too_old`, `unverified_package`, `integrity_mismatch`, `process_left`, `recovery_pending`, `rollback_failed`, `quarantined`) with their board texts (§3.2, §3.12).
- The build plan has eleven tasks in six waves (§7): the store and the manager parts are split (T3, T4); the pure binding (T6) and the stage/activate/fence engine (T7) are new tasks; the docs task is T11.
- Owner questions 9 and 10 (§8).
- Residuals: the station trusts the local package manager's tarball check for the extracted bytes (§6 T5); Bun's own records keep the old Codex version (§6 T13); `verify_current` has no integrity check for an owner's npm or Bun reinstall (§6 T14).
