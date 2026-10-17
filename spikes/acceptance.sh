#!/bin/sh
# Offline M1 acceptance run for the Phase 0 spikes (spikes/README.md, DL-026).
#
# Runs every check of the goal and both replays, records each exit code, and
# exits non-zero if any command fails. It needs no network, no provider CLI,
# and no socket (s4-link falls back to FIFOs, DL-025). It writes only to
# `target/` and to a temporary log directory.
#
#   sh spikes/acceptance.sh            # cargo-nextest if installed, else cargo test
#   FD_TEST_RUNNER=test sh spikes/acceptance.sh   # force `cargo test`
#
# On a fresh clone, run `cargo deny fetch` once with network access first.

set -u
cd "$(dirname "$0")/.." || exit 2

export CI=true # insta: a snapshot mismatch fails instead of writing a new file
log_dir=$(mktemp -d "${TMPDIR:-/tmp}/fd-acceptance.XXXXXX") || exit 2
failed=0
n=0

run() {
    n=$((n + 1))
    log="$log_dir/$n.log"
    "$@" >"$log" 2>&1
    code=$?
    if [ "$code" -eq 0 ]; then
        printf 'ok     exit=0  %s\n' "$*"
    else
        printf 'FAILED exit=%s %s   (log: %s)\n' "$code" "$*" "$log"
        failed=$((failed + 1))
    fi
    # The summary of each test run, if there is one (cargo test: summed
    # over all test binaries).
    grep -E '^ +Summary ' "$log" | tail -n 1 | sed 's/^/         /'
    awk '/^test result: /{p+=$4; f+=$6; b++} END{if (b) printf "         %d test binaries: %d passed, %d failed\n", b, p, f}' "$log"
    grep -cE '^ok +' "$log" | sed 's/^/         replayed ok: /' | grep -v ': 0$'
    return 0
}

runner=${FD_TEST_RUNNER:-}
if [ -z "$runner" ]; then
    if cargo nextest --version >/dev/null 2>&1; then runner=nextest; else runner=test; fi
fi
if [ "$runner" = nextest ]; then
    tests="cargo nextest run"
else
    tests="cargo test"
fi

run cargo fmt --all --check
run cargo clippy --workspace --all-targets -- -D warnings
# shellcheck disable=SC2086 # $tests is two words on purpose
run $tests --workspace
run cargo deny check
run cargo run -q -p s1-claude --bin replay-claude
run cargo run -q -p s2-codex --bin replay-codex

if [ "$failed" -eq 0 ]; then
    echo "acceptance: all $n commands passed (test runner: $runner; logs: $log_dir)"
else
    echo "acceptance: $failed of $n commands FAILED (test runner: $runner; logs: $log_dir)"
    exit 1
fi
