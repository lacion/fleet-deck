## What does this PR do?



---

- [ ] `bun run ci`, `bun run typecheck`, and `bun run test` pass locally (run the full test suites serially)
- [ ] Changed `src/daemon/`? Ran `bun run bundle` and committed `src/daemon/fleetd.bundle.mjs`
- [ ] Changed `bin/`? Ran `bun run bundle:bin` and committed `bin/fleetdeck.mjs`
- [ ] Changed hook sources in `scripts/`? Ran `bun run bundle:hooks` and committed the generated `scripts/fleet-*.mjs`
- [ ] Changed `board/`? Ran `bun run build:board` and committed `src/daemon/board-dist/`
- [ ] No new production dependencies (`dependencies` stays empty; Bun provides the runtime APIs)
- [ ] Docs/README updated if behavior changed
