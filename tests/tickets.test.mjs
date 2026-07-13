// tests/tickets.test.mjs
//
// Pure unit tests for scripts/fleetd/tickets.mjs — the Jira-key grammar shared
// by branch auto-detect (ticketFromBranch, permissive) and the manual `ticket`
// command (normalizeTicket, strict). No daemon, no I/O: this file exercises the
// naming rules directly against the regex vectors pinned in the 0.6.0 plan's
// "Naming rules (precise)" section.
//
// The strict Atlassian shape: an uppercase letter then 1–9 more uppercase alnum
// (key 2–10 chars), a hyphen, then an integer with NO leading zero, never
// embedded in a longer alphanumeric run; leftmost match wins.

import test from 'node:test';
import assert from 'node:assert/strict';
import { TICKET_RE, ticketFromBranch, normalizeTicket } from '../scripts/fleetd/tickets.mjs';

// ---------------------------------------------------------------------------
// TICKET_RE / ticketFromBranch — the plan's exact "Vectors:" list.
// ticketFromBranch is the public wrapper over TICKET_RE (leftmost embedded
// match anywhere in a branch string), so it is the natural surface for those
// vectors; a couple assert against TICKET_RE directly for boundary clarity.
// ---------------------------------------------------------------------------

test('ticketFromBranch: plan regex vectors extract the leftmost embedded key or null', () => {
  const cases = [
    ['feature/PROJ-123-checkout', 'PROJ-123'], // key embedded mid-branch
    ['fd/raven-PROJ-123', 'PROJ-123'],         // our own worktree/branch shape round-trips
    ['proj-123', null],                        // lowercase key: not a key
    ['PROJ-001', null],                        // leading zero: no partial match either
    ['A-1', null],                             // key too short (needs 2–10 chars)
    ['ABCDEFGHIJ-42', 'ABCDEFGHIJ-42'],        // 10-char key: the upper bound matches
    ['ABCDEFGHIJK-1', null],                   // 11-char key: over the bound, rejected
    ['PROJ-1-ENG-2', 'PROJ-1'],                // leftmost match wins
    ['XPROJ-123abc', null],                    // embedded in a longer alnum run: rejected
    ['viper-c7a7', null],                      // our hex callsign suffix: not a key
    ['audit-cleanup', null],                   // ordinary branch words: not a key
    ['P2P-9', 'P2P-9'],                        // digits allowed inside the key body
  ];
  for (const [branch, expected] of cases) {
    assert.equal(ticketFromBranch(branch), expected, `ticketFromBranch(${JSON.stringify(branch)})`);
  }
});

test('ticketFromBranch: non-string / empty inputs return null (never throw)', () => {
  assert.equal(ticketFromBranch(null), null);
  assert.equal(ticketFromBranch(undefined), null);
  assert.equal(ticketFromBranch(''), null);
  assert.equal(ticketFromBranch(42), null);
  assert.equal(ticketFromBranch({}), null);
});

test('TICKET_RE: boundary guards — a key must not be glued to surrounding alnum', () => {
  // Trailing boundary: the digit run must end at a non-alnum (or string end).
  assert.equal(ticketFromBranch('PROJ-123'), 'PROJ-123');
  assert.equal(ticketFromBranch('PROJ-123/foo'), 'PROJ-123'); // '/' is a boundary
  assert.equal(ticketFromBranch('PROJ-123_foo'), 'PROJ-123'); // '_' is a boundary (not [A-Za-z0-9])
  assert.equal(ticketFromBranch('PROJ-12a'), null);           // trailing letter glued: rejected
  // Leading boundary: an uppercase run immediately before must not be consumed.
  assert.equal(ticketFromBranch('xPROJ-123'), null);          // lowercase glued in front
  assert.equal(ticketFromBranch('9PROJ-123'), null);          // digit glued in front
  // A hyphen in front IS a boundary, so a key after '<animal>-' is fine.
  assert.equal(ticketFromBranch('otter-ENG-7'), 'ENG-7');
});

test('TICKET_RE carries no global-flag lastIndex state (safe to reuse)', () => {
  // No /g flag → exec never advances lastIndex, so repeated calls are stable.
  assert.equal(TICKET_RE.exec('feature/PROJ-123')?.[0], 'PROJ-123');
  assert.equal(TICKET_RE.exec('feature/PROJ-123')?.[0], 'PROJ-123');
  assert.equal(TICKET_RE.lastIndex, 0);
});

// ---------------------------------------------------------------------------
// normalizeTicket — the strict, whole-string form used by the manual command.
// The whole argument must BE a key (after trim + uppercase); anything with
// surrounding text, a leading zero, or an out-of-range key length is null.
// ---------------------------------------------------------------------------

test('normalizeTicket: the plan cases (proj-55 → PROJ-55, PROJ-007 → null, A-1 → null, ABCDEFGHIJK-1 → null)', () => {
  assert.equal(normalizeTicket('proj-55'), 'PROJ-55');       // lowercased hand-typed key canonicalizes up
  assert.equal(normalizeTicket('PROJ-007'), null);           // leading zero on the number
  assert.equal(normalizeTicket('A-1'), null);                // key too short
  assert.equal(normalizeTicket('ABCDEFGHIJK-1'), null);      // key too long (11 chars)
});

test('normalizeTicket: canonicalizes, trims, and enforces the whole-string shape', () => {
  assert.equal(normalizeTicket('PROJ-55'), 'PROJ-55');
  assert.equal(normalizeTicket('  proj-55  '), 'PROJ-55');   // trims surrounding whitespace
  assert.equal(normalizeTicket('Proj-55'), 'PROJ-55');       // mixed case → upper
  assert.equal(normalizeTicket('ABCDEFGHIJ-42'), 'ABCDEFGHIJ-42'); // 10-char key is the upper bound
  assert.equal(normalizeTicket('P2P-9'), 'P2P-9');           // digits inside the key body
});

test('normalizeTicket: rejects anything that is not exactly one key', () => {
  assert.equal(normalizeTicket('feature/PROJ-55'), null);    // surrounding text: strict, unlike branch detect
  assert.equal(normalizeTicket('PROJ-55-extra'), null);      // trailing segment
  assert.equal(normalizeTicket('PROJ-55 ENG-6'), null);      // two keys
  assert.equal(normalizeTicket('PROJ55'), null);             // missing hyphen
  assert.equal(normalizeTicket('PROJ-'), null);              // missing number
  assert.equal(normalizeTicket('PROJ-0'), null);             // zero is a leading-zero integer
  assert.equal(normalizeTicket('clear'), null);              // the command's sentinel is not a key
  assert.equal(normalizeTicket(''), null);
  assert.equal(normalizeTicket(null), null);
  assert.equal(normalizeTicket(undefined), null);
});
