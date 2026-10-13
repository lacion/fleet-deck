# Settings and accounts in the board (DL-172, DL-173)

Status: design, revision 3. It answers round 1 (/tmp/fd-goal/adv-settings-accounts-r1.md; [R1]..[R14] below: R10 is the round-1 leakage item, R11 the elevation item, R12 the Jira PKCE item, R13 the OAuth API-scheme item, R14 the argv item) and round 2 (/tmp/fd-goal/adv-settings-accounts-r2.md; tags [S1]..[S4]).

Owner decisions, 2026-10-13:
- "all settings to control fleetdeck should be in the ui and … updated in flight": edit all, and restart when needed;
- "configure all trackers and forges directly in the UI";
- "use oauth for all 4".

## 0. Doctrine rule 5, amended (DL-173)

> A credential is stored only on the station that uses it, never on the deck or the board, and never in logs, audits, argv, command storage, receipts or responses.
>
> One exception is the **credential relay**: a value that the owner types in the board (an API token, an OAuth client secret, a provider API key) or an OAuth authorization code crosses the deck and the link once, in memory, inside one request and its answer. The relay never stores it, never logs it and never echoes it. The answer holds metadata only. A lost answer means that the owner types the value again.

**The relay [R6]:**
- A dedicated deck route family `/api/secrets/*` and link methods `accounts.*`.
  - These link methods are *not* receipt mutations.
  - They never go through `commands`, the outbox, receipts or the command routes.
  - The deck handler holds the value in a `Secret<String>` (Debug and Display redacted), forwards it, and drops it.
- **Body limit:** 16 KiB.
- **Errors:**
  - Every error answer is a fixed code with a fixed text.
  - No handler formats an error that holds the request body.
  - `ApiError::internal` is not used in these handlers. A typed error enum maps to fixed codes [R11].
- **Test:** a store test proves that no table holds the value after a relay call. It searches every text column of `deck.db` for a canary value.

## 1. Settings (DL-172)

### 1.1 Scope and the field table

- `#/settings` (owner only) has these tabs:
  - **Deck**
  - **one tab per station**
  - **Accounts** (section 2)
- **One field table per holder,** in code beside the parser: `FieldMeta { key, label, help, kind, min?, max?, options?, apply: Live | Restart | CliOverride, danger: bool, secret: bool }`. A test checks that every parser field has exactly one entry. A new parser field without an entry fails the build.
- **Secret fields [R1]:**
  - `secret: true`, for example `provider_instances[*].env` values and any `*_token`/`*_key` key.
  - A read never returns their value. It returns `{ set: bool }`.
  - A save without the key keeps the stored value unchanged.
  - A new value goes only through the relay route `POST /api/secrets/stations/{id}/settings { key, value }`, under the same lock as a save (1.3).
  - The value is stored in `<station data>/secrets/<name>` (mode 0600, a private path, 2.1), never in the TOML file [S4].
  - The TOML file keeps a reference instead: `env.ANTHROPIC_API_KEY = { secret = "provider-claude-ANTHROPIC_API_KEY" }`.
  - The parser accepts a plain string (as today) or `{ secret = "<name>" }`.
  - A save that sets a secret converts a plain value to a reference and moves the plain value into the store. The file then holds no credential.
  - **Write order [T1]:**
    1. Write each new secret under a fresh, never-reused name (`<name>-<random>`): an O_EXCL 0600 temp file, write, fsync, rename to the final name, fsync the `secrets/` directory.
    2. Only then run the TOML commit of 1.3, which points to the new name.
    3. Only after the TOML rename succeeds, delete the secrets that no reference names any more: the old name, and a plain inline value that was moved. A crash before step 3 leaves an orphan secret file, which is harmless. The holder deletes orphans at its next start.
    4. A crash between steps 1 and 2 leaves the old TOML, which still has its old reference or its inline value.
  - **Tests:** a crash test simulates a stop after each step (1, 2, 3) and checks that the installed TOML always resolves to a complete secret.
  - The deck has no secret-bearing settings.
  - The response allowlist is enforced in the holder: the read path builds the answer from the field table, and only fields with `secret: false` are copied. A test sets a canary secret and asserts that it is absent from the read and save answers.
- **CLI overrides [R13]:**
  - A field that the running process took from a command-line flag (for example `fleetdeck up --listen`) has `apply: CliOverride`, with the flag name.
  - The UI shows it read-only: "Set by the command line (`--listen`). Change the command to change it."
  - The file value is still editable. It applies when the process starts without the flag.

### 1.2 Wire

- **Deck:**
  - `GET /api/settings/deck` returns `{ values, fields, revision, overrides }`.
  - `POST /api/settings/deck { changes, revision, elevation? }` returns `{ values, applied, restart_needed, unapplied, revision }`.
- **Station:**
  - Link methods `settings.get` and `settings.set` (scope `admin`, feature `settings.v1`). They are *not* receipt mutations: the revision makes a retry safe, because a repeat with an old revision answers `settings_changed`.
  - Deck routes `GET` and `POST /api/stations/{id}/settings`.
- **Owner only:** every route requires `Principal::Owner`.
  - A token or a member gets 403 `owner_only`, also with the `admin` scope [R3].
  - A method table test lists these routes as owner only.
  - The audit row holds the changed keys, never values.

### 1.3 Save: serialized, atomic, private [R5]

The holder runs one save at a time, under a per-holder async mutex. Inside the lock:
1. Read the file bytes and compare their SHA-256 with `revision`. When they differ, answer 409 `settings_changed`. This also catches a hand edit since the board read.
2. Apply the changes to a `toml_edit` document of those bytes, so that the comments and the order stay.
3. Parse the new text with the existing parser (the full validation). When it fails, answer 400 with the field key and a fixed message, and keep the value out of the message.
4. Prepare the live changes (1.4, step 1). When that fails, answer 400 `cannot_apply` with the key, and the file is unchanged.
5. Write the new bytes:
   - create `<file>.tmp-<random>` in the same directory with `O_CREAT|O_EXCL` and mode 0600;
   - write, and fsync;
   - parse the temp file again (the exact bytes);
   - rename over the file, and fsync the directory.
   On an error before the rename, remove the temp file and answer 500 `settings_write_failed`; the file is the old one. After the rename, see "Commit boundary".
6. Publish the prepared changes (1.4, step 2). This step cannot fail.

- The loader warns at start when a config file is readable by the group or by others. A save always leaves the file 0600.
- **One writer while the holder runs [S1]:**
  - While a deck or a station runs, its holder is the only supported writer of its config file: the board, or the local CLI `fleetdeck settings set`, which calls the same holder API over the owner-only local socket.
  - A raw hand edit is supported only while the process is stopped. operations.md and a comment at the top of each config file say so ("Edit with the board or `fleetdeck settings`; edit by hand only while Fleet Deck is stopped").
  - When the holder starts, it reads the file.
  - At every save, the holder compares the file's bytes and mtime with what it last read or wrote. A difference (a hand edit while it ran) gives 409 `settings_changed_on_disk` with the advice to reload. This detects a hand edit but does not lock against one; that case stays unsupported.
- **Commit boundary [S2]:** the rename is the commit point. Before it, any error leaves the old file and the old policy, and the answer is an error.
  - After a successful rename, the holder always publishes the new snapshot from the installed bytes (step 6), even when the directory fsync fails.
  - In that case, the answer is success with `durability: "uncertain"`, and the board shows "Saved and applied; the disk did not confirm the write. Check the disk."
  - There is no rollback after the rename.
  - So the runtime always matches the installed file.

### 1.4 Live apply: one versioned snapshot [R7]

- **One snapshot.**
  - The station gets a `PolicyHub`: `RwLock<Arc<PolicySnapshot>>` with a version number.
  - `PolicySnapshot` holds configuration only.
  - The runtime state moves out of `Policy` into its own holders, which are built once: coordinator bindings, the integration service handle, assist, shells.
  - Every consumer reads the current snapshot when it handles a request or a dispatch, never once at startup:
    - the manager decisions;
    - target dispatch, which checks again at dispatch;
    - file routes;
    - shell open;
    - the hibernation ticker, which reads the snapshot and its interval each tick;
    - assist, which reads `model_help` per call;
    - integrations, whose trackers come from the snapshot and the account store.
- **Two steps.**
  - Step 1, *prepare* (fallible): build the new snapshot, and build the objects that must be rebuilt, for example the tracker clients. Nothing is published.
  - Step 2, *publish* (infallible): swap the pointer.
- **Effects on running things:**
  - Approvals that are already open are not changed.
  - A running shell keeps running when `allow_shell` turns off. New shells are refused, and the UI says so.
  - A queued command is checked again at dispatch against the new snapshot.
  - A running session keeps its folder when `allowed_roots` narrows. New requests outside the roots are refused.
- **Deck:** the same pattern, with `conflict_mail` and `name` as live fields.
- **Result fields:** `applied` lists the live keys. `restart_needed` lists the restart keys. `unapplied` is always empty, because publish cannot fail; the field exists for a future live key that cannot apply at once.

### 1.5 Danger changes need a recent sign-in [R12]

- **Danger fields:**
  - turning on `allow_setup_cmd` or `allow_shell`;
  - adding to `allowed_roots`;
  - any `listen` or `link_listen` that is not loopback;
  - any `provider_instances` binary or environment change;
  - `host`;
  - `offline_approval_policy` set to a more permissive value.
- **Elevation:** a save with a danger change needs a server-checked elevation [S3].
  - `POST /api/auth/elevate { password }` or `{ code }`.
    - The **owner password**, when the owner has set one (argon2id, `store/auth.rs`).
    - Otherwise a **one-time elevation code** that the owner gets on the deck machine with `fleetdeck open --elevate` through the owner-only local socket. The code is 10 characters, single use, lives 5 minutes, and is stored hashed.
  - Only a `Principal::Owner` board login can elevate. A token or a member gets `owner_only`.
  - It answers `{ elevation, expires_at }`, which is bound to the session hash and lives 10 minutes.
  - The save passes `elevation`. The deck checks it for danger changes, also for a station save (the deck enforces it before it forwards).
  - The same failure limiter as the login applies.
- **Confirm:** the UI asks for the password, or tells the owner to run `fleetdeck open --elevate` and type the code. It shows the exact danger changes.

### 1.6 Restart [R4, R13]

- **Request:** `POST /api/settings/restart { target: "deck" | station_id, stop_direct_sessions: bool }` and link method `station.restart`. Both are owner only and need an elevation.
- **Before a restart,** the holder lists what a restart ends:
  - every live session that is hosted `direct` (the default); these are stopped, and their open approvals expire;
  - open shells, which are closed;
  - running setup commands and prechecks, which are killed (their group);
  - for `fleetdeck up`, both parts restart.

  Shim-hosted (durable) sessions are detached and kept.
- **Refusal:** when the list has a direct session and `stop_direct_sessions` is false, the answer is 409 `restart_would_stop_sessions` with the list. The UI shows the sessions and "Restart and stop N sessions".
- **Re-exec:** the process re-executes itself with the immutable startup context that was captured at start. That context holds the argv, the environment and the executable path from `/proc/self/exe` (Linux) or `current_exe()` (macOS). No request data enters it.
- **Failure:** when `execve` fails after the drain, the process exits with code 75 and logs "restart failed: <errno>". The board shows the deck as offline. This is documented in operations.md.

## 2. Accounts (DL-173)

### 2.1 Private paths [R2]

- **Credential directory:** `<station data>/accounts/` and the whole station data directory are *private paths*.
  - Every file, search, diff, session diff and artifact route refuses them for every principal, the owner too. This holds also when they are inside an allowed root or are the workspace root.
  - The check uses canonical paths, before any read, and does not depend on `member_deny`.
- **Config files [S4]:** the deck's and the station's own config files (`station.toml`, `deck.toml`, whatever paths the running process loaded), their directory's temp files (`*.tmp-*`) and the `secrets/` store are private paths too. The check takes the actual loaded paths, so the default split layout (`~/.config/fleetdeck` and `~/.local/share/fleetdeck`) and a custom `--data-dir` are both covered.
- **Defaults:** add `.config/gh/`, `.config/glab-cli/`, `.claude/.credentials.json`, `.codex/auth.json` and `.ssh/` to this list. The owner cannot read them from the board either.
- **Tests:** each route (list, read, search, turn diff, session diff, artifact publish) reads the data directory, `station.toml` and the secret store through a root that contains them, for the owner and for a member, and gets a refusal. The test uses the default split layout.

### 2.2 GitHub and GitLab: the CLI device flow [R8, R9]

**Start:** `POST /api/stations/{id}/accounts/forge/login { kind, host }` (owner only), via link method `accounts.forge_login`.
- **Host:**
  - The host must be in the station's list of known forge hosts: `github.com`, `gitlab.com`, and the hosts of configured projects.
  - It is a bare host name; `[a-z0-9.-]`, at most 253 characters.
- **Command:**
  - GitHub: `gh auth login --hostname <host> --web --git-protocol https --skip-ssh-key`.
  - GitLab: `glab auth login --hostname <host> --device`.
- **Environment:**
  - It is scrubbed, with `GH_TOKEN`, `GITHUB_TOKEN`, `GH_ENTERPRISE_TOKEN`, `GITHUB_ENTERPRISE_TOKEN`, `GITLAB_TOKEN`, `GITLAB_ACCESS_TOKEN` and `OAUTH_TOKEN` removed.
  - `BROWSER=true` and `GH_PROMPT_DISABLED=1` are set.
  - The process has no TTY and its own process group.
- **Token in the environment:** when the station's own environment has one of those token variables, the status says "A token in the station environment overrides the login (GH_TOKEN)", and the login is refused with `env_token_present`.
- **Output:**
  - The station reads **both stdout and stderr**. Each stream is capped at 64 KiB and drained to the end; the rest is dropped.
  - It parses the code and the URL with patterns pinned per CLI version range, and tested on recorded fixtures of `gh` 2.102 and `glab` 1.120 (no TTY; with an existing login; with a refusal).
- **Validation:**
  - The code must match `^[A-Z0-9]{4}-[A-Z0-9]{4}$`, or the pinned glab pattern of at most 16 characters.
  - The URL must be `https`, have no userinfo, have a host equal to the forge host (`github.com` for GitHub; the GitLab host), and have a path from a fixed list (`/login/device`, `/oauth/device`).
  - Anything else fails with `unexpected_cli_output`.
  - The board shows the code as text, and the URL as a link only after this check.
- **Limits:**
  - One login per (station, host) at a time.
  - At most three logins per station at once.
  - The process group is killed after 15 minutes.
  - `glab` versions or GitLab servers without device flow (the CLI error) give `device_flow_unsupported`. The UI then offers the token fallback (2.4).
- **Status:** `pending | done { user } | failed { code } | expired`, from `GET …/accounts/forge/login/{login_id}`.

### 2.3 Jira Cloud and Linear: OAuth with an owner-registered app [R3, R10, R11, R14]

**App:**
- The owner registers an app once:
  - Atlassian 3LO, with scopes `read:jira-work write:jira-work read:jira-user offline_access`;
  - Linear, with scopes `read write`.
- The client id and secret go through the relay: `POST /api/secrets/stations/{id}/accounts/apps`.
- The station stores them under `accounts/`, mode 0600. A read gives `{ client_id, has_secret, generation }`, and `generation` increases on each change.

**Flows:**
- **Jira:** a confidential client with a client secret and a session-bound `state`. No PKCE claim [R14].
- **Linear:** PKCE (S256), plus the client secret and the state.

**Start:** `POST /api/accounts/oauth/start { station, kind }` (owner only).
- The deck makes the transaction:
  - `state`: 32 random bytes;
  - bound to: the SHA-256 of the owner login session, the station, the kind, the app generation, the exact redirect URI (`<board origin>/oauth/callback`, from the request's `Origin`, which must be a board origin), and `created_at`.
- The deck keeps it in memory only, with a 10-minute TTL.
- It calls `accounts.oauth_start { kind, state, redirect_uri }` on the station. The station keeps `{ state, verifier }`, and returns `authorize_url`. The deck checks the URL: https, and the host `auth.atlassian.com` or `linear.app`.

**Callback:** `GET /oauth/callback?code&state`.
- It needs no cookie: the provider redirect is cross-site, so the SameSite=Strict cookie is not sent.
- The handler:
  - claims the transaction by `state` atomically (one use);
  - keeps `code` in that in-memory transaction for 120 seconds;
  - answers `303 See Other` to `/#/settings/accounts?finish=<transaction id>`, with `Cache-Control: no-store` and `Referrer-Policy: no-referrer`.
- An unknown or used state gets the same 303 with `?oauth_error=invalid_state`.
- The handler never logs the query string. The access log (if any) records the path only.
- A redirect does not leave the callback URL as its own history entry.

**Finish:** the board page loads under the same origin, with the cookie, and calls `POST /api/accounts/oauth/finish { transaction }` with CSRF.
- The deck checks that the caller is the owner and that its session hash equals the one bound at start. So the same live owner login is required [R3].
- Then it forwards `{ state, code }` with `accounts.oauth_finish` (relay rules), and drops the transaction.
- The station checks the state, the generation and the redirect URI, exchanges the code (Jira: with the client secret; Linear: also with the PKCE verifier), and stores the account.

**Account types and API use [R10]:**
- Jira OAuth: `Authorization: Bearer`, API origin `https://api.atlassian.com/ex/jira/{cloudid}`; display URLs come from the site URL of the accessible resources.
- Jira token: Basic `email:token` on the site URL, as today.
- Linear OAuth: `Authorization: Bearer <token>`.
- Linear key: `Authorization: <key>`, as today.
- Assets use the same scheme as their account: an OAuth account gets images only through the API origin.

**Refresh:**
- There is one refresh owner per account: a mutex in the station.
- Atlassian refresh tokens rotate, so the new pair is persisted atomically (temp file and rename, 0600) before use.
- When a refresh fails, the account becomes `needs_login`.

**Leakage [R11]:**
- Fixed error codes only.
- No upstream body text in logs: an upstream error is reduced to its status and a fixed code.
- The deck and the station set `RLIMIT_CORE` to 0 at start.
- Credential holders are `Secret` and zeroize on drop.
- No tracing layer records request bodies; a test checks the tracing setup.
- The board stores no credential in local storage or in session storage, and its forms clear their fields after a submit.

### 2.4 Token fallback

- Inputs: a Jira email and API token, a Linear personal key, or a GitHub or GitLab personal token when the device flow is not supported.
- The token goes through the relay `POST /api/secrets/stations/{id}/accounts/token`.
- For a forge, it is passed to `gh auth login --with-token` or `glab auth login --stdin` on **stdin**, never in argv.
- The read gives only `{ kind, site, user, method: "token" }`.

### 2.5 Projects

- A project picks a connected account, then a container from `issues.containers { account }`, which returns `[{ key, name }]`:
  - Jira: `/rest/api/3/project/search`;
  - Linear: `teams`.
- `[[trackers]]` entries from `station.toml` still work. They show as "from the config file" and are read-only in the UI.

## 3. Threats and defenses (summary)

| Threat | Defense |
| --- | --- |
| Secrets in settings answers | field table allowlist, `{set}` for secrets, canary test (R1) |
| Credential files through file routes | private paths for every principal (R2) |
| Callback CSRF / login swap | state bound to the owner session hash + station + kind + generation + redirect URI; finish requires the same owner login; one-use claim (R3) |
| Restart kills work | list and refuse; explicit "stop N sessions"; durable sessions kept (R4) |
| Lost edits / torn file | one writer while running; per-holder mutex; revision and mtime check; O_EXCL 0600 temp, re-parse, rename; rename is the commit point, then always publish; uncertain durability reported (R5, S1, S2) |
| Secrets in command storage | separate relay routes, never commands/outbox/receipts; canary test (R6) |
| Stale policy | one versioned snapshot, read per request; prepare then infallible publish (R7) |
| gh output on stderr; env tokens | both streams; env tokens removed or refused (R8) |
| Phishing URL / big output | host, path and scheme checks; 64 KiB caps; process limits (R9) |
| Wrong auth scheme for OAuth | typed account kinds with their own headers and origins (R10) |
| Leaks in history, logs, dumps | 303 to a clean URL, no-store, no-referrer, fixed codes, no core dumps (R11) |
| Stolen board session | a server-checked elevation (owner password or a local one-time code) for danger changes (R11, S3) |
| Secrets written to station.toml | secret store in the data dir with TOML references; config files are private paths (S4); fresh-name secret write, fsync before the TOML commit, old secret removed after it (T1) |
| argv overrides | CliOverride fields shown read-only; immutable startup context (R13) |
| Jira PKCE assumption | confidential client with a bound state; PKCE only for Linear (R14) |
