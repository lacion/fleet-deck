# Security Policy

Fleet Deck is a local daemon that watches your Claude Code sessions, types into
tmux 3.4+ panes, and spawns new agents on your behalf — including, behind a two-step
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
| 0.16.x  | ✅ |
| < 0.16  | ❌ |

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
visited, or another host on your network when LAN mode is on.

Two honest caveats, spelled out rather than implied:

- **The loopback trust zone is wider than "you" on a multi-user machine** —
  every other local OS user can reach `127.0.0.1` too. `FLEETDECK_REQUIRE_TOKEN=on`
  closes exactly that gap: every route (hooks included, since 0.16.0) demands
  the bearer.
- **`FLEETDECK_REQUIRE_TOKEN` does NOT protect you from your own agents.** The
  token file is `0600` at `$FLEETDECK_HOME/token`, and the daemon exports
  `FLEETDECK_HOME` into every pane's environment — so any process running as
  your UID, including a fleet-spawned agent or a compromised dependency inside
  one, can read the key and present it. Full same-UID confinement is not
  achievable from inside the same account; treat agent sandboxes as a
  convenience, not a boundary. What 0.16.0 *does* guarantee is that hook
  traffic, mail, terminal input, gateway settings and unsupervised spawns
  require the bearer — which removes every *tokenless* attack from local
  processes, other OS users, and anything sandboxed away from the filesystem.

Since 0.16.0 the daemon mints a token on **every** boot (loopback included) and
prints the credentialed local URL (`fleetd board http://127.0.0.1:4711/?t=…` to
the `0600` log). Every hook event arrives through a command shim
(`scripts/fleet-hook.mjs`, `fleet-sessionstart.mjs`, `fleet-watch.mjs`) that
reads the token file and attaches it — Claude Code http hooks cannot carry
headers, which is why the shims exist. A tokenless hook is always refused (no
state is ingested), but it is answered in the hook dialect — a restart whisper
for sessions started before 0.16.0, escalating to a single turn-blocking
restart instruction per session — rather than a bare 401, so a pre-upgrade
session can never sit silently dark.

Since 0.16.1, `FLEETDECK_TRUST_LOOPBACK=on` waives the four loopback power
gates (`/ws/term`, `POST /mail`, `gateway_*` writes, the unsupervised arm) for
plain-loopback callers. It is the deliberate inverse of `REQUIRE_TOKEN` for
machines where the loopback trust zone genuinely contains one person — a
single-user Coder workspace reached over `coder port-forward`, a personal
laptop. It changes nothing else: hooks still authenticate through their shims,
proxied and LAN callers keep their walls, and the daemon refuses the flag
outright when combined with LAN mode or `REQUIRE_TOKEN` (the configurations
that assert the opposite). Do not set it on a box other people can log into.

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
  another's pane. Since 0.16.0, `/hook/*`, `POST /mail`, `/ws/term` input,
  `gateway_*` settings writes and the unsupervised-spawn arm all require the
  bearer even on loopback; hook forgery by a *tokenless* caller is in scope.
  The daemon's mail identities (`orchestrator`, `fleetdeck`, `fleetdeck-answer`,
  `human`) and its `[FLEETDECK ...]` authority frames are reserved server-side —
  forging either through the external API is in scope.
- **Escaping the unsupervised-execution confirmation.** Unsupervised workers run
  with `--dangerously-skip-permissions` and produce no permission cards, so
  they're gated behind a deliberate two-step red confirmation — enforced **in
  the daemon**, not just the board UI: since 0.16.0 an unsupervised spawn or
  adopt body must echo a single-use arm token minted by the token-gated
  `POST /api/spawn/arm-unsupervised`. Any path that reaches an unsupervised
  worker without a human passing through that flow is in scope.
- **Bring-up nudge overreach.** The spawn bring-up nudge sends ONE Enter if no
  hook lands in time — but since 0.16.0 it reads the pane first and **never
  answers Claude Code's folder-trust or MCP-approval dialogs** (those are the
  human checkpoints on a repo's `.claude/settings.json` hooks and `.mcp.json`
  servers). A nudge that auto-answers a trust dialog is a bug; report it.
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
  an SSH tunnel. Be clear about what "hostile" buys an attacker, because it is
  more than sniffing: with no TLS on the listener, an *active* on-path attacker
  (rogue AP, ARP spoof, a compromised IoT device on a "mostly trusted" home
  network) needs **no token at all** — they rewrite the board's JavaScript in
  flight and act as you with your stored token, or win `fleetdeck.local` via
  mDNS and serve a pixel-perfect copy of the key-entry gate (the real one is
  public by design) to phish the key out of you. mDNS advertisements carry no
  cryptographic identity to pin against. A hostile LAN you chose to join is
  your risk, not a Fleet Deck defect — but don't mistake the risk for
  passive-only.
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
  into the bundle. Since 0.16.0 CI and publish installs run with
  `--ignore-scripts` (esbuild's binary arrives as an optional platform dep, no
  lifecycle script needed), dependency PRs arrive one package at a time (no
  grouped bumps), and `CODEOWNERS` plus a hook-integrity CI job cover the
  unbundled hook-execution path the drift gate can't see.
- **Published via npm OIDC trusted publishing** with provenance attestation — no
  long-lived npm token in CI, and the registry artifact traces back to the tagged
  commit and workflow that built it. Since 0.16.0 the publish job also requires
  a human approval (the `release` GitHub environment) before the OIDC
  credential mints, and refuses any tag whose commit is not an ancestor of
  `main` — a tag can no longer publish an unreviewed tree.

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

One knob deserves an explicit callout: the global file explorer's root is
configurable (`browse_root` setting / `FLEETDECK_BROWSE_ROOT`; on a Coder
workspace it auto-detects `/workspace`) — and **its default is your home
directory**, the highest-density credential location on the machine. The
explorer is read-only and the root is resolved **server-side only** — the
browser can never name a root or escape it (`..`, absolute paths and symlinks
out of the root are refused) — and since 0.16.0 a credential denylist applies
on top: `.git`, `.ssh`, `.aws`, `.gnupg`, `.netrc`, `.kube`,
`.docker/config.json`, and everything under `$FLEETDECK_HOME` (the token, the
DB, captures) are never served through it. But anything *else* under the root
is what any bearer-token holder can read in LAN/standalone mode — so point
`browse_root` at your projects directory, not your home, when the board leaves
loopback.

**Install channel honesty:** `claude plugin marketplace add lacion/fleet-deck`
tracks the repo's **default branch**, not a pinned release — every push to
`main` becomes code that runs at your next SessionStart. The drift gates,
CODEOWNERS, hook-integrity CI job and the `release`-environment npm publish
gate narrow that, but the npm channel (`npm i -g fleetdeck`, standalone mode)
is the one that ships only tagged, human-approved, provenance-attested
artifacts. If you want releases and only releases, prefer npm or pin your
marketplace clone to a tag.
