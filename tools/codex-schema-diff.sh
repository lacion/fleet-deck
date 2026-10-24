#!/bin/sh
# R8.2 (DL-162): diffs the app-server JSON schema of the installed `codex`
# against the schema that the Codex adapter pins (spikes/s2-codex/schema/).
# A local check for the owner, not part of `make check` (it needs the CLI).
#
#   make codex-schema-diff                  # compare with the pinned 0.160.0 (made with --experimental)
#   FD_CODEX_SCHEMA_PIN=0.160.0 sh tools/codex-schema-diff.sh
#
# Exit 0: the schemas are equal. Exit 1: they differ (the diff is printed).
# Exit 2: the CLI is missing or cannot generate the schema.

set -u
cd "$(dirname "$0")/.." || exit 2

pin=${FD_CODEX_SCHEMA_PIN:-0.160.0}
pinned="spikes/s2-codex/schema/$pin"
bundle=codex_app_server_protocol.schemas.json

if [ ! -f "$pinned/$bundle" ]; then
    echo "codex-schema-diff: no pinned schema at $pinned/$bundle" >&2
    exit 2
fi
if ! command -v codex >/dev/null 2>&1; then
    echo "codex-schema-diff: the codex CLI is not on PATH." >&2
    echo "  Install it, or generate the schema on a machine that has it:" >&2
    echo "    codex app-server generate-json-schema --experimental --out <dir>" >&2
    echo "  then diff <dir>/$bundle against $pinned/$bundle." >&2
    exit 2
fi
if ! codex app-server generate-json-schema --help >/dev/null 2>&1; then
    echo "codex-schema-diff: this codex has no 'app-server generate-json-schema'." >&2
    echo "  Run 'codex app-server --help' to find the schema command of this release," >&2
    echo "  write its output to a directory and diff it against $pinned." >&2
    exit 2
fi

tmp=$(mktemp -d "${TMPDIR:-/tmp}/fd-codex-schema.XXXXXX") || exit 2
trap 'rm -rf "$tmp"' EXIT
echo "codex-schema-diff: $(codex --version 2>&1 | head -n 1) against the pinned $pin"
if ! codex app-server generate-json-schema --experimental --out "$tmp" >/dev/null 2>&1; then
    echo "codex-schema-diff: schema generation failed." >&2
    exit 2
fi
if [ ! -f "$tmp/$bundle" ]; then
    echo "codex-schema-diff: the CLI wrote no $bundle (files: $(ls "$tmp" | tr '\n' ' '))." >&2
    exit 2
fi
if diff -u "$pinned/$bundle" "$tmp/$bundle"; then
    echo "codex-schema-diff: the schema equals the pinned $pin."
else
    echo "codex-schema-diff: the schema differs from the pinned $pin." >&2
    echo "  Read the diff, update the adapter and the fixtures, then raise TESTED_MIN_VERSION" >&2
    echo "  in crates/fleetdeck-providers/src/codex/mod.rs only after a live smoke passes." >&2
    exit 1
fi
