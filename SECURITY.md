# Security Policy

Fleet Deck v2 is a deck and a set of stations that start and drive Claude Code
and Codex sessions on your machines, from a web board. That is a lot of power
pointed at your machines, so security reports are taken seriously and answered
by a human.

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

A good report includes the version (`fleetdeck --version`), your OS, the
providers and their versions, and whether the board listened on loopback, a
private LAN or a tailnet, and the smallest steps that reproduce
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

Only the latest published release is supported. Fleet Deck 2.0 is not released
yet; until then, report issues against the current `v2` branch. There are no
backports: security fixes ship on the current release line only.

## Threat model / scope

The v2 security model is in [docs/v2/PLAN.md](docs/v2/PLAN.md) (owner decision
D13 and the security sections) and in the ADRs in [docs/v2/adr/](docs/v2/adr/).
In short:

- **Network.** The board and the deck link listen only on loopback, private LAN
  and Tailscale tailnet addresses. The deck refuses public addresses. There is
  no public exposure mode.
- **Board access.** A login (owner password or a one-time `fleetdeck open`
  link) gives a session with scopes (`read`, `drive`, `approve`, `admin`).
  Every route checks its scope; every state change needs a CSRF token and the
  board origin, and is audited.
- **Deck ↔ station link.** Each node has its own key. Pairing uses a one-time
  invitation and pins the peer key (ADR-004). An unpaired peer gets nothing.
- **Agents.** Approval and question requests fail closed: a request that no
  deck can answer follows the station's offline approval policy, and an unknown
  request is refused. Stations start agents only in their allowed roots.
- **Secrets.** Passwords, tokens, invitations and credentials are never logged
  or written to the audit log.

In scope: anything that lets a web page, another host, another local OS user or
an unpaired node read fleet data or drive an agent without the right scope.
Out of scope: what an agent does once you approve it, and processes that run as
your own OS user.
