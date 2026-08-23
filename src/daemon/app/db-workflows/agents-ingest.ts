// db-workflows/agents-ingest.ts — P8.6 slice 3: the agents-poll ingest yields Store.
//
// Third module of the P8.6 db-workflows family. The canonical convention — the
// four rules (root-context-only yields Store; ONE coarse operation per tick,
// never per statement; sync-stays-sync with `yield* Store` BEFORE the coarse op,
// never inside a transaction callback; failure translated ONCE so the workflow's
// boundary sees a byte-identical failure) — lives verbatim at
// db-workflows/retention.ts. Read it there; this header only records what is
// agents-ingest-specific.
//
// Agents-ingest-specific notes:
//
//   * ONE root-context leg. The agents poller runs as a P5 daemon-long schedule
//     forked under the shared Background owner — the root fiber, where the
//     root-published Store service is reachable — so the ingest leg yields Store,
//     exactly as the retention pilot yields it in each of its ops. The three
//     `q.<stmt>` calls inside core.ingestAgentsPoll (session upsert, agent-session
//     insert, session enumeration) are the SQLite seam; they do not move here and
//     do not change. This module only chooses where the handle comes from.
//
//   * COARSE boundary mirrors the legacy adapter one-for-one: the whole
//     synchronous core.ingestAgentsPoll runs inside the single Effect.try, and a
//     throw becomes the identical AgentsPollIngestError. The scheduler's fail-open
//     ingest boundary (agents-poll.ts ingestPoll) catches that tag and skips the
//     tick, so a decode-clean poll whose ingest throws never stops the loop —
//     byte-identical to the legacy path.
//
//   * The `yield* Store` prefix is a pure, infallible Context read: it changes no
//     failure, defect, or fail-open-skip semantics. It only lifts the work's
//     requirement from `never` to `Store`, discharged once by the whole-gen
//     provideService(Store) the background program already performs.

import * as Effect from 'effect/Effect';

import {
  type AgentsIngestCallbacks,
  type AgentsIngestWork,
  AgentsPollIngestError,
} from '../agents-poll.ts';
import { Store } from '../services/store.ts';

/**
 * Store-backed agents ingest work. Structurally identical to
 * legacyAgentsIngestWork, except it first yields the root-owned Store service, so
 * the work's requirement is `Store` rather than `never`. The coarse operation
 * boundary and the failure translation are the same: a synchronous ingest throw
 * becomes the identical AgentsPollIngestError via the one Effect.try, which the
 * scheduler's fail-open ingestPoll boundary catches by tag either way.
 */
export function makeStoreAgentsIngestWork(
  callbacks: AgentsIngestCallbacks,
): AgentsIngestWork<Store> {
  const ingestAgentsPoll = callbacks.ingestAgentsPoll.bind(callbacks);
  return (records) =>
    Effect.gen(function* () {
      // Declare the root-owned Store dependency. The synchronous ingest runs
      // whole inside the one Effect.try below — no yielding inside the work.
      yield* Store;
      return yield* Effect.try({
        try: () => ingestAgentsPoll(records),
        catch: (cause) => new AgentsPollIngestError({ cause }),
      });
    });
}
