#!/bin/sh
# R8.7 (DL-162): the board build is reproducible. It builds `web/` twice into
# two temporary directories and compares the SHA-256 of every file. The deck
# binary embeds web/dist (rust-embed), so equal files mean an equal embed.
#
#   make board-repro
#
# Exit 0: both builds have the same files with the same hashes. Exit 1: they
# differ (the differences are printed). Exit 2: a build failed.
# It writes only to two temporary directories (removed at the end).

set -u
cd "$(dirname "$0")/.." || exit 2

tmp=$(mktemp -d "${TMPDIR:-/tmp}/fd-board-repro.XXXXXX") || exit 2
trap 'rm -rf "$tmp"' EXIT

# macOS has `shasum`, not `sha256sum`.
if command -v sha256sum >/dev/null 2>&1; then
    hash256() { sha256sum "$1" | cut -d' ' -f1; }
else
    hash256() { shasum -a 256 "$1" | cut -d' ' -f1; }
fi

bun install --cwd web --frozen-lockfile --offline --ignore-scripts >/dev/null 2>&1 || {
    echo "board-repro: bun install failed" >&2
    exit 2
}
for n in 1 2; do
    if ! (cd web && bunx vite build --outDir "$tmp/build-$n" --emptyOutDir >"$tmp/build-$n.log" 2>&1); then
        echo "board-repro: build $n failed" >&2
        tail -n 30 "$tmp/build-$n.log" >&2
        exit 2
    fi
    (cd "$tmp/build-$n" && find . -type f | LC_ALL=C sort | while IFS= read -r f; do
        printf '%s  %s\n' "$(hash256 "$f")" "$f"
    done) >"$tmp/hashes-$n.txt"
done

files=$(wc -l <"$tmp/hashes-1.txt" | tr -d ' ')
if [ "$files" -eq 0 ]; then
    echo "board-repro: the build wrote no files" >&2
    exit 2
fi
if diff -u "$tmp/hashes-1.txt" "$tmp/hashes-2.txt"; then
    echo "board-repro: two builds gave the same $files files with the same hashes"
else
    echo "board-repro: the two builds differ" >&2
    exit 1
fi
