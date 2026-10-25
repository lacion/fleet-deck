# v1 lessons: research notes (2026-10-13)

Five readers compared the v1 planning documents of branch `fd/typescript-migration` with v2 at `c7820bf`. Their notes are the evidence behind `docs/v2/FLEETDECK-V2.md`. Where a note and that document disagree, the document wins: the architect checked these claims in the code before writing it:
- Claude rate-limit windows are mapped from `rate_limit_event` (`crates/fleetdeck-providers/src/claude/map.rs`), so they are partial, not missing (note d).
- No task state ends a task when its worker is lost, so dead-worker synthesis is missing (note c, item 11, says "has").

| File | Documents |
| --- | --- |
| `a-v1-future.md` | fleetdeck-v1.0, fleetdeck-future, ts-migration, phase3, phase4, phase6 |
| `b-lessons-ux.md` | orca-lessons, t3code-lessons, ux-feedback-plan |
| `c-architecture.md` | v1 README, architecture, design-spec, foundations, foundations-hardening, validation-and-gates |
| `d-providers.md` | p1-cc-provider, p1-codex-provider, p2-harvest-surface, p3-issue-pr-spawning, p4-usage-accounts |
| `e-fleet-ui.md` | p5-programmable-fleet, p6-unified-views, p7-drive-and-observe, ui-spec |
