#!/bin/sh
# Runs a heavy command (cargo build, clippy, nextest, make check, bun e2e) in
# the shared user slice `fdbuild.slice` at a low priority, so parallel builds
# and tests of several agents never take the whole machine from the deck and
# the station. The slice caps CPU and memory for all of them together:
# ~/.config/systemd/user/fdbuild.slice. Without systemd (macOS, a container)
# it only lowers the priority.
#
#   sh tools/heavy.sh make check
#   sh tools/heavy.sh cargo nextest run -p fleetdeck-station
set -eu
if [ "$#" -eq 0 ]; then
    echo "usage: sh tools/heavy.sh <command> [args...]" >&2
    exit 2
fi
if command -v systemd-run >/dev/null 2>&1 \
    && systemctl --user cat fdbuild.slice >/dev/null 2>&1; then
    exec systemd-run --user --scope --quiet --same-dir --slice=fdbuild.slice \
        --setenv=PATH="$PATH" nice -n 10 "$@"
fi
exec nice -n 10 "$@"
