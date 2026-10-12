# Fleet Deck v2 checks. `make check` is the one acceptance command (goal-mode
# reviewers may run `make check` and `cargo test`, but not other cargo tools).
# It is offline: see tools/check.sh for the one-time cache fill.
.PHONY: check check-spikes test test-web test-lint ts-types smoke-live package-check perf
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
# The format and lint checks of a normal review round (DL-309): a reviewer
# may run `make test*`, so the three round checks have one offline target.
test-lint:
	cargo fmt --all --check
	timeout 1800 cargo clippy --workspace --all-targets -- -D warnings
	bun install --cwd web --frozen-lockfile --offline --ignore-scripts
	bun run --cwd web lint
# Regenerate the board's TypeScript types from fleetdeck-proto (PLAN §11).
# The drift test `fleetdeck-proto::ts_drift` fails until the output is committed.
ts-types:
	cargo run -q -p fleetdeck-proto --example export_ts -- web/src/gen
# Opt-in live smoke tests (P5-5, DL-067): the real `claude` and `codex` CLIs,
# one tiny prompt each in a temporary directory. Not part of `make check`;
# needs signed-in CLIs and network. See docs/v2/LIVE-SMOKE.md.
smoke-live:
	FD_LIVE_SMOKE=1 timeout 900 cargo test -p fleetdeck-providers --test live_smoke -- --ignored --nocapture --test-threads 1
# Packaging (M3 7-5, DL-400): static checks of dist-workspace.toml, the
# systemd and launchd units and the container file, plus the offline smoke
# test of the built binary. Also part of `make check` (workspace tests).
package-check:
	cargo test -p fleetdeck-cli --test packaging
# Performance bars of PLAN §13 (M3 7-4, DL-401): one deck and ten stations
# on loopback with the offline fake agent, release profile. Writes
# target/perf/report.md and fails when a bar fails. Not part of `make check`.
perf:
	cargo build --release --offline -p fleetdeck-testkit --bins
	timeout 900 cargo test --release --offline -p fleetdeck-cli --test perf -- --ignored --nocapture --test-threads 1
