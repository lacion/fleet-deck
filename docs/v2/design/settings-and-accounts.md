# Settings and accounts in the board (DL-172, DL-173)

Status: design, for the adversary DESIGN REVIEW. Owner decisions, 2026-10-13:
- "all settings to control fleetdeck should be in the ui and we should be able to update them in flight"; edit all, restart when needed;
- "configure all trackers and forges directly in the UI";
- "can we use oauth for all 4 of them?"

## 1. Settings (DL-172)

### 1.1 Scope

Every field of `deck.toml` and of each station's `station.toml` is shown and editable on a new page `#/settings` (owner only):
- tabs "Deck", one tab per station, and "Accounts" (section 2);
- the file stays the one source of truth: the UI reads the file, and a save writes the file.

### 1.2 Wire

- **Deck:**
  - `GET /api/settings/deck` returns `{ values, fields: [FieldMeta] }`.
  - `POST /api/settings/deck { changes: {key: value}, revision }` returns `{ values, applied: [key], restart_needed: [key], revision }`.
- **Station:**
  - Link methods `settings.get` and `settings.set` (scope `admin`, a new feature `settings.v1`), with the same shapes.
  - Deck routes `GET/POST /api/stations/{id}/settings`.
  - A receipt mutation, so that a retry is safe.
- **Field metadata.** `FieldMeta { key, label, help, kind (bool|int|string|enum|path|paths|list|secret_ref|table), min?, max?, options?, live: bool, danger: bool }` comes from code, next to the config parser, in one table per file. A test checks that every parser field has exactly one entry.
- **Revisions.** `revision` is the SHA-256 of the file bytes. A save with an old revision gets 409 `settings_changed`, and the UI reloads it.
- **Owner only.** All routes need the owner's board login (`owner_only` for tokens and members), CSRF, and an audit row with the changed keys. The audit row never holds values, because some values are paths or names.

### 1.3 Write and apply

- **Write.** The holder (deck or station) validates the whole new config with the existing parser before it writes anything. Then it writes the file atomically (temp file, fsync, rename) and keeps the comments and the order with `toml_edit` (already in the lock file through `toml`; it becomes a direct dependency of fleetdeck-core).
- **Live fields** apply at once through a new `apply(&old, &new)` per holder.
  - Station: `max_sessions`, `allow_setup_cmd`, `allow_shell`, `max_shells`, `offline_approval_policy`, `hibernate_after_min`, `member_deny`, `model_help.*`, `trackers` (the integration service is rebuilt), `repo_groups`, `allowed_roots` (only for new requests; a running session keeps its folder).
  - Deck: `conflict_mail`, `name`.
- **Restart fields:** station `listen`, `host`, `shim_buffer_mib`, `provider_instances`; deck `listen`, `link_listen`, `tailnet_hostname`. The answer lists them in `restart_needed`, and the UI shows "Applies after a restart" with a "Restart" button.
- **Restart.**
  - Deck route `POST /api/settings/restart { target: "deck" | station_id }` and link method `station.restart`.
  - The process drains (the same graceful stop as SIGTERM, which never kills a provider session: sessions are resumed after the restart, as today) and re-executes itself with the same argv and environment (`execve` of `/proc/self/exe`, or `std::env::current_exe()` on macOS).
  - `fleetdeck up` restarts both parts.
  - The board shows "Restarting…" and reconnects.
- **Danger fields** (`allow_setup_cmd`, `allow_shell`, `allowed_roots`, `listen` with a non-loopback address) need a second confirm in the UI. `allowed_roots` cannot be emptied.

### 1.4 What is not a setting

The station identity key, the pairing, the board login keys and the tokens are not settings. Section 2 handles accounts.

## 2. Accounts: GitHub, GitLab, Jira, Linear (DL-173)

Doctrine rule 5 today: credentials stay on the station, never on the deck, the link, the board, logs or argv.

Proposed change: **credentials are never stored on the deck or shown on the board.** Two short-lived values may cross the deck and the link once, write-only, and are never logged or audited:
- an OAuth authorization code (single use, minutes);
- an OAuth client secret or an API token that the owner types once.

### 2.1 GitHub and GitLab: the CLI's own OAuth device flow

- `POST /api/stations/{id}/accounts/forge/login { kind: github|gitlab, host }` (owner only, CSRF), via link method `accounts.forge_login`.
- The station runs `gh auth login --hostname <host> --web --git-protocol https` or `glab auth login --hostname <host> --device`, with the scrubbed environment, no TTY, and `BROWSER=` set to a no-op so that nothing opens on the station machine.
- It reads the one-time code and the verification URL from stdout (patterns in code with fixtures). It answers `{ login_id, code, url, expires_in }`, and the board shows them with "Copy code" and "Open <url>".
- The CLI waits for the user and stores its token in its own config (`gh` keyring or hosts file; `glab` config), as a manual login does today.
- The board polls `GET …/accounts/forge/login/{login_id}` → `pending | done { user } | failed { message } | expired`. One login per host at a time; the station kills the CLI after 15 minutes.
- "Log out": `gh auth logout --hostname <host>` / `glab auth logout --hostname <host>`.
- Status comes from `integrations.status` (already there).
- No token ever crosses the deck.

### 2.2 Jira Cloud and Linear: OAuth 2.0 authorization code with PKCE

A self-hosted deck cannot ship a shared client secret, so the owner registers one OAuth app once:
- Atlassian: developer console, OAuth 2.0 (3LO), scopes `read:jira-work write:jira-work read:jira-user offline_access`;
- Linear: Settings, API, OAuth applications, scopes `read write`.

The UI guides this step by step and shows the exact redirect URL to paste.

- **Redirect URL:** `<board origin>/oauth/callback`. This is a deck route without a login cookie (the provider redirects the browser). The board origin is the one that the owner uses, and the UI shows it.
- **App setup:** "Add a Jira (or Linear) app" with client id and client secret, via `POST /api/stations/{id}/accounts/apps` and link method `accounts.app_set`.
  - The station stores the secret in `<station data>/accounts/<kind>-app.json`, mode 0600, and never returns it. A later read gives only `{ client_id, has_secret: true }`.
- **Connect:** "Connect Jira" calls `POST …/accounts/oauth/start { kind }`, and the link method `accounts.oauth_start` answers `{ authorize_url }`.
  - The station makes `state` (32 random bytes, base64url) and a PKCE verifier and challenge (S256), and keeps them in memory for 10 minutes.
  - It also gives the deck the state's hash and its station id, so that the deck can route the callback.
  - The board opens `authorize_url` in a new tab.
- **Callback:** `GET /oauth/callback?code&state` on the deck.
  - It looks up the state hash (one use; 10-minute TTL; an unknown state gets a plain error page) and forwards `{ state, code }` to that station with link method `accounts.oauth_finish`.
  - The station checks the state, exchanges the code at the token endpoint with the client secret and the PKCE verifier, and stores `{ access_token, refresh_token, expires_at, site/workspace }` in `<station data>/accounts/<kind>-<account id>.json`, mode 0600.
  - For Jira, the station reads the accessible resources (`/oauth/token/accessible-resources`) and stores the cloud id and site URL for each site.
  - The page then shows "Connected. You can close this tab." and redirects to `#/settings/accounts`. The page has no script that reads the URL.
  - The code is in the URL for one hop (deck → station). The deck route never logs the query, and the access log redacts `code` and `state`.
- **Refresh:** the station refreshes the token before expiry. A failed refresh marks the account `needs_login`, and the board shows "Reconnect".
- **API token fallback** (no OAuth app): a Jira email and API token, or a Linear personal API key, typed once in the UI. It crosses deck → station write-only (`accounts.token_set`) and is stored 0600. It never comes back; the read gives only `{ kind, site, user, method: "token" }`.
- **Trackers:** the station's integration service reads accounts from these files, in place of `station.toml` `[[trackers]]` (still read, and shown as "from the config file", read-only in the UI).
- **Projects:** a project picks a connected account and then a Jira project or a Linear team from a list. This needs new methods:
  - `issues.containers { account }` → `[{ key, name }]`: Jira `/rest/api/3/project/search`, Linear `teams`.

### 2.3 Threats to review

- **CSRF on the callback.** An attacker sends the owner a link with their own code. Defenses: the state is bound to a start from the owner's login (one use, 10 minutes), and PKCE.
- **Open redirect.** The callback redirects only to the fixed board path.
- **Code or token leakage:** in logs, the audit, the browser history (the code stays in the history of the callback URL), the Referer header (the callback page sends `Referrer-Policy: no-referrer`), and crash reports.
- **A member or a token.** Every accounts route is owner only. A member never sees accounts.
- **Station files.** Mode 0600 under the station data directory; refused when the directory is group or world writable.
- **The CLI device flow.** Parsing untrusted stdout. A CLI that opens a browser on the station is blocked with `BROWSER`. Two logins at once are refused.
- **Settings writes as a privilege change.** Turning on `allow_setup_cmd` or `allow_shell`, or widening `allowed_roots`, from a stolen board session. Mitigation: the owner login plus a second confirm. Proposed: also require a re-login within 10 minutes for danger fields. Open question for the review.
- **Restart through `execve`.** The argv and the environment are taken from the running process, never from the request.
