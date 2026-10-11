#!/bin/sh
# Fleet Deck v2 acceptance run: `make check` (GATE-M1 G13, PLAN §14 Phase 1).
#
# Runs every check of the M2 goal (step 4) and every recorded replay, records
# each exit code, and exits non-zero if any command fails. It needs no
# network: web dependencies come from the local bun cache (`--offline`), and
# `cargo deny` uses the local advisory database. It needs no provider CLI.
# It writes only to `target/`, `web/node_modules/`, `web/dist/`,
# `web/test-results/`, a temporary log directory, and the temporary node
# directories of the real-process browser flows (removed after the run).
#
#   make check                                   # the normal entry point
#   FD_TEST_RUNNER=test make check               # force `cargo test` instead of nextest
#
# On a fresh machine, fill the caches once with network access:
#   cargo deny fetch && (cd web && bun install --frozen-lockfile)
#   (cd web && bunx playwright install chromium-headless-shell)
#
# The CI workflow `.github/workflows/v2.yml` fills the caches and then runs
# this same script, so local and CI checks cannot drift apart.

set -u
cd "$(dirname "$0")/.." || exit 2

export CI=true # insta: a snapshot mismatch fails instead of writing a new file
log_dir=$(mktemp -d "${TMPDIR:-/tmp}/fd-check.XXXXXX") || exit 2
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
    # A short summary of the run, if the log has one.
    grep -E '^ +Summary ' "$log" | tail -n 1 | sed 's/^/         /'
    awk '/^test result: /{p+=$4; f+=$6; b++} END{if (b) printf "         %d test binaries: %d passed, %d failed\n", b, p, f}' "$log"
    grep -cE '^ok +' "$log" | sed 's/^/         replayed ok: /' | grep -v ': 0$'
    awk '/^ *[0-9]+ pass$/{p=$1} /^ *[0-9]+ fail$/{f=$1} END{if (p != "") printf "         bun test: %s pass, %s fail\n", p, (f == "" ? 0 : f)}' "$log"
    grep -E '^Checked [0-9]+ files?' "$log" | sed 's/^/         /'
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

# Rust (all crates, including the Phase 0 spikes).
# cargo-deny reads its advisory database from target/advisory-dbs (deny.toml,
# DL-025). A fresh checkout has no target/: seed it from the default cargo
# location when that copy exists, so the check stays offline.
cargo_home=${CARGO_HOME:-$HOME/.cargo}
if ! ls -d target/advisory-dbs/advisory-db-* >/dev/null 2>&1 &&
    ls -d "$cargo_home"/advisory-dbs/advisory-db-* >/dev/null 2>&1; then
    mkdir -p target/advisory-dbs && cp -R "$cargo_home"/advisory-dbs/advisory-db-* target/advisory-dbs/
fi

run cargo fmt --all --check
run cargo clippy --workspace --all-targets -- -D warnings
# shellcheck disable=SC2086 # $tests is two words on purpose
run $tests --workspace
run cargo deny check

# TS-types drift (PLAN §11): web/src/gen must equal a fresh export. The second
# test of the file proves that the check fails on stale, missing and extra
# files. Fix a failure with `make ts-types`.
run cargo test -q -p fleetdeck-proto --test ts_drift

# Web board (PLAN §11): dependencies from the lockfile and the local cache only.
run bun install --cwd web --frozen-lockfile --offline --ignore-scripts
run bun run --cwd web typecheck
run bun run --cwd web lint
run bun run --cwd web test
run bun run --cwd web build
# Phone-viewport browser tests of the built board (Playwright, Chromium).
run bun run --cwd web e2e
# The MVP flows against a real deck and two real stations with the offline
# fake agent (M2 P4-5); its setup builds `fleetdeck` and the fakes.
run bun run --cwd web e2e:real

# Recorded provider replays (offline; fixtures in spikes/*/fixtures).
run cargo run -q -p s1-claude --bin replay-claude
run cargo run -q -p s2-codex --bin replay-codex

if [ "$failed" -eq 0 ]; then
    echo "check: all $n commands passed (test runner: $runner; logs: $log_dir)"
else
    echo "check: $failed of $n commands FAILED (test runner: $runner; logs: $log_dir)"
    exit 1
fi
