# M2 gate — result, correction, and open items

Date: 2026-10-12. Scope: PLAN Phases 1–5 (M2). Decision log: DL-070 (correction), DL-071 (gate closed).

This record contains only the facts that the architect recorded for the M2 gate. It does not add findings.

## 1. Result

- The M2 gate **passed** on the final M2 HEAD `548d9478c51c3e7bc2c34c3ab81dcaba451a8114`.
- The first independent gate review failed. A correction (DL-070) fixed the two MAJOR defects. A fresh adversary code review of the corrected HEAD gave **VERDICT: PASS**.
- Three items stay open. They go to M3 as required hardening items (§6).
- The owner has **not yet** run the MVP demo. The demo checklist is [DEMO-M2.md](./DEMO-M2.md). All automated M2 evidence ran on one machine (§7).

## 2. First review (FAIL)

- Target: `b139891` and the pending work.
- Reviewer: adversary conversation `conv-5b2778fe` (`gpt-6.1-sol`).
- Verdict: **FAILED**, with two MAJOR defects:
  1. A malformed Codex question answer claimed `runtime_requests.receipt` before native validation. Because of this, later answers were silently `Repeated`.
  2. `session.state_changed` (`lost`/`exited`) did not end the timeline tree. A recovery `background.updated` with an empty list marked a killed background child as done.
- An independent `make check` on the same target passed all 14 commands (Rust 823 passed, 3 skipped; web 80). This shows that the checks did not cover the two defects.

## 3. Correction

- Commit `767cc99`: the fix (DL-070).
- Commit `548d947`: documentation only.
- Final M2 HEAD: `548d9478c51c3e7bc2c34c3ab81dcaba451a8114`.

## 4. Independent checks

The architect ran `make check` in a clean clone of `548d947`:

| Check | Result |
|---|---|
| `make check` | exit 0, `check: all 14 commands passed` |
| Rust (nextest) | 837 passed, 3 skipped |
| TS drift | 2 passed |
| Web (`bun test`) | 88 pass, 0 fail |
| Claude replays | 29 |
| Codex replays | 18 |
| Browser suites (`e2e`, `e2e:real`) | both passed |

## 5. Adversary re-review (PASS)

- Mode: CODE REVIEW of `548d947`.
- Reviewer: subagent `sa-f448d108` (`gpt-6.1-sol`), clean clone `/tmp/fd-adversary-m2.UTbIq4/repo`.
- Verdict: **VERDICT: PASS**. No new MAJOR or BLOCKER finding.
- The reviewer ran the two reproductions again.
- The reviewer ran `make check` itself: all 14 commands passed (837 Rust, 88 web, 29 Claude and 18 Codex replays).
- Focused runs:

| Test filter | Tests |
|---|---|
| `manager::tests::codex` | 5 |
| `store::effects_tests::m2_gate` | 8 |
| proto fold | 11 |
| `the_station_validator_is_the_core_validator_over_the_event_input` | 1 |
| `timeline.test.ts` | 15 |

## 6. Open items carried to M3

The re-review found these items. They are not fixed. PLAN Phases 6 and 7 list them as required M3 hardening items.

| Id | Item | Evidence | Follow-up |
|---|---|---|---|
| P2-a | A first-prompt failure is only logged. The start path logs "the first prompt was not sent" and returns `Dispatch::Confirmed`. | `crates/fleetdeck-station/src/manager/target.rs:146-163` | Record failure evidence for the initial input. |
| P2-b | Codex message-channel answer effects never confirm. These requests have no node id and use the input id `answer-{app_id}`. The observer confirms answers only through node-correlated tool events. | `crates/fleetdeck-providers/src/codex/requests.rs:340-348,395-411`; `crates/fleetdeck-station/src/store/effects.rs:571-595` | Correlated provider input-start confirmation. |
| UNVERIFIED | Provider survival after a station `SIGKILL`. The host uses a separate process group and `kill_on_drop(true)`. Nothing proves that the child stops after an uncatchable station kill. This is not a proven defect. | `crates/fleetdeck-providers/src/host/direct.rs:50-60` | An offline child-survival test. |

## 7. Not validated (owner/platform)

These were **not** performed:

- A physical two-machine demo.
- A real phone.
- A real private LAN or a real Tailscale tailnet.
- A macOS run.

All M2 browser evidence is Playwright at 390×844 against local processes on loopback. The owner checklist for these items is [DEMO-M2.md](./DEMO-M2.md). The owner has not yet run it.
