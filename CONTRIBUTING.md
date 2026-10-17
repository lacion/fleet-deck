# Contributing to Fleet Deck

Fleet Deck v2 is a Rust workspace (`crates/`) with a React board (`web/`). The design is in [docs/v2/PLAN.md](docs/v2/PLAN.md) and the code map is in [docs/v2/internals.md](docs/v2/internals.md). The v1 (Node daemon and Claude Code plugin) code was removed in the v2 cutover; its README is kept in [docs/v0/README.md](docs/v0/README.md).

## Checks

Run these before you send a change:

```sh
make test-lint   # cargo fmt, clippy, board lint (the checks of a normal review)
make check       # the full offline acceptance run (see tools/check.sh)
```

On a fresh machine, fill the caches once with network access (see the header of `tools/check.sh`).

## Rules

- Modules stay under 800 lines (`tools/module-size.sh`).
- No `unwrap()` outside tests; secrets use `Secret<T>`.
- Tests use port 0 or a free port, never a fixed port.
- Normal tests use the fake agents of `fleetdeck-testkit` and recorded fixtures; they need no network. The live smoke tests run only with `make smoke-live`.
- Regenerate the board's TypeScript types with `make ts-types` after a change in `crates/fleetdeck-proto`.
- Record each decision that deviates from the plan in `docs/v2/DECISIONS-LOG.md`.
