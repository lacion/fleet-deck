# Security options that the owner can change: a default agent mode, grant limits with "no limit", bypass for workers, and the relaxed-security posture (B27)

- **Status:** Draft for the architect, revision 2 (2026-10-24): the adversary DESIGN REVIEW findings are fixed (see "Revision 2 (design review)" near the end). Risk: **high** (§4: authorization of agent-started sessions, grant limits, a migration of the grant table). The adversary does a DESIGN REVIEW before the design is frozen and a PRE-PR REVIEW before the merge.
- **Backlog:** `docs/v2/BACKLOG.md` item B27 (lines 343-357).
- **Owner request (2026-10-22):** the secure configuration stays the default. The owner can turn each security control off, and can set a default mode for the agents. Examples: a default agent mode for every spawn; grants with no limits (no spawn cap, no idle limit, no answer cap); the bypass-permissions mode for workers. Each change gets a clear warning.
- **Code base:** branch `v2`, HEAD `d1fa4205`. All file:line references are on this commit. (The backlog cites `7333db5d`; every backlog line number was checked again on `d1fa4205`.)
- **Evidence:** the code only. No live session was started and no live database was read. The file sizes of the files that the build touches are in `/home/luismorales/fleetdeck-briefs/backlog2/security-options/evidence-file-sizes.txt`.

## 1. Verified facts

### 1.1 The mode of a start without a mode

1. `SessionSpec.mode: Option<String>` (`crates/fleetdeck-proto/src/command.rs:37-39`): "a mode that the provider does not offer runs as the most restrictive mode (L8)". This is true for Claude only: the Codex adapter refuses an unoffered mode with `ProviderError::InvalidMode` before any process (`crates/fleetdeck-providers/src/codex/driver.rs:196-203`, `codex/durable_launch.rs:35-39`; revision 2).
2. The board spawn form offers "Default mode" with the value `""` (`web/src/spawn/AgentFields.tsx:89`) and sends `mode: null` for it (`web/src/spawn/SpawnForm.tsx:408`).
3. Claude: `Capabilities::effective_mode(None)` is the most restrictive native mode (`crates/fleetdeck-proto/src/capability.rs:161-166`), `default` (`crates/fleetdeck-providers/src/claude/args.rs:27-29`). The station always passes `--permission-mode` (`args.rs:103-107`, DL-004).
4. Codex: `Mode::or_safest(None)` is `DEFAULT_MODE = "untrusted/workspace-write"` (`crates/fleetdeck-providers/src/codex/mod.rs:103`, `:136-140`).
5. A start from issue or pull request text (`facts.origin_text`) is limited to `default`/`plan` (Claude) or `untrusted/*` without full access (Codex), and gets the safest mode when it gives none (`crates/fleetdeck-station/src/manager/decide.rs:386-421`, applied at `:446-448`).
6. A coordinator start (MCP `spawn`, `start_in_project`) with no mode is checked on the deck as the provider default: `mode_within(family, None, ceiling)` reads `None` as `default` or `untrusted/workspace-write` (`crates/fleetdeck-deck/src/store/coordinator_ops.rs:132-150`). `start_in_project` builds its spec with `mode: None` (`store/coordinator_fleet.rs:504`) and calls the same `start` (`coordinator_ops.rs:679`).
7. No setting gives a default mode today. There is no per-project mode either.

### 1.2 The bypass mode

1. `allow_bypass_permissions` (station, default `false`, `crates/fleetdeck-core/src/config.rs:261-262`, `:485`) is a `Restart` field with the elevation (`crates/fleetdeck-core/src/settings/fields.rs:103-110`; danger rule `settings/holder.rs:237-240`). The adapters offer `bypassPermissions` (Claude) and every `*/danger-full-access` mode (Codex) only when it is on (`claude/args.rs:48-52`, `codex/mod.rs:159-167`, set at `crates/fleetdeck-station/src/manager/adapters.rs:38`, `:46`).
2. The worker grant setting refuses a bypass ceiling: `CLAUDE_CEILINGS` and `CODEX_CEILINGS` leave out `bypassPermissions` and `never/danger-full-access` (`crates/fleetdeck-core/src/config_worker_grant.rs:29-41`, check at `:162-175`, test at `:333`).
3. Every coordinator grant (owner and worker) is refused a bypass mode on the deck, in three places:
   - a start: `coordinator_ops.rs:715-719` ("a coordinator never starts a session in a bypass mode");
   - a resume or a mode change: `mode_refusal` (`store/coordinator_fleet.rs:309-331`);
   - the dispatch check of the station (`coordinator.authority_check` with `LaunchMode`): `store/coordinator.rs:736-755` calls `mode_refusal` and answers `mode_not_allowed`.
4. `valid_ceiling` refuses a bypass ceiling for every grant (`coordinator_ops.rs:154-158`); `check_worker_grant` uses it (`store/worker_grant.rs:252-258`), and so does a live cap change (`store/grant_spawn_caps.rs:47-49`).
5. The station refuses a resume or a mode change into a bypass mode for every command with authority (`crates/fleetdeck-station/src/manager/fleet_control.rs:40`, `:52-62`, `:89-101`). It does not check a `Start` there (a start has no target session, `:80-82`); the deck check of item 3 covers it.
6. A worker grant has only the classes read, mail, drive and spawn (`store/worker_grant.rs:229-262`), so it can never resume a session or change a mode. Only the start path matters for workers.
7. The fleet skill tells every session: "never start a session in a bypass mode" (`crates/fleetdeck-station/src/fleet_skill/SKILL.md:20`).
8. A coordinator start on a station needs `allow_fleet_control` on that station (`fleet_control.rs:77-79`).

### 1.3 The grant limits

1. Constants of every grant (`crates/fleetdeck-deck/src/store/grants.rs:38-54`): `MAX_SPAWN_CAP = 50`, `MAX_IDLE_TTL_MS = 24 h`, `MAX_TTL_MS = 7 days`, `MAX_ANSWER_CAP = 5000`; the defaults `DEFAULT_SPAWN_CAP = 3`, `DEFAULT_IDLE_TTL_MS = 30 min`, `DEFAULT_TTL_MS = 12 h`, `DEFAULT_ANSWER_CAP = 500`.
2. Constants of worker grants (`store/worker_grant.rs:45`, `:49`): `WORKER_GRANT_CAP = 200` (worker grants in effect, deck-wide; checked at `:589` and at a renewal, `store/worker_grant_end.rs:73-75`), `MAX_WORKER_SPAWN_CAP = 10` (`worker_grant.rs:247-249`, `grant_spawn_caps.rs:38-40`).
3. The checks of the constants: `prepare_grant` (`worker_grant.rs:163-187`, one text for all limits), the board request (`http/grant_spawn.rs:35-36`, `http/grant_check.rs:124-125`, `http/grant_owner_start.rs:192-193`), the owner grant change (`store/owner_grants.rs:396-399`), the live cap change (`grant_spawn_caps.rs:35-40`). Several return `&'static str`.
4. The worker grant setting `[worker_grant]` has `spawn_cap` 0-10 (default 2), `idle_minutes` 5-1440 (default 30), `lifetime_hours` 1-168 (default 12) (`config_worker_grant.rs:60-72`, `:176-189`). A worker grant has no answer class, so no answer cap.
5. **The database also holds the limits.** The `tokens` table has `CHECK (spawn_cap BETWEEN 0 AND 50)`, `CHECK (idle_ttl_ms BETWEEN 1 AND 86400000)` and `CHECK (answer_cap IS NULL OR answer_cap BETWEEN 0 AND 5000)` (latest rebuild V38, `store/coordinator_input_any_schema.rs:32`, `:35`, `:48`; the `kind` column came later with V43, `store/tokens_kind_schema.rs:7-10`). SQLite cannot change a CHECK, so a higher limit needs a table rebuild.
6. The board form of an owner grant checks `0..50` and `0..5000` itself (`web/src/spawn/fleetControl.ts:71`, `:79-84`, `:86`, `:159-164`) and always sends `idle_ttl_ms: 0` (the deck default of 30 min, `SpawnForm.tsx:419`). The board shows the caps as numbers (`web/src/fleet/SessionAccess.tsx:299`, `web/src/session/FleetAnswersDialog.tsx:43`).
7. Every idle check is `now.saturating_sub(idle_since) > idle_ttl_ms` or the reverse (`store/grant_scopes.rs:46-52`, `:116`; `coordinator.rs:261`; `coordinator_query.rs:145`; `coordinator_desktop/admit.rs:122`; `coordinator_desktop/list.rs:55`), and a grant is in effect only while `now <= expires_at` (`grant_scopes.rs:48`). The lifetime is never extended (only shortened, `coordinator_ops.rs:255`).
8. The station does not bound the caps of a `CoordinatorSpec`; it only needs `idle_ttl_ms > 0` (`crates/fleetdeck-station/src/manager/coordinator_command.rs:387-399`).

### 1.4 The `[security]` section

1. The comment "nothing reads them yet" (`crates/fleetdeck-core/src/config_deck.rs:72`, and the same for the station `[limits]`, `config.rs:318`) is **stale**. Knobs K2 (`3ef902e3`) wired every deck security knob: `DeckKnobs::of` (`crates/fleetdeck-deck/src/knobs.rs:73-93`) feeds the password check (`store/auth.rs:205`), the login lifetimes (`auth.rs:317-389`), the token lifetime (`auth.rs:524`), the elevation (`elevation.rs:7`), the display lifetimes, the invitation and both desktop rates. K3 wired the station limits (`StationTimeouts::from_limits`, `manager/policy.rs:195`).
2. The knobs design rules apply (`docs/v2/design/knobs.md` §1): bounds with a floor that is "the least safe value that Fleet Deck accepts at all" (rule 4), the elevation for a weaker value (rule 5), no silent clamp (rule 8).

### 1.5 The settings framework

1. One field table per file (`settings/fields.rs`, `settings/fields_knobs.rs`); `FieldMeta` has `kind`, `min`, `max`, `options`, `apply`, `danger` (`crates/fleetdeck-proto/src/settings.rs:55-78`). `FieldKind` has no "number or no limit" kind (`settings.rs:18-38`).
2. A save writes the TOML by kind (`settings/apply.rs:38-65`), parses the new text, computes `Holder::danger(old, new)` and needs the elevation for a non-empty list (`settings/holder.rs:37-38`, deck `:369-409`, station `:225-309`).
3. Every save is audited with its keys, never the values (`crates/fleetdeck-deck/src/http/settings_routes.rs:105-124`). The audit does not say which keys relaxed a control.
4. The board shows a hazard line under one field only: the bypass switch (`web/src/settings/SettingsFields.tsx:171-180`). The sections are explicit key lists (`web/src/settings/model.ts:9-120`).
5. The deck applies the deck settings live through `DeckLive` (`crates/fleetdeck-deck/src/live.rs:15-33`), whose `knobs: DeckKnobs` the store reads with `knobs()` (`store/knobs_store.rs:15-17`). The station applies its settings live through `Policy::reconfigured` and `policy.swap` (`crates/fleetdeck-station/src/manager/mod.rs:548-570`).

### 1.6 What the deck knows about a station

1. `StationProvider` (`crates/fleetdeck-proto/src/link.rs:318-337`) has the instance, the kind, the version, the sign-in state and the `capabilities` with the offered `modes`. It has no `deny_unknown_fields`, so an older deck ignores a new field.
2. The deck reads `station.info` again every 60 s (`crates/fleetdeck-deck/src/link/info.rs:17`) and gives it to a coordinator decision in `Context.infos` (`store/coordinator.rs:162-174`). The cards of the sessions (with `summary.mode`) are in `Context.sessions`; `session_family` reads the family and the mode of one session (`coordinator_fleet.rs:293-305`).
3. `FleetEnv` (`store/coordinator.rs:122-128`) is read in the turn of the decision (`store/mod.rs:325-340`).

### 1.7 File sizes (the 800-line guard, `crates/fleetdeck-cli/tests/module_size.rs:47-55`)

`store/worker_grant.rs` 798, `store/owner_grants.rs` 799, `store/schema.rs` 799, `store/coordinator_ops.rs` 788, `store/coordinator.rs` 785, `store/mod.rs` 780, `station manager/policy.rs` 785, `manager/decide.rs` 752, `proto link.rs` 751, `web/src/spawn/SpawnForm.tsx` 792. The build plan moves code out of the first four before it adds code (§5).

### 1.8 Corrections to the backlog

1. "`[security]` … nothing reads them yet": stale (§1.4). The sign-in lifetimes, the password length and the desktop rates are owner settings already. B27 only fixes the two comments.
2. The grant limits are not only code constants: the `tokens` CHECK constraints enforce them too (§1.3 item 5). A raise needs deck migration V52.
3. The bypass refusal is not only the worker ceiling list: the deck refuses a bypass mode for every coordinator start, resume, mode change and dispatch check (§1.2 item 3).
4. There are two more limits that the backlog does not name: `MAX_SPAWN_CAP = 50` (owner grants) and `MAX_TTL_MS = 7 days` (lifetime). B27 makes the first a setting and keeps the lifetime fixed (§3.2).

## 2. Answers to the questions of the backlog

**Q1. Which controls become options, and which stay fixed?**

| Control | B27 |
| --- | --- |
| Bypass mode for workers | Option `worker_grant.allow_bypass` (§3.4). |
| Grant caps and limits | Options `[grant_limits]` with "no limit" (§3.2). The worker grant values `worker_grant.spawn_cap` and `worker_grant.idle_minutes` also accept "no limit". |
| Default agent mode | Option `[default_mode]` per station (§3.5). |
| Sign-in lifetimes, password length, desktop rates | Options already (K1/K2). The floors stay fixed (knobs rule 4). |
| Grant lifetime (7 days, worker 168 h) | Stays fixed. "No idle limit" ends a grant at its lifetime, so the lifetime stays the hard cap of every grant. |
| Member file deny list (DL-154) | Stays fixed: it protects secrets from shared members, and the owner can only add patterns. |
| Approval step for tool calls | No new control. The agent mode is the approval step: the owner chooses it per start, per station default (§3.5), or per grant ceiling. `offline_approval_policy` exists already. |

**Q2. One switch per control, or one "trusted home network" profile?** One switch per control. A profile hides what it sets; the posture line of §3.6 gives the overview that a profile would give, and shows each value. (Owner question 3.)

**Q3. A default agent mode, per station or per project?** Per station: `[default_mode]` in `station.toml`, one mode per provider family. It is used when a start gives no mode, from every source except issue or pull request text. It is never more permissive than the station allows: a bypass default needs `allow_bypass_permissions` in the same file. A coordinator start gets the lower of the station default and the grant ceiling. Per project is not cheap (a deck migration, the project screens and the project start paths) and is out of scope (owner question 2).

**Q4. Each relaxation needs its own review.** This design gets one adversary DESIGN REVIEW that covers the bypass for workers, the "no limit" grants and the default mode, and one PRE-PR REVIEW. The approval step is not relaxed by B27. A later item that relaxes it gets its own review.

**Q5. Show the active posture on the board.** Yes: `SettingsView.relaxed` lists every control that is weaker than its secure default, in the file, in the running process (a restart field not yet applied) or from a command-line flag, and each settings tab shows it as a "Relaxed security" line (§3.6, §3.7). Every save that relaxes a control writes a second audit row `settings.relaxed`.

## 3. Design

### 3.1 Rules

1. **Secure by default.** With no new key in a file, the build changes no behavior. Every default is today's value.
2. **One switch per control.** Each relaxation is one key, saved through the settings framework, with the elevation when it relaxes (`danger`), a hazard text under the field while it is relaxed, and a line in the posture.
3. **No silent clamp** (knobs rule 8). A save that makes two values disagree is refused with a text that names the other key.
4. **"No limit" is explicit.** In a file it is the word `"none"`; on the board it is a "No limit" checkbox; in a grant row it is `NO_LIMIT` (§3.3).
5. **The lifetime stays the hard cap of every grant.** No B27 option extends `expires_at`.
6. **Limits apply at grant creation and at a cap change.** A lower limit does not change a grant that exists (owner question 4). The owner revokes a grant on the board.

### 3.2 Grant limits (`[grant_limits]` in `deck.toml`)

New core module `crates/fleetdeck-core/src/config_grant_limits.rs` (`pub mod config_grant_limits;` in `lib.rs`):

```rust
//! The `[grant_limits]` table of `deck.toml` (B27 §3.2): the ceilings of every
//! grant. The word "none" turns a ceiling off.

use serde::Deserialize;

use crate::config::ConfigError;

/// A ceiling that the owner can turn off: `Some(n)`, or `None` (no limit).
pub type Limit = Option<u32>;

/// The word of "no limit" in `deck.toml` and in the settings values.
pub const NO_LIMIT_WORD: &str = "none";

/// The `[grant_limits]` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrantLimits {
    /// The most worker grants in effect, deck-wide (was `WORKER_GRANT_CAP`).
    pub worker_grants: Limit,
    /// The highest spawn cap of an owner grant (was `MAX_SPAWN_CAP`).
    pub spawn_cap: Limit,
    /// The highest spawn cap of a worker grant (was `MAX_WORKER_SPAWN_CAP`).
    pub worker_spawn_cap: Limit,
    /// The longest idle time of a grant, in hours (was `MAX_IDLE_TTL_MS`).
    pub idle_hours: Limit,
    /// The highest answer cap of a grant (was `MAX_ANSWER_CAP`).
    pub answer_cap: Limit,
}

impl Default for GrantLimits {
    fn default() -> Self {
        Self {
            worker_grants: Some(200),
            spawn_cap: Some(50),
            worker_spawn_cap: Some(10),
            idle_hours: Some(24),
            answer_cap: Some(5000),
        }
    }
}

/// The bounds of a number (inclusive), by key.
pub const BOUNDS: &[(&str, u32, u32)] = &[
    ("worker_grants", 1, 100_000),
    ("spawn_cap", 1, 100_000),
    ("worker_spawn_cap", 1, 100_000),
    ("idle_hours", 1, 168),
    ("answer_cap", 1, 100_000),
];

/// A number or a word in the file.
#[derive(Deserialize)]
#[serde(untagged)]
pub(crate) enum RawLimit {
    Count(u32),
    Word(String),
}

impl RawLimit {
    /// `Some(n)` inside `min..=max`, `None` for `"none"`.
    ///
    /// # Errors
    /// `field`: "use a whole number from {min} to {max}, or \"none\"".
    pub(crate) fn validate(self, field: &str, (min, max): (u32, u32)) -> Result<Limit, ConfigError>;
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawGrantLimits {
    worker_grants: Option<RawLimit>,
    spawn_cap: Option<RawLimit>,
    worker_spawn_cap: Option<RawLimit>,
    idle_hours: Option<RawLimit>,
    answer_cap: Option<RawLimit>,
}

impl RawGrantLimits {
    pub(crate) fn validate(self) -> Result<GrantLimits, ConfigError>;
}

/// True when `new` is above `old`; `None` is above every number.
#[must_use]
pub fn raised(old: Limit, new: Limit) -> bool;

/// The keys (`grant_limits.<key>`) that `new` raises against `old`.
#[must_use]
pub fn raises(old: &GrantLimits, new: &GrantLimits) -> Vec<&'static str>;

/// `n <= limit`; every `n` is within `None`.
#[must_use]
pub fn within(n: u32, limit: Limit) -> bool;

/// The value of an omitted cap (revision 2): the built-in default, lowered
/// to the limit (`min(default, limit)`; `None` keeps the default). The
/// resolved value is then checked like a given value.
#[must_use]
pub fn default_within(default: u32, limit: Limit) -> u32;

/// The keys of `[grant_limits]` (a test compares them with the field table).
#[cfg(test)]
pub(crate) const RAW_GRANT_LIMITS_KEYS: &[&str] =
    &["worker_grants", "spawn_cap", "worker_spawn_cap", "idle_hours", "answer_cap"];
```

`DeckConfig` gets `pub grant_limits: GrantLimits` (`RawDeck.grant_limits: Option<RawGrantLimits>`, `RAW_DECK_KEYS` gets `"grant_limits"`). `DeckConfig::parse` validates `[grant_limits]` before `[worker_grant]` and gives the limits to `RawWorkerGrant::validate(self, &GrantLimits)`.

**`[worker_grant]` changes** (`config_worker_grant.rs`):

```rust
pub struct WorkerGrantConfig {
    pub enabled: bool,
    pub stations: Vec<String>,
    /// The most sessions that a worker can start; `None`: no limit (B27).
    pub spawn_cap: Limit,
    pub claude_mode: String,
    pub codex_mode: String,
    pub lifetime_hours: u32,
    /// The idle time; `None`: no idle limit, the grant ends at its lifetime (B27).
    pub idle_minutes: Limit,
    /// `allow_bypass`: the mode ceilings may be the bypass modes (B27 §3.4).
    pub allow_bypass: bool,
}
```

Defaults: `spawn_cap: Some(2)`, `idle_minutes: Some(30)`, `allow_bypass: false`; the rest unchanged. `RAW_WORKER_GRANT_KEYS` gets `"allow_bypass"`. The validation, in this order (each error is `ConfigError::Invalid` with the field `worker_grant.<key>`):

| Key | Rule | Message |
| --- | --- | --- |
| `spawn_cap` | a number `0..=L` where `L = grant_limits.worker_spawn_cap` (or `0..=100_000` when that is none). `grant_limits.spawn_cap` does not apply to a worker grant (revision 2). | `use a value from 0 to {L}, the grant limit of the deck` |
| `spawn_cap` | `"none"` only when `grant_limits.worker_spawn_cap` is none | `"none" needs grant_limits.worker_spawn_cap = "none"` |
| `idle_minutes` | a number `5..=L*60` where `L = grant_limits.idle_hours` (or `5..=10_080` when none) | `use a value from 5 to {L*60}, the grant limit of the deck` |
| `idle_minutes` | `"none"` only when `grant_limits.idle_hours` is none | `"none" needs grant_limits.idle_hours = "none"` |
| `claude_mode` | `CLAUDE_CEILINGS`, plus `bypassPermissions` when `allow_bypass` | `bypassPermissions needs worker_grant.allow_bypass = true` (for the bypass mode); the old text otherwise |
| `codex_mode` | `CODEX_CEILINGS`, plus `never/danger-full-access` when `allow_bypass` | `never/danger-full-access needs worker_grant.allow_bypass = true`; the old text otherwise |

`widens(old, new)` (the elevation rule) adds: `allow_bypass` false → true; `spawn_cap` and `idle_minutes` with `raised`. The bypass ceilings are already the top of the mode order, so `mode_within` reports them as wider.

**Field table** (new file `crates/fleetdeck-core/src/settings/fields_security.rs`, chained into `deck_fields()` and `station_fields()` like `fields_knobs.rs`):

| Key | Kind | Range | Apply | Danger | Label | Help |
| --- | --- | --- | --- | --- | --- | --- |
| `grant_limits.worker_grants` | Limit | 1-100000 | Live | yes | Most worker grants | The most worker grants in effect at the same time, on all stations. Default 200. No limit lets every session that you start get a worker grant. |
| `grant_limits.spawn_cap` | Limit | 1-100000 | Live | yes | Highest session limit of a grant | The highest session limit that you can give a grant on the board. Default 50. |
| `grant_limits.worker_spawn_cap` | Limit | 1-100000 | Live | yes | Highest session limit of a worker grant | The highest value of Worker grant session limit. Default 10. |
| `grant_limits.idle_hours` | Limit | 1-168 | Live | yes | Longest idle time of a grant (hours) | The longest time without use that a grant can have. Default 24. No limit lets a grant stay in effect without use until its lifetime ends. |
| `grant_limits.answer_cap` | Limit | 1-100000 | Live | yes | Highest answer limit of a grant | The highest number of approvals and questions that one grant can answer. Default 5000. |
| `worker_grant.allow_bypass` | Bool | | Live | yes | Allow bypass mode for workers | Lets Worker grant Claude mode and Worker grant Codex mode be the bypass mode. A worker can then start a session that runs every tool without asking, only on a station that allows bypass permissions and only while the worker itself runs in a bypass mode. |

The existing entries in `fields.rs:481-532` change: `worker_grant.spawn_cap` becomes kind `Limit`, range 0-100000, help "The most sessions that one worker can start. Default 2. No limit needs Highest session limit of a worker grant set to no limit."; `worker_grant.idle_minutes` becomes kind `Limit`, range 5-10080, help "A worker grant ends after this many minutes without use. Default 30. No limit: the grant ends at its lifetime or when its session ends."; `worker_grant.claude_mode` options get `bypassPermissions` (last); `worker_grant.codex_mode` options get `never/danger-full-access` (last). The parser refuses a bypass option while `allow_bypass` is off.

**`FieldKind::Limit`** (proto `settings.rs`): "A whole number in `min..=max`, or the text `none` (no limit, B27)." The value in a view is a JSON number or the string `"none"`. `apply.rs` writes a number in range as a TOML integer, `"none"` as the TOML string `"none"`, and refuses every other value with `SaveError::invalid(key)`.

**Values** (`holder.rs`): `grant_limits.<key>` and the two worker keys show a number or `"none"`; `worker_grant.allow_bypass` a bool.

**Danger** (`DeckHolder::danger`): `config_grant_limits::raises(old, new)` is appended (a raise or a change to none needs the elevation; a lower limit does not). The worker keys come from `widens`.

**Deck knobs** (`crates/fleetdeck-deck/src/knobs.rs`): `DeckKnobs` gets

```rust
    /// `[grant_limits]` (B27 §3.2), read at each grant creation and cap change.
    pub grant: fleetdeck_core::config_grant_limits::GrantLimits,
    /// `worker_grant.allow_bypass` (B27 §3.4), read at each decision and dispatch check.
    pub worker_bypass: bool,
```

with `GrantLimits::default()` and `false` in `Default`, and `config.grant_limits` and `config.worker_grant.allow_bypass` in `DeckKnobs::of`.

**The spawn limit of a grant is chosen by its kind** (revision 2). New function in `store/grant_rules.rs`:

```rust
/// The spawn ceiling of a grant of `kind`: `limits.spawn_cap` for an owner
/// (coordinator) grant, `limits.worker_spawn_cap` for a worker grant. The two
/// limits are independent: a worker grant is never checked against
/// `limits.spawn_cap`.
pub(super) fn spawn_limit(kind: GrantKind, limits: &GrantLimits) -> Limit;
```

**Defaults are resolved before the check** (revision 2). An omitted cap gets `default_within(DEFAULT, limit)`, and the check then runs on the resolved value. So an omitted cap is never above a lowered limit, and no grant fails because of a value that the caller did not give. The idle default (30 min) is always within `idle_hours` (minimum 1 h), so it needs no resolution.

| Omitted value | Where it is filled today | B27 |
| --- | --- | --- |
| answer cap (answer class) | `prepare_grant` (`worker_grant.rs:187`, after the check) | `prepare_grant` fills `default_within(DEFAULT_ANSWER_CAP, limits.answer_cap)` **before** its answer check |
| answer cap of an owner change | `plan` (`owner_grants.rs:396`) | `default_within(DEFAULT_ANSWER_CAP, limits.answer_cap)`, then the check |
| spawn cap (spawn class) | `grant_check.rs:93`, `grant_owner_start.rs:107`, `plan` (`owner_grants.rs:380`) | `default_within(DEFAULT_SPAWN_CAP, limits.spawn_cap)` |
| spawn cap (no spawn class) | `NewGrant::default` (3, `grants.rs:165`) | unchanged; `prepare_grant` checks the spawn cap only for a grant with the spawn class (the cap of a grant without it is never used) |

The HTTP prechecks of the answer cap (`grant_check.rs:119-127`, `grant_owner_start.rs:187-195`) resolve the omitted value the same way and check the resolved value. `prepare_grant` stays the check that every creator passes.

**Where the limits are read** (each a replacement of a constant):

| Place | Today | B27 |
| --- | --- | --- |
| `prepare_grant` (moved, §5 T3) | `spawn_cap > 50` (every grant), idle `> 24 h`, answer `> 5000` (given values only) | first the defaults above; then, for a grant with the spawn class, `within(spawn_cap, spawn_limit(kind, limits))` unless `spawn_cap == NO_LIMIT`; idle per §3.3; `within(answer_cap, limits.answer_cap)` on the resolved answer cap unless `NO_LIMIT` |
| `check_worker_grant` | `spawn_cap > 10` | no spawn check (`prepare_grant` does it with `spawn_limit`) |
| worker attach (`worker_grant.rs:589`) | `>= WORKER_GRANT_CAP` | `limits.worker_grants.is_some_and(|n| in_effect >= n as usize)` |
| `over_worker_cap` (`worker_grant_end.rs:73-75`) | `>= WORKER_GRANT_CAP` | same rule |
| `grant_spawn_caps.rs:35-40` (live cap change) | `> 50` for every grant, then `> 10` for a worker grant | one check: `within(cap, spawn_limit(grant.kind, limits))` unless `NO_LIMIT` |
| `grant_spawn.rs:35` (owner board request) | `> 50` | `limits.spawn_cap` (an owner grant) |
| `grant_check.rs:124`, `grant_owner_start.rs:192`, `owner_grants.rs:397` | `> 5000` (given value) | the limit, on the resolved value |

A `NO_LIMIT` value is accepted only when the matching limit is none. The constants stay in the code as the defaults (`MAX_SPAWN_CAP` and the others keep their values; a deck test checks that `GrantLimits::default()` equals them).

**Refusal texts** (fixed `&'static str`, so `spawn_caps` and `plan` keep their types):

| Constant (new, `store/grant_rules.rs`) | Text |
| --- | --- |
| `OVER_SPAWN_LIMIT` | `spawn_cap is above the grant limit of the deck (Settings, Grant limits)` |
| `OVER_WORKER_SPAWN_LIMIT` | `a worker grant's spawn cap is above the grant limit of the deck (Settings, Grant limits)` |
| `OVER_ANSWER_LIMIT` | `answer_cap is above the grant limit of the deck (Settings, Grant limits)` |
| `OVER_IDLE_LIMIT` | `the idle time is above the grant limit of the deck (Settings, Grant limits)` |
| `NO_LIMIT_REFUSED` | `no limit needs the grant limit of the deck set to no limit (Settings, Grant limits)` |
| `BAD_LIFETIME` | `a grant needs a lifetime of at most 7 days and at least one station` |

The old texts `spawn_cap is 0 to 50`, `answer_cap is 0 to 5000`, `a worker grant has a spawn cap of at most 10` and the combined `prepare_grant` text go away.

### 3.3 "No limit" in a grant row, and deck migration V52

1. New proto module `crates/fleetdeck-proto/src/grant_limit.rs`:
   ```rust
   //! The "no limit" value of a grant cap (B27 §3.3).

   /// The spawn cap, the answer cap or the idle time (as `idle_ttl_ms` of a
   /// board request) of a grant that has no limit: the largest `u32`.
   pub const NO_LIMIT: u32 = u32::MAX;
   ```
   The board uses the same value as `NO_LIMIT = 4294967295` (a safe JavaScript integer).
2. **Spawn cap and answer cap:** the grant row stores `NO_LIMIT`. The spawn check `used >= i64::from(grant.spawn_cap)` (`coordinator_ops.rs:734`) and the answer check need no change: the station limits (`max_sessions`, 1-256) and the lifetime stop a runaway agent long before `u32::MAX`.
3. **Idle time:** "no idle limit" stores `idle_ttl_ms = ttl_ms` (the lifetime of the grant). Every idle check (§1.3 item 7) then never ends the grant before `expires_at`, because `now - idle_since <= now - created_at <= ttl_ms` while `now <= expires_at`. No idle check changes, no sentinel reaches the arithmetic, and the station gets a normal positive `idle_ttl_ms`. A board request asks for it with `idle_ttl_ms = NO_LIMIT`; a worker grant with `idle_minutes = None`. A number above the lifetime is stored as the lifetime.
4. **Deck migration V52** (new file `crates/fleetdeck-deck/src/store/grant_limits_schema.rs`, the next entry of `migrations()`; V52 is reserved, the merge renumbers if needed): rebuild `tokens` in the pattern of V38 (`coordinator_input_any_schema.rs:22-75`, `PRAGMA defer_foreign_keys = ON`, a copy table `tokens_v51_copy`, an `INSERT` into the new `tokens`, both indexes again). The new table is the V38 table plus the V43 column `kind TEXT NOT NULL DEFAULT 'coordinator' CHECK (kind IN ('coordinator', 'worker'))` as the last column, with three checks changed:
   ```sql
   spawn_cap   INTEGER NOT NULL CHECK (spawn_cap BETWEEN 0 AND 4294967295),
   idle_ttl_ms INTEGER NOT NULL CHECK (idle_ttl_ms BETWEEN 1 AND 604800000),
   answer_cap  INTEGER CHECK (answer_cap IS NULL OR answer_cap BETWEEN 0 AND 4294967295),
   ```
   Every other column, default, check and index stays. Forward only; the module doc says how to reverse it (rebuild with the V38 checks after every grant above the old checks is revoked and deleted).
5. `store/schema.rs` is at 799 lines: the V52 entry joins the V48-V50 comment line (`schema.rs:790`, "V48 to V52: …") and the V40 comment (`schema.rs:777`) is folded into the V39 comment above it, so the file stays at 799 lines.

### 3.4 Bypass mode for worker grants

**The rule.** A coordinator start in a bypass mode (`is_bypass(family, mode)`) is accepted only when all of these are true, checked in this order:

1. The grant is a worker grant (`grant.kind == GrantKind::Worker`). Else the old refusal "a coordinator never starts a session in a bypass mode". Owner grants keep the absolute rule (owner question 5).
2. The deck setting `worker_grant.allow_bypass` is on now (`FleetEnv.knobs.worker_bypass`, read in the turn of the decision). Else: `a worker can start a bypass session only while the deck allows bypass mode for workers`.
3. The mode is within the grant ceiling (`mode_within`). The ceiling is a bypass mode only when the setting allowed it at the grant creation (§3.2). Else the existing text "the mode is above the grant's ceiling `{ceiling}`".
4. The target station offers the mode: the `capabilities.modes` of the provider instance in `ctx.infos` contain it (they contain it only with `allow_bypass_permissions` on and applied, §1.2 item 1). Else: `the station does not allow bypass permissions`.
5. The caller session runs in a bypass mode itself: `session_family(ctx, &grant.station_id, &grant.session_id)` gives a family and a mode with `is_bypass(family, mode)`. Else: `a worker can start a bypass session only while it runs in a bypass mode itself` (owner question 1; with the answer "no", this step goes away).

Each accepted bypass start writes one audit row in the decision transaction: actor `coordinator`, action `coordinator.worker_bypass_start`, target the station id, the command id, result `ok`, detail `grant={grant_id};caller={station}/{session};mode={family}:{mode}`. The call row of `audit_call` stays (`coordinator.rs:368-372`); this second row makes every bypass start easy to find, and it is written in the decision transaction.

**The dispatch check** (revision 2). `mode_refusal` does not change: it keeps the absolute bypass rule. `authority_check` (`coordinator.rs:749-755`) calls the new `launch_refusal` in its place. `launch_refusal` reads the stored command of the outbox row, which `authority_check` has already parsed (`command`). A launch mode that is a bypass mode is valid only when all of these are true:

1. The stored command is a `SessionCommand::Start` whose `spec.mode` is **exactly** the launch mode. The deck writes a bypass mode into a start only after rules 1-5 and in the transaction of the audit row `coordinator.worker_bypass_start`. A launch mode that the station filled itself (a `spec.mode` of `None` or another mode) is therefore never accepted in a bypass mode.
2. The grant is a worker grant.
3. `worker_grant.allow_bypass` is on now (`self.knobs().worker_bypass`, read before the database call, like `self.fleet_control()`).
4. The mode is within the grant ceiling.

Every launch mode that is not a bypass mode goes through `mode_refusal` as today. A resume, an import and a set_mode in a bypass mode are always refused (`mode_not_allowed`). So turning the setting off stops a bypass start that is still in the outbox, at its dispatch. Rule 5 (the caller's mode) is a decision rule only (§3.9 item 4).

**The ceilings of a worker grant.** `check_worker_grant` accepts a bypass ceiling when the setting is on at the creation (`valid_worker_ceiling(family, mode, allow_bypass)`). A live cap change (`spawn_caps`) keeps a stored bypass ceiling of a worker grant when the request repeats it unchanged; it never sets a new bypass ceiling (text: the existing "a mode ceiling is not valid for its family").

**The station** needs no new rule for a bypass start: it is launched only when the adapter offers it (§1.2 item 1). An offered-mode miss fails closed: Claude runs the most restrictive mode (L8, `claude/args.rs:103-107`), Codex refuses the start with `InvalidMode` (`codex/driver.rs:196-203`; revision 2 corrects the L8 claim). The resume and mode-change rule of `fleet_control.rs` stays absolute.

**The fleet skill** (`SKILL.md:20`) becomes: "Rules: start a session in a bypass mode only when `fleet_view` with `view: "me"` shows a bypass mode as your ceiling for that provider; never ask for, print or pass on a credential; a refused call is final, so tell the owner instead of retrying."

New deck module `crates/fleetdeck-deck/src/store/worker_bypass.rs`:

```rust
//! The bypass start of a worker grant and the default mode of a coordinator
//! start (B27 §3.4, §3.5).

/// Why a start of `grant` in the bypass `mode` of `family` on `station` is
/// refused, or `None` when it may run (rules 1-5 of §3.4, in order).
pub(super) fn bypass_refusal(
    grant: &Grant,
    ctx: &Context,
    (station, instance): (&StationId, &ProviderInstanceId),
    (family, mode): (&str, &str),
) -> Option<String>;

/// The audit row of an accepted bypass start, in the caller's transaction.
pub(super) fn audit_bypass_start(
    tx: &Connection,
    grant: &Grant,
    station: &StationId,
    (family, mode): (&str, &str),
    (command_id, now): (&str, i64),
) -> Result<()>;

/// The lower of two modes of `family` (Claude: the earlier in the mode
/// order; Codex: the lower policy and the lower sandbox). `None` for an
/// unknown family or mode.
#[must_use]
pub fn meet(family: &str, a: &str, b: &str) -> Option<String>;

/// The concrete mode of a coordinator start that gives none (§3.5,
/// revision 2): the station default of the instance lowered to the grant
/// ceiling (`meet`); the provider default (`default`,
/// `untrusted/workspace-write`) when the station reports no default (an
/// older station). `None` only for an unknown family or mode (the start is
/// then refused with the ceiling text).
pub(super) fn resolved_mode(
    grant: &Grant,
    ctx: &Context,
    (station, instance): (&StationId, &ProviderInstanceId),
    (family, ceiling): (&str, &str),
) -> Option<String>;

/// The dispatch rule of a launch mode (revision 2, "The dispatch check"):
/// `true` when `authority_check` must answer `mode_not_allowed`.
pub(super) fn launch_refusal(
    grant: &Grant,
    command: &SessionCommand,
    (family, launch_mode): (Option<&str>, Option<&str>),
    worker_bypass: bool,
) -> bool;
```

### 3.5 The default mode of a station (`[default_mode]` in `station.toml`)

New core module `crates/fleetdeck-core/src/config_default_mode.rs` (declared from `config.rs` with `#[path]`, re-exported as `config::DefaultModes`):

```rust
/// `[default_mode]` of `station.toml` (B27 §3.5): the mode of a start that
/// gives none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DefaultModes {
    /// `claude`; `None`: the provider default `default`.
    pub claude: Option<String>,
    /// `codex`; `None`: the provider default `untrusted/workspace-write`.
    pub codex: Option<String>,
}

pub const CLAUDE_PROVIDER_DEFAULT: &str = "default";
pub const CODEX_PROVIDER_DEFAULT: &str = "untrusted/workspace-write";

impl DefaultModes {
    /// The mode for `family` ("claude" or "codex"): the key, or the provider default.
    #[must_use]
    pub fn for_family(&self, family: &str) -> Option<&str>;
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawDefaultModes {
    claude: Option<String>,
    codex: Option<String>,
}

impl RawDefaultModes {
    /// # Errors
    /// An unknown mode; a bypass or full-access mode while `allow_bypass` is off.
    pub(super) fn validate(self, allow_bypass: bool) -> Result<DefaultModes, ConfigError>;
}

/// The keys (`default_mode.<family>`) whose new mode is not within the old one.
#[must_use]
pub fn widens(old: &DefaultModes, new: &DefaultModes) -> Vec<&'static str>;
```

- **Validation** (field `default_mode.claude` / `default_mode.codex`): Claude: one of `default, plan, acceptEdits, dontAsk, auto, bypassPermissions`, message `use one of …`; `bypassPermissions` needs `allow_bypass_permissions = true` (message `bypassPermissions needs allow_bypass_permissions = true`). Codex: `<policy>/<sandbox>` from the lists of `config_worker_grant.rs` (made `pub(crate)`); a `danger-full-access` sandbox needs `allow_bypass_permissions = true` (message `danger-full-access needs allow_bypass_permissions = true`). So a save that turns `allow_bypass_permissions` off while a default is a bypass mode is refused, and the owner lowers the default first.
- **Fields** (`fields_security.rs`): `default_mode.claude` (Enum, the six Claude modes, Live, danger) "Default Claude mode", help "The mode of a new Claude session when the start gives no mode: from the board, a schedule or a worker. Default: default (asks before changes). A start from issue or pull request text keeps a restrictive mode."; `default_mode.codex` (Enum, the nine Codex modes, Live, danger) "Default Codex mode", help "The mode of a new Codex session when the start gives no mode: from the board, a schedule or a worker. Default: untrusted/workspace-write. A start from issue or pull request text keeps a restrictive mode." The values show the provider default when the key is absent. `RAW_STATION_KEYS` gets `"default_mode"`.
- **Danger:** `StationHolder::danger` appends `config_default_mode::widens(old, new)`.
- **The station** (new file `crates/fleetdeck-station/src/manager/default_mode.rs`, revision 2):
  ```rust
  /// The default modes of a running station (B27 §3.5).
  #[derive(Debug, Clone, Default)]
  pub struct DefaultModeState {
      /// `[default_mode]` of the file (applied live).
      pub configured: DefaultModes,
      /// `allow_bypass_permissions` as the adapters were built at the start
      /// of the process (`adapters.rs:38`, `:46`). `reconfigured` keeps it:
      /// the adapters change only at a restart.
      pub bypass_offered: bool,
  }

  impl DefaultModeState {
      /// The default that a start without a mode really gets on `kind`: the
      /// configured mode, or `None` (the provider default) when the mode is a
      /// bypass or full-access mode and `bypass_offered` is false (the
      /// adapters do not offer it until the restart). `None` for `Unknown`.
      #[must_use]
      pub fn effective(&self, kind: ProviderKind) -> Option<&str>;
  }
  ```
  `Policy` gets one field `default_modes: DefaultModeState` (set in `Policy::new` from the config; `reconfigured` takes `configured` from the new config and keeps `bypass_offered`, one line next to `next.stop = self.stop;`) and the accessor `default_mode(&self, kind) -> Option<&str>` that returns `effective(kind)`. In `decide.rs::start`, after the `origin_text` branch (`:446-448`): `else if spec.mode.is_none() && d.authority().is_none() { spec.mode = policy.default_mode(provider).map(str::to_owned); }`. So:
  - The default applies to a start and an import **without** command authority: the board, schedules, mail, the local CLI.
  - A start with command authority (a coordinator start) is never filled by the station. A new deck always sends a concrete mode; an older deck that sends `None` gets the provider default, as today.
  - A bypass or full-access default that the adapters do not offer yet (`allow_bypass_permissions` saved on, no restart yet) is not used: the start gets the provider default. This is the same for Claude and Codex, so a Codex start never fails with `InvalidMode` because of the default (`codex/driver.rs:196-203`, `codex/durable_launch.rs:35-39`).
  - The issue and pull request path keeps `restrict_mode`.
- **The report:** `StationProvider` gets

  ```rust
      /// The mode that a start without a mode gets on this station (B27
      /// §3.5): `[default_mode]` or the provider default. `None` from an
      /// older station.
      #[serde(default, skip_serializing_if = "Option::is_none")]
      #[ts(optional)]
      pub default_mode: Option<String>,
  ```

  `provider_entry` (`manager_providers.rs:137-158`) fills it with the **effective** default of the instance's kind: `policy.default_mode(kind)`, or the provider default when that is `None` (`Unknown` kind: `None`). A deferred bypass default is therefore reported as the provider default until the restart.
- **The deck, for a coordinator start** (`coordinator_ops.rs::start`, also reached by `start_in_project`; revision 2): when `spec.mode` is `None`, the deck **always** sets `spec.mode = Some(resolved_mode(..))` before the bypass and ceiling checks. With a reported default it is `meet(family, station_default, ceiling)`; without one (an older station) it is the provider default, and the ceiling check then decides as today (a ceiling below the provider default refuses the start, as today). `resolved_mode` gives `None` only for an unknown mode, and the start is then refused with the ceiling text. So every coordinator start in the outbox has a concrete mode, the station never fills it (it has command authority), and the station launches exactly the mode that the deck checked. When the resolved mode is a bypass mode, the rules of §3.4 apply to it; when they refuse, the start is refused with `this station's default mode is a bypass mode that your grant cannot use here: give a mode`. The deck never turns a start without a mode silently into a bypass start.
- **The board start, schedules and the local CLI** send `mode: null` and get the effective station default from the station.

### 3.6 The relaxed-security posture and its audit

Proto (`settings.rs`):

```rust
/// Where a relaxed control is in effect (B27 §3.6, revision 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "settings/")]
pub enum RelaxedState {
    /// In the file and in the running process.
    Active,
    /// In the file; the running process applies it at the next restart.
    AfterRestart,
    /// Not in the file any more; the running process keeps it until the
    /// next restart.
    UntilRestart,
    /// From a command-line flag of the running process (the file value is
    /// not used).
    CommandLine,
}

/// One security control that is weaker than its secure default (B27 §3.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "settings/")]
pub struct Relaxed {
    /// The settings key.
    pub key: String,
    /// The fixed text of the board line.
    pub text: String,
    /// Where it is in effect (revision 2).
    pub state: RelaxedState,
}

// SettingsView gets:
    /// The relaxed controls of this file (B27 §3.6). `None` from an older
    /// station.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub relaxed: Option<Vec<Relaxed>>,

// SettingsSaved gets:
    /// The keys of this save that relaxed a control (its danger keys, B27
    /// §3.6). Empty from an older station.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional)]
    pub relaxed: Vec<String>,
```

New core module `crates/fleetdeck-core/src/settings/posture.rs`, and a new provided method `Holder::relaxed(parsed: &Self::Parsed) -> Vec<(String, String)>` (the key and the text of each relaxed control of one config); the writer sets `SettingsSaved.relaxed` to the danger keys of the save (`settings/writer.rs:348`).

**The posture is the effective posture** (revision 2), not only the file. It merges the file and the running process:

```rust
/// The posture of the file `file` for a process that runs `running`, with
/// the command-line `overrides` (keys). `running = None` (no process known,
/// for example a test): every item of `file` is `Active`.
pub fn posture<H: Holder>(
    file: &H::Parsed,
    running: Option<&H::Parsed>,
    overrides: &[&str],
) -> Vec<Relaxed>;
```

Let `F` be `H::relaxed(file)` and `R` be `H::relaxed(running)`. For each key of `F ∪ R`, in the order of the field table:

| The key is | Item |
| --- | --- |
| in `overrides` | in `R`: `CommandLine` with the text of `R`; else no item (the flag's value is not relaxed) |
| a `Live` field | in `F`: `Active` with the text of `F` (a save publishes a live field at once) |
| a `Restart` field, in `F` and `R` | `Active`, the text of `F` |
| a `Restart` field, in `F` only | `AfterRestart`, the text of `F` |
| a `Restart` field, in `R` only | `UntilRestart`, the text of `R` |

`view()` (`settings/view.rs:79`) sets `relaxed: Some(posture::<H>(&parsed, running, &override_keys))`. The running config reaches the view through two new entry points beside the old ones (the old ones pass `None`, so no test caller changes): `view_station_running(path, env, store, overrides, running: &StationConfig)` and `view_deck_running(path, overrides, running: &DeckConfig)`.

- **The station:** `StationSettings` gets `running: Option<Arc<StationConfig>>` and a builder `with_running(Arc<StationConfig>)` (like `with_settle`). `serve.rs:397` calls it with the config that the process started with. `get` calls `view_station_running` when it is set.
- **The deck:** `DeckSettings` gets `running: Option<Arc<DeckConfig>>`. `serve.rs:214` sets it to the `config` of `RunningDeck::start`, which already has the command-line values (`up.rs:96-100`: `--listen`). `deck_view` calls `view_deck_running` when it is set.
- **The default mode:** `default_mode.claude` and `default_mode.codex` are `Live` fields, but a bypass or full-access default is deferred until the adapters offer it (§3.5). `StationHolder::relaxed` cannot see the adapters, so `posture` has one special rule: a `default_mode.*` item whose mode needs bypass is `AfterRestart` when `running` has `allow_bypass_permissions` off.

**The text of an item:** a bool key: `{label}`; any other key: `{label}: {value} (default {default})`, with the value `"none"` written `no limit`; `listen` and `link_listen`: `{label}: every network interface`. The label is `FieldMeta.label`.

**The deck posture** (`DeckHolder::relaxed`, for one config): the keys of `DeckHolder::danger(&DeckConfig::parse("")?, current)` (the default config against the current one), with three changes: `listen` and `link_listen` count only for an unspecified address (`0.0.0.0` or `::`), because a tailnet address is the normal remote setup; `tailscale_bin` is left out (a path, not a relaxation); the defaults come from the parsed empty file. This covers `fleet_control`, `desktop_control`, `tailscale_cert`, the ten `security.*` knobs in their weaker direction, `retention.audit_*` lowered, every widened `worker_grant.*` key (including `allow_bypass` and a bypass ceiling) and every raised `grant_limits.*` key.

**The station posture** (`StationHolder::relaxed`, for one config), explicit because a default station config needs the machine's home directory:

| Key | Relaxed when |
| --- | --- |
| `allow_setup_cmd`, `allow_shell`, `allow_bypass_permissions`, `allow_fleet_control`, `allow_desktop`, `allow_desktop_capture`, `allow_desktop_input`, `allow_desktop_click`, `allow_desktop_input_any`, `fleet_summary`, `allow_check_cmd`, `allow_local_remotes` | on |
| `offline_approval_policy` | its rank is above `wait` (`policy_rank`) |
| `listen` | an unspecified address |
| `retention.raw_log_mib` | above 16 |
| `default_mode.claude`, `default_mode.codex` | not within the provider default (`mode_within`) |

A guard test lists the station fields with `danger: true`; each is a posture key or one of `STATION_NOT_POSTURE = ["allowed_roots", "capture_monitors", "provider_instance", "host", "tailscale_bin", "retention.history_days"]` (configuration, or a value whose default is already the widest). A new danger field then cannot be forgotten. The deck has the same guard with `DECK_NOT_POSTURE = ["tailscale_bin", "tailnet_auto"]`.

**The audit:** after a successful save with a non-empty `relaxed`, the deck writes a second row: actor `owner`, action `settings.relaxed`, target `deck` or the station name, detail the keys joined with `,` (`http/settings_routes.rs`, after the existing `audit(...)` of the deck save and of the station save). The values are never written.

### 3.7 Board

- **Settings, `FieldKind` `limit`** (new `web/src/settings/LimitEditor.tsx`): a number input (`min`/`max` of the field) and a checkbox "No limit". While it is checked the input is disabled and the edit value is `"none"`. `FieldEditor.tsx` dispatches `case 'limit'` to it.
- **Sections** (`model.ts`): "Worker grants" gets `worker_grant.allow_bypass` after `worker_grant.codex_mode`; a new section "Grant limits" after "Worker grants" with the five `grant_limits.*` keys in the order of §3.2; "Sessions" gets `default_mode.claude` and `default_mode.codex` after `allow_bypass_permissions`.
- **Hazard lines** (`SettingsFields.tsx`, the `hazardOf` function becomes a table; icon and class as today):

| Key | Shown while | Text |
| --- | --- | --- |
| `allow_bypass_permissions` | `true` | Sessions on this station can run every tool without asking. (unchanged) |
| `worker_grant.allow_bypass` | `true` | Workers can start sessions that run every tool without asking. |
| `worker_grant.claude_mode` | `bypassPermissions` | Sessions that a worker starts can skip every permission check. |
| `worker_grant.codex_mode` | `never/danger-full-access` | Sessions that a worker starts can skip every permission check. |
| `worker_grant.spawn_cap`, `grant_limits.spawn_cap`, `grant_limits.worker_spawn_cap` | `"none"` | No limit: only the grant lifetime and the station's Most sessions stop a runaway agent. |
| `worker_grant.idle_minutes`, `grant_limits.idle_hours` | `"none"` | No idle limit: a grant stays in effect until its lifetime ends, also when nobody uses it. |
| `grant_limits.answer_cap` | `"none"` | No limit: an agent can answer every approval while its grant lasts. |
| `grant_limits.worker_grants` | `"none"` | No limit: every session that you start can get a worker grant. |
| `default_mode.claude` | not `default` | Sessions that start without a mode, also scheduled ones and worker sessions, start in this mode. |
| `default_mode.codex` | not `untrusted/workspace-write` | (same text) |

- **Posture line** (new `web/src/settings/PostureLine.tsx`, at the top of the tab body in `SettingsTab.tsx`, design system tones):
  - `relaxed` absent: muted text "This station does not report its security posture. Update the station to see it."
  - empty: calm text "Security: every control is at its secure default." The list is empty only when no control is relaxed in the file, in the running process or by a command-line flag (revision 2).
  - not empty: a hazard box, title "Relaxed security", the sentence "1 control is changed from its secure default." or "{n} controls are changed from their secure default.", then one list item per item: `Relaxed.text` and a suffix by `state`: `active` none; `after_restart` " (after the next restart)"; `until_restart` " (still in effect until the next restart)"; `command_line` " (set on the command line)". Then "Change a control below to restore its default." and, when an item is `until_restart` or `after_restart`, "Restart the {station|deck} to apply the saved values."
- **Spawn form:** the empty mode option reads `Station default ({modeLabel(d)})` when the provider reports `default_mode` `d`, else "Default mode" (`AgentFields.tsx:89`, the value stays `""`).
- **Fleet control fields** (`fleetControl.ts`, `FleetControlFields.tsx`): the session limit and the answer limit get a checkbox "No limit" (sends `NO_LIMIT`); a new input "Idle limit (minutes)", default `30`, with a checkbox "No limit" (sends `idle_ttl_ms = NO_LIMIT`; a number sends `minutes * 60000`). `grantFields` returns `idle_ttl_ms` always (`0` is no longer sent by `SpawnForm.tsx:419`; that line goes away). The board no longer checks the upper bounds of these three: the problems become "The session limit is a whole number.", "The answer limit is a whole number." and "The idle limit is a whole number of minutes from 1 to 10080.". The deck refuses a value above its limit with the texts of §3.2.
- **Display of a cap:** a new helper `web/src/fleet/limitText.ts`: `limitText(n: number): string` gives `"no limit"` for `NO_LIMIT`, else the number. `SessionAccess.tsx:299` shows `{used} of {limitText(cap)}`; `FleetAnswersDialog.tsx:43` shows `Answer limit: {used} of {limitText(cap)} used`. `GrantSpawn.tsx` (the live cap change) gets the "No limit" checkbox for the session limit.

### 3.8 Compatibility

| Case | Behavior |
| --- | --- |
| New deck, older station | No `default_mode` in `station.info`: a coordinator start without a mode gets the provider default written into the command (revision 2), and the ceiling check decides as today. The station view has no `relaxed`: the board shows "does not report". A station save has no `relaxed`: no `settings.relaxed` row (the keys row stays). |
| New deck, station upgraded between the decision and the dispatch | The command in the outbox has a concrete mode (revision 2), and a station never fills the mode of a command with authority. A row that a pre-B27 deck queued with `mode: None` is never accepted in a bypass launch mode (the dispatch rule needs `spec.mode` equal to the bypass launch mode). |
| Older deck, new station | The station does not apply its default to a command with authority (revision 2): a coordinator start with no mode gets the provider default, as today. The older deck ignores `default_mode` and `relaxed` (no `deny_unknown_fields`). Update the deck first, as usual. |
| Deck database | V52 is forward only. An older deck binary cannot open a V52 database (the existing rule of every migration). |
| `deck.toml` with `"none"` or `[grant_limits]` | An older deck refuses the file (`deny_unknown_fields`, a string in an integer key). A downgrade needs those keys removed first. The release notes say so. |
| `station.toml` with `[default_mode]` | An older station refuses the file; same note. |
| Feature flags | None. The new fields are optional and their absence means "older peer". The deck and the board ship together. |
| Existing grants | Unchanged by the migration (all values are inside the new checks). A grant made before B27 never has `NO_LIMIT`. |

### 3.9 Errors and edge cases

1. **A setting changes while a decision runs.** The deck reads `FleetEnv.knobs` in the turn of the decision (§1.6 item 3); the dispatch check reads it again. The worst case is one start decided under the old value and then refused at its dispatch.
2. **`allow_bypass_permissions` is saved on, but the station has not restarted.** The adapters do not offer bypass yet, so rule 4 of §3.4 refuses the bypass start. A bypass or full-access default is not used until the restart: a start without a mode gets the provider default, for Claude and for Codex (revision 2; the Codex adapter refuses an unoffered mode with `InvalidMode`, so the station must not send it). The station reports the provider default in `StationProvider.default_mode`, and the posture shows the default as "after the next restart". All fail closed.
3. **The station default changes.** The deck reads `station.info` every 60 s. Until then a coordinator start gets the old default. The deck writes the mode explicitly, so the station launches what the deck checked.
4. **The caller session changes its mode after the decision.** Rule 5 is checked at the decision only. The dispatch check accepts a bypass launch only for the exact bypass mode that the decision wrote (and audited), for a worker grant, with the setting on and within the ceiling. Accepted: the owner changes a mode on the board, and the bypass start was allowed a moment before.
5. **A worker grant with a bypass ceiling after `allow_bypass` is turned off.** The grant keeps its stored ceiling, but rule 2 refuses every new bypass start at once, and at the dispatch. Its non-bypass starts go on.
6. **A lower `grant_limits` value.** Existing grants keep their values (owner question 4); new grants and cap changes use the new limit. A save that puts `worker_grant.spawn_cap` or `idle_minutes` above a new limit is refused (§3.2 table).
7. **`idle_ttl_ms = NO_LIMIT` with a limited `idle_hours`.** Refused with `NO_LIMIT_REFUSED`.
8. **A board idle above the lifetime.** Stored as the lifetime (no idle limit in effect), only when it is within `idle_hours` or that limit is none.
9. **A hand edit of `deck.toml` or `station.toml`.** Not audited and needs no elevation (today's rule: the file belongs to the owner). The posture line still shows the result.
10. **`allow_bypass_permissions` is saved off, but the station has not restarted.** The adapters still offer bypass until the restart. The posture keeps the item with "still in effect until the next restart" (revision 2), so the board never says "every control is at its secure default" while bypass is still offered.
11. **A command-line flag relaxes a control** (the deck `--listen 0.0.0.0:…`). The posture shows it from the running config with "set on the command line", also when the file is secure (revision 2).
12. **An omitted cap and a lowered limit.** A grant without an answer cap (answer class) or a spawn cap (spawn class) gets `default_within(default, limit)`: with `grant_limits.answer_cap = 1` it gets 1, not 500 (revision 2). The resolved value is checked like a given one.
13. **A worker spawn limit above the owner spawn limit.** `grant_limits.worker_spawn_cap = 100` with `grant_limits.spawn_cap = 50`: a worker grant with a spawn cap of 60 is accepted (creation and cap change); an owner grant with 60 is refused (revision 2, `spawn_limit`).

### 3.10 Out of scope

- A default mode per project (owner question 2).
- A bypass ceiling for owner (board) grants (owner question 5).
- A "trusted home network" profile (owner question 3).
- A higher grant lifetime; the member deny list; the floors of the `[security]` knobs.
- A summary of the posture of all stations on one tab (each tab shows its own).

## 4. Threat notes and risk class

**High.** B27 lets agent-started sessions skip every permission check, removes limits of grants (authorization), changes the mode of unattended starts, and rebuilds the grant table.

| # | Threat | Defense |
| --- | --- | --- |
| T1 | A worker that reads hostile text (a web page, an issue) starts a bypass session and runs any command as the owner. | Off by default. Three explicit switches, each with the elevation and a hazard text: the station's `allow_bypass_permissions`, the deck's `worker_grant.allow_bypass`, and a bypass value of `worker_grant.claude_mode`/`codex_mode`. The worker must itself run in a bypass mode (rule 5): a supervised worker cannot escalate. The deck setting is read at the decision and at the dispatch, so "off" works at once. The dispatch accepts a bypass launch only when the stored command has that exact bypass mode, which the deck writes only after rules 1-5 and with the audit row in the same transaction (revision 2): a mode that a station fills itself never passes as bypass. Each bypass start has an audit row. The posture line lists all three. |
| T2 | A worker with no spawn limit starts sessions without end. | The station's `max_sessions` (1-256) bounds the running sessions; the worker grant has the lineage scope and its lifetime (1-168 h) and ends with its session (`apply_session_ended`). The hazard text and the posture line warn. |
| T3 | A grant with no idle limit stays usable after its session stops working. | The lifetime stays the hard cap (7 days, worker 168 h); a worker grant ends with its session; the owner can revoke. |
| T4 | A grant with no answer limit answers every approval. | Only an owner grant with the answer class has an answer cap; the class needs the elevation on the board; the lifetime caps it. An omitted cap gets the default lowered to the limit and is checked like a given cap, so a lowered limit also binds a request without a cap (revision 2). |
| T5 | A permissive station default reaches unattended starts (schedules, workers, the local CLI). | The default is never above the station ceiling (a bypass default needs `allow_bypass_permissions` in the same file); issue and pull request text starts keep `restrict_mode`; a coordinator start gets the lower of the default and its grant ceiling, and never a silent bypass; the station never applies its default to a command with authority (revision 2); a bypass default that the adapters do not offer yet is not used (provider default instead, Claude and Codex); the elevation for a more permissive default; the posture line. |
| T6 | The "no limit" value overflows an arithmetic check. | Spawn and answer checks compare `used >= cap` in `i64`; no sum with `NO_LIMIT`. The idle time is stored as the lifetime, a normal value; no idle check sees a sentinel. |
| T7 | The V52 rebuild loses rows, the `kind` column, an index or the foreign key of `coordinator_lineage`. | The V38 pattern (deferred foreign keys, copy and insert). A test compares every row and column before and after, and the index list. |
| T8 | A relaxation is not visible, or the board says "secure" while a relaxed value is still in effect. | The posture is the effective posture (revision 2): the file merged with the config of the running process and its command-line flags, with the states `active`, `after_restart`, `until_restart` and `command_line`; the guard tests force every danger field into the posture or into an explicit exclusion list; every relaxing save writes `settings.relaxed`. |
| T9 | The posture line or the audit leaks a value. | The posture shows the values of non-secret keys only (none of the posture keys is secret); the audit row has keys only. |
| T10 | The deck and the station disagree about the mode of a coordinator start. | The deck always writes a concrete mode into the command (the provider default for an older station, revision 2); the station launches it and never fills a command with authority; the dispatch check compares the real launch mode again. |
| T11 | A version skew or an upgrade between the decision and the dispatch turns a checked start into a bypass start (revision 2, adversary finding 1). | A concrete mode in every coordinator start; no station default for a command with authority; the dispatch rule "bypass launch mode = the stored `spec.mode`". A pre-B27 outbox row with `mode: None` can never launch in bypass. Test T4-11. |
| T12 | A ceiling of one grant kind blocks or widens the other kind (revision 2, finding 3). | `spawn_limit(kind, limits)` picks one ceiling per kind, in creation and in a cap change. |

The adversary does a DESIGN REVIEW of this document before T3 and T4 start, and a PRE-PR REVIEW of the final branch.

## 5. Build plan

Workers reset their worktree to `v2` first. Every heavy command goes through `umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh <command>`. A worker compiles only (`cargo check -p <crate> --tests`; web: `bun run typecheck`, `bun run lint`), writes its tests, and keeps each file under 800 lines (`wc -l`). The tests run at the merge into `v2` (DL-520). A UI worker loads the `fleet-design-system` skill first.

Order: T1 first. Then T2, T3 and T5 (no shared files; at most two at a time on this machine). T4 after T3. T6 after T2. T7 after T1 and T3. T8 last.

| Task | Kind | Size | Depends on |
| --- | --- | --- | --- |
| T1 Core and proto: the new keys, `FieldKind::Limit`, `NO_LIMIT`, `StationProvider.default_mode` | backend | ≈ 380 code, ≈ 300 tests | none |
| T2 Posture (effective: file, running config, flags) and its audit | backend | ≈ 300 code, ≈ 330 tests | T1 |
| T3 Deck: grant limits from the settings, "no limit", resolved defaults, the spawn limit by kind, migration V52 | backend | ≈ 330 code, ≈ 380 tests | T1 |
| T4 Deck: bypass for workers, the dispatch rule, a concrete mode in every coordinator start | backend | ≈ 240 code, ≈ 400 tests | T3 |
| T5 Station: the default mode (no fill with authority, deferred bypass default) | backend | ≈ 110 code, ≈ 270 tests | T1 |
| T6 Board: settings (limit editor, hazards, sections, posture line) | ux | ≈ 250 code, ≈ 200 tests | T1, T2 |
| T7 Board: spawn form and grant displays | ux | ≈ 180 code, ≈ 200 tests | T1, T3 |
| T8 Docs | docs | ≈ 200 lines | T1-T7 |

### T1 (backend): core and proto, the settings registry of B27

- **Owned files:**
  - `crates/fleetdeck-proto/src/settings.rs` (`FieldKind::Limit` only)
  - `crates/fleetdeck-proto/src/grant_limit.rs` (new), `crates/fleetdeck-proto/src/lib.rs` (one `pub mod grant_limit;` line)
  - `crates/fleetdeck-proto/src/link.rs` (`StationProvider.default_mode` and the literal in its test at `:726`)
  - `web/src/gen/**` (regenerated with `sh …/heavy.sh make ts-types`)
  - `crates/fleetdeck-core/src/lib.rs` (one line), `config_grant_limits.rs` (new), `config_default_mode.rs` (new), `config_worker_grant.rs`, `config_deck.rs` (the table, the comment at `:72` becomes "(knobs K1 and K2)"), `config.rs` (the field, the `#[path]` module, the comment at `:318` becomes "(knobs K1 and K3)"), `config_raw.rs`
  - `crates/fleetdeck-core/src/settings/fields.rs` (the four worker entries), `settings/fields_security.rs` (new), `settings/apply.rs`, `settings/holder.rs` (values and danger only), `settings/mod.rs` (one `mod` line), `settings/tests/security.rs` (new), `settings/tests/mod.rs` (one line), `settings/tests/fields.rs` (the key lists)
  - The mechanical type fixes outside core, nothing else in these files: `crates/fleetdeck-deck/src/store/worker_grant.rs:615` (`grant.spawn_cap = w.spawn_cap.unwrap_or(NO_LIMIT);`) and `:620` (`grant.idle_ttl_ms = w.idle_minutes.map_or(grant.ttl_ms, |m| i64::from(m) * 60_000);`, after the `ttl_ms` line, the same line count), `crates/fleetdeck-deck/src/store/worker_grant_authority_tests.rs:120` (`spawn_cap: Some(1)`), and `default_mode: None` in the `StationProvider` literals of `crates/fleetdeck-station/src/assist/tests.rs:76`, `crates/fleetdeck-cli/src/provision/fake.rs:168`, `crates/fleetdeck-deck/src/routing_place_tests.rs:30`, `crates/fleetdeck-deck/src/routing_tests.rs:44`, `crates/fleetdeck-deck/tests/common/fleet.rs:57`, `crates/fleetdeck-deck/src/store/coordinator_tests.rs:55`, `crates/fleetdeck-deck/src/store/fleet_view_tests.rs:115`, `crates/fleetdeck-station/src/manager/manager_providers.rs:150` (`default_mode: None` here; T5 fills it).
- **Changes:** §3.2 (the module, the deck table, the worker changes, the field table, `FieldKind::Limit`, values, danger), §3.3 item 1 (`NO_LIMIT`), §3.5 (the module, the station field, validation, fields, danger, `StationProvider.default_mode`).
- **Tests** (`settings/tests/security.rs` and the module tests):
  1. `grant_limits_default_to_the_old_constants`: `DeckConfig::parse("")` gives 200, 50, 10, 24, 5000.
  2. `a_grant_limit_is_a_number_in_range_or_none`: `spawn_cap = "none"` → `None`; `0`, `100001`, `"unlimited"` → errors with field `grant_limits.spawn_cap`; `idle_hours = 169` → error.
  3. `worker_grant_values_stay_within_the_grant_limits`: the six rows of the §3.2 table, each with its exact message, and the accepted cases (`spawn_cap = 11` with `worker_spawn_cap = 20`; `"none"` with `"none"`; `idle_minutes = 2880` with `idle_hours = 48`).
  4. `a_bypass_ceiling_needs_allow_bypass`: both families, refused and accepted.
  5. `widening_covers_the_new_worker_keys`: `allow_bypass`, `spawn_cap` `Some(2)` → `None`, `idle_minutes` `Some(30)` → `None`, `claude_mode` `acceptEdits` → `bypassPermissions`; no key for the reverse changes.
  6. `a_raised_grant_limit_needs_the_elevation`: `DeckHolder::danger` for each key: raised and none → the key; lowered → nothing.
  7. `the_default_mode_never_passes_the_station_ceiling`: Claude `bypassPermissions` and Codex `untrusted/danger-full-access` refused without `allow_bypass_permissions` and accepted with it; an unknown mode refused; turning `allow_bypass_permissions` off with a bypass default refused.
  8. `a_more_permissive_default_mode_needs_the_elevation`: `default` → `acceptEdits` is danger `default_mode.claude`; the reverse is not.
  9. `a_limit_field_saves_a_number_or_none`: through `SettingsWriter`: `"none"` writes `spawn_cap = "none"` in `[grant_limits]`; `30` writes an integer; `"x"` and `100001` give `SaveError::invalid`.
  10. `the_values_show_none_and_the_provider_defaults`: `grant_limits.answer_cap` `"none"`; `default_mode.codex` `"untrusted/workspace-write"` when absent.
  11. proto (`link.rs` test module): `a_station_provider_without_default_mode_parses` (JSON of an older station) and `default_mode_is_left_out_when_none`.
  12. The existing field-table tests pass with the new keys (`RAW_*_KEYS`).
  13. `default_within_lowers_the_default_to_the_limit` (revision 2): `(500, Some(1))` → 1; `(500, Some(5000))` → 500; `(500, None)` → 500.
- **depends_on:** none.
- **Acceptance (at the merge):**
  - `cargo nextest run -p fleetdeck-core --no-fail-fast -E 'test(settings) | test(config)'`
  - `cargo nextest run -p fleetdeck-proto --no-fail-fast -E 'test(link) | test(ts_drift)'`
  - `cargo clippy -p fleetdeck-core -p fleetdeck-proto --all-targets -- -D warnings`
  - `cargo check --workspace --tests` compiles.
  - The guards: `cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'`.

### T2 (backend): the posture and its audit

- **Owned files:**
  - `crates/fleetdeck-proto/src/settings.rs` (`RelaxedState`, `Relaxed`, `SettingsView.relaxed`, `SettingsSaved.relaxed`)
  - `web/src/gen/**` (regenerated)
  - `crates/fleetdeck-core/src/settings/posture.rs` (new: `posture`, the merge table of §3.6), `settings/holder.rs` (the trait method and the two `relaxed` impls that call `posture.rs`), `settings/view.rs` (`view_station_running`, `view_deck_running`), `settings/writer.rs`, `settings/mod.rs` (one line and the re-exports), `settings/tests/posture.rs` (new), `settings/tests/mod.rs` (one line)
  - The `SettingsView`/`SettingsSaved` literals: `crates/fleetdeck-station/src/local.rs:178`, `crates/fleetdeck-deck/src/local.rs:179`, `crates/fleetdeck-cli/src/settings_tests.rs:69`, `:105`
  - `crates/fleetdeck-deck/src/http/settings_routes.rs` (the second audit row; `DeckSettings.running`; `deck_view`), `crates/fleetdeck-deck/src/serve.rs` (the `DeckSettings` literal at `:214`: `running: Some(Arc::new(config.clone()))`), `crates/fleetdeck-deck/tests/settings_support/mod.rs` (the literal: `running: None`)
  - `crates/fleetdeck-station/src/settings_svc.rs` (`running`, `with_running`, `get`), `crates/fleetdeck-station/src/serve.rs` (`.with_running(Arc::new(config.clone()))` at `:397`)
  - `crates/fleetdeck-deck/tests/settings_relaxed.rs` (new)
- **Changes:** §3.6 (with the effective posture of revision 2).
- **Tests:**
  1. `a_default_deck_has_no_relaxed_control`.
  2. `the_deck_posture_lists_every_relaxed_control`: `fleet_control = true`, `security.min_password_chars = 8`, `[grant_limits] spawn_cap = "none"`, `worker_grant.allow_bypass = true` → four items, exact texts (for example `Shortest password: 8 (default 12)` with the K1 label, `Highest session limit of a grant: no limit (default 50)`, `Allow bypass mode for workers`).
  3. `a_tailnet_address_is_not_relaxed_but_every_interface_is`: `listen = ["100.64.0.1:7420"]` → no item; `["0.0.0.0:7420"]` → one item `…: every network interface`.
  4. `the_station_posture_lists_every_relaxed_control`: each row of the §3.6 table on and off.
  5. `every_danger_field_is_a_posture_key_or_excluded` (deck and station guards).
  6. `a_save_reports_the_keys_that_relaxed`: `SettingsSaved.relaxed` equals the danger keys; empty for a tightening save.
  7. `a_view_has_the_posture`: `view_deck` and `view_station` give `relaxed: Some(..)`, every item `active`.
  8. `tests/settings_relaxed.rs`: an elevated deck save of `worker_grant.allow_bypass` writes the rows `settings.save` (keys) and `settings.relaxed` (detail `worker_grant.allow_bypass`); a save that only lowers a value writes no `settings.relaxed` row; a station save through the fixture writes `settings.relaxed` with the station name.
  9. `a_restart_field_saved_secure_stays_until_the_restart` (revision 2): `running` with `allow_bypass_permissions = true`, the file with `false` → one item `allow_bypass_permissions`, state `until_restart`; never the empty list.
  10. `a_restart_field_saved_relaxed_waits_for_the_restart`: the reverse → state `after_restart`.
  11. `a_command_line_value_is_in_the_posture`: deck file `listen = ["100.64.0.1:7420"]`, `running.listen = ["0.0.0.0:7420"]`, overrides `["listen"]` → one item, state `command_line`; with a secure flag value and a relaxed file → no item.
  12. `a_live_field_follows_the_file`: `worker_grant.allow_bypass` true in the file, false in `running` → `active`.
  13. `a_bypass_default_waits_for_the_bypass_restart`: station file with `allow_bypass_permissions = true` and `default_mode.codex = "never/danger-full-access"`, `running` with `allow_bypass_permissions = false` → both items `after_restart`.
- **depends_on:** T1.
- **Acceptance (at the merge):**
  - `cargo nextest run -p fleetdeck-core --no-fail-fast -E 'test(settings)'`
  - `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'binary(settings_relaxed) | test(settings_routes) | binary(board_knobs)'`
  - `cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'test(settings)'`
  - `cargo clippy -p fleetdeck-core -p fleetdeck-deck -p fleetdeck-cli -p fleetdeck-station --all-targets -- -D warnings`
  - The guards (as T1). `wc -l crates/fleetdeck-deck/src/http/settings_routes.rs` is below 800.

### T3 (backend): the deck reads the grant limits; "no limit"; migration V52

- **Owned files:**
  - `crates/fleetdeck-deck/src/knobs.rs`
  - `crates/fleetdeck-deck/src/store/grant_rules.rs` (new: `prepare_grant`, `check_worker_grant` and `valid_worker_ceiling` moved from `worker_grant.rs:159-262`, with a `limits: &GrantLimits` parameter, and the text constants of §3.2), `store/grants.rs` (the `#[path]` declaration of `grant_rules`, the import, `create_grant` passes `&self.knobs().grant`, the doc at `:434-435`)
  - `crates/fleetdeck-deck/src/store/worker_grant.rs` (the move out, the import, `attach` gets `limits: &GrantLimits`, the worker count rule at `:589`)
  - `crates/fleetdeck-deck/src/store/worker_grant_end.rs`, `store/grant_spawn_caps.rs` (a `limits` parameter, §3.2, and the kept bypass ceiling of §3.4), `store/grants.rs` `renew_grant` call of `over_worker_cap`
  - `crates/fleetdeck-deck/src/store/owner_grants.rs` (first move `Plan` and `plan`, `:342-402`, to the new `store/owner_grant_plan.rs` declared with `#[path]`; then `Ctx` gets `pub limits: GrantLimits`, set at the three `Ctx {` sites `:673`, `:720`, `:772` from `self.knobs().grant`; `give_tx` calls `prepare_grant(grant, &ctx.limits)`), `store/owner_grant_plan.rs` (new)
  - `crates/fleetdeck-deck/src/http/grant_spawn.rs`, `http/grant_check.rs`, `http/grant_owner_start.rs`, `http/grant_live_routes.rs` (they read `board.live.get().knobs.grant`)
  - `crates/fleetdeck-deck/src/store/grant_limits_schema.rs` (new, V52), `store/schema.rs` (the entry, §3.3 item 5), `crates/fleetdeck-deck/tests/merged_migrations.rs` (`DECK_LATEST`)
  - Tests: `store/grant_limits_tests.rs` (new, declared from `grant_rules.rs`), `tests/grant_limits.rs` (new), and the old-text assertions in `store/grant_spawn_caps_tests.rs`, `store/owner_change_tests.rs`, `tests/live_grant_routes.rs`, `tests/worker_grant_caps.rs`, `tests/grant_spawn.rs`
- **Changes:** §3.2 (the knobs, `default_within` at every default, `spawn_limit` by kind, every place of the table, the texts), §3.3 items 2-5, §3.4 "The ceilings of a worker grant".
- **Tests:**
  1. `the_default_limits_equal_the_old_constants`.
  2. `a_worker_grant_with_no_spawn_limit_stores_no_limit`: `worker_spawn_cap = None`, `worker_grant.spawn_cap = None`, `grant_limits.spawn_cap` at its default 50 → `grant.spawn_cap == NO_LIMIT`; 60 coordinator starts of that grant are not refused for the cap (station `max_sessions` set high in the fixture).
  3. `no_idle_limit_stores_the_lifetime`: worker `idle_minutes = None`, `lifetime_hours = 48` → `idle_ttl_ms == 48 h`; after 25 h without use the grant is still in effect; after 48 h it is expired.
  4. `the_worker_grant_count_follows_the_setting`: `worker_grants = Some(2)`: the third start gets the decision `none` with reason `worker_cap` and the audit row `coordinator.worker_grant_skipped`; `None`: 201 worker grants are in effect.
  5. `an_owner_grant_above_a_limit_is_refused_with_the_new_text`: spawn cap 51 → `OVER_SPAWN_LIMIT`; with the limit 100 → accepted; `NO_LIMIT` with a number limit → `NO_LIMIT_REFUSED`; with none → accepted and stored.
  6. `the_answer_and_idle_limits_of_an_owner_grant`: the same three cases for `answer_cap` and for `idle_ttl_ms` (25 h with 24 → `OVER_IDLE_LIMIT`; `NO_LIMIT` with none → stored as the lifetime).
  7. `a_lower_limit_keeps_existing_grants`: a grant with spawn cap 40; the limit set to 10 (`set_knobs`); its starts go on to 40; a new grant with 40 is refused.
  8. `a_cap_change_keeps_a_stored_bypass_ceiling_and_never_adds_one`: a worker grant created with `allow_bypass` and a bypass Claude ceiling; a spawn-cap change with the same ceilings → accepted; a change that sets a bypass Codex ceiling → refused.
  9. `v52_keeps_every_grant_and_widens_the_checks`: a database at the version before V52 with one owner grant and one worker grant (every column set, `kind = 'worker'`, a `coordinator_lineage` row); migrate; every value equal; `PRAGMA index_list(tokens)` has both indexes; `PRAGMA foreign_key_check` is empty; an insert with `spawn_cap = 4294967295`, `answer_cap = 4294967295`, `idle_ttl_ms = 604800000` succeeds; `idle_ttl_ms = 604800001` fails.
  10. The changed old-text assertions.
  11. `an_omitted_answer_cap_is_lowered_to_the_limit` (revision 2): `grant_limits.answer_cap = Some(1)`; an owner grant with the answer class and no `answer_cap`, through `POST` of the board grant route (`grant_check.rs`) and through the owner start route (`grant_owner_start.rs`) → stored `answer_cap == 1`; an owner change that adds the answer class without a cap (`plan`) → 1; a given `answer_cap = 2` → `OVER_ANSWER_LIMIT`.
  12. `an_omitted_spawn_cap_is_lowered_to_the_limit`: `grant_limits.spawn_cap = Some(2)`; a grant with the spawn class and no `spawn_cap`, through both owner routes and an owner change → stored 2; a grant without the spawn class is accepted (its unused cap is not checked).
  13. `the_spawn_limit_follows_the_grant_kind` (finding 3): `spawn_cap = Some(50)`, `worker_spawn_cap = Some(100)`, `worker_grant.spawn_cap = 60` → the worker attach stores 60; a worker cap change to 80 → accepted, to 101 → `OVER_WORKER_SPAWN_LIMIT`; an owner grant with 60 → `OVER_SPAWN_LIMIT`; `spawn_cap = Some(100)`, `worker_spawn_cap = Some(10)`: a worker cap change to 20 → `OVER_WORKER_SPAWN_LIMIT`.
- **depends_on:** T1.
- **Acceptance (at the merge):**
  - `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'test(grant) | test(worker_grant) | test(owner_grants) | test(owner_change) | test(knobs) | binary(grant_limits) | binary(live_grant_routes) | binary(worker_grant_caps) | binary(grant_spawn) | binary(merged_migrations) | binary(worker_grant)'`
  - `cargo clippy -p fleetdeck-deck --all-targets -- -D warnings`
  - The guards (as T1). `wc -l` of `store/worker_grant.rs`, `store/owner_grants.rs`, `store/schema.rs`, `store/grants.rs`: each below 800.

### T4 (backend): the deck allows a worker bypass start and resolves the default mode

- **Owned files:**
  - `crates/fleetdeck-deck/src/store/coordinator_modes.rs` (new: `CLAUDE_MODES`, `CODEX_POLICIES`, `CODEX_SANDBOXES`, `CODEX_DEFAULT`, `is_bypass`, `mode_within`, `valid_ceiling` moved from `coordinator_ops.rs:40-53`, `:112-158`, re-exported from `coordinator_ops.rs` with `pub use` so no caller changes)
  - `crates/fleetdeck-deck/src/store/coordinator_ops.rs` (the move; in `start`, the default resolution and the call of `bypass_refusal` and `audit_bypass_start` in place of `:715-720`)
  - `crates/fleetdeck-deck/src/store/coordinator.rs` (`FleetEnv.knobs: DeckKnobs`; `authority_check` reads `self.knobs().worker_bypass` before the database call and calls `launch_refusal(&grant, &command, (family, launch.mode), worker_bypass)` in place of `mode_refusal` at `:753`; `mode_refusal` itself does not change)
  - `crates/fleetdeck-deck/src/store/mod.rs` (`fleet_env` sets `knobs: self.knobs()`; one line)
  - `crates/fleetdeck-deck/src/store/worker_bypass.rs` (new), `store/worker_bypass_tests.rs` (new), `crates/fleetdeck-deck/tests/worker_bypass.rs` (new)
  - `crates/fleetdeck-deck/src/store/fleet_control_narrowing_tests.rs` and `store/coordinator_tests.rs` (only the `FleetEnv` literals, if any, and the assertions of an outbox `spec.mode` that was `None` and is now the provider default)
- **Changes:** §3.4 (rules 1-5, the audit row, the dispatch check of revision 2, the texts) and the deck part of §3.5 (a concrete mode in every coordinator start).
- **Tests:**
  1. `a_worker_starts_a_bypass_session_when_every_switch_allows_it`: `knobs.worker_bypass = true`, a worker grant with the Claude ceiling `bypassPermissions`, the target station info offers it, the caller card in `bypassPermissions` → accepted; the outbox command has `mode = "bypassPermissions"`; one audit row `coordinator.worker_bypass_start` with the detail of §3.4.
  2. `each_missing_switch_refuses_the_bypass_start`: an owner grant (old text); the setting off (rule 2 text); the ceiling `auto` (ceiling text); a station without bypass in its modes (rule 4 text); the caller in `acceptEdits` (rule 5 text). No audit row, `spawn_used` unchanged.
  3. `the_codex_bypass_follows_the_same_rules`: `never/danger-full-access`.
  4. `the_dispatch_check_applies_the_switch`: the accepted start of test 1; then `worker_bypass = false`; `authority_check` with `LaunchMode { claude, bypassPermissions }` → `mode_not_allowed`; with the switch on → valid; a resume launch in bypass of the same grant → `mode_not_allowed`; the same start with the launch mode `acceptEdits` (an L8 fallback) → valid.
  5. `resume_and_set_mode_never_go_into_bypass`: an owner grant with the manage class; both refused with the old text, also with `worker_bypass = true`.
  6. `a_start_without_a_mode_gets_the_station_default_within_the_ceiling`: default `acceptEdits`, ceiling `auto` → `acceptEdits`; default `auto`, ceiling `acceptEdits` → `acceptEdits`; Codex default `never/workspace-write`, ceiling `on-request/danger-full-access` → `on-request/workspace-write`; the same through `start_in_project`.
  7. `an_older_station_gets_the_provider_default_written`: `default_mode: None` → the outbox command has `mode: Some("default")` (Claude) and `Some("untrusted/workspace-write")` (Codex); a Codex ceiling `untrusted/read-only` → refused with the ceiling text, as today.
  8. `a_bypass_station_default_is_never_used_silently`: default `bypassPermissions`, a worker ceiling `bypassPermissions`, the caller not in bypass → refused with the default-mode text; with every switch → a bypass start with its audit row.
  9. `meet_is_the_lower_mode`: Claude and Codex tables; unknown → `None`.
  10. `tests/worker_bypass.rs` (over the link with the fixture of `common`): a worker session in bypass calls `spawn` without a mode on a station whose default is `bypassPermissions` and that offers it; the station receives the start with that mode and its dispatch check is valid.
  11. `an_upgrade_between_decision_and_dispatch_never_launches_bypass` (finding 1): a worker grant with the ceiling `bypassPermissions`, `worker_bypass = true`, the caller in `acceptEdits`, the station info without `default_mode` → the outbox start has `mode: Some("default")`; `authority_check` for that command with `LaunchMode { claude, bypassPermissions }` (a station that filled a bypass default) → `mode_not_allowed`. A hand-written outbox row with `spec.mode: None` (a pre-B27 row) and the same launch mode → `mode_not_allowed`. A row with `spec.mode: Some("auto")` and the launch mode `bypassPermissions` → `mode_not_allowed`.
- **depends_on:** T3 (`DeckKnobs.worker_bypass`, `grant_rules.rs`).
- **Acceptance (at the merge):**
  - `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'test(worker_bypass) | test(coordinator) | test(fleet_control) | test(worker_grant) | binary(worker_bypass) | binary(coordinator_fleet) | binary(grant_stations) | binary(deck_link)'`
  - `cargo clippy -p fleetdeck-deck --all-targets -- -D warnings`
  - The guards (as T1). `wc -l` of `store/coordinator_ops.rs`, `store/coordinator.rs`, `store/coordinator_fleet.rs`, `store/mod.rs`: each below 800.

### T5 (backend): the station applies and reports its default mode

- **Owned files:**
  - `crates/fleetdeck-station/src/manager/policy.rs` (the field `default_modes: DefaultModeState`, `new`, `reconfigured` keeps `bypass_offered`, the test constructor at `:267`, the accessor; at most 12 lines added, the file stays below 800)
  - `crates/fleetdeck-station/src/manager/default_mode.rs` (new: `DefaultModeState`, `effective`, the mapping from `ProviderKind` to the family), `manager/mod.rs` (one `mod` line)
  - `crates/fleetdeck-station/src/manager/decide.rs` (the `else if spec.mode.is_none() && d.authority().is_none()` of §3.5 after `:446-448`)
  - `crates/fleetdeck-station/src/manager/manager_providers.rs` (`default_mode` in `provider_entry`)
  - `crates/fleetdeck-station/src/fleet_skill/SKILL.md` (the rule of §3.4)
  - `crates/fleetdeck-station/src/manager/default_mode_tests.rs` (new)
- **Changes:** the station part of §3.5, and the skill text of §3.4.
- **Tests:**
  1. `a_start_without_a_mode_gets_the_station_default`: Claude default `acceptEdits` → the stored summary mode is `acceptEdits` and the launch has `--permission-mode acceptEdits`.
  2. `a_start_with_a_mode_keeps_it`.
  3. `a_start_from_issue_text_ignores_the_default`: `origin_text` with default `auto` → `default`.
  4. `the_codex_default_is_used`: `on-request/workspace-write`.
  5. `an_import_without_a_mode_gets_the_default`.
  6. `station_info_reports_the_default_mode`: unset → `Some("default")` and `Some("untrusted/workspace-write")`; set → the value; a deferred bypass default → the provider default.
  7. `a_live_settings_change_updates_the_default`: `prepare` + `publish` with a new `[default_mode]`; the next start uses it.
  8. `a_bypass_default_without_the_restart_runs_supervised` (finding 5): a station started with `allow_bypass_permissions = false`; `publish` of a config with `allow_bypass_permissions = true` and `default_mode.claude = "bypassPermissions"` → a start without a mode is stored with `default` and launches `--permission-mode default`.
  9. `a_codex_full_access_default_without_the_restart_runs_supervised` (finding 5): the same with `default_mode.codex = "never/danger-full-access"` → the start succeeds (no `InvalidMode`) with `untrusted/workspace-write`.
  10. `a_bypass_default_after_the_restart_is_used`: a station started with `allow_bypass_permissions = true` and the bypass default → the start runs in `bypassPermissions`.
  11. `a_start_with_authority_never_gets_the_default` (finding 1): default `auto`; a coordinator start (a `Decision` with command authority) without a mode → the stored mode is `default` (the provider default), not `auto`.
- **depends_on:** T1.
- **Acceptance (at the merge):**
  - `cargo nextest run -p fleetdeck-station --no-fail-fast -E 'test(default_mode) | test(decide) | test(manager_providers) | test(fleet_skill) | test(policy)'`
  - `cargo clippy -p fleetdeck-station --all-targets -- -D warnings`
  - The guards (as T1). `wc -l` of `manager/policy.rs` and `manager/decide.rs`: each below 800.

### T6 (ux): the settings page

- **Owned files:** `web/src/settings/LimitEditor.tsx` (new), `web/src/settings/FieldEditor.tsx`, `web/src/settings/SettingsFields.tsx`, `web/src/settings/PostureLine.tsx` (new), `web/src/settings/SettingsTab.tsx`, `web/src/settings/model.ts`, `web/src/settings/copy.ts`, `web/src/settings/fixtures.ts`, `web/src/settings/limitEditor.test.tsx` (new), `web/src/settings/posture.test.tsx` (new).
- **Changes:** §3.7 (settings parts): the limit editor, the sections, the hazard table, the posture line (with the `state` suffixes of revision 2). Copy entries in `copy.ts` for the new labels only where the field label needs a board text.
- **Tests:**
  1. `limitEditor.test.tsx`: a number edit sends the number; "No limit" sends `"none"` and disables the input; unchecking restores the last number; the input respects `min`/`max`.
  2. `posture.test.tsx`: the three states of the posture line with their exact texts; the item list; the singular and the plural sentence; the four item suffixes of `RelaxedState`; the restart sentence with an `until_restart` or `after_restart` item.
  3. The hazard texts: one case per row of the hazard table.
- **depends_on:** T1, T2.
- **Acceptance (at the merge):** `bun run --cwd web typecheck`, `bun run --cwd web lint`, `bun test --cwd web src/settings`; `sh tools/design-check.sh` passes.

### T7 (ux): the spawn form and the grant displays

- **Owned files:** `web/src/spawn/fleetControl.ts`, `web/src/spawn/FleetControlFields.tsx`, `web/src/spawn/AgentFields.tsx`, `web/src/spawn/spawn.ts` (a `defaultModeOf(provider)` helper), `web/src/spawn/SpawnForm.tsx` (remove `:419`; pass the provider's default to `AgentFields`; the file must not grow), `web/src/fleet/limitText.ts` (new), `web/src/fleet/SessionAccess.tsx`, `web/src/fleet/GrantSpawn.tsx`, `web/src/session/FleetAnswersDialog.tsx`, the tests `web/src/spawn/fleetcontrol.test.tsx`, `web/src/fleet/grantSpawn.test.tsx`, `web/src/spawn/spawn.test.tsx` (the mode option), `web/src/fleet/limitText.test.ts` (new).
- **Changes:** §3.7 (spawn form, fleet control fields, display of a cap).
- **Tests:**
  1. `grantFields` sends `spawn_cap: 4294967295` and `answer_cap: 4294967295` with "No limit"; `idle_ttl_ms: 1800000` by default; `idle_ttl_ms: 4294967295` with "No limit".
  2. The new problem texts; no upper bound for the session and answer limits.
  3. The mode option reads `Station default (Accept edits)` with a reported default, `Default mode` without.
  4. `limitText(4294967295)` is `no limit`; `SessionAccess` and `FleetAnswersDialog` show it.
  5. `GrantSpawn`: "No limit" sends `NO_LIMIT`; it counts as widening (the elevation).
- **depends_on:** T1, T3.
- **Acceptance (at the merge):** `bun run --cwd web typecheck`, `bun run --cwd web lint`, `bun test --cwd web src/spawn src/fleet src/session`; `wc -l web/src/spawn/SpawnForm.tsx` is at most 792.

### T8 (docs)

- **Owned files:** `docs/v2/operations.md` (a part "Security options" with board steps: Settings → Deck → Worker grants / Grant limits; Settings → a station → Sessions; the posture line; the downgrade note of §3.8), `docs/v2/SECURITY-CHECKLIST.md` (rows for T1-T7 with threats T1-T10 in short and the test names), `docs/v2/adr/ADR-008-fleet-control.md` (§5.2 amendment: a worker grant may start a session in a bypass mode under §3.4, DL-543), `docs/v2/DECISIONS-LOG.md` (the entries below), `docs/v2/BACKLOG.md` (B27 status).
- **depends_on:** T1-T7.

### At the merge (coordinator)

- `make check` once, after T7.
- The adversary PRE-PR REVIEW of the final branch (high risk), with the acceptance commands above as its fast checks.

## 6. Decisions made in this draft

1. **One switch per control, no profile** (§2 Q2). Rejected: a "trusted home network" profile (it hides the values it sets).
2. **The `[security]` knobs are done** (§1.4). B27 fixes two stale comments only.
3. **The grant limits are deck settings in a new table `[grant_limits]`** with the old constants as the defaults (§3.2). Rejected: raising the constants (no owner choice); one "unlimited grants" switch (it hides which limit is off).
4. **"No limit" is the word `"none"` in a file, a checkbox on the board, `NO_LIMIT = u32::MAX` in a grant row; no idle limit is stored as the lifetime** (§3.3). Rejected: a nullable column (a larger migration and a type change through the proto and the board); a sentinel idle time (it reaches the idle arithmetic and the station); `0` as "no limit" (`0` already means "no starts" for a spawn cap).
5. **A new `FieldKind::Limit`** for a number or "none" (§3.2). Rejected: a bool key beside each number (twice the keys, and a value that can contradict its switch).
6. **The lifetime stays fixed and stays the hard cap** of every grant (§3.1 rule 5).
7. **A lower limit applies to new grants and cap changes only** (§3.1 rule 6; owner question 4).
8. **Deck migration V52 rebuilds `tokens`** with wider checks (§3.3). Rejected: keeping the checks (no raise possible).
9. **Bypass for workers needs three explicit switches and a worker that runs in a bypass mode itself; each start is audited** (§3.4). Rejected: the station switch alone (the owner turns it on for his own sessions, not for agents); bypass for owner grants (not asked; owner question 5).
10. **The deck checks the bypass switch at the decision and at the dispatch**, so "off" stops a queued start (§3.4).
11. **The default mode is per station, per provider family, in `station.toml`** (§3.5). Rejected: per instance (more keys for no asked need); per project (not cheap; owner question 2); a deck-wide default (a station decides what it allows).
12. **The default mode is validated against `allow_bypass_permissions` in the same file**, so it is never above the station ceiling (§3.5).
13. **A coordinator start without a mode gets the lower of the station default and its grant ceiling, written into the command by the deck, and never a silent bypass** (§3.5). Revision 2: the deck always writes a concrete mode (the provider default for an older station), and the station never applies its default to a command with authority.
14. **Issue and pull request text starts ignore the default** (§3.5).
15. **The posture is computed by the holder from the parsed file and sent in `SettingsView.relaxed`**; a guard test keeps it complete (§3.6). A tailnet address is not a relaxation; an unspecified address is.
16. **A relaxing save writes a second audit row `settings.relaxed`** with the keys (§3.6).
17. **Risk class high:** adversary DESIGN REVIEW before T3 and T4, PRE-PR REVIEW at the merge.
18. **The dispatch accepts a bypass launch only for the exact bypass mode in the stored start** (revision 2, §3.4). Rejected: re-running rule 5 at the dispatch (the caller's card can change for normal reasons, and it still would not prove that the decision checked a bypass mode); a separate "authorized bypass" column in the outbox (the stored `spec.mode` already carries it, written in the audit transaction).
19. **An omitted cap gets the default lowered to the limit** (`default_within`), then the normal check (revision 2, §3.2). Rejected: refusing a request that omits a cap when the default is above the limit (a caller cannot fix a value that it did not give).
20. **The spawn ceiling is chosen by grant kind** (`spawn_limit`, revision 2, §3.2). Rejected: a dependency rule "worker_spawn_cap ≤ spawn_cap" (two settings that the owner must change together, for no security gain).
21. **The posture is the effective posture** (revision 2, §3.6): the file merged with the running config and the command-line flags, with a state per item. Rejected: a "configured posture" label only (the board could then say "secure" while bypass is still offered).
22. **A bypass or full-access default waits for the adapters** (revision 2, §3.5): until the restart that applies `allow_bypass_permissions`, a start without a mode gets the provider default, for Claude and Codex. Rejected: refusing the save of the default (the owner saves both keys in one save and then restarts); sending the mode and relying on L8 (the Codex adapter fails the start with `InvalidMode`).

## 7. Owner questions

The build goes on with the defaults.

1. **Must a worker itself run in a bypass mode to start a session in a bypass mode?** It stops a supervised worker that reads hostile text from escalating. **Default: yes.** The answer "no" removes rule 5 of §3.4 (one check and two tests).
2. **A default mode per project too?** It needs a deck migration, the project screens and the project start paths. **Default: not in B27** (per station only).
3. **A "trusted home network" profile that sets many controls at once?** **Default: no** (one switch per control, and the posture line shows the whole state).
4. **Does a lower grant limit also lower the grants that exist?** **Default: no** (new grants and cap changes only; the owner revokes a grant on the board).
5. **May owner (board) grants with fleet control also get a bypass ceiling?** **Default: no** (only worker grants, as asked).

## Revision 2 (design review)

The adversary DESIGN REVIEW (`/home/luismorales/fleetdeck-briefs/backlog2/security-options/adv-design.md`, verdict FAIL, base `d1fa4205`, reviewed `c0b61779`) gave two BLOCKER and three MAJOR findings. Each finding was checked against the code on `d1fa4205`. All five are confirmed and fixed in this revision.

| # | Finding | Check against the code | Change |
| --- | --- | --- | --- |
| 1 | BLOCKER: a start that the deck queued with `mode: None` can be filled with a bypass default by a station that upgrades before the dispatch; the old dispatch rule (rules 1-3) accepted it without rule 5 and without the audit row. | Confirmed. `fleet_gate` lets a decision for a station without a live link through (`coordinator_gate.rs:46-50`), `enqueue` queues it (`coordinator_ops.rs:196-210`), and `authority_check` checks only the launch mode against the grant (`coordinator.rs:749-755`). The station fills a missing mode in `decide.rs::start` (revision 1). | §3.5: the deck always writes a concrete mode into a coordinator start (the provider default for an older station); the station never applies its default to a command with authority (`d.authority().is_none()`). §3.4: the new dispatch rule `launch_refusal` accepts a bypass launch mode only when the stored start has exactly that mode (written only after rules 1-5, in the audit transaction), for a worker grant, with the setting on, within the ceiling. §3.8 rows, §4 T1/T10/new T11, T4 test 11, T5 test 11, decision 18. |
| 2 | BLOCKER: an omitted answer cap gets `DEFAULT_ANSWER_CAP` (500) after the check, so it passes a lowered `grant_limits.answer_cap`. | Confirmed: `prepare_grant` checks only `Some(cap)` and fills 500 after (`worker_grant.rs:178-188`); the HTTP prechecks check only a given value (`grant_check.rs:119-127`, `grant_owner_start.rs:187-195`). The same is true for the spawn cap default 3 (`grant_check.rs:93`, `grant_owner_start.rs:107`, `owner_grants.rs:380`) and the answer default of an owner change (`owner_grants.rs:396`). | §3.2: new `default_within(default, limit)`; every omitted spawn or answer cap is resolved to `min(default, limit)` before the check, and the check runs on the resolved value. A grant without the spawn class is not checked for its unused spawn cap. §3.9 item 12, §4 T4, T3 tests 11-12, decision 19. |
| 3 | MAJOR: a worker spawn limit above 50 (or none) passes the config validation but fails at grant creation, because `prepare_grant` applies `grant_limits.spawn_cap` to every grant. | Confirmed: `prepare_grant` checks `MAX_SPAWN_CAP` for every grant before `check_worker_grant` (`worker_grant.rs:164-174`, `:247`); `spawn_caps` does the same (`grant_spawn_caps.rs:35-40`). | §3.2: new `spawn_limit(kind, limits)`; one spawn check per grant, by kind, in `prepare_grant` and in `spawn_caps`; `check_worker_grant` loses its spawn check. The two limits are independent. §3.9 item 13, §4 T12, T3 test 2 (now valid) and test 13, decision 20. |
| 4 | MAJOR: the posture is read from the file, so it can say "secure" while a restart field or a command-line flag keeps a relaxed value in effect. | Confirmed: `allow_bypass_permissions` is a `Restart` field (`settings/fields.rs:103-110`); the adapters are built once from it (`adapters.rs:38`, `:46`) and `provider_entry` reports their capabilities (`manager_providers.rs:155`); `Policy::reconfigured` takes the file value (`policy.rs:541-556`); the view only annotates the overrides (`settings/view.rs:67-86`), while `up.rs:96-100` merges `--listen` into the running config. | §3.6: the posture is the effective posture: `posture::<H>(file, running, overrides)` merges the file with the running config and the flags; each `Relaxed` item has a `RelaxedState` (`active`, `after_restart`, `until_restart`, `command_line`). The station and the deck settings services keep the config they started with (`with_running`, `DeckSettings.running`). §3.7 suffixes and the restart sentence, §3.9 items 10-11, §4 T8, T2 owned files and tests 9-13, T6 test 2, decision 21. |
| 5 | MAJOR: a full-access Codex default saved before the restart that applies `allow_bypass_permissions` makes every start without a mode fail with `InvalidMode`, not run supervised. | Confirmed: the Codex adapter refuses an unoffered mode before any process (`codex/driver.rs:196-203`, `codex/durable_launch.rs:35-39`); only Claude falls back (`claude/args.rs:103-107`). | §3.5: `DefaultModeState { configured, bypass_offered }` in the station policy; `bypass_offered` is the startup value and `reconfigured` keeps it; `effective` gives the provider default for a bypass or full-access default until the restart, for Claude and Codex. `StationProvider.default_mode` reports the effective default; the posture shows the default as `after_restart`. §3.9 item 2 corrected, §4 T5, T5 tests 8-10, decision 22. |

No finding was rejected.

## Decision log entries (to append)

## DL-540: B27 one switch per security control, secure by default, warned and shown in a posture (design agent, 2026-10-24)

- Context: B27; design `docs/v2/design/security-options.md` §2, §3.1.
- Decision: each relaxation is one settings key with today's secure value as the default, the elevation when it relaxes, a hazard text on the board while it is relaxed, and a line in the posture of §3.6. No profile sets several controls at once. The floors of the `[security]` knobs, the grant lifetime and the member deny list stay fixed. The `[security]` knobs are already read since K2; B27 fixes the stale comments of `config_deck.rs:72` and `config.rs:318`.
- Rejected: a "trusted home network" profile.

## DL-541: B27 grant limits are deck settings `[grant_limits]` with "no limit" (design agent, 2026-10-24)

- Decision: `grant_limits.worker_grants` (200), `spawn_cap` (50), `worker_spawn_cap` (10), `idle_hours` (24) and `answer_cap` (5000) replace `WORKER_GRANT_CAP`, `MAX_SPAWN_CAP`, `MAX_WORKER_SPAWN_CAP`, `MAX_IDLE_TTL_MS` and `MAX_ANSWER_CAP`. `spawn_cap` applies to owner grants and `worker_spawn_cap` to worker grants, never both. An omitted spawn or answer cap gets `min(default, limit)` and is then checked like a given cap. Each is a number or `"none"` (new `FieldKind::Limit`). `worker_grant.spawn_cap` and `worker_grant.idle_minutes` also take `"none"` when their limit is none. A raise needs the elevation. A limit applies at grant creation and at a cap change. A grant row stores no limit as `NO_LIMIT = u32::MAX` (spawn and answer caps) or as `idle_ttl_ms = ttl_ms` (idle). The lifetime stays the hard cap.
- Rejected: a nullable column; `0` as "no limit"; a bool switch beside each number.

## DL-542: B27 deck migration V52 rebuilds `tokens` with wider checks (design agent, 2026-10-24)

- Decision: `spawn_cap BETWEEN 0 AND 4294967295`, `idle_ttl_ms BETWEEN 1 AND 604800000`, `answer_cap IS NULL OR BETWEEN 0 AND 4294967295`; every other column (with `kind`), check, index and the foreign key of `coordinator_lineage` stay (the V38 pattern). Forward only. V52 is reserved; the merge renumbers if needed.

## DL-543: B27 a worker grant may start a session in a bypass mode (design agent, 2026-10-24)

- Decision: a coordinator start in a bypass mode is accepted only for a worker grant, while the deck setting `worker_grant.allow_bypass` is on (read at the decision and at the dispatch), within a bypass ceiling that the setting allowed at the grant creation, on a station that offers the mode (`allow_bypass_permissions` applied), and while the worker session runs in a bypass mode itself. Each accepted start writes the audit row `coordinator.worker_bypass_start`. The dispatch check accepts a bypass launch mode only when the stored start has exactly that mode, for a worker grant, with the setting on and within the ceiling. Owner grants, resume and set_mode keep the absolute rule of ADR-008 §5.2. The fleet skill says when a bypass start is allowed.
- Rejected: the station switch alone; bypass for owner grants.

## DL-544: B27 a default agent mode per station (design agent, 2026-10-24)

- Decision: `[default_mode]` in `station.toml` (`claude`, `codex`; absent: the provider default) is the mode of a start or an import that gives none, except a start from issue or pull request text. A bypass or full-access default needs `allow_bypass_permissions` in the same file. A more permissive default needs the elevation. The station applies it only to a start without command authority, and a bypass or full-access default only after the restart that makes the adapters offer it (until then: the provider default). The station reports the effective default in `StationProvider.default_mode`. For a coordinator start without a mode the deck always writes a concrete mode into the command: the lower of the reported default and the grant ceiling, or the provider default for an older station. It refuses a resolved bypass mode that the rules of DL-543 refuse.
- Rejected: per project (not in B27); per provider instance; a deck-wide default.

## DL-545: B27 the relaxed-security posture and its audit (design agent, 2026-10-24)

- Decision: `SettingsView.relaxed` lists every control that is weaker than its secure default in the file, in the running process or from a command-line flag, each with a state (`active`, `after_restart`, `until_restart`, `command_line`). The holder computes the items of one config (deck: the danger keys of the default against the current config, with listen addresses counted only when unspecified; station: an explicit list). Guard tests keep every danger field in the posture or in an explicit exclusion list. Each settings tab shows a "Relaxed security" line. `SettingsSaved.relaxed` carries the danger keys of a save, and the deck writes the audit row `settings.relaxed` with the keys, never the values.

## DL-546: B27 is high risk: adversary design review and pre-PR review (design agent, 2026-10-24)

- Decision: B27 lets agent-started sessions skip permission checks, removes grant limits and rebuilds the grant table. It gets the adversary DESIGN REVIEW before T3 and T4 start and the PRE-PR REVIEW of the final branch. `SECURITY-CHECKLIST.md` gets the rows of T1-T7.
