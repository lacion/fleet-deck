# Review comments on the Changes diff (DL-179)

- **Status:** Proposed (revision 1, 2026-10-14). Has a deck migration, so it needs an adversary DESIGN REVIEW PASS before code.
- **Owner request (2026-10-14):** "it would be amazing if we could add comments to the diff in changes, similar to a PR github and be able to reference that in the chat".
- **Replaces:** the R1.3 line notes (`web/src/chat/notes.ts`: one-line notes in `localStorage`, sent once as a "Review notes:" follow-up). The notes UI and storage go away; old notes in `localStorage` are read once and offered as drafts (§6).

## 1. Model

A **comment** belongs to one session and sits on one line, or a range of lines, of one file of a diff of that session. Comments form **threads** (a top comment and its replies). A **review** is the set of draft top comments and replies that the owner sends to the agent in one follow-up.

Deck migration V20 (last in the list):

```sql
CREATE TABLE review_comments (
    comment_id   TEXT    PRIMARY KEY,           -- ULID
    station_id   TEXT    NOT NULL,
    session_id   TEXT    NOT NULL,
    thread_id    TEXT    NOT NULL,              -- = comment_id for a top comment
    path         TEXT    NOT NULL,              -- repo-relative, as in the diff
    side         TEXT    NOT NULL CHECK (side IN ('new', 'old')),
    line_start   INTEGER NOT NULL CHECK (line_start >= 1),
    line_end     INTEGER NOT NULL CHECK (line_end >= line_start AND line_end - line_start < 200),
    anchor_text  TEXT    NOT NULL,              -- the commented lines when made (≤ 4 KiB)
    diff_kind    TEXT    NOT NULL CHECK (diff_kind IN ('spawn', 'turn')),
    turn_id      TEXT,                          -- set for diff_kind = 'turn'
    body         TEXT    NOT NULL,              -- Markdown, 1..8000 chars
    author       TEXT    NOT NULL,              -- 'owner' or 'member:<name>'
    state        TEXT    NOT NULL CHECK (state IN ('draft', 'sent')),
    resolved     INTEGER NOT NULL DEFAULT 0 CHECK (resolved IN (0, 1)),
    review_id    TEXT,                          -- set when sent
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
) STRICT;
CREATE INDEX review_comments_session ON review_comments (station_id, session_id, thread_id);
CREATE TABLE reviews (
    review_id    TEXT    PRIMARY KEY,           -- ULID
    station_id   TEXT    NOT NULL,
    session_id   TEXT    NOT NULL,
    command_id   TEXT    NOT NULL,              -- the follow-up command
    comment_count INTEGER NOT NULL,
    sent_by      TEXT    NOT NULL,
    sent_at      INTEGER NOT NULL
) STRICT;
```

- Limits: 500 comments per session (more → `409 too_many_comments`), 50 drafts per review.
- Removing a station deletes its rows (in the station removal transaction, like the outbox). Archiving a session keeps them; purging the deck data of a session (if any purge exists) deletes them.
- The rows hold only owner or member text and copies of diff lines that the board already showed. No credential, no private path: the deck refuses a `path` that is absolute, has a `..` component, a NUL, or more than 1024 bytes. A private file never appears in a session diff (DL-173), so a comment made from the board never points at one; a hand-made request can name any relative path, but it only stores the owner's own text under that name.

## 2. Routes (deck, all under `/api/sessions/{station}/{session}/review`)

| Method and path | Scope | Rule |
| --- | --- | --- |
| `GET …/review` | read (owner, member with access to the session, share guest with read) | All threads of the session with their comments, newest thread last; the open review's drafts of the caller only (other authors' drafts are hidden). |
| `POST …/review/comments` `{ path, side, line_start, line_end, anchor_text, diff_kind, turn_id?, body, thread_id? }` | drive (owner or member who can drive), CSRF | Makes a draft (top comment, or a reply when `thread_id`). |
| `PATCH …/review/comments/{id}` `{ body?, resolved? }` | drive, CSRF | `body`: only the author, only a draft. `resolved`: any driver, top comments only. |
| `DELETE …/review/comments/{id}` | drive, CSRF | Only the author, only a draft. |
| `POST …/review/send` `{ comment_ids }` | drive, CSRF | Sends the caller's listed drafts as one follow-up (§3). |

- Every mutation writes an audit row (`review.comment`, `review.send`, ids only) and calls `board_changed()`; the board refetches `GET …/review` for the open session on that signal (no new socket delta type).
- Errors: `404 unknown_session`, `404 unknown_comment`, `403 not_author`, `409 not_draft`, `400 invalid_body`, `409 too_many_comments`, `409 session_not_live` (send only, the station link is down or the session cannot take input; a hibernated session is resumed by the station).

## 3. Send a review

1. In one deck transaction: check the drafts (caller's, this session, still drafts), build the follow-up text (below), insert the follow-up command through the normal command path (`session.follow_up`, origin board), insert the `reviews` row, set the drafts `state = 'sent'`, `review_id`.
2. The follow-up text, each comment ≤ 8000 chars and the whole text ≤ 64 KiB (more → `400 review_too_large`):

```
Review from the owner (review r-<review short id>, <n> comments). Reply to each comment by its id.

[c-<short id>] <path> line <a>[-<b>] (<new|old> side)
> <anchor line 1>
> <anchor line 2 …, at most 6 lines>
<body>

[c-<short id>] …
```
   A reply in an earlier thread is sent as `[c-<id>] reply to [c-<thread short id>] …` with its parent's path and lines.
3. The short id is the last 6 characters of the ULID, lower case; the deck makes them unique within the session (a clash extends to 8).

## 4. Board

- **Changes tab** (Since spawn and per-turn diffs, `web/src/chat/Diff.tsx`): a click on a line number opens a comment editor under the line (multi-line textarea, Markdown, "Add comment" / "Cancel"); shift-click a second line number to make a range first. Deleted lines (`-`) can be commented on the old side. Threads show under their last line as cards: author, time, body (Markdown rendered with the existing safe renderer), the short id `c-xxxxxx`, "Reply", "Resolve"/"Unresolve", and for drafts "Edit"/"Delete" and a "Draft" badge.
- **Outdated comments:** when the current diff no longer has the anchor (the lines at `line_start..line_end` of that side do not equal `anchor_text`), the thread shows in a "Outdated" list at the top of its file (and in a "Comments not in this diff" list when the file left the diff). It is never lost.
- **Review bar** (sticky at the bottom of Changes when the caller has drafts): "Review: <n> drafts — Send review / Discard all". Send uses §3.
- **Chat:** a sent review shows in the chat as a "Review" card (from the follow-up's input id, matched to the `reviews` row) with one row per comment (`c-xxxxxx path:line` + the first line of the body); a row opens Changes at that file and line and highlights the thread. Plain `c-xxxxxx` and `[c-xxxxxx]` text in any chat message (agent or owner) of that session that matches a comment of the session becomes a link chip to that thread.
- **Composer `#` menu:** typing `#c` or `#` and 1+ characters opens a menu of the session's comments (short id, path:line, first body line; max 20, newest first) built from the loaded threads; a pick inserts `[c-xxxxxx] `. The agent sees the id and can match it to the review text.
- **Phone:** the editor and the threads take the full width; a long-press on a line number is not needed (a tap opens the editor).

## 5. Not in scope

- No agent-side tool to reply to a comment (a later MCP tool can use the same table).
- No GitHub sync of comments (the forge PR review writes stay separate, DL-164).
- No comments on files that are not in a diff.

## 6. Migration of the old notes

On first load of Changes for a session, the board reads `fd-notes:<station>/<session>` from `localStorage`. Unsent notes become draft comments through `POST …/review/comments` (one each, `anchor_text` = the current line text, or empty when the line is not in the diff, then the comment shows as outdated); then the key is removed. Sent notes are dropped.

## 7. Tests

- Deck: migration valid and last; each route's scope, CSRF, author rules, member and guest rules, audit rows; send builds the exact text, inserts one command, flips the drafts in one transaction; a crash between steps leaves either no change or all of it; limits; path rules; station removal deletes rows.
- Board: editor, range, old side, threads, resolve, outdated detection, review bar, chat Review card and link chips, `#` menu, notes import, phone layout.
