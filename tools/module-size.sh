#!/bin/sh
# Module size rule of the M3 goal: every Rust module under crates/ and every
# TypeScript module under web/src/ stays under 800 lines. Prints each file
# over the limit and exits non-zero if one exists. Part of `make check`.

set -u
cd "$(dirname "$0")/.." || exit 2

limit=${FD_MODULE_LIMIT:-800}
over=$(find crates web/src \( -name target -o -name node_modules \) -prune -o \
    -type f \( -name '*.rs' -o -name '*.ts' -o -name '*.tsx' \) -print0 |
    xargs -0 wc -l | awk -v limit="$limit" '$2 != "total" && $1 >= limit {print}')

if [ -n "$over" ]; then
    echo "modules with $limit or more lines:"
    echo "$over"
    exit 1
fi
echo "module size: every module under crates/ and web/src/ has fewer than $limit lines"
