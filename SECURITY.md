# Security Policy

Fleet Deck is a local daemon that watches your Claude Code sessions, types into
tmux panes, and spawns new agents on your behalf — including, behind a two-step
red confirmation, unsupervised ones. That is a lot of power pointed at your
machine, so security reports are taken seriously and answered by a human.

This is a solo, unfunded project. Luis Morales is the only maintainer. The
promises below are what one person can actually keep, written honestly.

## Reporting a vulnerability

**Please do not open a public GitHub issue for anything security-relevant.** A
public issue tells everyone the hole exists before there's a fix.

- **Preferred:** GitHub Private Vulnerability Reporting. Go to the repo's
  **Security** tab → **Report a vulnerability**, or straight to
  <https://github.com/lacion/fleet-deck/security/advisories/new>. This keeps the
  report private, threads the discussion, and lets a fix ship as a coordinated
  advisory.
- **If you can't use GitHub:** email **luismmorales@gmail.com**. Say "Fleet Deck
  security" in the subject so it doesn't get lost.

A good report includes the version (`0.9.1`, etc.), your OS and Node version,
whether you were in loopback or LAN mode, and the smallest steps that reproduce
it. A proof-of-concept web page or `curl` is worth a thousand words.

## What to expect

- **Acknowledgment within 7 days.** Usually much faster, but 7 days is the
  number one person can honestly commit to.
- **A fix or a documented mitigation within 30 days** for anything reachable
  from the network or from a web page you merely visit — the classes that can
  hurt you without you doing anything wrong. Lower-severity issues get fixed on a
  best-effort basis.
- **No bug bounty.** There's no money behind this project, so there's no money to
  pay out. What is offered is a real fix and real credit.
- **Credit in the release notes** if you want it — name, handle, or link, your
  call. Say so in the report, or ask to stay anonymous.

## Supported versions

| Version | Supported |
| --- | --- |
| 0.13.x  | ✅ |
| < 0.13  | ❌ |

Fleet Deck is pre-1.0. Only the latest release gets security fixes — if you're
behind, the fix is to upgrade.

## Threat model / scope

The threat model as of v0.5.0 lives in
[`docs/AUDIT-2026-07.md`](docs/AUDIT-2026-07.md) — the July 2026 deep audit that
these boundaries come from. Read it for the details behind any line below, but note
it predates the v0.8 reverse-proxy surface (`FLEETDECK_TRUSTED_ORIGINS`,
`FLEETDECK_PROXY_AUTH`) and the v0.9 `POST /api/paste-image` endpoint — both are in
the in-scope list below.

The design intent, in one sentence: **the daemon trusts the local user
completely and everyone else not at all.** It exists to act on your behalf, so if
code is already running as you, it's already inside the trust boundary. The
interesting attack surface is everything that is *not* you — a web page you
visited, or another host on your network when LAN mode is on. One honest caveat:
on a multi-user machine the loopback trust zone is wider than "you" — every other
local OS user can reach `127.0.0.1` too, and so is trusted by default unless you
set `FLEETDECK_REQUIRE_TOKEN=on` to demand the bearer token even over loopback.

### In scope

Reports in these areas are wanted:

- **Loopback bypass from a web page.** The daemon binds `127.0.0.1` and, by
  design, needs no token there. That trust is defended by an
  `Origin`/`Host`/`Sec-Fetch` allowlist plus an `application/json` requirement on
  every state-changing route and on **both** WebSocket upgrades (`/ws` and
  `/ws/term`). Anything that defeats those checks is in scope: **CSRF**, a bad
  `Origin`/`Host` slipping through, **cross-site WebSocket hijacking** of the
  snapshot or terminal streams, or **DNS rebinding** that makes a remote page
  look loopback. Mutating GET routes (`/mail`, `/api/watch`) are gated too — if
  one isn't, that's a bug.
- **LAN-mode authentication bypass or token leakage.** With `FLEETDECK_BIND=0.0.0.0`
  a bearer token is mandatory (the daemon refuses to start without one). Reaching
  any fleet data or mutating route without a valid token — as `Authorization:
  Bearer` or `?t=` — is in scope. So is the token turning up where it shouldn't:
  in the `/ws` broadcast, in `fleetd.log`, or in a hook-payload capture file.
- **Unauthorized use of the daemon's powers.** Getting it to **spawn a session**,
  **inject keystrokes** into a pane, or **deliver mail across sessions** without
  proper authorization — including any way to make one agent's mail bleed into
  another's pane.
- **Escaping the unsupervised-execution confirmation.** Unsupervised workers run
  with `--dangerously-skip-permissions` and produce no permission cards, so
  they're gated behind a deliberate two-step red confirmation. Any path that
  spawns an unsupervised worker without a human passing through that flow is in
  scope.
- **Secrets leaking into logs or captures.** A real secret surviving into
  `fleetd.log` or a hook-payload capture file (capture is off by default, `0600`)
  past the redaction — which strips secret-looking key names, well-known
  credential shapes, and the daemon's own token. Free-text secrets are only
  best-effort caught, so a token that lands in an unexpected field and rides
  through is exactly the report wanted.
- **Reverse-proxy trust bypass.** Behind a proxy, `FLEETDECK_TRUSTED_ORIGINS`
  widens the same-origin wall to exactly the origins you name and
  `FLEETDECK_PROXY_AUTH=trust` delegates authentication to the proxy. Reaching
  fleet data or a mutating route from an origin you did *not* name, or getting
  `trust` mode to waive the bearer token when the proxy has not actually
  authenticated the browser, is in scope.
- **Image-paste ingest (`POST /api/paste-image`).** The paste endpoint decodes
  browser-supplied base64, sniffs the magic bytes (the client's mime claim is
  never trusted), and writes an owner-only file under `FLEETDECK_HOME`. Anything
  that defeats the type sniff, escapes that directory (symlink or path
  traversal), or turns a paste into an arbitrary-file write is in scope.

### Out of scope

These aren't bugs in Fleet Deck's model — please don't report them as
vulnerabilities:

- **Anything that needs an already-compromised local OS user account.** The
  daemon intentionally trusts the local user; code running as you can already do
  everything you can. That's the whole point of the tool, not a flaw.
- **Deliberately disclosing your own bearer token** — pasting it in a chat,
  committing it, screenshotting the Share panel into a public deck. Treat it like
  an SSH key.
- **Running LAN mode on a network you don't trust.** LAN mode is opt-in and the
  README says plainly: not on café, conference, or hotel Wi-Fi — use Tailscale or
  an SSH tunnel. A hostile LAN you chose to join is your risk, not a Fleet Deck
  defect.
- **Vulnerabilities in Claude Code itself.** Report those to Anthropic. Fleet
  Deck rides on top of the CLI; it doesn't own it.
- **The `demo/` scripts.** They're live acceptance gates that start real,
  billed Claude sessions and set test-only env vars. They aren't a supported way
  to run a fleet and aren't part of the security surface.

## Supply-chain notes

For anyone triaging a Socket, Snyk, or similar scanner alert against this
package — the short version of why the daemon does what it does:

- **No shell execution anywhere in the daemon.** Every subprocess is argv-only
  `execFile`/`spawn` routed through `scripts/fleetd`'s single exec primitive —
  there is no `child_process.exec`, no `sh -c`, no string command line. As of
  0.13.0 `FLEETDECK_AGENTS_CMD` is whitespace-split into argv and run the same
  way; quotes, pipes, `$()` and redirection are never interpreted. A flagged
  "command execution" sink is that primitive, and what reaches it is argv, not
  attacker-controlled shell.
- **`POST /command` and the terminal WebSocket do not execute commands.** Mail is
  queued and delivered as a sanitized bracketed paste (control bytes and
  paste-escape markers stripped); terminal keystrokes reach the pane via `tmux
  send-keys -H` (hex literals). Nothing runs unless the human's Claude session
  runs it — the same trust boundary as typing into that terminal yourself.
- **Payload capture is opt-in, `0600`, size-capped, and redacted** — secret-looking
  key names, well-known token shapes, and the daemon's own token are stripped. A
  secret buried in free text may survive that redaction, which is exactly why the
  feature defaults off.
- **No telemetry, no update checks, no callouts.** Every `fetch`/WebSocket in the
  shipped artifacts targets loopback or the same origin that served the board.
  The only unsolicited network emission is opt-in LAN-mode mDNS multicast on the
  local segment.
- **Package contents:** `bin/`, one esbuild bundle plus the compiled board — both
  committed to git and drift-gated in CI (the publish workflow rebuilds them and
  `git diff --exit-code`s the result, so what's on npm is what's in the tag). No
  install/postinstall scripts, and a single runtime dependency (`ws`), inlined
  into the bundle.
- **Published via npm OIDC trusted publishing** with provenance attestation — no
  long-lived npm token in CI, and the registry artifact traces back to the tagged
  commit and workflow that built it.

## Hardening notes for users

You mostly don't have to do anything: the defaults are the safe path. Fleet Deck
binds `127.0.0.1` and nothing else can reach it; LAN mode is strictly opt-in and
makes a token mandatory the moment you turn it on (there is no unauthenticated
listener — the daemon refuses to start one). The token is generated `0600` into
`$FLEETDECK_HOME/token`, and `fleetd.log` and any hook-payload captures are
`0600` too. If you do open the board to your network, put it on a network you
trust (Tailscale or an SSH tunnel beats exposing it directly), and treat the
token like an SSH key. The README's
[**The fine print**](README.md#the-fine-print-read-this-bit) and
[**LAN mode**](README.md#opening-the-board-on-your-other-machine-lan-mode)
sections are the fuller version of all of this.
