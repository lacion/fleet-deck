# Built-in Linear app (DL-185)

- **Status:** Proposed (revision 1, 2026-10-14). Needs an adversary DESIGN REVIEW before code.
- **Owner request (2026-10-14):** "for jira and linear, github and gitlab can we create oauth apps that will just work for everyone? apps that we can publish?" Owner choices: "Linear public app, no broker", then for the redirect "Localhost only".
- **Changes:** DL-173 (the owner registers a Jira or Linear OAuth app before a sign-in). Jira, GitHub and GitLab are unchanged.

## 1. Facts today

- The station keeps one owner app per tracker (`StoredApp { client_id, client_secret, generation }`, `crates/fleetdeck-station/src/accounts/store.rs:44`), set through the deck's credential relay (`AppSet`), never stored on the deck.
- `accounts.oauth_start` (`crates/fleetdeck-station/src/accounts/oauth.rs:153`) needs the stored app, checks the redirect (`https`/`http`, no userinfo, path `oauth/callback`), and for Linear builds `https://linear.app/oauth/authorize?client_id=…&redirect_uri=…&response_type=code&scope=read,write&state=…&code_challenge=<S256>&code_challenge_method=S256`; the transaction keeps the PKCE verifier in memory.
- The deck makes the redirect `<board Origin>/oauth/callback`, binds the transaction to the owner's login session, and relays `{ state, code }` to `accounts.oauth_finish`.
- `finish_linear` (:317) and `refresh` (:426) send `client_id` and `client_secret` to `https://api.linear.app/oauth/token`; `refresh` uses the app that is stored now, not the one that made the account.
- Linear's OAuth documentation (checked 2026-10-14): with PKCE the token exchange takes `code_verifier` and `client_secret` is optional; a token made with PKCE refreshes with `client_id` only.

## 2. Design

### 2.1 The built-in app

- The station carries one constant: `LINEAR_BUILTIN_CLIENT_ID = "66edc8f79eb0603f2aded18705bdfb99"` (the public client id of the "Fleet Deck" Linear app, registered by the project owner as a public app). No client secret exists in the code, the repository or any build.
- The Linear app registers exactly two redirect URIs: `https://localhost:7420/oauth/callback` and `https://127.0.0.1:7420/oauth/callback`.
- The effective Linear app of a station is the owner's stored app when one is set (unchanged behaviour), else the built-in app. Jira has no built-in app.

### 2.2 Sign-in with the built-in app

- `accounts.oauth_start` for Linear without an owner app: the redirect URI must be exactly one of the two registered URIs (byte-equal after the existing checks), else `invalid_params` with the text "The built-in Linear app works only from the board on the deck machine: open https://localhost:7420 and sign in there, or set up your own Linear app." The authorize URL is the one of §1 with the built-in client id (PKCE S256 as today).
- The transaction records `app = builtin` (with generation `0`); an owner app that is set while the sign-in is open makes the finish fail as an app change (the existing generation rule).
- `accounts.oauth_finish`: the token request sends `grant_type`, `code`, `redirect_uri`, `client_id` and `code_verifier`, and no `client_secret`.

### 2.3 The account remembers its app

- A stored Linear account gets `app: "builtin" | "owner"` and the `client_id` that made it (new fields; an account stored before this change reads as `owner` with the stored app's client id).
- `refresh` uses the account's own app: `builtin` → `client_id` = the built-in id, no secret; `owner` → the stored owner app only when its `client_id` equals the account's; else `unavailable` "the Linear app of this account changed; connect again".
- Setting, replacing or removing the owner app does not touch accounts made with the built-in app, and the reverse.

### 2.4 Board

- Settings → Accounts → Linear without an owner app shows "Uses the built-in Fleet Deck app" and the Connect button; "Set up your own OAuth app…" stays available.
- When the board's own origin is not `https://localhost:7420` or `https://127.0.0.1:7420`, the section shows the note "Sign in from the board on the deck machine: https://localhost:7420. The built-in app accepts only that address." (the station still decides; the board text is a hint).

## 3. Security

- The client id is public by design; there is no secret to leak.
- A code is useless without the PKCE verifier, which stays in the station's memory and is bound to one transaction (one use, the existing 10-minute life). The deck's state binding to the owner's login session is unchanged.
- Linear delivers a code only to the two registered loopback URIs, so a code reaches only the browser of the machine that started the sign-in. A local process that listens on port 7420 while the deck is down could receive a code but cannot redeem it without the verifier.
- Anyone can start a Linear authorization with the public client id and show "Fleet Deck" on the consent page, but the code goes only to the victim's own loopback address and is useless without that victim's station verifier.
- No token, code, verifier or client id is logged; the existing redaction and audit rules apply.

## 4. Failure behaviour

| Situation | Result |
| --- | --- |
| Built-in app, board opened on a tailnet or LAN address | `invalid_params` with the localhost text; nothing is sent to Linear |
| The deck's board does not listen on 127.0.0.1:7420 | The built-in app cannot be used; the owner sets up an own app |
| An owner app is set while a built-in sign-in is open | The finish fails as an app change; sign in again |
| Owner app replaced with another client id | Owner-app accounts must connect again; built-in accounts keep working |
| Linear rejects a token exchange without a secret | `upstream` with Linear's error text; the owner sets up an own app |

## 5. Tests

- Start: built-in app used without an owner app; a redirect other than the two URIs is refused with the text; the owner app still wins when set; Jira without an app is still refused.
- Finish and refresh: the form body has no `client_secret` for a built-in account (fake curl argv/body capture) and has it for an owner account; refresh of a built-in account after an owner app is set still uses the built-in id; an owner account after the owner app's client id changed gets `unavailable`.
- An old stored account (no `app` field) reads as `owner`.
- Board: the "built-in" text and the localhost note by origin.
