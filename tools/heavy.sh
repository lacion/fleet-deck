#!/bin/sh
# Runs a heavy command (cargo build, clippy, nextest, make check, bun e2e) so
# that parallel builds and tests of several agents never take the whole
# machine from the deck and the station. The limits come from this machine,
# at every run:
#   reserved cores  = max(4, 20 % of the cores)
#   build jobs      = cores - reserved          (CARGO_BUILD_JOBS)
#   test threads    = max(2, cores / 2)         (NEXTEST_TEST_THREADS)
#   memory          = soft: total - max(8 GiB, 20 %), hard: total - max(6 GiB, 15 %)
# With a systemd user manager (Linux, WSL2) every heavy command runs in the
# one shared slice `fdbuild.slice`, whose CPU and memory caps hold for all of
# them together; elsewhere (macOS) only the jobs, the threads and a low
# priority apply.
#
#   sh tools/heavy.sh make check
#   sh tools/heavy.sh cargo nextest run -p fleetdeck-station
set -eu
if [ "$#" -eq 0 ]; then
    echo "usage: sh tools/heavy.sh <command> [args...]" >&2
    exit 2
fi
cores=$(getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)
reserve=$((cores / 5))
[ "$reserve" -lt 4 ] && reserve=4
jobs=$((cores - reserve))
[ "$jobs" -lt 1 ] && jobs=1
threads=$((cores / 2))
[ "$threads" -lt 2 ] && threads=2
# An explicit value in the environment wins.
CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-$jobs}
NEXTEST_TEST_THREADS=${NEXTEST_TEST_THREADS:-$threads}
export CARGO_BUILD_JOBS NEXTEST_TEST_THREADS

if command -v systemd-run >/dev/null 2>&1 && systemctl --user show-environment >/dev/null 2>&1; then
    total_kib=$(awk '/^MemTotal:/ { print $2 }' /proc/meminfo)
    gib=$((1024 * 1024))
    soft_keep=$((total_kib / 5)); [ "$soft_keep" -lt $((8 * gib)) ] && soft_keep=$((8 * gib))
    hard_keep=$((total_kib * 15 / 100)); [ "$hard_keep" -lt $((6 * gib)) ] && hard_keep=$((6 * gib))
    high=$((total_kib - soft_keep)); [ "$high" -lt "$gib" ] && high=$gib
    max=$((total_kib - hard_keep)); [ "$max" -lt "$high" ] && max=$high
    exec systemd-run --user --scope --quiet --same-dir --slice=fdbuild.slice \
        -p CPUWeight=20 \
        --setenv=PATH="$PATH" --setenv=CARGO_BUILD_JOBS="$CARGO_BUILD_JOBS" \
        --setenv=NEXTEST_TEST_THREADS="$NEXTEST_TEST_THREADS" \
        sh -c 'systemctl --user set-property --runtime fdbuild.slice \
                   CPUQuota="$1"% CPUWeight=20 MemoryHigh="$2"K MemoryMax="$3"K \
                   >/dev/null 2>&1 || true
               shift 3
               exec nice -n 10 "$@"' heavy "$((jobs * 100))" "$high" "$max" "$@"
fi
exec nice -n 10 "$@"
