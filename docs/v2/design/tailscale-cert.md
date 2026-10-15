# A trusted board certificate from Tailscale (DL-190)

- **Status:** Design, revision 2 (2026-10-15). Revision 1 got DESIGN REVIEW FAIL (1 blocker, 8 major, 1 minor, 1 unverified; §5); §6 maps them. Needs a PASS before code.
- **Owner report (2026-10-15):** a device link opened on the phone (on the tailnet) gives a certificate/domain error; the IP-address link works after accepting the self-signed warning. Owner: "lets do the propper fix".
- **Goal:** the board answers on its tailnet name (`<host>.<tailnet>.ts.net`) with a certificate that browsers trust, so a phone or another computer opens it with no warning. Nothing changes for the loopback and IP-address entries.

## 1. Facts today

- The board serves one certificate (`crates/fleetdeck-deck/src/http/tls.rs`): the owner's `board.crt`/`board.key` in the deck data directory when both exist, else a kept self-signed certificate for all board names (`board-selfsigned.*`; on the owner's machine: `100.89.30.8`, `127.0.0.1`, the IPv6 tailnet address, `lighting.taildf78ad.ts.net`, `localhost`).
- Observed on the owner's phone (2026-10-15): the device link with the `ts.net` name failed with a certificate error that the browser did not let the owner pass; the IP-address link worked after the self-signed warning was accepted. (The cause, for example HSTS for the domain, is not established here; a trusted certificate removes the error in every case.)
- The device link (`POST /api/auth/device_link`, DL-146) lists one login URL per board origin and the certificate SHA-256 and `self_signed`; the board chooses the first entry that is not "This computer".
- The certificate fingerprint is shown to the owner (`fleetdeck open`, the device link dialog); no program pins it.
- `tailscale cert [--cert-file -] [--key-file -] [--min-validity <d>] <domain>` gets a Let's Encrypt certificate for this machine's tailnet name (Tailscale 1.102.4 on the owner's machine; `-` writes PEM to stdout, so it works through Tailscale for Windows from WSL). It needs "HTTPS Certificates" enabled once in the tailnet's admin console. The name is then published in public Certificate Transparency logs.
- `TailscaleCli` (`crates/fleetdeck-core/src/tailscale.rs`) runs the chosen program with a minimal environment, no stdin, a deadline, capped stdout (2 MiB) and stderr (64 KiB).

## 2. Design

### 2.1 Setting

- Deck setting `tailscale_cert` (bool, default `false`, `apply: Live`; turning it on is a danger change and needs the owner's elevation). Label "Trusted certificate from Tailscale". Help: "The board gets a certificate for its tailnet name that browsers trust. Tailscale must allow HTTPS certificates (admin console → DNS → HTTPS Certificates). The name `<host>.<tailnet>.ts.net` becomes public in Certificate Transparency logs."
- Opt-in, because the publication in public logs cannot be undone.

### 2.2 The one identity (the certificate name)

- The certificate name is the deck's **`tailnet_hostname`** (the value that already makes the board Origin `https://<name>:<port>`, `serve.rs:187`, `guard.rs` `Origins`), lower case. Issuance runs only when `tailnet_hostname` is set, ends in `.ts.net`, and equals the DNS name of this machine in `tailscale status` (case-insensitive, trailing dot removed). Else the state is `name_mismatch` with the text "The tailnet name of this machine is `<status name>`, the board uses `<tailnet_hostname>`. Use tailnet again (Settings → Deck → Tailnet), then restart the deck." (With "Use Tailscale automatically" on, the deck keeps `tailnet_hostname` current, tailnet §14.4.)
- The same name drives the SNI choice (§2.5), the device-link entries and `fleetdeck open` (§2.6). A certificate SAN or a client SNI never adds an Origin.

### 2.3 Running `tailscale cert` with secret output

- New method `TailscaleCli::cert(domain, deadline)`: runs `cert --cert-file - --key-file - --min-validity 720h <domain>` with the §15.1 program choice and minimal environment, **in a secret mode**: no stdout or stderr byte is ever logged, stored or returned. Stdout is capped at 64 KiB, stderr at 16 KiB; the run ends at 90 s.
- A failed run maps stderr to a fixed reason code by fixed substrings, and only the code is logged and shown: `https_disabled` ("HTTPS certificates are not enabled for this tailnet: admin console → DNS → HTTPS Certificates"), `not_logged_in`, `timeout`, `not_installed`, `failed` (anything else). The raw text is dropped.
- The existing `run` keeps logging stderr for `status` and `whois`; `cert` uses a separate path that never calls that log.

### 2.4 Checking a certificate (admission and use)

- **Parse:** stdout must hold one or more `CERTIFICATE` blocks and exactly one private-key block; anything else is `failed` (code `bad_output`).
- **Path validation** (offline, `rustls-webpki`, already a workspace dependency): the leaf must chain, through the intermediates of stdout only, to one of two embedded public trust anchors: ISRG Root X1 and ISRG Root X2 (Let's Encrypt, which issues every Tailscale certificate). A root in stdout is never trusted. Checks: signatures, validity of every certificate, basic constraints, the server-auth usage, and the subject name equal to the certificate name (§2.2). The key must match the leaf.
- **Admission** (a new certificate from `tailscale cert`): path validation passes now and the leaf is valid for at least 7 more days. Else it is refused (`bad_certificate`) and the previous certificate stays.
- **Use** (a stored certificate at start, or the one in memory): path validation passes **now** (not expired, not yet valid fails). A usable certificate with fewer than 30 days left triggers a renewal at once. At the end of its validity the resolver stops serving it (§2.5); the state becomes `expired`.

### 2.5 Storage, lifecycle and serving

- **One file:** the chain and the key are stored together in `board-tailscale.pem` (mode 0600) in the deck data directory: written to a temp file, synced, renamed, and the directory synced (one atomic replacement of the pair). The previous file stays until the rename. No separate key file.
- **One owner:** a single lifecycle task does every issuance, renewal, storage and publication. It holds a **generation** = (setting on/off, certificate name, program choice), incremented by the setting change, a `tailnet_hostname` change and shutdown. Each run captures the generation at its start and checks it again, under one lock, **before** the file rename and **before** the resolver swap; a stale run discards its result. Turning the setting off increments the generation first, then removes the certificate from the resolver and deletes the file.
- **Schedule:** at start (a stored file that passes **use** checks is served at once, before any `tailscale` call; the start never waits for `tailscale`, as tailnet §15.3), then every 12 h; after a failure every 1 h; with fewer than 14 days left every 1 h.
- **Serving (SNI resolver):** the board TLS configuration uses `rustls::server::ResolvesServerCert`:
  - the owner's `board.crt` pair, when present, for every name (unchanged precedence);
  - else, when the client SNI equals the certificate name (ASCII case-insensitive) and a certificate passes **use** now: the Tailscale certificate;
  - else the self-signed certificate (no SNI, IP connections, every other name).
  The resolver reads a snapshot (an `Arc`) that the lifecycle task swaps; open connections keep theirs.

### 2.6 Device link, CLI and board

- One **live endpoint provider** in the deck answers, for each board origin, from the current resolver snapshot: the URL, the SHA-256 of the certificate that this origin gets, and `trust`: `trusted` (that certificate passes §2.4 path validation for the origin's name now), `self_signed`, or `unknown` (an owner certificate that does not validate against the two anchors: it may still be trusted by a browser, the deck cannot tell).
- `DeviceLinkEntry` gets `certificate_sha256` and `trust` per entry; the old top-level `certificate_sha256` / `self_signed` stay and describe the fallback (self-signed) certificate. The board's default entry is the first `trusted` entry, else today's rule. A `self_signed` `ts.net` entry gets the note "Needs a trusted certificate: turn on 'Trusted certificate from Tailscale' in Settings → Deck."
- `fleetdeck open` (local login link, `local_auth.rs`) uses the same provider: it prints the first `trusted` URL first, with its certificate fingerprint, then the others.
- When owner files exist, the Tailnet card says "Your own certificate files (`board.crt`) are used for every name; remove them to use the Tailscale certificate."
- `GET /api/tailnet` gets `certificate: { enabled, state: "off" | "valid" | "renewing" | "failed" | "expired" | "name_mismatch", name?, not_after?, reason_code? }`; the usable certificate and the last renewal error are reported separately (a `valid` certificate can have a `reason_code` of the last failed renewal).

## 3. Security notes

- The private key: in one 0600 file, never logged (secret mode, §2.3), never in an API answer, never in an error text.
- `tailscale` output is untrusted: bounded, parsed strictly, validated against two embedded anchors and the expected name before any use.
- Login, CSRF and Origin checks are unchanged; the certificate name is already a board Origin (§2.2); nothing adds Origins.
- Public CT logs reveal the machine and tailnet name: the opt-in and the help text.

## 4. Tests

- Secret mode: a fake program that prints a dummy key to stderr and exits 1 → the logs (captured subscriber) and the API state hold only the reason code; a dummy key on stdout with exit 0 and a bad chain → `bad_certificate`, nothing logged.
- Path validation: a self-signed leaf with the right SAN and key → refused; a chain with a missing intermediate, a bad signature, a wrong SAN, a CA leaf, an expired or not-yet-valid certificate → refused; a root in stdout is not used as an anchor; a valid test chain (a test-only anchor injected through a seam) → admitted.
- Storage: interrupted write (a failing rename) keeps the previous pair; the file mode; a start with a stored valid file serves it before any program call; a start with a stored expired file → self-signed and `expired`.
- Lifecycle: off during a running issuance → nothing stored or served after; a `tailnet_hostname` change during issuance → the result is discarded; renewal schedule near expiry.
- Resolver: SNI equal (any case) → Tailscale certificate; another SNI, no SNI (IP connection), owner files present → as §2.5; a swap is seen by a new connection; at expiry → self-signed.
- Identity: `tailnet_hostname` absent or different from the status name → `name_mismatch`, no issuance.
- Device link and CLI: per-entry `trust` and fingerprint from the live snapshot after a renewal; `fleetdeck open` prints the trusted URL first; owner files → `unknown`/`trusted` classification and the card text.

## 5. Review round 1 (2026-10-15, Codex adversary): FAIL

- **B1** the runner logs raw stderr on failure (`tailscale.rs:311-315`): a key could reach the logs.
- **M** "the chain parses" does not establish trust; validate the path against an explicit anchor policy.
- **M** two atomic file writes are not an atomic pair.
- **M** turning off or a name change does not invalidate a running issuance.
- **M** admission, continued use and expiry need distinct rules; stop serving at expiry.
- **M** the issued name must be the board's Origin identity (`tailnet_hostname`); never add Origins from SANs.
- **M** owner files are not proof of browser trust.
- **M** one fingerprint cannot describe two certificates; per-entry metadata from a live snapshot.
- **M** `fleetdeck open` has no live trusted-entry source.
- **m** tests for these boundaries.
- **U** the HSTS cause is not established.

## 6. Review map (revision 1 → revision 2)

| Finding | Answer |
| --- | --- |
| B1 key in logs | §2.3 secret mode, reason codes only |
| chain trust | §2.4 path validation against ISRG Root X1/X2, embedded |
| pair atomicity | §2.5 one 0600 file with chain and key |
| lifecycle races | §2.5 one task, generation checked before rename and swap |
| admission/use/expiry | §2.4, §2.5 schedule and resolver expiry rule |
| identity and Origin | §2.2 `tailnet_hostname` only, `name_mismatch` |
| owner trust | §2.6 `trust` computed per certificate, `unknown` |
| per-entry fingerprint | §2.6 live endpoint provider |
| CLI | §2.6 `fleetdeck open` uses the provider |
| tests | §4 |
| HSTS claim | §1 states only the observation |
