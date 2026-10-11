# Fleet Deck v2 checks. `make check` is the one acceptance command (goal-mode
# reviewers may run `make check` and `cargo test`, but not other cargo tools).
# It is offline: see tools/check.sh for the one-time cache fill.
.PHONY: check check-spikes test test-web ts-types
# Every check of the M2 goal: fmt, clippy, nextest, deny, TS-types drift,
# web typecheck + lint + test + build, and every recorded replay (G13).
check:
	sh tools/check.sh
# The M1 (Phase 0) acceptance run of the spikes only (DL-026).
check-spikes:
	sh spikes/acceptance.sh
test:
	cargo test --workspace
# The web checks only (dependencies from the local bun cache).
test-web:
	bun install --cwd web --frozen-lockfile --offline --ignore-scripts
	bun run --cwd web typecheck
	bun run --cwd web lint
	bun run --cwd web test
# Regenerate the board's TypeScript types from fleetdeck-proto (PLAN §11).
# The drift test `fleetdeck-proto::ts_drift` fails until the output is committed.
ts-types:
	cargo run -q -p fleetdeck-proto --example export_ts -- web/src/gen
