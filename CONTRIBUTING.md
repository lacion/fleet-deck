# Contributing to Fleet Deck

Thanks for pointing a wrench at this. Fleet Deck is a single-maintainer project
with a deliberately small surface — one Node daemon, one React board, no runtime
dependency you didn't already have. The rules below keep it that way; most of
them are scars, not preferences.

## TL;DR

```bash
git clone https://github.com/lacion/fleet-deck
cd fleet-deck
npm install                 # installs ws + esbuild; that's the whole tree
npm test                    # 303 contract tests, serial, against a real daemon

# regenerate the two committed build artifacts before you push (see below)
npm run bundle              # → scripts/fleetd/fleetd.bundle.mjs
npm run build:board         # → scripts/fleetd/board-dist/
```

## Prerequisites

- **Node >= 22.5** — a hard floor, not a suggestion. The daemon keeps its state
  in Node's built-in `node:sqlite`, which first shipped in 22.5. There is no
  polyfill and no fallback; an older Node won't boot the daemon.
- **tmux** — needed for anything spawn-, terminal-, or pane-related, and the
  test suite drives a *real* tmux server to exercise it. It never touches your
  tmux, though: every test daemon runs its tmux commands against an isolated
  named server (`tmux -L fleetdeck-test-<port>`, set via `FLEETDECK_TMUX_SOCKET`
  in `tests/helpers/daemon.mjs`), and the demo scripts do the same with a
  per-run `fdaccept-<pid>` socket. Your default server is left alone.
- **Linux, WSL2, or macOS.** Windows-native is untested.

## Running from source

```bash
npm start                   # node scripts/fleetd/fleetd.mjs (the SOURCE, not the bundle)
```

`npm start` runs `scripts/fleetd/fleetd.mjs` directly, so it's the fast loop for
daemon work — no rebundle between edits.

Don't collide with a real fleet. If you already run Fleet Deck day to day, its
daemon owns port 4711 and `~/.fleetdeck`. Point your scratch instance somewhere
else:

```bash
FLEETDECK_PORT=4712 FLEETDECK_HOME=/tmp/fd-scratch npm start
```

**Board dev loop.** The board is a separate Vite app under `board/`:

```bash
cd board
npm install                 # board has its own dependency tree
npm run dev                 # Vite dev server on http://127.0.0.1:5173
```

Vite proxies the daemon's endpoints so the dev board talks to a real fleetd:
`/state`, `/health`, `/mail`, `/command`, `/api`, and the `/ws` (plus `/ws/term`)
WebSocket upgrades all forward to `127.0.0.1:4711`. The proxy also rewrites the
`Origin` header to the daemon's own address, because fleetd's C1 gate rejects
any request whose Origin isn't itself — verify the exact list and the reasoning
in `board/vite.config.js` before you touch it.

## Tests

```bash
npm test                    # node --test --test-concurrency=1
npm run test:bundle         # same suite, run against the shipped bundle
```

`npm test` is serial **by design** (`--test-concurrency=1`). Every test boots a
real daemon on a real port against a real tmux server; run them in parallel and
they contend for ports and tmux, producing one or two failures per run — a
*different* one each time, each passing in isolation. That's a flaky harness
masquerading as flaky tests. Leave the concurrency at 1.

`npm run test:bundle` points the same suite at `scripts/fleetd/fleetd.bundle.mjs`
(via `FLEETDECK_TEST_DAEMON_SCRIPT`) so you can prove the bundle behaves exactly
like the source your users don't run.

## ⚠️ Committed build artifacts — read this or your PR ships a bug you fixed

Two build outputs are **checked into git**, and they are the #1 contributor
gotcha. The daemon your users run is the *bundle*; the board they load is the
*built* board. If either drifts from its source, you ship stale code.

- **Touched anything under `scripts/fleetd/`** (except the bundle file itself)?
  Run `npm run bundle` and commit `scripts/fleetd/fleetd.bundle.mjs`. The plugin
  ships and runs the bundle, not your `.mjs` source — an un-rebundled fix simply
  does not exist for anyone who installs Fleet Deck.
- **Touched anything under `board/`?** Run `npm run build:board` and commit
  `scripts/fleetd/board-dist/`. That's where Vite writes the built board and
  where the daemon serves it from.

Self-check before you push: run the relevant command, then `git status`. If it
leaves the bundle or `board-dist/` dirty, that diff belongs in your PR. CI
enforces this mechanically — it rebuilds both artifacts and fails the PR if
either differs from what you committed — so catch it yourself first and save a
round trip.

## Don't casually run the demos

`demo/run-*.sh` are live acceptance gates that start **real** Claude sessions
with real token cost — `run-smoke.sh` alone spins up two `claude -p
--dangerously-skip-permissions` sessions. They never run in CI, and they should
never be something you fire off to "see what happens." Run them deliberately,
when you're specifically validating end-to-end behavior.

## End-to-end plugin test

To exercise the real install path against your clone:

```bash
claude plugin marketplace add /path/to/your/clone
claude plugin install fleetdeck@fleetdeck
```

Your next `claude` brings the fleet up. Remember: the installed plugin runs the
**bundle**, so after any `scripts/fleetd/` change you must `npm run bundle` *and*
restart the daemon before the plugin sees it.

## Opening a PR

- **Tests pass locally.** `npm test` green — and don't parallelize it.
- **Artifacts regenerated and committed.** Bundle for `scripts/fleetd/` changes,
  `board-dist/` for `board/` changes.
- **No new runtime dependencies without prior discussion.** `ws` is the *only*
  runtime dependency, and the README promises users "Node 22.5+ and that's the
  whole list." Adding to that list breaks a promise; open an issue first.
- **No model calls in the daemon core.** This is a hard design constraint, not a
  code-style note. The core is deterministic — telemetry, routing, conflict
  detection, question relay — and it spends zero of the user's tokens. A PR that
  adds an inference call to the daemon will not be merged.
- **Docs/README updated if behavior changed.**
- **Version bumps are the maintainer's job** — don't touch `version` in any
  `package.json` or plugin manifest.

## Maintainer notes

Dependabot PRs that bump `esbuild` or `vite` will *legitimately* fail the
artifact drift check: a new bundler version re-emits the bundle or `board-dist/`
with different output, so the committed artifact no longer matches source. Check
out the branch, run `npm run bundle` / `npm run build:board`, commit the
regenerated artifacts, and push.

## Security

Never report a vulnerability in a public issue. Follow [SECURITY.md](SECURITY.md):
use GitHub's private vulnerability reporting (Security → Report a vulnerability)
so it's triaged before it's public. This matters more than usual here: with LAN
mode on, the API can spawn unsupervised agents and type into their terminals.
