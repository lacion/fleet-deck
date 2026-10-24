# The station updates Claude Code and Codex by itself (B28)

- **Status:** Draft for the architect, revision 1 (2026-10-24). Risk: **high** (§6): the station runs code from the internet as the owner, on a timer. The adversary does a DESIGN REVIEW before the design is frozen and a PRE-PR REVIEW before the merge.
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

**Q1. Which install methods does the station detect, and which does it update?** It detects native (Claude), npm global, Bun global, Homebrew, a system package and unknown (§3.3). It updates native Claude with the vendor's own `claude install <version>`, and npm and Bun globals with the exact install command that the vendor's updater runs, pinned to one version. Homebrew, system packages, the Codex standalone installer (a `curl | sh` at run time) and unknown layouts are **report only** (owner question 6).

**Q2. Schedule.** A nightly window on the station's own clock, default 03:00 to 05:00 local time, **off by default** (owner question 1). The owner can also click "Update now" or "Check" per station and per provider at any time. No update runs at the station start (a restart storm must not become an update storm); the start only finishes an interrupted attempt (§3.7).

**Q3. Pin or follow.** Follow a vendor channel, with a pin per provider as the override. Claude: `stable` (default) or `latest`. Codex: `latest` (its only channel). A new version installs only when it is at least `min_age_hours` old (default 48). The fixed bounds stay: never a `KNOWN_BROKEN` version, never below `TESTED_MIN_VERSION`, never below the handoff minimum. No upper cap (owner question 8).

**Q4. Gate.** Before the install: the version gate (§3.4) and the vendor signature (§3.5). After the install: the version, the file checksum (native), and a protocol handshake with the adapter's real launch arguments (§3.6). A failure goes back to the previous version.

**Q5. Running sessions.** An update starts only when no session of that provider has a live process, and it holds new starts and resumes until it ends (§3.7). The owner can force an update while sessions run only for native Claude, where the running version stays on disk (owner question 4).

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
    /// `launcher` with every link resolved.
    pub real: PathBuf,
    pub tool: Tool,
    /// Set when the method is report only or a tool is missing.
    pub blocked: Option<&'static str>,
}

pub enum Tool {
    /// `versions` is `~/.local/share/claude/versions`; `gpgv` is the verifier.
    Native { versions: PathBuf, gpgv: Option<PathBuf> },
    /// `npm` is `<prefix>/bin/npm`; `package` is `@anthropic-ai/claude-code` or `@openai/codex`.
    Npm { prefix: PathBuf, npm: PathBuf, package: &'static str },
    /// `bun` is `<bun_install>/bin/bun`.
    Bun { bun_install: PathBuf, bun: PathBuf, package: &'static str },
    None,
}

/// Pure: classifies `launcher`/`real` with `home` and the facts in `probe`.
pub fn classify(kind: ProviderKind, launcher: &Path, real: &Path, launcher_is_link: bool,
                home: Option<&Path>, exists: &dyn Fn(&Path) -> bool) -> (InstallMethod, Tool, Option<&'static str>);

/// Resolves the launcher on the instance PATH (same rule as the launch) and calls `classify`.
pub fn detect(kind: ProviderKind, binary: &Path, env: &ChildEnv) -> Result<Install, &'static str>;
```

Rules of `classify`, in this order (paths are compared by components, never by substring):

| # | Condition | Method | Tool | `blocked` |
| --- | --- | --- | --- | --- |
| 1 | Claude; `launcher_is_link`; `real.parent() == home/.local/share/claude/versions`; the file name matches `^\d{1,6}\.\d{1,6}\.\d{1,6}$` | `Native` | `Native { versions, gpgv }` | `gpgv_missing` when no `gpgv` |
| 2 | Claude; `real` under `home/.local/share/claude/versions` but rule 1 fails (a configured absolute path to one version file, no link) | `Unknown` | `None` | `report_only_method` |
| 3 | `real` has the components `<prefix>/lib/node_modules/<package>/…` (`@anthropic-ai/claude-code` for Claude; `@openai/codex` for Codex) and `<prefix>/bin/npm` exists | `Npm` | `Npm { prefix, npm, package }` | — |
| 4 | `real` is under `<bun_install>/install/global/node_modules/<package>/` with `<bun_install>` = `$BUN_INSTALL` of the child env or `home/.bun`, and `<bun_install>/bin/bun` exists | `Bun` | `Bun { … }` | — |
| 5 | rule 3 matches but `<prefix>/bin/npm` is missing; or rule 4 without `bun` | `Npm` / `Bun` | `None` | `tool_missing` |
| 6 | `real` starts with `/opt/homebrew/`, `/usr/local/Caskroom/`, `/usr/local/Cellar/` or `/home/linuxbrew/.linuxbrew/` | `Homebrew` | `None` | `report_only_method` |
| 7 | `real` starts with `/usr/` or `/opt/` (and not rule 6) | `SystemPackage` | `None` | `report_only_method` |
| 8 | anything else (the Codex standalone installer, a custom script) | `Unknown` | `None` | `report_only_method` |

- `gpgv` lookup: the first executable of `gpgv` on the instance PATH, then `/usr/bin/gpgv`, `/opt/homebrew/bin/gpgv`, `/usr/local/bin/gpgv`, `/home/linuxbrew/.linuxbrew/bin/gpgv`.
- `detect` returns `Err(reason::NOT_FOUND)` when the launcher is not found.
- Lighting: Claude → rule 1 (native, `gpgv` found); Codex → rule 4 (Bun). The shadowed npm copy (§1.6) is never seen, because detection starts from the launcher that the station runs.

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

**Claude (native and npm).**

1. Target: the pin, or the text of `CLAUDE_RELEASES/<channel>` (`stable` or `latest`; at most 64 bytes, trimmed). It must pass `is_release`.
2. Manifest: `CLAUDE_RELEASES/<target>/manifest.json` (at most 256 KiB) and `…/manifest.json.sig` (at most 16 KiB).
3. Signature (`verify_claude_manifest`): in a new private temp directory (0700) under `<station dir>/cli-update/tmp`, write `claude-code.gpg` (`CLAUDE_KEYRING`), `manifest.json`, `manifest.json.sig` and an empty `gnupg` directory (0700). Run `gpgv --homedir <tmp>/gnupg --keyring <tmp>/claude-code.gpg --status-fd 1 <tmp>/manifest.json.sig <tmp>/manifest.json` with the environment `PATH`, `LANG=C` only, time limit 20 s. It passes only when gpgv exits 0 **and** a stdout line starts with `[GNUPG:] VALIDSIG ` and its last field equals `CLAUDE_KEY_FPR`. A `VALIDSIG` of another key → `signature_unknown_key`; anything else → `signature_invalid`. The temp directory is removed after.
4. The manifest must parse, its `version` must equal the target, and `buildDate` (RFC 3339 UTC) gives `published_at`. The checksums: every `platforms.<p>.checksum` whose key is `<os>-<arch>` or `<os>-<arch>-musl` (`os`: `linux` or `darwin`; `arch`: `x64` or `arm64`, from `std::env::consts`). None → `signature_invalid`.
5. Claude by npm (`@anthropic-ai/claude-code`): the target comes from `NPM_REGISTRY/-/package/@anthropic-ai%2fclaude-code/dist-tags` (`stable` or `latest`), and the npm checks below apply as well. The Claude manifest is still read for `published_at` and the signature (the npm package installs the same native binary).

**npm and Bun (`@openai/codex`, `@anthropic-ai/claude-code`).**

6. Target: the pin, or `dist-tags.latest` (Codex) / `dist-tags.<channel>` (Claude), from `NPM_REGISTRY/-/package/<name>/dist-tags` (at most 64 KiB).
7. The version document `NPM_REGISTRY/<name>/<target>` (at most 1 MiB): `name` and `version` must match; `dist.integrity` starts with `sha512-`; at least one `dist.signatures[]` entry whose `keyid` is in `NPM_KEYS` verifies (`verify_npm_signature`); else `signature_unknown_key` (a keyid that is not pinned) or `signature_invalid`.
8. `verify_npm_signature(name, version, integrity, sig_b64, spki_b64)`: the SPKI DER must be 91 bytes with the prefix `3059301306072a8648ce3d020106082a8648ce3d030107034200`; the last 65 bytes are the point. `ring::signature::UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, point).verify(format!("{name}@{version}:{integrity}").as_bytes(), &sig_der)`.
9. Platform packages: for each `optionalDependencies` entry whose key ends with `-<os>-<arch>` or `-<os>-<arch>-musl`: the spec `npm:<n>@<v>` names `<n>@<v>`; a plain spec `<v>` names `<key>@<v>`; `<v>` must be `<target>` or `<target>-<suffix>` with a suffix of `[a-z0-9-]{1,32}`. Each is fetched and checked as in step 7.
10. Provenance: for `@openai/codex` only, the main version document must have `dist.attestations.provenance.predicateType == "https://slsa.dev/provenance/v1"`; else `provenance_missing`. (The station checks the presence, not the Sigstore bundle; §6 T5.)
11. `published_at`: `Last-Modified` of `HEAD NPM_REGISTRY/<name>/-/<basename>-<target>.tgz` (curl config `head`, `write-out = "%header{last-modified}"`), parsed as an RFC 1123 GMT date. No header → `None` (§3.4 step 5).

A check (`check_only`) runs §3.3, steps 1-11 and §3.4, and remembers `available` and `checked_at` in memory. It writes no attempt row and changes nothing.

### 3.6 Install, verify, probe and the way back (`cli_update/install.rs`)

Every child runs without a shell, with a fixed argv, with the instance child environment plus `DISABLE_AUTOUPDATER=1`, stdin closed, a time limit, and its stdout and stderr appended to `<station dir>/cli-update/logs/<attempt_id>.log` (mode 0600, at most 256 KiB, the newest 20 files kept). No child output goes into the database, the wire or the station log.

| Method | Install argv | Time limit |
| --- | --- | --- |
| Native | `<real> install <target>` (the installed binary, already verified) | 10 min |
| Npm | `<prefix>/bin/npm install --global --no-fund --no-audit --registry=https://registry.npmjs.org/ --@openai:registry=https://registry.npmjs.org/ [--ignore-scripts] <package>@<target>`; `PATH` of the child starts with `<prefix>/bin` (the right `node`). `--ignore-scripts` for `@openai/codex` only (Claude's npm package needs its `postinstall`). For Claude the scope flag is `--@anthropic-ai:registry=…`. | 10 min |
| Bun | `<bun> add --global --registry=https://registry.npmjs.org/ <package>@<target>` with `BUN_INSTALL=<bun_install>` | 10 min |

Before the install: free space of the file system of `versions` (native) or `<prefix>` / `<bun_install>` (npm, Bun) ≥ 1 GiB (`rustix::fs::statvfs`), else `disk_full` (refused). Native: record `from_version` and the SHA-256 of `real` (`rollback_sha256`).

After the install (`verify_installed`):

1. Detect again (§3.3). The method must be the same.
2. `<launcher> --version` parses (the adapters' `parse_version`) to the target, else `version_mismatch`.
3. Native: the SHA-256 of `versions/<target>` is one of the manifest checksums (§3.5 step 4), else `checksum_mismatch`. Npm and Bun: `<package dir>/package.json` `version` is the target, else `version_mismatch`.
4. The probe (§3.6.1) passes, else `handshake_failed`.

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

#### 3.6.2 The way back

| Method | Rollback | Then |
| --- | --- | --- |
| Native | When `versions/<from_version>` exists and its SHA-256 equals `rollback_sha256`: create the link `<launcher>.fd-rollback` → `versions/<from_version>` and rename it over `<launcher>` (atomic). Else no rollback. | Probe again |
| Npm, Bun | §3.5 steps 6-9 for `from_version` (signature check), then the install argv with `@<from_version>` | Probe again |

The probe after the rollback passes with `version == from_version` → `RolledBack` (reason of the first failure). Else → `RollbackFailed`; the station marks every instance of the kind broken (§3.8) with the text `"{name} failed after an update and the way back failed. Reinstall it on this station: {hint}"`, where `hint` is `claude install <from>` (native), `npm install -g <package>@<from>` (npm) or `bun add -g <package>@<from>` (Bun).

### 3.7 One attempt (`cli_update/run.rs`)

```text
run_attempt(kind, trigger, check_only, force) -> CliUpdateRunAnswer
 1. Single flight: try_lock(self.running). Held → refused another_update.
 2. install = detect(kind)                      (Err → refused not_found)
    installed = probe version (update_probe::check version only)
 3. blocked (report_only_method, second_install, tool_missing, gpgv_missing)
      → check_only: answer the state; else record ReportOnly (report_only_method,
        second_install) or Refused (tool_missing, gpgv_missing); end.
 4. target, published_at, verification = §3.5
      Err(channel_unreachable) → record Failed; other Err(r) → record Refused(r); end.
 5. verdict = gate(§3.4)  UpToDate → record UpToDate; Err(r) → record Refused(r); end.
    ("record" = one row inserted already ended, `cli_update_insert(a, Some(end))`;
     with check_only no row is written in steps 3-5, the answer carries the result.)
 6. check_only → state.available = target, checked_at = now; answer; end (no row).
 7. Insert the attempt row (outcome running, from_version, target, rollback_sha256).
 8. Hold: manager.begin_provider_update(kind):
      restarting → end row Refused(station_restarting)
      settle_starts(10 s) false → release; end row SkippedBusy(starts_in_flight)
      live = manager.live_sessions_of(kind)
      live not empty and not (force and method == Native)
         → release; end row SkippedBusy(sessions_running); answer busy = live
 9. Answer the caller now: attempt_id, started. The rest runs in a task.
10. Disk check; install (§3.6); verify_installed.
11. Pass → manager.refresh_providers(); release; end row Updated (to = target).
12. Fail at 10 → rollback (§3.6.2); manager.refresh_providers(); release;
      end row Failed (install failed, nothing changed: the version is still from_version)
           | RolledBack | RollbackFailed.
13. The whole of 10-12 has a limit of 15 min; past it → kill the child, step 12 with reason timeout.
```

- `Failed` versus `RolledBack`: when the install command fails and the probe after it still shows `from_version` with a passing handshake, the outcome is `Failed` (no rollback was needed). When anything changed, the way back runs.
- A scheduled `SkippedBusy` writes no row at step 8; the schedule (§3.9) writes one `SkippedBusy` row per window when the provider stayed busy in all of it. An owner attempt that is busy writes no row either: the answer carries `busy`, and the route audits the refusal (§3.11).
- **Recovery at the station start:** `Store::cli_update_interrupt_running` marks every `running` row `Interrupted` and returns them. For each, the station probes the install: when the version is not `from_version` or the handshake fails, it runs §3.6.2 as a new attempt with the trigger `recovery` (from = the found version, target = the interrupted `from_version`). This runs once, before the scheduler's first tick, and it does not wait for a deck.

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

- `begin_claim`: `hold_starts: self.restarting() || self.updating().is_some()`. Queued starts and resumes of **every** provider wait while an update runs (a few minutes at most); they are not failed.
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

### 3.8 The provider report follows the install (`manager_providers.rs`)

1. `providers: tokio::sync::OnceCell<…>` becomes `providers: tokio::sync::Mutex<Option<Vec<StationProvider>>>` (`manager/mod.rs:126`, `:396`).
2. `providers()` keeps its contract (the first call probes; later calls reuse) under the mutex.
3. New `refresh_providers()`: probes now under the same mutex, replaces the cache, and applies the effects of the new result: `target.set_broken(instance, kind, status == Broken)` (clears a mark too), and `coordinator.replace_versions(kind, versions of that kind)`.
4. `SessionTarget::set_broken(instance, kind, broken: bool)` (`target_base.rs`); `mark_broken` stays as `set_broken(…, true)`.
5. `Bindings::replace_versions(provider, Vec<String>)` (`bindings_handoff.rs`).
6. `refresh_providers()` runs after every attempt that changed or may have changed an install, and after an external change (§3.9).
7. `RollbackFailed`: after the refresh, `set_broken_with(instance, text)` keeps the text of §3.6.2 until the next refresh that shows a passing version.

### 3.9 The schedule (`cli_update/schedule.rs`)

- A task started by `serve.rs` with the shutdown channel. First tick 2 min after the start (after the recovery), then every 15 min.
- **External change, every tick, also when updates are off:** for each provider, `<launcher> --version` (10 s). When it differs from the cached `providers()` version and no attempt runs: `refresh_providers()`, and one attempt row with trigger `external`, outcome `updated`, from → to, reason `outside_fleet_deck`. The audit log then also shows Claude's own updates.
- **The window**, when `provider_updates.enabled`: `offset` is the fixed offset of `timezone`, or for `local` the output of `date +%z` (10 s; on failure the tick is skipped). The window is `[window_start, window_start + window_hours)` in that offset; it may cross midnight. A station starts its first try at `window_start + jitter`, `jitter = (first byte of the SHA-256 of the station name) % min(60, window_minutes / 2)` minutes, so the fleet does not update at one moment.
- In the window, for Claude then Codex: when no attempt of that provider with the trigger `schedule` ended in this window (except a busy one), run `run_attempt(kind, Schedule, false, false)`. A busy result is kept in memory; at the first tick after the window, a provider that was busy at every try gets one `SkippedBusy` row (reason `sessions_running`).
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

**The audit import** (`link/cli_update_audit.rs`, `store/cli_update_audit.rs`, new):

1. After each `station.info` answer with `cli_update: Some(report)`, on a link with `cli_update.v1`: when `report.last_seq` is above the stored cursor, a task asks `cli_update.attempts { after_seq: cursor, limit: 100 }` until a page has fewer than 100 rows.
2. `DeckStore::import_cli_update_attempts(station_id, station_name, incarnation, rows)` runs one transaction: it reads the cursor row; when its `incarnation` differs from the answer's, the cursor counts as 0; it inserts one audit row for each row with `seq` above the cursor, and stores `MAX(seq)` with the incarnation.
3. The audit row: actor `station:<station name>`, action `cli_update.attempt`, target `<station name>/<provider>`, result `ok` for `updated`, `up_to_date`, `report_only`, `skipped_busy`; `refused` for `refused`; `failed` for `failed`, `rolled_back`, `rollback_failed`, `interrupted`, `unknown`. Detail: `"<trigger> <method> <from or -> -> <to or target or ->: <outcome>"` plus `" (<reason>)"` when there is a reason; versions pass `is_release` or are written as `?`; at most 300 characters. `at` is the deck clock at the import.

Deck migration **V53** (`crates/fleetdeck-deck/src/store/cli_update_cursor_schema.rs`, new):

```sql
CREATE TABLE cli_update_cursor (
    station_id  TEXT    PRIMARY KEY,
    incarnation TEXT    NOT NULL,
    seq         INTEGER NOT NULL
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
    rollback_sha256 TEXT,
    outcome         TEXT    NOT NULL,
    reason          TEXT,
    started_at      INTEGER NOT NULL,
    ended_at        INTEGER
);
CREATE INDEX cli_update_attempts_provider ON cli_update_attempts (provider, seq);
```

Station store API (`crates/fleetdeck-station/src/store/cli_updates.rs`, new):

```rust
pub struct NewAttempt { pub attempt_id: String, pub provider: ProviderKind, pub trigger: UpdateTrigger,
    pub method: InstallMethod, pub channel: String, pub from_version: Option<String>,
    pub target_version: Option<String>, pub rollback_sha256: Option<String> }
pub struct AttemptEnd { pub outcome: UpdateOutcome, pub reason: Option<String>,
    pub to_version: Option<String>, pub target_version: Option<String> }

impl Store {
    /// Inserts a row (outcome `running`, or the final outcome when `end` is given); returns `seq`.
    pub async fn cli_update_insert(&self, a: NewAttempt, end: Option<AttemptEnd>) -> Result<u64>;
    pub async fn cli_update_end(&self, attempt_id: String, end: AttemptEnd) -> Result<()>;
    /// Finished rows after `after_seq`, oldest first, stopping before the first `running` row.
    pub async fn cli_update_attempts(&self, after_seq: u64, limit: u32) -> Result<Vec<CliUpdateAttempt>>;
    pub async fn cli_update_last(&self, provider: ProviderKind) -> Result<Option<CliUpdateAttempt>>;
    /// The highest `seq` of a finished row; 0 when none.
    pub async fn cli_update_last_seq(&self) -> Result<u64>;
    /// Marks every `running` row `interrupted` (ended now) and returns them as they were.
    pub async fn cli_update_interrupt_running(&self) -> Result<Vec<CliUpdateAttempt>>;
    /// Keeps the newest `keep` rows (500 in production); returns the count deleted.
    pub async fn cli_update_prune(&self, keep: u32) -> Result<usize>;
}
```

`cli_update_prune(500)` runs in the existing history prune task (`serve.rs:406`).

### 3.12 The board

**Station card** (`web/src/board/StationCliUpdate.tsx`, new; one line in `StationsPanel.tsx` after the providers list, `:271-320`). Shown only when `info.cli_update` exists. Under the provider list, one line per provider of `cli_update.providers`, then the schedule line:

| State | Text | Tone |
| --- | --- | --- |
| `running` is this provider's last attempt | `Claude: updating to 2.1.310…` | normal |
| last `updated`, trigger `schedule`/`owner`/`recovery` | `Claude: updated 2.1.308 → 2.1.310 · 3 h ago` | quiet |
| last `updated`, trigger `external` | `Claude: changed outside Fleet Deck, 2.1.308 → 2.1.309 · 3 h ago` | quiet |
| last `up_to_date` | `Claude: up to date · checked 3 h ago` | quiet |
| last `skipped_busy` | `Claude: not updated, sessions ran · 3 h ago` | quiet |
| last `refused` | `Claude: not updated, {reason text} · 3 h ago` | quiet |
| last `report_only` or `blocked == report_only_method` | `Claude: update by hand ({method text})` | quiet |
| last `failed` | `Claude: update failed, kept 2.1.309 · 3 h ago` | hazard |
| last `rolled_back` | `Claude: 2.1.310 failed the check, back on 2.1.309 · 3 h ago` | hazard |
| last `rollback_failed` | `Claude: update and way back failed. Reinstall Claude on this station.` | hazard |
| last `interrupted` | `Claude: update interrupted · 3 h ago` | hazard |
| `available` set and nothing runs | append ` · 2.1.310 available` | — |
| schedule line | `Updates: off` or `Updates: nightly 03:00-05:00 local` | quiet |

- Method texts: native "native installer", npm "npm", bun "Bun", homebrew "Homebrew", system_package "a system package", unknown "an unknown method".
- Reason texts: `gpgv_missing` "install GnuPG (gpgv) to verify Claude updates"; `tool_missing` "the package manager was not found"; `disk_full` "less than 1 GB free"; `channel_unreachable` "the vendor did not answer"; `signature_invalid` "the release signature did not verify"; `signature_unknown_key` "the release has an unknown signing key"; `provenance_missing` "the package has no provenance"; `known_broken` "this version is known to break Fleet Deck"; `below_tested` "this version is older than Fleet Deck supports"; `below_handoff` "this version cannot run fleet tools"; `too_new` "the new version is still in its wait time"; `publish_time_unknown` "the release time is unknown"; `station_restarting` "the station was restarting"; `starts_in_flight` "sessions were starting"; `sessions_running` "sessions ran"; `another_update` "another update ran"; `second_install` "a second install, update it by hand"; `not_found` "the program was not found"; `not_a_version` "the vendor gave no release version"; others "see the station". The age uses `agoText` (as `Last seen`).
- For the owner (`actions.canAdmin`), the line ends with a link `Updates…` to Settings → the station tab (`formatHash({name: 'settings', tab: 'station', station: station.station_id, …})`), like "Sign in…" (`StationsPanel.tsx:306-318`).
- No new token or primitive: quiet text uses `fd-note`; the hazard tone uses the existing hazard attribute of the provider line. The worker applies the `fleet-design-system` skill.

**Settings** (Settings → station tab):

- `model.ts`: a section `{ title: 'Provider updates', keys: ['provider_updates.enabled', 'provider_updates.window_start', 'provider_updates.window_hours', 'provider_updates.timezone', 'provider_updates.claude_channel', 'provider_updates.claude_pin', 'provider_updates.codex_pin', 'provider_updates.min_age_hours'] }` after "Providers".
- `CliUpdateCard` (`web/src/settings/CliUpdateCard.tsx`, new) in the slot `'Provider updates': { after: card }` (`SettingsTab.tsx`), only for a live station with `cli_update.v1` (the GET answers; a 501 shows "This station cannot update its providers yet. Update the station first.").
- Card title: `Update now`. One row per provider: `Claude 2.1.309 · native installer · stable channel` (Codex: `· latest channel`; a pin: `· pinned to 2.1.305`), the last-attempt text of the card table, and two buttons `Check` and `Update now…`. A provider that is not `updatable` shows the blocked text and no `Update now…`.
- `Check`: POST with `check_only: true` (no elevation). Result line: `2.1.310 is available.` / `Up to date.` / `Not available: {reason text}.`
- `Update now…`: asks the elevation (`ask(['update'])`), then opens the dialog:
  - Title: `Update Claude on {station}?` / `Update Codex on {station}?`
  - Claude text: `The station installs Claude {target} from Anthropic ({channel} channel), checks the signed release and checks that Fleet Deck can drive it. If a check fails, it goes back to {installed}. New sessions wait until the update ends, about a minute.`
  - Codex text: `The station installs Codex {target} from the npm registry with {npm|Bun}, checks the registry signature and checks that Fleet Deck can drive it. If a check fails, it installs {installed} again. New sessions wait until the update ends, about a minute.`
  - Buttons `Update` and `Cancel`. `{target}` is `available` of the last check, or "the newest version" when no check ran.
- Busy answer (`refused == sessions_running`): native: `{n} Claude sessions run on this station. They keep their version; new sessions use the new one.` with a check box `Update while they run` (sends `force: true`). Npm and Bun: `{n} Codex sessions run on this station. An update replaces the files in place: stop these sessions first.` and no check box.
- Running: `Updating Claude to {target}…`; the card polls the GET every 3 s until `running` is `null`, then shows the last-attempt text.
- Other refusals: `Not updated: {reason text}.`

### 3.13 Compatibility

1. **Older station, newer deck:** no `cli_update.v1` → no `cli_update` in `station.info`, no card line, the Settings card shows the 501 text, no audit import.
2. **Newer station, older deck:** the station runs the schedule from its own `station.toml` and keeps the attempts (pruned at 500). A newer deck imports them later. An older deck ignores the unknown `StationInfo` field (serde default, `#[serde(other)]` in every enum).
3. **Settings:** every key of `[provider_updates]` is optional; an older `station.toml` parses unchanged. An older board shows the new keys under "Other" (the section model puts unknown keys there, `model.ts:141`).
4. **Behavior after the upgrade with defaults:** no change: updates are off, no `DISABLE_AUTOUPDATER`, Claude's own updater keeps running as today. New at once: the station sees version changes (§3.9) and refreshes its report; the audit log records them.
5. **Migrations:** station V49 and deck V53 (reserved numbers; the merge renumbers). Both only create a table; to reverse, drop the table.
6. **The restart freeze:** the hold reuses `hold_starts`; a restart and an update exclude each other (§3.7.1).

### 3.14 Errors and edge cases

| Case | Result |
| --- | --- |
| The deck is offline at night | The station updates; the audit import runs at the next link. |
| The vendor channel is down | The row is `Failed` with `channel_unreachable`; nothing changed; the next window tries again. |
| Claude's own updater changes the version in a terminal of the owner | §3.9 records `external`; the gate is not applied to it; a `KNOWN_BROKEN` result is marked broken by the refresh (§3.8). |
| The owner pins an older Claude | Native `claude install <pin>` installs it; `minimumVersion` in the owner's Claude settings can refuse it → `Failed (install_failed)`, nothing else changed. |
| Two instances of Claude with one binary | One install; the busy check counts the sessions of both. |
| Two instances with two binaries | The second reports `second_install`. |
| A pre-release on `latest` | `not_a_version`, refused. |
| A failed install that left npm half done | The way back reinstalls `from_version`; if that fails too, `RollbackFailed` and starts are refused with the reinstall hint. |
| The station stops during the install | Recovery at the next start (§3.7). |
| A session starts while the update runs | It waits in the queue (`hold_starts`) and starts after the update. |
| A model help call runs during the update | It can fail once (npm, Bun) and is not retried; native is not affected. Accepted. |
| `date +%z` fails | The tick is skipped; the window is not missed for more than 15 min. |
| No `gpgv` on a native Claude station | `blocked = gpgv_missing`, never updated; the card says so. |
| The npm registry rotates its key | `signature_unknown_key` until a Fleet Deck release pins the new key. |
| A board terminal runs `claude` | Not a session: not counted. Native keeps its version file; an npm update can break it. |

### 3.15 Out of scope

- Homebrew, apt/dnf/apk, pacman, WinGet, pnpm, Vite+ and the Codex standalone installer updates (report only).
- A deny list that the deck pushes to older stations (the deny list stays in the station build).
- Full Sigstore verification of npm provenance (the presence is checked).
- A Fleet Deck self-update (the station upgrade stays manual).
- Writing to the owner's `~/.claude/settings.json` (owner question 7).

## 4. Errors and limits (summary)

- Time limits: a fetch 20 s; gpgv 20 s; an install 10 min; a probe step 30 s; a whole attempt 15 min; the hold settle 10 s.
- Sizes: a pointer 64 bytes; a manifest 256 KiB; a signature 16 KiB; dist-tags 64 KiB; a version document 1 MiB; a log file 256 KiB, 20 files.
- Rows: 500 attempts per station.

## 5. Decisions made in this draft

1. **The vendor's own commands only, with an exact version.** Native Claude: `claude install <version>` with the installed (verified) binary. Npm, Bun: the install command of `codex update`, but with `<package>@<version>`, so the gate decides the version before anything changes. Rejected: `claude update` / `codex update` (they pick the version themselves, after the gate); downloading the native binary ourselves (we would re-implement `bootstrap.sh`); `curl | sh` of the Codex standalone installer.
2. **Detection from the launcher that the station runs**, never from the tools on PATH (§1.6 item 2).
3. **Report only** for Homebrew, system packages, the Codex standalone installer and unknown layouts.
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

## 6. Threat notes and risk class

| # | Threat | Mitigation |
| --- | --- | --- |
| T1 | A compromised vendor channel or registry ships a malicious version, and many stations install it at night without a human. | Vendor signature (Claude GPG key pinned in the binary; npm registry key pinned); Codex provenance required; 48 h soak; Claude `stable` channel by default; off by default; jitter; every attempt audited. Residual: a release that the vendor signs and that passes the soak runs as the owner — the same trust as `claude update` by hand. |
| T2 | A man in the middle changes the channel pointer or a document. | HTTPS only (`proto =https`, no redirects); the pointer only names a version; the signed manifest or the registry signature decides what is accepted. |
| T3 | Argument injection through a version string or a path. | Every version in an argv passes `is_release` (digits and dots only). Paths come from the station's own file system (detection), never from the wire. No shell. |
| T4 | The board or a stolen owner session triggers updates or pins an old version. | Owner only, with an elevation for an update; audited. A pin cannot go below `TESTED_MIN_VERSION`, the deny list or the handoff minimum. A pin is a `danger` setting (its own elevation). |
| T5 | npm provenance is only checked for presence. | Documented residual; the registry signature binds the tarball integrity. A later change can add Sigstore verification. |
| T6 | An update breaks a running session (in-place files, a changed protocol). | No update while sessions of the provider run (force only for native, where versions sit side by side); starts are held; the handshake probe before release; the way back. |
| T7 | A failed update leaves no working CLI. | Native: the previous file stays and the link is swapped back after a checksum check. Npm, Bun: a signed reinstall of the previous version. `RollbackFailed` refuses starts with a reinstall hint instead of failing in a session. |
| T8 | Secrets in output (an `.npmrc` token in an npm error). | Output goes only to a 0600 log file on the station; the wire, the database and the station log hold fixed codes. |
| T9 | The station updates the wrong copy (the shadowed npm Codex on Lighting). | Detection from the launcher; the tool comes from the launcher's own prefix. |
| T10 | Disk fills (291 MB per Claude version). | 1 GiB free space check; the native installer prunes old versions. |
| T11 | `DISABLE_AUTOUPDATER` hides security fixes. | Only when the owner turns the station updater on; the nightly update replaces it. |

**Risk class: high.** The change runs code from an external source as the owner on a timer, adds a supply-chain surface, changes the start admission (the restart freeze), and adds an owner action with an elevation. Adversary DESIGN REVIEW before the build; PRE-PR REVIEW of the final branch; `SECURITY-CHECKLIST.md` gets a row (T8 of the plan).

## 7. Build plan

Workers reset their worktree to `v2` first. Every heavy command goes through `umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh <command>`. Workers compile only (`cargo check -p <crate> --tests`; web: `bun run typecheck`, `bun run lint`); they write the tests and do not run them. Every file stays under 800 lines (`wc -l`). At most two workers run at the same time (machine rule).

Order: **wave 1** T1 ∥ T2; **wave 2** T3 ∥ T4; **wave 3** T6 ∥ T7; **wave 4** T5; **wave 5** T8. (T6 and T7 need only T1; T5 needs T2, T3 and T4.)

### T1 (backend): wire types, feature, settings (≈ 560 lines)

- **Owned files:**
  - `crates/fleetdeck-proto/src/cli_update.rs` (new), `crates/fleetdeck-proto/src/cli_update_tests.rs` (new), `crates/fleetdeck-proto/src/lib.rs` (`pub mod cli_update;`)
  - `crates/fleetdeck-proto/src/link.rs` (the `cli_update` field, and `cli_update: None` in its two literals)
  - `crates/fleetdeck-proto/src/version.rs` (feature, `ALL`, one test)
  - `crates/fleetdeck-link/src/methods_table_tail.rs` (three rows, `TAIL` 68 → 71)
  - `crates/fleetdeck-core/src/config_provider_updates.rs` (new), `crates/fleetdeck-core/src/config_provider_updates_tests.rs` (new), `crates/fleetdeck-core/src/config.rs`, `crates/fleetdeck-core/src/config_raw.rs`
  - `crates/fleetdeck-core/src/settings/fields_updates.rs` (new), `settings/fields.rs` (one `.chain`), `settings/mod.rs` (one `mod`), `settings/holder.rs` (the view values), `settings/tests/fields.rs` (the table list)
  - every file with a `StationInfo {` literal (add `cli_update: None,`): the 22 files of `grep -rln "StationInfo {" crates` on `d1fa4205`
  - `web/src/gen/**` (generated only, `make ts-types`)
- **Changes:** §3.2 (all types, the field, the feature, the rows); §3.10 (`ProviderUpdatesConfig`, `UpdateZone`, `ClaudeChannel`, `RawProviderUpdates` with `deny_unknown_fields`, `RAW_PROVIDER_UPDATES_KEYS`, `provider_updates` in `RAW_STATION_KEYS` and `RawStation`, `StationConfig.provider_updates`, the eight fields with their labels and helps, the view values: `window_start` as `HH:MM`, `timezone` as `local`/`UTC`/`±HH:MM`, pins as `""` when `None`). `is_release` lives in `fleetdeck-proto/src/cli_update.rs` as `pub fn is_release(&str) -> bool` (the gate and the config use it).
- **Tests:**
  - `cli_update_tests.rs`: each enum round-trips and an unknown string decodes to `Unknown`; `CliUpdateRun` refuses an unknown field and defaults `check_only`/`force` to false; `is_release` accepts `2.1.310`, `0.164.0` and refuses `2.1.310-beta`, `--help`, `2.1`, `1.2.3.4`, `1234567.0.0`, `""`.
  - `version.rs`: `the_cli_update_feature_has_its_name_follows_wall_apps_and_is_not_required`.
  - `config_provider_updates_tests.rs`: defaults (off, 03:00, 2, local, stable, no pins, 48); a full table; each bad value with its field in the error (window `24:00`, `3:00`, hours 0 and 13, timezone `+15:00` and `CET`, pin `2.1`, min age 337); an unknown key is refused.
  - `settings/tests/fields.rs`: the existing key-equality test passes with `("provider_updates", RAW_PROVIDER_UPDATES_KEYS)`.
- **Acceptance at the merge:** `cargo nextest run -p fleetdeck-proto -p fleetdeck-core -p fleetdeck-link`; `make ts-types` gives no diff.

### T2 (backend): the update probe (≈ 380 lines)

- **Owned files:** `crates/fleetdeck-providers/src/update_probe.rs` (new), `crates/fleetdeck-providers/src/update_probe_tests.rs` (new), `crates/fleetdeck-providers/src/lib.rs` (`pub mod update_probe;`).
- **Changes:** §3.6.1 exactly. Reuse `claude::args::{cli_args, Launch, parse_version, capabilities}`, `claude::control::initialize`, `codex::probe::parse_version`, `codex::app_server_args`, `codex::rpc`. Spawn with `env_clear().envs(env.iter())`, `kill_on_drop(true)`, stderr null.
- **Tests** (`update_probe_tests.rs`, fake CLIs as `#!/bin/sh` scripts in a temp dir, as other provider tests do):
  - `a_claude_that_answers_initialize_with_models_passes`;
  - `a_claude_error_answer_fails_with_error_answer`; `a_claude_answer_without_models_fails_with_no_models`; `a_silent_claude_fails_with_no_answer_within_the_limit` (limit 300 ms);
  - `a_missing_program_fails_with_no_start_and_no_version`;
  - `the_claude_probe_passes_the_adapter_launch_arguments` (the script writes its argv to a file; the test compares with `cli_args(&Launch::default(), &capabilities(false))`);
  - `a_codex_that_answers_initialize_and_account_read_passes`; `a_codex_error_on_initialize_fails`.
- **Acceptance:** `cargo nextest run -p fleetdeck-providers -E 'test(update_probe)'`.

### T3 (backend): station store, hold and provider refresh (≈ 560 lines)

- **Owned files:**
  - `crates/fleetdeck-station/src/store/schema_v49.rs` (new), `store/cli_updates.rs` (new), `store/cli_updates_tests.rs` (new), `store/migrations.rs` (one `M::up`), `store/mod.rs` (two `mod` lines)
  - `crates/fleetdeck-station/src/manager/restart_gate.rs`, `manager/provider_update.rs` (new), `manager/provider_update_tests.rs` (new), `manager/manager_providers.rs`, `manager/target_base.rs`, `manager/target.rs` (the backstop, ≤ 8 lines), `manager/mod.rs` (the field type at `:126` and `:396`, `mod provider_update;`)
  - `crates/fleetdeck-station/src/coordinator/bindings_handoff.rs`
  - `crates/fleetdeck-station/src/settings_svc.rs` (the restart refusal, ≤ 10 lines)
- **Changes:** §3.11 (schema V49, store API, ordering and prune), §3.7.1, §3.8.
- **Tests:**
  - `cli_updates_tests.rs`: insert/end/last; `attempts` stops before a running row and pages by `after_seq`; `last_seq` ignores running rows; `interrupt_running` marks and returns; `prune(2)` keeps the two newest; the CHECK refuses `provider = 'pi'`.
  - `provider_update_tests.rs`: `begin_update_fails_while_restarting`; `a_restart_admission_after_begin_update_is_refused_and_clears_restarting`; `a_queued_start_is_held_during_an_update_and_runs_after_the_hold_drops`; `live_sessions_of_lists_only_the_kind`; `a_deferred_start_of_the_updating_kind_fails_with_provider_updating` (via the backstop); `begin_provider_update_reports_starts_in_flight_after_the_settle_bound` (bound 50 ms).
  - `manager_providers.rs` tests: `refresh_providers_replaces_the_cache_and_clears_a_broken_mark`; `refresh_providers_replaces_the_handoff_versions`.
- **Acceptance:** `cargo nextest run -p fleetdeck-station -E 'test(cli_update) | test(provider_update) | test(refresh_providers) | test(restart)'` plus the module-size and layering guards.

### T4 (backend): detection, gate and verification (≈ 600 lines)

- **Owned files:** `crates/fleetdeck-station/src/cli_update/mod.rs` (new: module declarations and re-exports only), `cli_update/detect.rs`, `cli_update/gate.rs`, `cli_update/verify.rs`, `cli_update/channel.rs`, `cli_update/keys/claude-code.gpg` (new: `gpg --dearmor` of `/home/luismorales/fleetdeck-briefs/backlog2/cli-update/claude-code.asc`, fingerprint checked), `cli_update/detect_tests.rs`, `cli_update/gate_tests.rs`, `cli_update/verify_tests.rs`, `cli_update/channel_tests.rs`, `cli_update/testdata/*` (new), `crates/fleetdeck-station/src/lib.rs` (`pub mod cli_update;`).
- **Changes:** §3.3, §3.4, §3.5. `channel.rs` builds its curl configs with `crate::integrations::http::config` / `exec::run_for` (no new HTTP client). The RFC 3339 and RFC 1123 date parsers are small pure functions in `channel.rs`.
- **Tests:**
  - `detect_tests.rs` (temp trees): the Lighting layouts (native link; Bun global; a shadowed npm copy that is not chosen); npm prefix with and without `bin/npm`; a configured absolute version file (report only); Homebrew, `/usr/bin`, unknown; `gpgv` present and missing.
  - `gate_tests.rs`: every row of §3.4 in order, with `KNOWN_BROKEN` simulated through `classify`-level inputs where the constants are empty (use a target below `TESTED_MIN_VERSION` for `below_tested`, below `VERIFIED` for `below_handoff`, a `shim_host` case below `BRIDGE_VERIFIED` for Codex 0.162.0).
  - `verify_tests.rs`: the recorded `@openai/codex@0.163.0` document (testdata) verifies; one changed byte in `integrity` fails `signature_invalid`; an unknown keyid fails `signature_unknown_key`; a bad SPKI prefix fails; provenance missing fails for Codex and passes for Claude; platform packages are selected for linux-x64 (glibc and musl) only; the manifest checksum selection; `VALIDSIG` parsing of the recorded status lines (pass, other key, no line). A gpgv run test that is skipped when `gpgv` is absent: the recorded 2.1.309 manifest and signature pass; a changed manifest fails.
  - `channel_tests.rs`: pointer text trimming and refusal of a pre-release; dist-tags parsing; RFC 3339 and RFC 1123 parsing; the curl config holds `proto = "=https"`, `no-location` and no user value.
- **Acceptance:** `cargo nextest run -p fleetdeck-station -E 'test(cli_update)'`.

### T5 (backend): attempts, schedule and wiring (≈ 600 lines)

- **Owned files:** `crates/fleetdeck-station/src/cli_update/install.rs`, `cli_update/run.rs`, `cli_update/schedule.rs`, `cli_update/service.rs` (new: `CliUpdates`, `call` for the three link methods, `report()`), their `_tests.rs` files, `cli_update/mod.rs` (add the modules), `crates/fleetdeck-station/src/serve.rs` (start the service, recovery, the schedule task, the prune call, `assist_instances` env), `crates/fleetdeck-station/src/link/intake.rs` (one dispatch arm, the `cli_update` value in `station.info`; ≤ 8 lines, stays under 800), `crates/fleetdeck-station/src/manager/adapters.rs` (`DISABLE_AUTOUPDATER`), `crates/fleetdeck-station/src/manager/mod.rs` (the service holder: one field, one init line), `manager/provider_update.rs` (`set_cli_update` / `cli_update` / `cli_update_report`).
- **Changes:** §3.6, §3.6.2, §3.7 (with recovery), §3.9, §3.10 (env), the link methods (`cli_update.status` → `CliUpdateReport`; `cli_update.run` → `CliUpdateRunAnswer`, `invalid_params` for bad params, `unsupported` without a service; `cli_update.attempts` with `limit` clamped to 1-100).
- **Tests** (fakes: a fake `Installer` trait for the commands and a fake `Verifier`/`Channel` for §3.5, so no network and no real CLI):
  - `run_tests.rs`: updated; up to date writes a row; refused gate writes a row; busy owner attempt writes no row and returns `busy`; force on native proceeds with live sessions; force on npm is still busy; install fails with no change → `Failed`; probe fails → native link swap → `RolledBack`; npm probe fails → reinstall of `from` → `RolledBack`; rollback fails → `RollbackFailed` and starts refused with the hint; timeout kills and rolls back; `check_only` writes no row and sets `available`; a second concurrent attempt → `another_update`; recovery after an interrupted native attempt swaps back.
  - `schedule_tests.rs`: the window with a fixed offset, across midnight, the jitter bound; one scheduled attempt per window; a busy window writes one `SkippedBusy` row after the window; an external version change writes one `external` row and refreshes; off → no attempt but external changes are still recorded.
  - `install_tests.rs`: the exact argv of each method (Codex npm with `--ignore-scripts`, Claude npm without); the npm child `PATH` starts with `<prefix>/bin`; the env has `DISABLE_AUTOUPDATER=1`; the log file is 0600 and capped; the link swap is atomic (rename) and refuses a checksum mismatch.
  - `adapters.rs` test: a Claude adapter env has `DISABLE_AUTOUPDATER=1` only when `provider_updates.enabled`; an instance `env` value wins.
- **Acceptance:** `cargo nextest run -p fleetdeck-station -E 'test(cli_update) | test(adapters)'`; `cargo check -p fleetdeck-station --tests`.

### T6 (backend): deck routes and the audit import (≈ 520 lines)

- **Owned files:** `crates/fleetdeck-deck/src/http/cli_update_routes.rs` (new), `http/cli_update_routes_tests.rs` (new), `http/mod.rs` (two routes, one `mod`), `http/methods_table.rs` (two rows), `crates/fleetdeck-deck/src/store/cli_update_audit.rs` (new), `store/cli_update_audit_tests.rs` (new), `store/cli_update_cursor_schema.rs` (new), `store/schema.rs`, `store/mod.rs` (two `mod` lines), `crates/fleetdeck-deck/src/link/cli_update_audit.rs` (new), `link/info.rs` (the trigger, ≤ 10 lines), `link/mod.rs` (one `mod`), `crates/fleetdeck-deck/tests/cli_update.rs` (new).
- **Changes:** §3.11. `schema.rs` stays at 799 lines: merge the two comment lines `// V46 (MCP P4b): …` and `// V47 (Fleet Sync FS-3): secret_files.` into one line `// V46 (MCP P4b): session_owner_grant, required_classes; V47 (Fleet Sync FS-3): secret_files.`, and change `// V48 to V50: …` to end with `; V53 (B28): the audit cursor of provider updates.` before the new `M::up(super::cli_update_cursor_schema::SQL),`.
- **Tests:**
  - `cli_update_audit_tests.rs`: import writes one audit row per new attempt with the actor, action, target, result and detail of §3.11; a second import of the same rows writes nothing; a new incarnation restarts at 0; the cursor and the rows commit together (a failing insert leaves the cursor); a version that fails `is_release` shows `?`.
  - `cli_update_routes_tests.rs`: owner only (403 for a token without the board login); `elevation_required` for an update and not for a check; 501 without the feature; the POST audit row for ok and for refused; `unknown_station`, `station_unreachable`.
  - `tests/cli_update.rs` (deck with a fake station link): an `info` answer with `last_seq = 2` leads to a `cli_update.attempts` call and two audit rows.
- **Acceptance:** `cargo nextest run -p fleetdeck-deck -E 'test(cli_update)'`; the migration tests of the deck.

### T7 (web): the card line and the Settings card (≈ 580 lines)

- **Owned files:** `web/src/board/StationCliUpdate.tsx` (new), `web/src/board/stationCliUpdate.test.tsx` (new), `web/src/board/StationsPanel.tsx` (one element), `web/src/settings/CliUpdateCard.tsx` (new), `web/src/settings/cliUpdate.ts` (new: the texts, the calls, the poll), `web/src/settings/cliUpdate.test.tsx` (new), `web/src/settings/model.ts` (the section), `web/src/settings/SettingsTab.tsx` (the slot, ≤ 10 lines).
- **Changes:** §3.12 exactly, with the generated types of T1. The worker applies the `fleet-design-system` skill; no new token, class or primitive.
- **Tests:** each row of the card table (text and tone), the method and reason texts, an unknown reason; the `Updates…` link only for `canAdmin`; no line without `info.cli_update`. Settings: the section order; `Check` sends `check_only: true` without an elevation; `Update now…` asks the elevation, shows the Claude and Codex dialog texts; the busy texts with and without the check box (`force` sent only from the check box); the 501 text; the poll stops when `running` is `null`.
- **Acceptance:** `bun run typecheck`, `bun run lint`, `bun test web/src/board/stationCliUpdate.test.tsx web/src/settings/cliUpdate.test.tsx` (through `heavy.sh`).

### T8 (docs): operations and the security checklist (≈ 150 lines)

- **Owned files:** `docs/v2/operations.md` (a section "Provider updates" after the station upgrade section), `docs/v2/SECURITY-CHECKLIST.md` (one row), `docs/v2/DECISIONS-LOG.md` (the entries below), `docs/v2/BACKLOG.md` (B28 status).
- **Changes:** how to turn updates on (Settings path), the window, channels and pins, what "report only" means and how to update by hand per method, the log files, the reinstall hints, `gpgv` on macOS (`brew install gnupg`).
- **Acceptance:** `make check` (docs lint).

### At the merge (coordinator)

1. The tests of every task, the module-size and layering guards, `make check`.
2. The adversary PRE-PR REVIEW of the final branch.
3. Live, read only first: on every station, Settings → station → Provider updates → `Check` for Claude and Codex. Record the method, `updatable`, `blocked` and the target per station in the merge note (this replaces the research that the refused grant stopped; see "Research limit" at the top).
4. Live, one write: with the owner present, `Update now…` for Codex on Lighting (Bun) and for Claude on one native station; then check the audit log, the card line and a new session.

## 8. Owner questions (the build proceeds with the defaults)

1. **Nightly updates on or off by default?** Default: **off**; when the owner turns them on, the window is 03:00 to 05:00 station local time. Reason: the update changes who updates Claude (§1.7), so it is the owner's choice per station.
2. **Claude channel?** Default: **stable** (about one week old). `latest` gets fixes sooner and also regressions sooner.
3. **Wait time for a new version?** Default: **48 hours** (`min_age_hours`), for Claude and Codex.
4. **Update while sessions run?** Default: **only for native Claude, after the owner ticks "Update while they run"**. Never for npm and Bun (files change in place). Never on the schedule.
5. **No `gpgv`, no Claude update?** Default: **yes, fail closed** (the card says "install GnuPG (gpgv)"). The other option is a checksum-only check, which trusts the same host for the file and its checksum.
6. **Homebrew?** Default: **report only**. The other option runs `brew upgrade --cask claude-code` / `codex`, where the station cannot choose or check the version before the change.
7. **Also stop Claude's own updater in the owner's terminals?** Default: **no**: only the Claude processes of the station get `DISABLE_AUTOUPDATER=1`; the station records the other changes as "changed outside Fleet Deck".
8. **An upper bound ("tested range" maximum)?** Default: **no upper bound**; the deny list and the tested minimum of the station build stay the fixed bounds. An upper bound in the build would stop every update until the next Fleet Deck release.

## Decision log entries (to append)

## DL-550: B28 the station updates Claude and Codex only with the vendor's commands, at an exact version (design agent, 2026-10-24)

- Context: B28; design `docs/v2/design/cli-update.md` §3.3, §3.6. Today nothing in Fleet Deck updates a provider CLI, but native Claude updates itself from inside station sessions.
- Decision: the station detects the install from the launcher that it runs. Native Claude: `<installed claude> install <version>`. Npm and Bun globals: the install command of `codex update` (the tool from the launcher's own prefix), with `<package>@<version>`, the npm registry forced, and `--ignore-scripts` for Codex. Homebrew, system packages, the Codex standalone installer and unknown layouts are report only. No shell; every version in an argv is digits and dots.
- Rejected: `claude update` / `codex update` (the version is chosen after the gate); our own download of the native binary; `curl | sh`.

## DL-551: B28 vendor signatures are checked and fail closed (design agent, 2026-10-24)

- Decision: Claude: the release `manifest.json` is verified with `gpgv`, a keyring pinned in the station binary, and the `VALIDSIG` fingerprint `31DDDE24DDFAB679F42D7BD2BAA929FF1A7ECACE`; the installed file's SHA-256 must be a manifest checksum of the station's OS and architecture. Npm: at least one registry ECDSA P-256 signature with a pinned key over `<name>@<version>:<integrity>`, for the package and its platform packages; Codex needs an SLSA provenance attestation (presence). No `gpgv`: no native Claude update. An unknown key: refused until a release pins it.
- Rejected: a new OpenPGP crate; a checksum-only check.

## DL-552: B28 the gate and the way back (design agent, 2026-10-24)

- Decision: a target must be a release version, not `KNOWN_BROKEN`, not below `TESTED_MIN_VERSION`, not below the handoff minimum (and the bridge minimum on a shim host), newer than the installed version unless pinned, and at least `min_age_hours` old (default 48) unless pinned. After the install: the version, the checksum (native) or `package.json` (npm, Bun), and a handshake with the adapter's launch arguments (Claude `initialize` with models; Codex `initialize` and `account/read`). A failure goes back: native by an atomic link swap to the kept previous file after a checksum check; npm and Bun by a signed reinstall of the previous version. A failed way back marks the instances broken with a reinstall hint.

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

- Decision, until the owner answers: updates off by default; Claude `stable`; 48 h wait; force only for native Claude; no `gpgv` means no native Claude update; Homebrew report only; the owner's own Claude keeps its updater; no upper version bound.
