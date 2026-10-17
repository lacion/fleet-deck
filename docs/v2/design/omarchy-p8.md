# P8: provisioning (revision 11)

- **Status:** proposed (revision 11, 2026-10-18). Not implemented. Accepted by the architect as written (O11-1, O11-2) from the advisor answer on the P5/P8 DESIGN REVIEW round 1. The adversary DESIGN REVIEW round 3 (last) reviews this annex before the P8 code.
- **Parent:** [omarchy.md](./omarchy.md) (§9, §9A, §9B). This annex overrides §9 and §9A where they differ. DL-230 items 3 and 4 stay binding and unchanged.
- **Sibling:** [omarchy-p5a.md](./omarchy-p5a.md).
- **Code references:** each `file:line` points to the code at `c00eea8f`.
- **Implementation boundary:** use the existing `rustix`, Tokio, ring, serde/JSON, TOML, SQLite and local-socket facilities. No new third-party library is required. Put provisioning, recovery and tests in separate modules. Keep each changed source file below 800 lines.

P8 keeps its dependency on P4. `fleetdeck omarchy install --yes` is not a command at `c00eea8f`. §11 of `omarchy.md` assigns it to P4 and makes P8 depend on P4 (`crates/fleetdeck-cli/src/cli.rs:38`).

Package publication can wait. Trusted package delivery cannot stay undefined: section 5 gives the installed-package prerequisite. "The owner will release packages" is not a bootstrap contract.

## 1. B1: fingerprint format

Use the fingerprint that is already implemented: the SHA-256 of the raw Ed25519 public-key bytes, shown as 64 lowercase hex characters. Do not use a certificate fingerprint (`crates/fleetdeck-core/src/identity.rs:96`).

- For human display, use **16 groups of four lowercase hex characters, separated by one ASCII space**. This is the current board formatter (`web/src/tailnet/model.ts:29`).
- The default CLI output is exactly that line plus a newline.
- JSON, TOML, comparisons and protocol fields use the ungrouped 64-character hex.

First-login notification:

```text
Title: Fleet Deck pairing
Body: Approve this station fingerprint on the board: <full grouped fingerprint>
```

Change the board instruction to: "Compare all 16 groups with the fingerprint shown on the new machine. Approve only if every group matches."

Keep the existing full-fingerprint approval check (`crates/fleetdeck-link/src/pair_hub.rs:681`).

**Tests:** exact grouped and plain output, complete notification text, distinct machine keys, no abbreviation, and a refusal on a full-fingerprint mismatch.

## 2. B4: the two public commands

Use the existing owner-only local sockets and their `op` and `status` tagged JSON framing (`crates/fleetdeck-station/src/local.rs:47`, `crates/fleetdeck-deck/src/local.rs:57`).

Add the feature identifiers `station_provision.v1` and `deck_provision.v1` to the corresponding new local responses.

### 2.1 Command one: `station fingerprint`

```text
fleetdeck station fingerprint [--json] [--probe-providers]
```

Local request:

```json
{"op":"station.fingerprint","probe_providers":false}
```

Response:

```ts
{
  status: "fingerprint";
  features: ["station_provision.v1"];
  fingerprint: string;
  paired_deck: string | null;
  link_up: boolean;
  build: { version: string; commit: string; release: boolean };
  binary_sha256: string;
  providers: StationProvider[] | null;
}
```

- Read the identity of the running node. Never generate a key. Never fall back to loading another identity from disk.
- `providers` is `null` unless requested. `--probe-providers` requires `--json`.
- `link_up` means a current authenticated control connection to `paired_deck`, with an acknowledgement received for the initial snapshot of that connection. An empty snapshot counts.
- Add an explicit connection-local readiness state. The current coordinator acknowledgement watermark alone does not tell "no acknowledgement" from the acknowledgement of sequence zero. Clear the readiness on disconnect.

### 2.2 Command two: `deck provision-file`

```text
fleetdeck deck provision-file <name> --deck <host>[:<port>] --out <absolute-path>
```

**`--deck` is required.** Do not guess from board origins.

Local request:

```json
{"op":"deck.provision_info","host":"deck.example.ts.net","port":7421}
```

Response:

```json
{
  "status":"provision_info",
  "features":["deck_provision.v1"],
  "deck":"deck.example.ts.net:7421",
  "deck_fingerprint":"<64 lowercase hex>"
}
```

The running deck supplies its identity and checks that:

- the host is its own Tailscale FQDN or Tailscale IP;
- Tailscale reports that identity locally;
- a live link listener serves the supplied or default port on that address, or on the corresponding wildcard;
- the port is 1-65535, and an omitted port is 7421;
- IPv6 uses brackets when it is serialized with a port.

The deck already exposes its bound link listeners. Use them, not the configured intentions (`crates/fleetdeck-deck/src/link/runtime.rs:262`). Reuse the existing target and fingerprint parsers and the name validator. Names have 1-64 characters, without control or bidi characters (`crates/fleetdeck-cli/src/tailnet_join.rs:65`).

### 2.3 The file

Write exactly these TOML keys, with a final newline:

```toml
version = 1
deck = "deck.example.ts.net:7421"
deck_fingerprint = "<64 lowercase hex>"
name = "laptop"
```

Use the existing TOML implementation through `fleetdeck-core`. Do not add a second parser. Do not build quoted TOML by hand. Reject unknown keys, files above 16 KiB, invalid fields, and versions other than one.

Output-file rules:

- The parent directory exists, is owned by the caller, and has mode 0700.
- No symlink components. Ancestors must not be writable by other users.
- The temporary leaf is `.provision.<32-random-hex>.tmp`, created exclusively, mode 0600.
- Write, sync the file, publish with an atomic **no-replace** operation, then sync the parent.
- Never replace an existing destination, including a dangling symlink.
- Clean up temporary files on errors. A crash after publication may leave a temporary name. Remove only a verified matching temporary file, never the published destination.
- Success: exit 0, empty stdout, stderr "Provisioning file written."

### 2.4 Output and errors

For both commands, JSON output is one compact line and no additional stdout. Runtime errors exit 1 and print one stderr line, `<code>: <text>`. Clap syntax errors keep exit 2.

| Code | Exact text |
| --- | --- |
| `unavailable` | `The local station is not running.` / `The local deck is not running.` |
| `unsupported` | `The running process does not support provisioning v1.` |
| `timeout` | `The local process did not answer in time.` |
| `invalid_response` | `The local process returned an invalid provisioning response.` |
| `invalid_request` | `The provisioning arguments are not valid.` |
| `deck_not_listening` | `The deck does not listen on that tailnet address and port.` |
| `output_exists` | `The output path already exists.` |
| `unsafe_output` | `The output path is not private or contains a link.` |
| `write_failed` | `The provisioning file could not be written.` |

- Normal local calls have a five-second total connect, request and read limit. Provider-probe calls use the limits in section 5.
- A deck restart invalidates an in-progress local call. It does not invalidate a completed provisioning file.
- A key rotation makes an old file fail the fingerprint validation. Regenerate the file.

**Tests:** running identity versus disk identity; old daemon; address and port mismatch; IPv6; all output channels and exits; TOML escaping; an existing file and a dangling symlink; an unsafe parent; an interrupted publication; a restart; a key rotation.

## 3. B2: bootstrap retries and idempotence

The current join CLI stops on refusals. Its loop handles the fingerprint confirmation, not an indefinite retry. The station refuses any existing pairing, including the intended one. Service installation restarts the service (`crates/fleetdeck-cli/src/tailnet_join.rs:269`, `crates/fleetdeck-station/src/link/runtime.rs:415`, `crates/fleetdeck-cli/src/service/systemd.rs:254`).

Implement the durable orchestration inside the installed binary, behind a hidden entry point:

```text
fleetdeck provision-bootstrap --file <absolute-path> [--retry]
```

This is an internal hook entry point, in addition to the two public commands. It avoids `jq`, Python and a shell TOML parser.

### 3.1 State files

```text
<station data>/provision.lock        0600
<station data>/provision-state.json  0600
```

Use an exclusive nonblocking OS file lock. A second instance exits 0 without work.

State fields:

```ts
{
  version: 1;
  config_sha256: string;
  station_fingerprint: string | null;
  phase: "validated" | "service" | "paired" | "link" | "integration" | "complete";
  error: string | null;
}
```

Write the state through an exclusive 0600 temporary file, a file sync, an atomic replacement and a directory sync. Reject symlinks, wrong owner or mode, invalid JSON and unsupported versions. The state is a checkpoint. It is not proof that an external effect still exists.

### 3.2 The sequence

The sequence is fixed:

1. Validate the private provisioning file and the trusted release prerequisite.
2. Verify or create the station configuration. Never overwrite a conflicting existing configuration.
3. Inspect the installed station service. Install it with `fleetdeck service install station` only when it is absent. If the matching unit is inactive, use `fleetdeck service start station`. If its binary or data directory differs, stop with `service_conflict`.
4. Poll `station fingerprint --json` every 500 ms for at most 60 seconds. Each call gets at most two seconds.
5. Save and show the full station fingerprint. A later change is the terminal `station_identity_changed`.
6. Inspect the actual paired deck:
   - the same full fingerprint: skip the join;
   - a different fingerprint: terminal `paired_elsewhere`;
   - none: run the existing tailnet join with the target, the fingerprint and the name of the file.
7. Poll the authenticated `link_up` every second for 60 seconds. If it is still down, wait 30 seconds and repeat the readiness checks without a new join.
8. Run the service-side provider verification of section 5.
9. Run `fleetdeck omarchy install --yes`, with a 60-second process limit. Check the P4 installer status and require no drift.
10. Commit `complete`. Keep the non-secret provisioning file and the state.

A rerun validates the config hash and the live effects before it skips steps. It never removes a pairing, regenerates a key, or rewrites an unrelated service.

### 3.3 Retry table

| Result | Action |
| --- | --- |
| `window_closed`, `not_accepted`, `expired`, `busy`, transport timeout or failure, `tailscale_unavailable`, `not_in_tailnet` | Retry after 30 seconds |
| `denied`, `cancelled`, `revoked` | Retry after 600 seconds (P8-1 D29: the owner rule is remote first; a request needs an owner-opened pairing window and the owner's approval of the exact fingerprint, so a retry gives nothing to anyone else) |
| `fingerprint_mismatch`, `request_id_conflict`, `pending`, `unsupported`, `invalid_request`, `retry_file`, local persistence failure | Stop; repair required |
| `already_paired` | Read the local pairing again; skip only if its full fingerprint matches |
| Any unrecognized error | Stop |

Do not parse human stderr. The internal runner uses typed local responses and typed transport errors. Keep the current join limits: station owner wait 660 seconds, CLI wait 690 seconds (`crates/fleetdeck-link/src/pair_dial.rs:25`, `crates/fleetdeck-cli/src/tailnet_join.rs:37`).

### 3.4 Resume, output and exit codes

- Retryable failures stay in the running bootstrap loop.
- A process death resumes at the next graphical login, or when the owner reruns the installed first-login hook.
- Terminal errors are kept and do not clear automatically at login.
- `--retry` clears only the terminal bootstrap error, after it revalidates the identity and the configuration. It never cancels a different pending pairing. `--retry` is optional: no outcome needs it (P8-1 D29).
- Bootstrap stdout is empty. Progress and error stderr contain the phase and a fixed code only.
- Exit 0 means complete, or another instance holds the lock. Exit 1 is an unexpected failure. Exit 2 is a retained terminal condition.

### 3.5 Correction of the B3 claim of §9A

§9A says that there is no half-redeemed state. That claim is wrong. A **remotely committed, locally uncommitted state does exist**. The existing recovery handles it with `pair-pending.json` and the deck completion record.

- Keep `pair-pending.json` until the local pairing transaction succeeds.
- The deck already commits the enrollment and the completion atomically.
- The station commits locally before it removes the retry file (`crates/fleetdeck-deck/src/store/pairing.rs:54`, `crates/fleetdeck-station/src/link/runtime.rs:540`).

**Tests:** a closed window followed by an opening; network loss; denial; the same and a different existing deck; every state-write and effect boundary; approval before the local commit; a crash after the local commit; an integration failure after pairing; a wrong service; duplicate hooks; a changed config or key; empty-snapshot readiness; a reconnect that invalidates readiness.

## 4. B5: medium custody

"No Fleet Deck secret" closes the invitation custody. It does not close the LUKS and Tailscale custody.

The procedure is in `docs/v2/operations.md`, section 14 "Provisioning media custody (P8)". `omarchy.md` §9A links to it. **P8 acceptance requires it.** Its content (the text is the same in `operations.md`):

- Supported source media are an owner-controlled LUKS-encrypted POSIX removable filesystem, or a read-only `cidata` ISO whose host image is held on encrypted storage in a 0700 directory as a 0600 file.
- Do not require Unix mode preservation inside an ISO. FAT and exFAT media are not accepted for secret-bearing P8 media.
- The installer copies only `fleetdeck/provision.toml` into `<config home>/fleetdeck/provision.toml`, and creates the destination privately as 0600. It does not keep source modes. It does not run source-media scripts.
- Reject source symlinks, nonregular files and files above 16 KiB. Publish the destination atomically. Do not replace a different existing file.
- After the installation, detach or eject the medium and remove the virtual CD attachment.
- Keep a source only in the protected encrypted location, with access limited to the owner. Disposal includes ISO copies, templates, snapshots and backups. File unlinking or a flash overwrite alone is not claimed as secure erasure.
- Each machine gets a distinct Tailscale enrollment key and a distinct disk-encryption credential. Do not clone an initialized Fleet Deck identity, Tailscale state or bootstrap state.

Exposure recovery is specific to the credential:

1. Revoke the exposed Tailscale auth key.
2. Inspect and remove unauthorized Tailscale nodes. Revoking the auth key does **not** deauthorize nodes that are already enrolled with it. See the [Tailscale auth-key documentation](https://tailscale.com/docs/features/access-control/auth-keys).
3. Replace the exposed LUKS passphrase or keyslot credential. If an attacker also had a disk or header copy, a credential replacement alone does not protect that copy. Require a disk-key replacement or a reinstall.
4. If a wrong Fleet Deck fingerprint was approved, remove that station from the deck. Removing the medium or revoking a Tailscale auth key does not revoke the Fleet Deck enrollment.

**Tests and acceptance:** a private destination from an ISO without useful Unix modes; an unsupported filesystem refusal; a source link refusal; the retained-source checklist; a detached VM ISO; a clone test with distinct station identities.

## 5. B6: package and hook trust

The code already has the Homebrew release configuration: tap `lacion/homebrew-tap`, formula `fleetdeck`. Keep it. AUR packaging stays release work (`dist-workspace.toml:15`).

Freeze this separation:

- The **trusted installer or image preparation step** installs the package.
- The **first-login hook never installs packages and never downloads code**.
- The AUR source is a reviewed, pinned `fleetdeck-bin` recipe. The release task builds its package and supplies its exact SHA-256.
- The trusted installer image carries `/etc/fleetdeck-provision-release.json`, root-owned, 0444, in a root-owned directory that other users cannot write. It records the exact release version, commit, package SHA-256, binary SHA-256 and hook hashes. It must not come from `cidata`.
- The installer verifies `/var/cache/fleetdeck-release/fleetdeck-bin.pkg.tar.zst` against that manifest, then runs:

```text
/usr/bin/pacman -U --noconfirm /var/cache/fleetdeck-release/fleetdeck-bin.pkg.tar.zst
```

All dependencies must already be present. A verification failure stops the installation. No AUR helper and no floating "latest" command runs at first login.

### 5.1 Installed files

The package installs:

```text
/usr/bin/fleetdeck                                  root:root 0755
/usr/lib/fleetdeck/provision-first-login             root:root 0755
/etc/xdg/autostart/fleetdeck-provision.desktop        root:root 0644
```

The desktop entry invokes the absolute hook path. The hook runs as the logged-in user. It requires an absolute XDG config path when one is set. It invokes the hidden bootstrap with the private config path. No sudo, package command, credential or medium-supplied executable appears in it.

### 5.2 Build identity

Bootstrap verifies the exact manifest build identity and the binary hash. The running station response must match the same build and hash. `fleetdeck --version` alone is not enough. The existing `build-info` already exposes the version, the commit and the release status. The existing service installation copies the binary into a service release directory. Reuse those facilities (`crates/fleetdeck-cli/src/main.rs:64`, `crates/fleetdeck-cli/src/service/program.rs:207`).

### 5.3 Provider probe

Remove the claim that the current `doctor` tests the installed service environment. It uses its own `ChildEnv::from_process()` (`crates/fleetdeck-cli/src/doctor.rs:274`).

Use `station fingerprint --json --probe-providers` instead:

- The running station does fresh adapter probes with its actual adapter environments.
- Reuse the provider-probe implementation, but bypass its process-lifetime result cache for this request.
- At most four probes run at the same time. Each gets ten seconds. The whole probe request gets 30 seconds. Unfinished instances report unknown.
- The total CLI deadline is 35 seconds.
- Require a parsed version and the status `ok` or `older_than_tested` for each configured provider.
- `broken` or `unknown` stops bootstrap with `provider_probe_failed`.
- `signed_out` is a sign-in warning. It is not evidence of a PATH failure. Provisioning does not copy provider credentials.
- Zero configured providers succeeds with "No providers are configured."

The existing station probes already call the adapters inside the station and report the version and auth state (`crates/fleetdeck-station/src/manager/manager_providers.rs:15`). Keep the service PATH filtering and canonicalization (`crates/fleetdeck-cli/src/service/path.rs:20`).

**Tests:** package and hash mismatch; an untrusted hook source; wrong ownership or mode; an exact build mismatch with identical version text; a service copy mismatch; a provider that is visible only in the interactive PATH; a filtered service PATH; a probe timeout; a broken version; a signed-out provider; zero providers; P4 installer drift.

### 5.4 Release prerequisite

B6 can pass design review with this explicit release interface. **Production P8 acceptance stays blocked until the release task of the owner supplies the pinned package, the trusted manifest and the installed hooks.** The current source cannot supply those future release identities.
