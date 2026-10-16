#!/bin/sh
# Runs a heavy command (cargo build, clippy, nextest, make check, bun e2e) so
# that the builds and tests of several agents never take the whole machine:
#
# 1. A queue for the whole machine: at most FD_HEAVY_SLOTS (default 1) heavy
#    commands run at a time, for every worktree and every agent; the others
#    wait for a free slot (a flock on a slot file).
# 2. A disk guard: a heavy command does not start while the filesystem of the
#    current directory has less than FD_HEAVY_MIN_FREE_GB (default 60) free.
# 3. Fewer disk writes: no incremental cache (CARGO_INCREMENTAL=0) and line
#    tables only as debug info (CARGO_PROFILE_DEV_DEBUG), unless the
#    environment sets them.
# 4. CPU and memory, from this machine at every run:
#      reserved cores  = max(4, 20 % of the cores)
#      build jobs      = cores - reserved          (CARGO_BUILD_JOBS)
#      test threads    = max(2, cores / 4)         (NEXTEST_TEST_THREADS)
#      memory          = soft: total - max(8 GiB, 20 %), hard: total - max(6 GiB, 15 %)
#    With a systemd user manager (Linux, WSL2) the command runs in the shared
#    slice `fdbuild.slice`, whose CPU and memory caps hold for all heavy
#    commands together; elsewhere (macOS) only the jobs, the threads and a
#    low priority apply.
#
#   sh tools/heavy.sh make check
#   sh tools/heavy.sh cargo nextest run -p fleetdeck-station
#   FD_HEAVY_SLOTS=2 sh tools/heavy.sh cargo build      # allow two at a time
set -eu
if [ "$#" -eq 0 ]; then
    echo "usage: sh tools/heavy.sh <command> [args...]" >&2
    exit 2
fi

# A heavy command inside a heavy command does not queue again: its parent
# holds the slot. `run` sets FD_HEAVY_ACTIVE for the command only.
nested=${FD_HEAVY_ACTIVE:-0}

# 2. The disk guard.
min_free_gb=${FD_HEAVY_MIN_FREE_GB:-60}
free_kib=$(df -Pk . | awk 'NR == 2 { print $4 }')
if [ -n "$free_kib" ] && [ "$free_kib" -lt $((min_free_gb * 1024 * 1024)) ]; then
    echo "heavy.sh: only $((free_kib / 1024 / 1024)) GB free on this disk; a heavy command needs ${min_free_gb} GB." >&2
    echo "heavy.sh: remove the worktrees of merged branches (git worktree remove ../<name>) and try again." >&2
    exit 75
fi

cores=$(getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)
reserve=$((cores / 5))
[ "$reserve" -lt 4 ] && reserve=4
jobs=$((cores - reserve))
[ "$jobs" -lt 1 ] && jobs=1
threads=$((cores / 4))
[ "$threads" -lt 2 ] && threads=2
# An explicit value in the environment wins.
CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-$jobs}
NEXTEST_TEST_THREADS=${NEXTEST_TEST_THREADS:-$threads}
CARGO_INCREMENTAL=${CARGO_INCREMENTAL:-0}
CARGO_PROFILE_DEV_DEBUG=${CARGO_PROFILE_DEV_DEBUG:-line-tables-only}
export CARGO_BUILD_JOBS NEXTEST_TEST_THREADS CARGO_INCREMENTAL CARGO_PROFILE_DEV_DEBUG

# The command, inside the slice when there is one.
run() {
    if command -v systemd-run >/dev/null 2>&1 && systemctl --user show-environment >/dev/null 2>&1; then
        total_kib=$(awk '/^MemTotal:/ { print $2 }' /proc/meminfo)
        gib=$((1024 * 1024))
        soft_keep=$((total_kib / 5)); [ "$soft_keep" -lt $((8 * gib)) ] && soft_keep=$((8 * gib))
        hard_keep=$((total_kib * 15 / 100)); [ "$hard_keep" -lt $((6 * gib)) ] && hard_keep=$((6 * gib))
        high=$((total_kib - soft_keep)); [ "$high" -lt "$gib" ] && high=$gib
        max=$((total_kib - hard_keep)); [ "$max" -lt "$high" ] && max=$high
        systemd-run --user --scope --quiet --same-dir --slice=fdbuild.slice \
            -p CPUWeight=20 \
            --setenv=PATH="$PATH" --setenv=CARGO_BUILD_JOBS="$CARGO_BUILD_JOBS" \
            --setenv=NEXTEST_TEST_THREADS="$NEXTEST_TEST_THREADS" \
            --setenv=CARGO_INCREMENTAL="$CARGO_INCREMENTAL" \
            --setenv=CARGO_PROFILE_DEV_DEBUG="$CARGO_PROFILE_DEV_DEBUG" \
            --setenv=FD_HEAVY_ACTIVE=1 \
            sh -c 'systemctl --user set-property --runtime fdbuild.slice \
                       CPUQuota="$1"% CPUWeight=20 MemoryHigh="$2"K MemoryMax="$3"K \
                       >/dev/null 2>&1 || true
                   shift 3
                   exec nice -n 10 "$@"' heavy "$((jobs * 100))" "$high" "$max" "$@"
    else
        FD_HEAVY_ACTIVE=1 nice -n 10 "$@"
    fi
}

# 1. The queue. Without flock the command runs at once (with a warning).
if [ "$nested" = 1 ]; then
    run "$@"
    exit $?
fi
if [ -n "${FD_HEAVY_MARK:-}" ]; then
    # Inside a slot: say that the command started, then run it.
    : > "$FD_HEAVY_MARK"
    unset FD_HEAVY_MARK
    run "$@"
    exit $?
fi
if ! command -v flock >/dev/null 2>&1; then
    echo "heavy.sh: flock is missing; the command runs without the queue." >&2
    run "$@"
    exit $?
fi
slots=${FD_HEAVY_SLOTS:-1}
lock_dir=${XDG_RUNTIME_DIR:-/tmp}/fleetdeck-heavy
mkdir -p "$lock_dir"
said=0
while :; do
    i=1
    while [ "$i" -le "$slots" ]; do
        # -o: the command's children do not hold the slot after it ends.
        # The mark tells a busy slot from a command that ran (any exit code).
        mark="$lock_dir/started.$$"
        rm -f "$mark"
        set +e
        FD_HEAVY_MARK="$mark" flock -n -o "$lock_dir/slot-$i" sh "$0" "$@"
        rc=$?
        set -e
        if [ -e "$mark" ]; then
            rm -f "$mark"
            exit "$rc"
        fi
        i=$((i + 1))
    done
    if [ "$said" -eq 0 ]; then
        echo "heavy.sh: waiting for a free slot (FD_HEAVY_SLOTS=$slots) ..." >&2
        said=1
    fi
    sleep 5
done
