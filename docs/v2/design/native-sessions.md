# Browse the native sessions of a station, with a preview, before an import (B7)

- **Status:** Accepted, revision 6 (2026-10-16). Owner decision after round 5 (1 blocker: idmapped and loop-backed mounts; 1 major: the fallback contract): "Apply fixes, build"; the fixes are in §1b and §2.4 (§9). The PRE-PR review checks the code. Revision 1: FAIL (3 blockers, 7 majors). Revision 2: FAIL (2 blockers partly fixed, 1 new; 7 majors fixed; §6).
- **Owner request:** "for every station we should be able to see a list of all the sessions available in that station, have a preview of the conversation and decide if we want to continue and import that session".

## 1. Facts (code map, 2026-10-16)

- Import today: the owner types a native id, picks the instance and the folder by hand; `SessionCommand::Import { spec, native_session_id, prompt }`; admission `policy.workspace`, `import_exclusion` (`native_session_in_use`, `unended_native(…, Purpose::Import)`), `native_session_held`; at dispatch `history::at_import` captures the history (a failure does not fail the import) and the adapters resolve the cwd again and resume by id (Claude `--resume <id>` in `projects/<encode_cwd(cwd)>/`, Codex `thread/resume`).
- `history/locate.rs`: `resolve_home`, `encode_cwd` (lossy), `open_root`/`descend`/`O_NOFOLLOW`, `check_meta` (regular file, station uid, `nlink == 1`, not private, size cap), a head identity scan (up to 1000 records, first match wins), Codex lookup with one 5000-entry budget, the shared one-permit history runner (`History::run`, the permit is held until the worker exits).
- No listing of native sessions exists.

## 1a. Threat model

- **Defended:** callers that are not the owner login (members, bearer tokens, coordinators); transcript content that is forged or corrupted (a wrong identity, a `cwd` that does not match its project folder, contradictory records); resource exhaustion (time, files, bytes, answer size); and an accidental change of the transcript between the preview and the import.
- **Not defended (same as every start and import today):** a process of the station user that replaces a transcript file or a folder between the station's check and the provider's own open of it. The provider opens its own store by id and its folder by path; a process with the station user's rights can already read and write that store and every folder directly. The verified launch directory for all starts is a separate hardening item (backlog B9).

## 1b. Verified import: supported filesystems and the trusted path

- **Scope of the guarantee.** Only an import started from the list ("Continue it", with a pin) is a **verified import**. The typed import and every other start keep today's behavior (backlog B9).
- **Supported filesystems (allow-list).** A verified import needs every component of (a) the provider home, (b) the transcript's directories and file, and (c) the import `cwd` to be on a local Linux filesystem whose `statfs` type is one of ext2/3/4, xfs, btrfs, tmpfs, zfs or f2fs. Any other type (NFS, CIFS/SMB, FUSE, overlay, 9p, WSL DrvFs, and every macOS filesystem) is not verifiable: Fleet Deck makes no claim there. **Mounts:** for each component, the mount that holds it (from `/proc/self/mountinfo`, matched by the component's device and the longest mount point) must not have the `idmapped` option and its source must not be a loop device (`/dev/loop*`); such a mount is not verifiable either.
- **Trusted path (on a supported filesystem).** Checked component by component from `/` on the exact stored path, **without resolving links first** (each component with `lstat`; a link component fails): every component is owned by the station user or by root and not writable by group or others; the one exception is a root-owned directory with the sticky bit whose next component is owned by the station user (that child is then checked like any other). Linux POSIX ACLs do not need a separate check: an effective named-user write shows in the group write bits, which fail the rule.
- **Where it is checked:** the list (every walked home, directory and file), the preview, and for a verified import at admission and again at dispatch right before the launch, on the canonical path that admission stored.
- **States.** On a supported filesystem a failed check gives `path_not_trusted` (for the `cwd`) or `not_trusted` (for a transcript) and the entry cannot be continued. On an unsupported filesystem the list and the preview still work for the owner, every entry carries `verified: false`, the board shows "Fleet Deck cannot verify this folder", and "Continue it" sends today's typed import (no pin, no verification claim).
- Together with §1a: on a supported local filesystem, no user other than the station user and root can change the checked folder or transcript between the check and the provider's own open; the remaining window belongs to the station user (§1a; B9).

## 2. Design

### 2.1 Homes

- One home per (provider kind, canonical home) from the station's configured instances, with each instance's full launch environment (`env_for`); a relative or missing effective home (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `HOME`) is `unavailable { reason: "home_not_absolute" }`, like history capture.

### 2.2 `station.native_sessions` (feature `native_sessions.v1`, deck → station, read)

```text
params { provider_instance?: ProviderInstanceId, limit?: 1..=200 (default 100) }
-> NativeSessionList { homes: [{ provider, instances, state: ok | unavailable { reason } }],
                       sessions: [NativeSession], truncated: bool, timed_out: bool }
NativeSession { provider, native_id, instances, pin: TranscriptPin, cwd?: String, title?: String,
                first_prompt?: String, modified_at: i64, size: u64, parent_native_id?: String,
                known?: { session_id, state },
                import: ok | outside_roots | in_use { session_id } | held { session_id } | execution_unknown
                        | cwd_mismatch | too_large | subagent_thread | path_not_trusted | not_trusted,
                verified: bool }
TranscriptPin { home: String /* canonical */, dev: u64, ino: u64 }
```

- **Admission and budget:** the list runs under the shared history permit (one history job at a time; a second request waits up to 2 s, then `busy`), with one absolute 10 s deadline checked in the walk and in every read, and a total budget: 2,000 directories, 20,000 directory entries, 64 MiB of bytes read. A worker stops at the deadline or the budget (a stop flag checked between entries and between reads) and returns the bounded result with `timed_out` or `truncated`.
- **Walk at fixed depth only**, directories opened with `open_root`/`descend` (no link followed), metadata by `fstatat` without following links: Claude `projects/*/*.jsonl` whose stem passes `check_native_session_id`; Codex `sessions/*/*/*/rollout-*.jsonl` and `archived_sessions/rollout-*.jsonl` whose id suffix passes it. Never anything else. The newest `limit` candidates by modification time are read; the rest count in `truncated`.
- **Per candidate:** open with `O_NOFOLLOW` relative to its directory descriptor, `check_meta`, then a head window (256 KiB) and a tail window (64 KiB) read through the same descriptor:
  - **Identity, every record:** Claude: every record in both windows that has a `sessionId` must name the stem, and every record that has a `cwd` must name the same `cwd`; the project directory name must equal `encode_cwd(cwd)`. Codex: the first record is `session_meta` with `payload.id` equal to the id suffix and a `cwd`; every later `session_meta` or `turn_context` record in either window must name the same id (when it names one) and the same `cwd`. A Codex id found both in `sessions/` and `archived_sessions/` makes both `not_trusted`. Any contradiction, a missing `cwd`, or a field that fails its shape → `import: not_trusted` and no text fields.
  - **Fields, all bounded:** `cwd` absolute, ≤ 4096 bytes, no NUL; `title` (Claude `ai-title`) ≤ 120 characters; `first_prompt` (the first user text, wrappers dropped) ≤ 200; `parent_native_id` (Codex `parent_thread_id`) must pass `check_native_session_id`.
- **`known`:** one query on the `sessions` table per answer.
- **`import` precedence** (the first that applies): `not_trusted` (the transcript), `subagent_thread`, `too_large`, `outside_roots`, `path_not_trusted`, `cwd_mismatch`, `in_use`, `held`, `execution_unknown`, else `ok`. **`verified`** is true only when the home, the transcript and the `cwd` are on supported filesystems and pass the trusted path.
- **`import`:** `outside_roots` (`policy.workspace(cwd)` fails); `cwd_mismatch` (the canonical form of `cwd` differs from `cwd`); `in_use`, `held`, `execution_unknown` from the same predicate as import admission (`import_exclusion` + `native_session_held`); `too_large` (over the history file cap); `subagent_thread` (Codex `parent_native_id` present); `not_trusted`.
- **Answer bound:** the serialized answer is at most 512 KiB; entries beyond it are dropped and `truncated` is set.

### 2.3 `station.native_preview`

- `params { provider_instance, native_id, pin: TranscriptPin }`. The station resolves the instance's home, requires it to equal `pin.home`, walks the fixed-depth set for this id only (Claude: the project directories whose `.jsonl` stem is the id; Codex: the date directories and `archived_sessions/` for this id suffix) under the same budgets, opens the file whose `(dev, ino)` equals the pin, and runs the per-record identity rules of §2.2 on the head window and on a tail window of at most 4 MiB (from the next newline), discarding all collected text on a contradiction (`unavailable { reason: "not_trusted" }`). Items come from the DL-189 `parse_line` (no tool output, no thinking, no sidechains), newest ≤ 300, ≤ 512 KiB. Under the history permit and the deadline. Nothing is stored.

### 2.4 Import from the list: two branches

- `SessionCommand::Import` gains `transcript: Option<TranscriptPin>` (`#[serde(default, skip_serializing_if)]`; an older station refuses through the feature check at the deck). The board sends a pin only for an entry with `verified: true`.
- **Verified branch (a pin):** before any launch the station locates the transcript for `(instance, native id, spec.cwd)` as the provider will, requires its `(dev, ino)` and canonical home to equal the pin, runs the trusted-path and supported-filesystem checks of §1b on the home, the transcript and the `cwd`, runs the per-record identity rules of §2.2 on the head window, and requires the DL-189 capture to succeed reading through the pinned descriptor; any failure fails the start ("The transcript changed or could not be verified. Browse again."). No relaunch.
- **Unverified branch (no pin):** exactly today's import (best-effort capture, a capture failure does not fail the import), the typed import included.
- **For every import (both branches):** a Codex transcript whose `session_meta` names a `parent_thread_id` fails the start ("This is a subagent thread; continue its parent."), an intentional rule of its own; a missing transcript or missing metadata does not trigger it.
- **Residual limit (§1a):** the provider opens its own transcript by id after the station's check; a same-user replacement in that window is not detected.

### 2.5 Deck routes (owner login only)

- `GET /api/stations/{station}/native-sessions?instance=` and `POST /api/stations/{station}/native-sessions/preview` (body `{ instance, native_id, pin }`).
- Owner login only (`guard::owner_only`): a member or a bearer token gets 403, also with the drive scope. Without the station feature: 409 `native_sessions_unsupported`, no forward. Parameters checked at the deck (`check_native_session_id`, instance id shape, pin shape). Fixed error texts; failures audited; answers not cached; the answer is passed through unchanged (no logging of titles, prompts or items).

### 2.6 Board

- New session → "Browse sessions on this station": a dialog with the list per instance (title or first prompt, folder, modified time, size, "Already in Fleet Deck" with a link, the import state). Choosing one opens the preview; "Continue it" fills the instance, the folder and the id and sends `session.import` with the pin. Disabled with the reason for every state other than `ok`. Codex child threads are shown under their parent and cannot be imported.

## 3. Tests

- Homes: a relative home is unavailable; instances sharing a home are listed once.
- Walk: links, hard links, another owner, private files, `subagents/` files and deeper files are never read; budgets (directories, entries, bytes, deadline) stop the walk with `truncated`/`timed_out`; a second request waits then gets `busy`.
- Identity: a Claude file whose project directory differs from `encode_cwd(cwd)`, a record with another `sessionId`, two different `cwd`s, a Codex id in both trees → `not_trusted` without text; the fixtures (Claude 2.1.300, Codex 0.162.1) give the expected fields.
- Import states, including `execution_unknown` and `subagent_thread`; the answer cap.
- Preview: the pinned file only; a swapped file (other inode) → unavailable; a contradiction in the tail → unavailable with no items.
- Import with a pin: the same file → resumes; another inode, a contradiction, a failed capture, a child thread → the start fails with the text, no launch. Without a pin: a child thread fails.
- Deck: owner 200; member and token 403; 409 without the feature; parameter checks; audit rows; nothing cached.
- Trusted path: a home, a project directory, a transcript or a `cwd` with a component owned by another user, writable by group or others, or a link → unavailable / `not_trusted` / `path_not_trusted`, and an import with such a `cwd` is refused at admission and at dispatch; a root-owned sticky parent with a station-user child is trusted.
- Codex: a later `session_meta` or `turn_context` with another id or `cwd` → `not_trusted` (list) and `unavailable` (preview) with no items.

## 4. Out of scope

- Tool output, subagent logs or attachments in the preview. Members browsing. Deleting native sessions.

## 5. Review map (revision 1 → revision 2)

| Finding | Answer |
| --- | --- |
| BLOCKER the import can launch outside the roots after a cwd replacement | §2.5 a verified launch directory for every start, import and resume |
| BLOCKER `cwd_mismatch` does not bind the Claude cwd to the project directory | §2.2 the project directory name must equal `encode_cwd(cwd)`; every record's `cwd` must agree |
| BLOCKER "Continue it" is not bound to the selected transcript | §2.4 `TranscriptPin` (home, dev, ino) checked at dispatch; a failed verification or capture fails the start; the residual provider-open window is documented |
| MAJOR the head identity check does not protect preview records | §2.2, §2.3 per-record identity on every window; text discarded on a contradiction |
| MAJOR `locate` breaks the preview bounds | §2.3 a pinned open with explicit head, walk and tail budgets; duplicates across trees refused |
| MAJOR the list has no shared admission or budget | §2.2 the history permit, one deadline, total budgets, a stop flag |
| MAJOR the import states omit an exclusion | §2.2 the full import predicate (`execution_unknown`) |
| MAJOR the child-thread rule exists only in the board | §2.4 enforced at the station for every import |
| MAJOR relative homes | §2.1 refused as unavailable; full launch environment; dedupe by kind and canonical home |
| MAJOR no serialized size bound | §2.2 every field bounded; the answer capped at 512 KiB |

## 6. Review map (revision 2 → revision 3)

| Finding | Answer |
| --- | --- |
| BLOCKER (r1 #1, partly fixed) cwd replacement before the launch | §1a: a same-user race is outside this design's threat model, as for every start and import today; the verified launch directory for all starts moves to backlog B9 (§2.5 of revision 2 removed) |
| BLOCKER (r1 #3, partly fixed) the provider lookup after the pin check | §1a and §2.4: the pin guards against accidental change; the capture reads the pinned descriptor; the provider's own lookup is the documented same-user residual |
| BLOCKER (new) `/proc/self/fd` cannot serve as the provider workspace | removed with the revision 2 §2.5 (B9 will design descriptor custody across the direct and shim hosts) |
| MAJOR (r1 #4, partly fixed) Codex later metadata | §2.2 every later `session_meta`/`turn_context` must agree |

## 7. Review map (revision 3 → revision 4)

| Finding | Answer |
| --- | --- |
| BLOCKER a replacement by another user when a parent is writable by others | §1b the trusted-path rule for homes, transcripts and every import `cwd`, at admission and at dispatch |

## 8. Review map (revision 4 → revision 5)

| Finding | Answer |
| --- | --- |
| BLOCKER macOS ACLs | §1b macOS filesystems are not verifiable: no pin, no claim; list and preview only, Continue = today's typed import |
| BLOCKER shown metadata does not prove exclusive access on network or mapped mounts | §1b an allow-list of local Linux filesystem types by `statfs`; everything else is not verifiable |
| MINOR `path_not_trusted` missing from the schema | §2.2 the state, `verified`, and the precedence |
| (notes) check order and scope | §1b checks on the exact stored path without resolving links first; the guarantee covers only verified imports |

## 9. Review map (revision 5 → revision 6, owner decision "Apply fixes, build")

| Finding | Answer |
| --- | --- |
| BLOCKER idmapped mounts and loop-device images | §1b mounts with the `idmapped` option or a loop-device source are not verifiable |
| MAJOR the unverified fallback conflicts with the contracts | §2.4 two branches: a pin only for verified entries; the unpinned branch is exactly today's import; the child-thread rule applies to every import |
| MINOR `path_not_trusted` and `verified` missing from the wire union | §2.2 (both present) |
