# Review comments on the Changes diff (DL-179)

- **Status:** Proposed (revision 2, 2026-10-14). Revision 1 (commit c694b2da) got adversary DESIGN REVIEW FAIL (6 blockers, 7 major, 1 unverified; report `/tmp/adversary-report-c694b2da.md`); §9 maps each finding. Needs a PASS before code.
- **Owner request (2026-10-14):** "it would be amazing if we could add comments to the diff in changes, similar to a PR github and be able to reference that in the chat".
- **Replaces:** the R1.3 line notes (`web/src/chat/notes.ts`). Old local notes are imported only on an explicit owner action (§7).

## 1. Model

A **comment** belongs to one session and sits on a line or a range of lines of one file of one **diff snapshot** of that session. Comments form **threads** (a top comment and its replies). A **review** is a set of the caller's drafts that the caller sends to the agent in one follow-up.

A **diff snapshot identity** is:
- `spawn:<base oid>` for the "Since spawn" diff (the base commit of the session, DL-159); the work tree changes over time, so its anchors are checked against the current diff (§5);
- `turn:<from_commit>..<to_commit>` for a turn diff (the checkpoint commit pair of `TurnDiff`, `crates/fleetdeck-proto/src/files.rs:222`), which never changes. A provider `turn_id` is never an identity (it can repeat after a provider restart).

Deck migration V20 (last in the list):

```sql
CREATE TABLE review_comments (
    comment_id   TEXT    PRIMARY KEY,           -- ULID
    short_id     TEXT    NOT NULL,              -- 'c-' + 6..12 lower-case base32, immutable
    station_id   TEXT    NOT NULL,
    session_id   TEXT    NOT NULL,
    thread_id    TEXT    NOT NULL,              -- = comment_id for a top comment
    snapshot     TEXT    NOT NULL,              -- 'spawn:<oid>' or 'turn:<from>..<to>'
    path         TEXT    NOT NULL,
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
    state         TEXT    NOT NULL CHECK (state IN ('sent', 'failed')),
    comment_count INTEGER NOT NULL,
    sent_at       INTEGER NOT NULL,
    UNIQUE (station_id, session_id, principal, send_key),
    UNIQUE (station_id, session_id, short_id)
) STRICT;
```

- Short ids: allocated at insert from random base32; on a unique clash, retry with a longer id (6, 8, 10, 12 chars; then fail `500`). Never changed or reused.
- Limits: 500 comments per session (`409 too_many_comments`), 1–50 comments per send.
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
| `GET …/review` | `{ threads, reviews, rev }`: threads with their visible comments (§2.1, §2.3), the session's reviews (`review_id`, `short_id`, `principal`, `state`, `sent_at`, comment short ids), and the session's `review_rev` (§4). |
| `POST …/review/comments` `{ snapshot, path, side, line_start, line_end, body, thread_id? }` | Top comment: the deck fetches that diff snapshot from the station **as the caller** (the existing file routes, with the member flag for a member, so the station's member deny list and private paths apply; the result is cached 60 s per (session, snapshot, principal kind)), finds `path` in it, and derives `anchor_text` from the lines `line_start..line_end` of `side`. A path or range that is not in that diff → `400 not_in_diff`. The client never sends `anchor_text`. Reply (`thread_id` set): the thread's top comment must be in this session and visible to the caller (sent, or the caller's own draft); the reply copies the top comment's snapshot, path, side, range and anchor. |
| `PATCH …/review/comments/{id}` `{ body?, resolved? }` | Lookup by id within this station and session and the caller's visibility; anything else (another session, another author's draft) → `404 unknown_comment`. `body`: author only, draft only (`409 not_draft`). `resolved`: a top comment that is sent (any writer), or the caller's own draft top comment. |
| `DELETE …/review/comments/{id}` | Same lookup. Author only, draft only. Deleting a draft top comment deletes its draft replies (all are the author's, because a draft top comment is visible to its author only). A sent comment is never deleted. |
| `POST …/review/send` `{ send_key, comment_ids }` | §3. |

- Path rule for every stored path: relative, no `..` component, no NUL, no control character (including `\n`, `\r`), ≤ 1024 bytes; else `400 invalid_body`. It must also be in the diff (above).
- Every mutation writes an audit row (`review.comment`, `review.edit`, `review.delete`, `review.resolve`, `review.send`; ids only).

### 2.3 Member-denied paths

- A comment whose `path` matches the station's member deny list (`crates/fleetdeck-proto/src/member_deny.rs`, the same patterns the station applies, R11.9) is visible to the owner and to tokens only. A member never gets it in GET, in a Review card, or in a chip; a member cannot create one (the member-flag diff does not contain the file → `not_in_diff`).
- The follow-up text never carries the anchor excerpt of a member-denied path; it carries `path` and lines only, with the note "(excerpt hidden: protected file)". So the session timeline, which members and guests may read, does not leak it.
- If the deny check cannot be computed (no deny list known for the station), a member gets no comments of that session (fail closed).

## 3. Send a review

1. The request carries a client `send_key` (a ULID made when the owner opens the review bar) and 1–50 distinct `comment_ids`.
2. **Replay first.** If a `reviews` row exists for (station, session, principal, send_key): with the same fingerprint (SHA-256 of the sorted ids) → return the stored result (`review_id`, `short_id`, `command_id`, `state`) without other checks; with another fingerprint → `409 send_key_reused`.
3. **Prepare** outside the transaction: the routing metadata of a follow-up to this session (the same preparation that the board's follow-up uses, without submitting).
4. **One deck transaction:** check the drafts (the caller's, this session, still drafts; every reply's top comment is sent or in this send — else `409 thread_root_unsent`); build the follow-up text (§3.1); insert the immutable pending command with `commands::insert` (`crates/fleetdeck-deck/src/store/commands.rs:186`), with `UserInput.author` set for a member (as `command_routes.rs:324` does); insert the `reviews` row; set the drafts `state = 'sent'` and `review_id`; bump `review_rev`. Commit.
5. After the commit: notify the router to dispatch the pending command (the path that `submit_by` uses after its own insert). A pending command that was committed but not dispatched before a crash is sent by the router's existing pending-command recovery at start.
6. **Failure after commit:** when the command ends rejected or dead, the deck, in the transaction that records that end, sets the review `state = 'failed'`, puts its comments back to `draft` (`review_id = NULL`), and bumps `review_rev`. The board shows "The review was not delivered: <reason>" on the review bar.

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
- **Chat:** an owner or member input whose text starts with the exact header of a review of this session that GET returned (`Review r-<id> from …`) shows as a "Review" card with one row per comment (`c-xxxxxx path:line` and the first body line) instead of the raw text; a row opens Changes at that file and line and highlights the thread. In any chat message of this session, a token `c-xxxxxx` or `[c-xxxxxx]` that equals the full short id of a comment visible to the caller **in this session** becomes a link chip (a React element; never inside a Markdown link or a code span; no prefix match; no lookup in another session). A stale answer for another session (after navigation) is dropped.
- **Composer `#` menu:** `#` and 1+ characters at a word start open a menu of the session's visible comments (short id, path:line, first body line; at most 20, newest first) from the loaded threads; a pick inserts `[c-xxxxxx] `.

## 6. Not in scope

- No agent tool to reply to a comment (a later MCP tool can use the same table). No GitHub sync. No comments on files outside a diff.

## 7. Old local notes

- The board keeps `fd-notes:<station>/<session>` in `localStorage` unchanged and shows, in Changes, "You have <n> local notes from an older version. Import them as draft comments?" only to a principal that can write (owner, or a member who may follow up). The import is an explicit button press.
- Import: one `POST …/review/comments` per unsent note with `import_key = sha256(station/session/file/line/text)` (new optional body field, stored in a deck table `review_imports (station_id, session_id, principal, import_key)` with a unique key, so a repeated import makes no duplicate). A note whose line is still in the current Since-spawn diff gets a `diff` anchor; else an `imported` anchor with `anchor_text = ''`, labelled "Imported note" in the UI and in the follow-up.
- The local key is removed only after every note got a success or an "already imported" answer. Sent notes are dropped. A viewer never sees the prompt.

## 8. Tests

- Deck: V20 valid and last; cleanup on station removal and on a snapshot without the session (and kept with it); per-principal matrix of every route (owner, member who can follow up, member who cannot, guest, token with and without read; CSRF); another author's draft → `404` on PATCH/DELETE/resolve and absent from GET; `not_in_diff` for a path or range not in the caller's diff; a member cannot comment on, nor read, a member-denied path, and the follow-up of an owner review hides its excerpt; anchors derived server-side (a client `anchor_text` field is refused as unknown); path rule (newline, `..`, absolute); send replay with the same key (same result, one command), another fingerprint (`409`), concurrent identical sends (one command); `thread_root_unsent`; atomicity (an injected failure after the command insert leaves no review and no flipped draft); dispatch after commit and recovery of a pending command; a rejected command flips the review to `failed` and the comments back to drafts; member header and `UserInput.author`; short id clash retry and immutability; `review_rev` on the card changes on each mutation; two boards: one writes, the other gets the delta.
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
