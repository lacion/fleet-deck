# Review comments on the Changes diff (DL-179)

- **Status:** Proposed (revision 3, 2026-10-14). Revision 1 (commit c694b2da) got DESIGN REVIEW FAIL (`/tmp/adversary-report-c694b2da.md`); revision 2 (commit 5693a6d9) got RE-REVIEW FAIL (5 blockers, 2 major; `/tmp/adversary-report-3fda6b7f.md`). §9 and §10 map the findings. Needs a PASS before code; this is the last review round allowed for this design.
- **Owner request (2026-10-14):** "it would be amazing if we could add comments to the diff in changes, similar to a PR github and be able to reference that in the chat".
- **Replaces:** the R1.3 line notes (`web/src/chat/notes.ts`). Old local notes are imported only on an explicit owner action (§7).

## 1. Model

A **comment** belongs to one session and sits on a line or a range of lines of one file of one **diff snapshot** of that session. Comments form **threads** (a top comment and its replies). A **review** is a set of the caller's drafts that the caller sends to the agent in one follow-up.

A **diff snapshot identity** is:
- `spawn:<base oid>` for the "Since spawn" diff (the base commit of the session, DL-159); the work tree changes over time, so its anchors are checked against the current diff (§5);
- `turn:<from_commit>..<to_commit>` for a turn diff (the checkpoint commit pair of `TurnDiff`, `crates/fleetdeck-proto/src/files.rs:222`), which never changes. A provider `turn_id` is never an identity (it can repeat after a provider restart).
- **Stable selector.** `TurnDiffQuery` (`files.rs:210`) gains an optional `checkpoint: Option<{ from_commit, to_commit }>` (serde default; station capability `turn_diff_by_checkpoint`). With it, the station returns the diff of the finalized checkpoint row of **this session** whose commit pair equals it exactly (`manager/files.rs` today picks the newest row with the `turn_id`, :449); no such row → `404 checkpoint_unavailable`. Comment creation, anchor checks and navigation for a `turn:` snapshot always use this selector. A station without the capability offers comments on the Since-spawn diff only; the board hides the turn-diff comment controls for it.

Deck migration V20 (last in the list):

```sql
CREATE TABLE review_comments (
    comment_id   TEXT    PRIMARY KEY,           -- ULID
    short_id     TEXT    NOT NULL,              -- 'c-' + 6..12 lower-case base32, immutable
    station_id   TEXT    NOT NULL,
    session_id   TEXT    NOT NULL,
    thread_id    TEXT    NOT NULL,              -- = comment_id for a top comment
    snapshot     TEXT    NOT NULL,              -- 'spawn:<oid>' or 'turn:<from>..<to>'
    path         TEXT    NOT NULL,              -- the commented side's path
    other_path   TEXT,                          -- the other side's path of a rename or copy (from the diff), else NULL
    side         TEXT    NOT NULL CHECK (side IN ('new', 'old')),
    line_start   INTEGER NOT NULL CHECK (line_start >= 1),
    line_end     INTEGER NOT NULL CHECK (line_end >= line_start AND line_end - line_start < 200),
    anchor_text  TEXT    NOT NULL,              -- derived by the deck (§2.2), ≤ 4 KiB; '' for an imported note without anchor
    anchor_kind  TEXT    NOT NULL CHECK (anchor_kind IN ('diff', 'imported')),
    body         TEXT    NOT NULL,              -- Markdown, 1..8000 chars
    author_kind  TEXT    NOT NULL CHECK (author_kind IN ('owner', 'member')),
    author_name  TEXT,                          -- member name; NULL for the owner
    state        TEXT    NOT NULL CHECK (state IN ('draft', 'sent')),
    resolved     INTEGER NOT NULL DEFAULT 0 CHECK (resolved IN (0, 1)),
    review_id    TEXT    REFERENCES reviews (review_id),
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    UNIQUE (station_id, session_id, short_id)
) STRICT;
CREATE INDEX review_comments_session ON review_comments (station_id, session_id, thread_id);
CREATE TABLE reviews (
    review_id     TEXT    PRIMARY KEY,          -- ULID
    short_id      TEXT    NOT NULL,             -- 'r-' + 6..12 base32
    station_id    TEXT    NOT NULL,
    session_id    TEXT    NOT NULL,
    send_key      TEXT    NOT NULL,             -- client ULID (§3)
    fingerprint   TEXT    NOT NULL,             -- SHA-256 of the sorted comment ids
    principal     TEXT    NOT NULL,             -- 'owner' or 'member:<name>'
    command_id    TEXT    NOT NULL,
    delivery      TEXT    NOT NULL CHECK (delivery IN ('pending', 'delivered', 'failed', 'unknown')),
    attempts      INTEGER NOT NULL DEFAULT 1 CHECK (attempts BETWEEN 1 AND 5),
    input_id      TEXT,                         -- from the accepted receipt (§3 step 5)
    reason        TEXT,                         -- why a delivery failed
    comment_count INTEGER NOT NULL,
    sent_at       INTEGER NOT NULL,
    UNIQUE (station_id, session_id, principal, send_key),
    UNIQUE (station_id, session_id, short_id)
) STRICT;
```

- Short ids: allocated at insert from random base32; on a unique clash, retry with a longer id (6, 8, 10, 12 chars; then fail `500`). Never changed or reused.
- Limits: 500 comments per session (`409 too_many_comments`), 1–50 comments per send, 200 reviews per session (§3), 5 send attempts per review. `review_imports` rows are at most one per imported note (≤ 500 per session, the old notes cap), deleted with the session's comments.
- **Cleanup:** in the station removal transaction (`crates/fleetdeck-deck/src/store/stations.rs:236`) delete the station's rows. In the snapshot apply (`read_model.rs:310-329`), in the same transaction that removes the timelines of sessions absent from the completed snapshot, delete the review rows of those same sessions; rows of sessions present in the snapshot stay. No foreign key to `sessions` (the snapshot path deletes and reinserts session rows).

## 2. Routes and authorization

### 2.1 Principals

| Principal | Read (GET) | Write (create, edit, delete, resolve, send) |
| --- | --- | --- |
| Owner | all sent comments; own drafts | yes |
| Member with access to the session | sent comments whose path is not member-denied (§2.3); own drafts | only when the member may follow up on the session: the route is registered with `.member()`, applies the session and role checks, and Send applies `MemberAction::FollowUp` and `ensure_driver` (`crates/fleetdeck-deck/src/http/access.rs:40,88`) |
| Share guest (read access) | as a member without drafts | no (`403`) |
| API token | with `read` scope: sent comments only, filtered as for the owner | no (`403 owner_or_member_only`); a token never has drafts and is never mapped to the owner for draft access |

### 2.2 Routes (all under `/api/sessions/{station}/{session}/review`; writes need CSRF for cookie logins)

| Route | Rule |
| --- | --- |
| `GET …/review?reviews_before=<review_id>` | `{ threads, reviews, rev }`: threads with their visible comments (§2.1, §2.3); the newest 50 reviews of the session (`review_id`, `short_id`, `principal`, `delivery`, `attempts`, `input_id`, `sent_at`, comment short ids), older ones by `reviews_before`; and `review_rev` (§4). |
| `POST …/review/comments` `{ snapshot, path, side, line_start, line_end, body, thread_id? }` | Top comment: the deck fetches that diff snapshot from the station **as the caller** (the existing file routes, with the member flag for a member, so the station's member deny list and private paths apply; the result is cached 60 s per (session, snapshot, principal kind)), finds `path` in it, and derives `anchor_text` from the lines `line_start..line_end` of `side`. A path or range that is not in that diff → `400 not_in_diff`. The client never sends `anchor_text`. Reply (`thread_id` set): the thread's top comment must be in this session and visible to the caller (sent, or the caller's own draft); the reply copies the top comment's snapshot, path, side, range and anchor. |
| `PATCH …/review/comments/{id}` `{ body?, resolved? }` | Lookup by id within this station and session and the caller's visibility; anything else (another session, another author's draft) → `404 unknown_comment`. `body`: author only, draft only (`409 not_draft`). `resolved`: a top comment that is sent (any writer), or the caller's own draft top comment. |
| `DELETE …/review/comments/{id}` | Same lookup. Author only, draft only. Deleting a draft top comment deletes its draft replies only when every one of them has the same author (always true, because a draft top comment is visible only to its author and a sent comment never returns to draft); any other case → `409 thread_has_replies`. A sent comment is never deleted. |
| `POST …/review/send` `{ send_key, comment_ids }` | §3. |

- Path rule for every stored path: relative, no `..` component, no NUL, no control character (including `\n`, `\r`), ≤ 1024 bytes; else `400 invalid_body`. It must also be in the diff (above).
- Every mutation writes an audit row (`review.comment`, `review.edit`, `review.delete`, `review.resolve`, `review.send`; ids only).

### 2.3 Member-denied paths

- A comment is **member-hidden** when its `path` or its `other_path` matches the station's member deny list (`crates/fleetdeck-proto/src/member_deny.rs`; the same two-endpoint rule as `MemberDeny::filter_patch`, :151-155). The deck reads `path` and `other_path` from the diff (the `diff --git a/<old> b/<new>` and rename headers), never from the client.
- A member-hidden comment is visible to the owner and to tokens only: never in a member's or guest's GET, Review card or chip. A member cannot create one (the member-flag diff omits the file → `not_in_diff`).
- **Send:** the follow-up text carries no anchor excerpt for a member-hidden comment (path and lines only, "(excerpt hidden: protected file)"), and no excerpt at all for any comment when the deny list of the station cannot be established at send time (fail closed: "(excerpt hidden)"). The session timeline, which members and guests may read, therefore never shows a denied excerpt.
- **Reads:** when the deny list cannot be established, a member or guest gets no comments of that session.

## 3. Send a review

1. The request carries a client `send_key` (a ULID made when the writer opens the review bar) and 1–50 distinct `comment_ids`.
2. **Prepare** outside the transaction: the routing metadata of a follow-up to this session (the same preparation that the board's follow-up uses, without submitting).
3. **One deck transaction, in this order:**
   1. **Replay first, inside the transaction:** a `reviews` row for (station, session, principal, `send_key`) with the same fingerprint → return its stored result (`review_id`, `short_id`, `command_id`, `delivery`) and commit nothing; another fingerprint → `409 send_key_reused`. Because this lookup and the insert of step 3.5 are in one write transaction (the deck has one writer connection), two identical concurrent sends get the same result.
   2. The per-session review cap: at most 200 `reviews` rows per session in their lifetime (`409 too_many_reviews`).
   3. Check the drafts (the caller's, this session, still drafts; every reply's top comment is sent or in this send → else `409 thread_root_unsent`).
   4. Build the follow-up text (§3.1) and insert the immutable pending command with `commands::insert` (`crates/fleetdeck-deck/src/store/commands.rs:186`), with `UserInput.author` set for a member.
   5. Insert the `reviews` row (`delivery = 'pending'`), set the drafts `state = 'sent'` and `review_id`, bump `review_rev`. Commit.
4. After the commit: notify the router to dispatch the command (the path that `submit_by` uses after its insert). The router's pending-command recovery sends a command committed before a crash.
5. **Publication is separate from delivery.** A sent comment stays sent forever (it is never turned back into a draft), so replies by other authors never lose their root. The review's `delivery` follows its command in the transaction that records the command's end: an accepted receipt → `delivered` (and the receipt's `input_id` is stored on the review, §4); rejected, failed or expired → `failed` (with the reason); ambiguous → `unknown`. These are the deck command states of `commands.rs:367-388`.
6. **Retry:** for a `failed` or `unknown` review, the writer who sent it can press "Send again": a new command (new `command_id`) with the same text, recorded on the same review (`reviews.attempts`, at most 5; the board warns for `unknown` that the agent may have the review already). The review keeps its id and short id.

(The `reviews` columns `delivery`, `attempts`, `input_id` and `reason` in §1 replace the revision-2 column `state`.)

### 3.1 Follow-up text

- Header from the principal: `Review r-<id> from the owner (<n> comments). Reply to each comment by its id.` or `Review r-<id> from member <name> (<n> comments). …`. Never "owner" for a member.
- Each comment:

````
[c-<id>] <path> line <a>[-<b>] (<new|old> side)
```<lang or text>
<anchor lines, at most 6>
```
<body>
````

  The code fence uses a run of backticks one longer than the longest backtick run in the anchor. `path` passed the path rule (no newline), so it cannot forge a header. A reply is `[c-<id>] reply to [c-<top id>]` with the body only. An imported comment (`anchor_kind = imported`) says `(no excerpt: imported note)`.
- Total ≤ 64 KiB, each body ≤ 8000 chars; more → `400 review_too_large`.

## 4. Live updates

- `DeckSession` (the card, `crates/fleetdeck-proto/src/board.rs:78`) gains `review_rev: Option<u32>` (serde default, skipped when `None`/0), filled in `sessions_for`. Every comment mutation, send and failure bumps the session's counter (deck table `review_revs (station_id, session_id, rev)` in V20, cleaned with the comments). A changed card triggers the existing socket delta, so every open board sees the change.
- The board refetches `GET …/review` when `review_rev` of the open session changes, after its own writes, and after a socket reconnect.

## 5. Board

- **Changes tab** (`web/src/chat/Diff.tsx`, Since spawn and turn diffs): a click or tap on a line number opens a comment editor under the line (textarea, Markdown, "Add comment" / "Cancel"); shift-click (or "Extend to here" on a second tap) makes a range. Deleted lines can be commented on the old side. Threads show under their last line: author (owner or member name), time, body (rendered with the existing safe Markdown renderer, `web/src/chat/Markdown.tsx`, `skipHtml`), the short id, "Reply", "Resolve"/"Unresolve", and for own drafts "Edit"/"Delete" and a "Draft" badge.
- **Outdated:** a thread is shown on its line only when the current diff has the same snapshot identity (turn) or, for `spawn:`, the same base oid and the lines `line_start..line_end` of that side equal `anchor_text`. Else it is listed under "Outdated" at the top of its file, or in "Comments not in this diff" when the file left the diff. When the diff is cut (`truncated`) and the line is past the cut, the thread is listed as "Not shown: the diff is cut", not as outdated.
- **Review bar** (sticky at the bottom of Changes when the caller has drafts): "Review: <n> drafts — Send review / Discard all"; it keeps its `send_key` until a send succeeds.
- **Chat:** `run.started` gains an optional `input_id` (serde default, skipped when `None`; the station fills it from the input record that started the run; older peers omit it), and the board keeps it on the user item. A user item shows as a "Review" card only when its `input_id` equals the `input_id` of a review of this session that GET returned (stored from the command receipt, §3 step 5); a message that only copies a header stays plain text. The card shows one row per comment (`c-xxxxxx path:line` and the first body line) and a "Show the text that the agent received" toggle that shows the exact follow-up text; a row opens Changes at that file and line and highlights the thread. In any chat message of this session, a token `c-xxxxxx` or `[c-xxxxxx]` that equals the full short id of a comment visible to the caller **in this session** becomes a link chip (a React element; never inside a Markdown link or a code span; no prefix match; no lookup in another session). A stale answer for another session (after navigation) is dropped.
- **Composer `#` menu:** `#` and 1+ characters at a word start open a menu of the session's visible comments (short id, path:line, first body line; at most 20, newest first) from the loaded threads; a pick inserts `[c-xxxxxx] `.

## 6. Not in scope

- No agent tool to reply to a comment (a later MCP tool can use the same table). No GitHub sync. No comments on files outside a diff.

## 7. Old local notes

- The board keeps `fd-notes:<station>/<session>` in `localStorage` unchanged and shows, in Changes, "You have <n> local notes from an older version. Import them as draft comments?" only to a principal that can write (owner, or a member who may follow up). The import is an explicit button press.
- Import: one `POST …/review/comments` per unsent note with `import_key = sha256(station/session/file/line/text)` (new optional body field, stored in a deck table `review_imports (station_id, session_id, principal, import_key)` with a unique key, so a repeated import makes no duplicate). A note whose line is still in the current Since-spawn diff gets a `diff` anchor; else an `imported` anchor with `anchor_text = ''`, labelled "Imported note" in the UI and in the follow-up.
- The local key is removed only after every note got a success or an "already imported" answer. Sent notes are dropped. A viewer never sees the prompt.

## 8. Tests

- Deck: V20 valid and last; cleanup on station removal and on a snapshot without the session (and kept with it); per-principal matrix of every route (owner, member who can follow up, member who cannot, guest, token with and without read; CSRF); another author's draft → `404` on PATCH/DELETE/resolve and absent from GET; `not_in_diff` for a path or range not in the caller's diff; a member cannot comment on, nor read, a member-denied path, and the follow-up of an owner review hides its excerpt; anchors derived server-side (a client `anchor_text` field is refused as unknown); path rule (newline, `..`, absolute); send replay with the same key (same result, one command), another fingerprint (`409`), concurrent identical sends (one command); `thread_root_unsent`; atomicity (an injected failure after the command insert leaves no review and no flipped draft); dispatch after commit and recovery of a pending command; a rejected command flips the review to `failed` and the comments back to drafts; member header and `UserInput.author`; short id clash retry and immutability; `review_rev` on the card changes on each mutation; two boards: one writes, the other gets the delta.
- Revision 3 regressions: a rename from a denied file to an allowed name is member-hidden and its excerpt is not sent; a send with the deny list unavailable carries no excerpt; two concurrent identical sends return the same result and one command; a failed delivery keeps the comments sent, another author's reply keeps its root, "Send again" makes a new command on the same review, at most 5; an `unknown` delivery is not retried automatically; an accepted command whose receipt is lost leaves `pending` until the router settles it; a member input that copies a review header plus other text renders as plain text; a turn comment uses the checkpoint selector and survives a reused provider `turn_id`; `checkpoint_unavailable`; a station without `turn_diff_by_checkpoint` shows no turn comment controls; review pagination; the 200-review cap.
- Board: editor, range, old side, threads, resolve, reply inheritance, outdated vs cut, review bar with a stable `send_key`, the Review card from the header, chips only for full ids of this session (not in links or code, not another session's identical id), hostile Markdown/HTML/URLs in bodies, the `#` menu, the import prompt (only for writers; partial failure keeps the local key; repeat import no duplicates), phone layout.

## 9. Review map (revision 1 → revision 2)

| Finding | Change |
| --- | --- |
| B anchors bypass member restrictions | §2.2 server-side diff fetch as the caller; §2.3 member-denied paths hidden from members and from follow-up excerpts. |
| B another author's draft can be resolved | §2.2 PATCH/DELETE lookup and resolve rule. |
| B send not atomic | §3 steps 3–6 with `commands::insert`, dispatch after commit, recovery. |
| B client-supplied anchors | §2.2 anchors derived by the deck; `not_in_diff`; path rule; §3.1 escaped fences. |
| B member reviews say "owner" | §3.1 header from the principal; `UserInput.author`. |
| B legacy import crosses authors | §7 explicit import by a writer, dedupe keys, local removal after success. |
| M send retry | §3 `send_key`, fingerprint, replay first, 1–50 ids. |
| M orphan replies | §2.2 reply rules, delete cascade of own draft replies, `thread_root_unsent`. |
| M no client invalidation | §4 `review_rev` on the card. |
| M Review card correlation | §5 header match against GET's reviews (no input-id correlation needed); §3 step 6 failed reviews. |
| M turn id not stable | §1 snapshot identity by checkpoint commits; §5 outdated vs cut. |
| M short ids not persisted | §1 stored, unique, immutable, retried. |
| M retention | §1 cleanup in the snapshot and station-removal transactions. |
| U tokens and mob drivers | §2.1 principal table. |
| M tests | §8. |

## 10. Re-review map (revision 2 → revision 3)

| Finding (rev 2 report) | Change |
| --- | --- |
| A rename endpoints and Send policy | §2.3 `other_path`, two-endpoint rule, fail-closed excerpts on Send. |
| B failure rollback orphans replies | §3 steps 5–6: publication separate from delivery, no return to draft, retry on the same review; §2.2 delete rule. |
| C replay outside the transaction | §3 step 3.1 replay first inside the transaction. |
| D copied header spoofs the card | §5 `run.started.input_id` and receipt correlation; the text toggle. |
| E commit pair not fetchable | §1 stable selector `checkpoint` on `TurnDiffQuery`, capability, `checkpoint_unavailable`. |
| F unbounded failed reviews | §1 limits (200 reviews, 5 attempts), §2.2 pagination. |
| G tests and terminal states | §3 step 5 state mapping; §8 revision-3 regressions. |
