# Built-in Linear app (DL-185)

- **Status:** Proposed (revision 2, 2026-10-14). Revision 1 got DESIGN REVIEW FAIL (2 blockers, 3 major, 1 unverified, 1 minor; `/tmp/fd-goal/adversary-report-linear-design.md`); §8 maps each finding. Needs a PASS before code.
- **Owner request (2026-10-14):** "for jira and linear, github and gitlab can we create oauth apps that will just work for everyone? apps that we can publish?" Owner choices: "Linear public app, no broker", then for the redirect "Localhost only".
- **Changes:** DL-173 (the owner registers a Jira or Linear OAuth app before a sign-in). Jira, GitHub and GitLab are unchanged.

## 1. Facts today

- The station keeps one owner app per tracker (`StoredApp { client_id, client_secret, generation }`, `crates/fleetdeck-station/src/accounts/store.rs:44`), set through the deck's credential relay (`AppSet`), never stored on the deck.
- `accounts.oauth_start` (`crates/fleetdeck-station/src/accounts/oauth.rs:153`) needs the stored app, checks the redirect (`https`/`http`, no userinfo, path `oauth/callback`), and for Linear builds `https://linear.app/oauth/authorize?client_id=…&redirect_uri=…&response_type=code&scope=read,write&state=…&code_challenge=<S256>&code_challenge_method=S256`; the transaction keeps the PKCE verifier in memory.
- The deck makes the redirect `<board Origin>/oauth/callback`, binds the transaction to the owner's login session, and relays `{ state, code }` to `accounts.oauth_finish`. The transactions live in memory on the deck and on the station: a restart of either drops them.
- `finish_linear` (:317) and `refresh` (:426) need the stored owner app (`oauth.rs:233-238`, `:428-432`) and send `client_id` and `client_secret`; `refresh` uses the app that is stored now, not the one that made the account.
- `save_account` (`crates/fleetdeck-station/src/accounts/mod.rs:372-395`) replaces an existing account whose kind, method, site, cloud id and email match.
- `accounts/http.rs:84-106` never returns an upstream body: a non-2xx answer is `upstream` "the server answered HTTP <status>", a curl failure is `upstream`/`timeout` with a fixed text (DL-173).
- Linear's OAuth documentation (read 2026-10-14): with PKCE the token request takes `code_verifier` and `client_secret` is optional; a token made with PKCE refreshes with `client_id` only. The documentation does not state how Linear matches a redirect URI.

## 2. Design

### 2.1 The built-in app

- The station carries one constant: `LINEAR_BUILTIN_CLIENT_ID = "66edc8f79eb0603f2aded18705bdfb99"` (the public client id of the "Fleet Deck" Linear app, registered by the project owner as a public app). No client secret exists in the code, the repository or any build. A client id is public by nature; it authenticates nothing.
- The Linear app registers exactly two redirect URIs: `https://localhost:7420/oauth/callback` and `https://127.0.0.1:7420/oauth/callback`. IPv6 (`[::1]`) is not supported.
- The scope stays `read,write` (Fleet Deck creates and comments on issues; no admin scope).
- The effective app of a new Linear sign-in is the owner's stored app when one is set (unchanged), else the built-in app. The code models an app as `EffectiveApp { source: Builtin | Owner, client_id, client_secret: Option<Secret> }`; the built-in app has no secret, and every path that needs the owner app today (`oauth.rs:233-238`, `:428-432`) uses the account's or transaction's own app instead.
- Jira has no built-in app.

### 2.2 Sign-in with the built-in app

- `accounts.oauth_start` for Linear without an owner app: the redirect URI must be exactly `https://localhost:7420/oauth/callback` or `https://127.0.0.1:7420/oauth/callback` (byte-equal after the existing checks). Else the stable code `builtin_needs_localhost` with the fixed text "The built-in Linear app works only from the board on the deck machine at https://127.0.0.1:7420 or https://localhost:7420. Open the board there, or set up your own Linear app." Nothing is sent to Linear.
- Because the redirect equals the origin that started the sign-in, the browser has already accepted the board certificate for that exact origin and holds a working owner session there; the callback lands on the same origin. If the deck restarted or stopped listening in between, the callback answers `invalid_state` (the in-memory transaction is gone) and the board says "The sign-in expired. Sign in again." No other recovery exists: Fleet Deck never asks the user to copy a callback URL or a code.
- The transaction records its `EffectiveApp` (built-in: generation `0`); an owner app that is set while the sign-in is open makes the finish fail as an app change (the existing generation rule).
- `accounts.oauth_finish` with the built-in app: the token request sends `grant_type`, `code`, `redirect_uri`, `client_id` and `code_verifier`, and no `client_secret`.

### 2.3 The account remembers its app

- A stored Linear account gets two fields: `app_source: "builtin" | "owner"` and `app_client_id: String` (the client id that made it). They are written in the same atomic file write as the account (the existing `put_account`).
- **Identity:** the upsert of `save_account` matches only an account with the same kind, method, site, cloud id, email, `app_source` and `app_client_id`. Connecting the same workspace with another app makes a new account record; nothing is silently replaced. The board lists both; the owner removes the one that is not wanted.
- **Refresh:** `refresh` uses the account's own app: `builtin` → `client_id` = the account's `app_client_id` (the built-in id when it was made), no secret; `owner` → the stored owner app only when its `client_id` equals the account's `app_client_id`, else the stable code `app_changed` with the fixed text "The Linear app of this account changed. Connect the account again." (no request to Linear).
- **Migration (once, at station start, before any command is served):** every Linear account file without `app_source` is rewritten in one atomic write per file to `app_source = "owner"` and `app_client_id` = the client id of the owner app stored at that moment. This is the same app that today's refresh would use, so the behaviour is unchanged. If no owner app is stored, the account gets `app_client_id = ""` and its refresh answers `app_changed` (today it fails too: no app). A file that cannot be rewritten stays as it is and is listed as unusable (the existing rule).
- Setting, replacing or removing the owner app does not touch accounts made with the built-in app, and the reverse.

### 2.4 Errors

- Every new error is a stable code with a fixed text: `builtin_needs_localhost`, `app_changed`. The deck relays these two codes and their texts to the board unchanged (they carry no credential); every other station error keeps today's handling.
- Linear's answers are never returned or logged: a failed token exchange or refresh is `upstream` "the server answered HTTP <status>" or `timeout`, as `accounts/http.rs` does today. No code, verifier, token or secret appears in an error, a log, an audit row, an argv or a response.

### 2.5 Board

- Settings → Accounts → Linear without an owner app shows "Uses the built-in Fleet Deck app" and the Connect button; "Set up your own OAuth app…" stays available. With an owner app, the section is as today.
- When the effective app is the built-in app and the board's origin is not `https://127.0.0.1:7420` or `https://localhost:7420`, the section shows the fixed text of `builtin_needs_localhost` next to the Connect button (the station still decides).
- An account with `app_changed` shows "Connect again".

## 3. Security

- No secret exists for the built-in app. The client id is public.
- **Honest sign-ins:** a code from a sign-in that the station started is useless without the verifier that only that station transaction holds (in memory, one use, 10 minutes). Linear sends it only to a registered loopback URI, so it reaches the machine that started the sign-in.
- **Public-client limits (accepted, stated):** anyone can start a Linear authorization with the public client id and show "Fleet Deck" on Linear's consent page. A hostile program on the user's own machine that listens on port 7420 (for example while the deck is stopped) can run its own PKCE flow and, after the user consents on Linear's page, get its own token; the station's verifier does not protect against that, because it is not the station's transaction. This is the normal exposure of every public OAuth client on a loopback redirect: it needs a local program and the user's consent. The board never asks the user to paste a callback URL, so a remote phishing page cannot get a code through Fleet Deck's flow.
- The deck's binding of a transaction to the owner's login session and the station's state checks are unchanged.

## 4. Release gate (verify against the real app before release)

Linear does not document its redirect matching. Before the built-in app ships, the owner runs these checks once against the real "Fleet Deck" app and records the result in this file:
1. A sign-in from `https://127.0.0.1:7420` and one from `https://localhost:7420` both complete, without a client secret; a refresh without a secret works.
2. An authorize request with an unregistered redirect (`https://127.0.0.1:7421/oauth/callback`, `https://127.0.0.1:7420/oauth/callback2`, `http://127.0.0.1:7420/oauth/callback`, `https://[::1]:7420/oauth/callback`) is refused by Linear.
If check 2 shows a loose match, the built-in app does not ship and this design returns to review.

## 5. Failure behaviour

| Situation | Result |
| --- | --- |
| Built-in app, board opened on a tailnet or LAN address, or on another port | `builtin_needs_localhost`; nothing is sent to Linear |
| The deck's board does not listen on 127.0.0.1:7420 (port busy, another `listen`) | The built-in app cannot be used; the owner sets up an own app |
| The deck or the station restarts during a sign-in | The callback gets `invalid_state`; "The sign-in expired. Sign in again." |
| An owner app is set while a built-in sign-in is open | The finish fails as an app change; sign in again |
| Owner app replaced with another client id | Owner-app accounts answer `app_changed`; built-in accounts keep working |
| The same workspace connected with the built-in app and later with an owner app | Two account records; the owner removes one |
| Linear rejects a token exchange or refresh | `upstream` "the server answered HTTP <status>"; no Linear text |

## 6. Tests

- Start: built-in app used without an owner app; each non-registered redirect (other host, port, scheme, `[::1]`, a path suffix) gets `builtin_needs_localhost`; the owner app still wins when set; Jira without an app is still refused.
- Finish and refresh: the form body has no `client_secret` for a built-in account (fake curl body capture) and has it for an owner account; refresh of a built-in account after an owner app is set still uses the account's `app_client_id`; an owner account after the owner app's client id changed gets `app_changed` and sends nothing.
- Identity: connecting the same workspace with the built-in app and then with an owner app gives two records; reconnecting with the same app replaces the same record.
- Migration: an old file without `app_source` is rewritten once to `owner` with the stored owner app's client id; without an owner app it gets `""` and refresh answers `app_changed`; a second start changes nothing.
- Errors: a fake Linear answer whose body contains a canary token, code and verifier gives an error, log line and response with none of them.
- Board: the "built-in" text, the localhost note by origin, "Connect again" for `app_changed`.

## 7. Not in scope

- A hosted relay page or broker; GitHub, GitLab and Jira built-in apps; IPv6 loopback.

## 8. Review map (revision 1 → revision 2)

| Finding | Change |
| --- | --- |
| BLOCKER upsert can switch an account to another app | §2.3 identity includes `app_source` and `app_client_id`; another app makes a new record |
| BLOCKER Linear error text in errors | §2.4 fixed codes and texts; Linear bodies never returned (the existing `http.rs` rule); §6 canary test |
| MAJOR migration guesses the app | §2.3 one atomic rewrite at station start, before any command, bound to the app stored at that moment (today's refresh behaviour); built-in refresh uses the saved client id |
| MAJOR attacker's own verifier | §3 states the public-client limits; no copy-the-callback recovery; minimal scope kept |
| MAJOR certificate, listener, IPv6 | §2.1 IPv6 not supported; §2.2 the redirect equals the starting origin (certificate and session already accepted there); restart → `invalid_state`; §5 rows |
| UNVERIFIED redirect matching | §4 release gate with the exact checks; no ship on a loose match |
| MINOR recovery messages hidden | §2.4 the two new codes pass the relay unchanged; §2.5 board texts |
