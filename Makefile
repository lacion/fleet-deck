# Fleet Deck v2 checks. `make check` is the one acceptance command (goal-mode
# reviewers may run `make check` and `cargo test`, but not other cargo tools).
.PHONY: check test ts-types
check:
	sh spikes/acceptance.sh
test:
	cargo test --workspace
# Regenerate the board's TypeScript types from fleetdeck-proto (PLAN §11).
# The drift test `fleetdeck-proto::ts_drift` fails until the output is committed.
ts-types:
	cargo run -q -p fleetdeck-proto --example export_ts -- web/src/generated
