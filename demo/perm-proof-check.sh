#!/bin/bash
# demo/perm-proof-check.sh — verdict logic for the Phase 3 permission-relay
# acceptance check. Sourced by demo/run-accept-phase3.sh and exercised
# directly by tests/accept-phase3-perm-proof.test.mjs.
#
# BUG-011: the check must certify that the board-approved Bash command
# actually ran — the proof file on disk with the exact bytes the prompt
# specified — not that the model's prose contains the marker (the prompt
# itself names the marker, so grep on the JSON output proves nothing).
#
# perm_proof_check <rc> <proof_file> <perm_json_log> <approval_confirmed>
#   rc                  exit status of the `claude -p` run
#   proof_file          path the approved command was told to create
#   perm_json_log       the run's --output-format json capture (diagnostics only)
#   approval_confirmed  non-empty only when the answer POST returned "ok":true
# Prints one line of diagnostics on failure. Returns 0 pass / 1 fail.
perm_proof_check() {
  local rc="$1" proof_file="$2" perm_json="$3" approved="${4:-}"
  local detail=""
  if [ "$rc" -ne 0 ]; then
    detail="claude exited rc=$rc"
  elif [ -z "$approved" ]; then
    detail="board answer POST did not succeed"
  elif [ ! -f "$proof_file" ]; then
    detail="proof file missing: $proof_file"
  elif ! printf 'FLEET_PERMISSION_OK' | cmp -s - "$proof_file"; then
    detail="proof file bytes differ from FLEET_PERMISSION_OK: $(cat "$proof_file")"
  else
    return 0
  fi
  if grep -q "FLEET_PERMISSION_OK" "$perm_json" 2>/dev/null; then
    detail="$detail (marker present in model prose only — not proof of execution)"
  fi
  echo "$detail"
  return 1
}
