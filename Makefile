# Fleet Deck v2 checks. `make check` is the one acceptance command (goal-mode
# reviewers may run `make check` and `cargo test`, but not other cargo tools).
.PHONY: check test
check:
	sh spikes/acceptance.sh
test:
	cargo test --workspace
