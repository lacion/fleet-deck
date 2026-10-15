# A trusted board certificate from Tailscale (DL-190)

- **Status:** Design, revision 1 (2026-10-15). Needs an adversary DESIGN REVIEW PASS before code.
- **Owner report (2026-10-15):** a device link opened on the phone (on the tailnet) gives a certificate/domain error; the IP-address link works after accepting the self-signed warning. Owner: "lets do the propper fix".
- **Goal:** the board answers on its tailnet name (`<host>.<tailnet>.ts.net`) with a certificate that browsers trust, so a phone or another computer opens it with no warning. Nothing changes for the loopback and IP-address entries.

## 1. Facts today

- The board serves one certificate (`crates/fleetdeck-deck/src/http/tls.rs`): the owner's `board.crt`/`board.key` in the deck data directory when both exist, else a kept self-signed certificate for all board names (`board-selfsigned.*`; on the owner's machine: `100.89.30.8`, `127.0.0.1`, the IPv6 tailnet address, `lighting.taildf78ad.ts.net`, `localhost`).
- Browsers treat `*.ts.net` as HTTPS-only with a valid certificate (HSTS preload, as far as known), so a self-signed certificate on the `ts.net` name cannot be bypassed on a phone. An IP-address entry can be bypassed.
- The device link (`POST /api/auth/device_link`, DL-146) lists one login URL per board origin and the certificate SHA-256 and `self_signed`; the board chooses the first entry that is not "This computer".
- The certificate fingerprint is shown to the owner (`fleetdeck open`, the device link dialog); no program pins it.
- `tailscale cert [--cert-file -] [--key-file -] [--min-validity <d>] <domain>` gets a Let's Encrypt certificate for this machine's tailnet name (Tailscale 1.102.4 on the owner's machine; `-` writes PEM to stdout, so it works through Tailscale for Windows from WSL). It needs "HTTPS Certificates" enabled once in the tailnet's admin console. The name is then published in public Certificate Transparency logs.
- `TailscaleCli` (`crates/fleetdeck-core/src/tailscale.rs`) runs the chosen program with a minimal environment, no stdin, a deadline, capped stdout (2 MiB) and stderr (64 KiB).

## 2. Design

### 2.1 Setting

- New deck setting `tailscale_cert` (bool, default `false`, `apply: Live`, danger when turned on: needs the owner's elevation). Label "Trusted certificate from Tailscale". Help text: "The board gets a certificate for its tailnet name that browsers trust. Tailscale must allow HTTPS certificates (admin console → DNS → HTTPS Certificates). The name `<host>.<tailnet>.ts.net` becomes public in Certificate Transparency logs."
- Opt-in because the publication in public logs cannot be undone (owner decision to confirm).

### 2.2 Getting and renewing the certificate

- When `tailscale_cert` is on, and Tailscale is available and not in userspace networking, the deck runs `tailscale cert --cert-file - --key-file - --min-validity 720h <self DNS name>` through `TailscaleCli` (new method `cert(domain)`), with a 90 s deadline (a first issuance can take that long), stdout capped at 64 KiB. `<self DNS name>` is the DNS name of `tailscale status` for this machine, lower case, without the trailing dot; it must end in `.ts.net` and pass the host-name syntax check.
- The output must hold one or more `CERTIFICATE` PEM blocks and exactly one private key block. Checks before use: the key matches the leaf; the leaf names exactly the DNS name (SAN); the leaf is valid now and for at least 24 h; the chain parses. A failed check keeps the previous certificate and records the error.
- Stored in the deck data directory: `board-tailscale.crt` (mode 0644) and `board-tailscale.key` (mode 0600), written atomically (the existing `write_file` staging). At start, a stored pair that passes the checks for the current DNS name is used at once, before any `tailscale` call (a phone works also while Tailscale is slow).
- Runs: at start (on the start thread's deadline rules of tailnet §15.3: the start never waits for it; the board starts with the stored or self-signed certificate and swaps in the new one when it arrives), then every 12 h, and right after the owner turns the setting on. `--min-validity 720h` makes Tailscale renew 30 days before expiry. Turning the setting off removes the Tailscale certificate from use at once and deletes the two files.

### 2.3 Serving: one certificate per name (SNI)

- The board TLS configuration uses a certificate resolver (`rustls::server::ResolvesServerCert`) instead of a single certificate:
  - the owner's `board.crt` pair, when present, for every name (unchanged precedence: the owner's files win over everything);
  - else, when the client's SNI equals the tailnet DNS name (ASCII case-insensitive) and a Tailscale certificate is loaded: the Tailscale certificate;
  - else the self-signed certificate (as today, for loopback, IP addresses and every other name).
- The resolver holds the Tailscale certificate behind a lock; a renewal swaps it without a restart. Existing connections keep their certificate.

### 2.4 Device link and board

- `DeviceLinkEntry` gets `trusted: bool`: true for the tailnet-name entry while a Tailscale (or owner) certificate is served for it; `DeviceLink.self_signed` stays for the other entries. The board's default entry is the first `trusted` entry, else the first entry that is not "This computer" (today's rule). An untrusted `ts.net` entry gets the note "Needs a trusted certificate: turn on 'Trusted certificate from Tailscale' in Settings → Deck."
- The Tailnet card shows the state: off; on and valid until `<date>`; on and failed with the reason (`tailscale cert` stderr first line, scrubbed; "HTTPS certificates are not enabled for this tailnet" mapped to the admin-console instruction).
- `GET /api/tailnet` gets `certificate: { enabled, state: "off" | "valid" | "failed", not_after?, reason? }`.

### 2.5 Station and CLI

- No change to stations or the link protocol (the link uses its own pinned keys). `fleetdeck open` prints the URL of the trusted entry first when one exists.

## 3. Security notes

- The Tailscale certificate key is a secret: mode 0600, never logged, never in an API answer. It authenticates only the tailnet name of this machine.
- The `tailscale` program choice and its limits stay as tailnet §15.1 (incl. Tailscale for Windows from WSL). Its output is untrusted input: parsed with bounds and checked against the expected name before use.
- Login, CSRF and Origin checks are unchanged; the tailnet-name origin is already a board origin (it is in the certificate names today).
- Public CT logs reveal the machine and tailnet name: the reason for the opt-in and the help text.

## 4. Tests (to be detailed in the worker briefs)

- `TailscaleCli::cert` with a fake program: PEM on stdout parsed; a key that does not match, a wrong SAN, an expired or short-lived leaf, two keys, oversize output → refused, previous certificate kept; timeout.
- Resolver: SNI = tailnet name → Tailscale certificate; other SNI, no SNI, IP → self-signed; owner files → owner certificate for all; a swap is seen by a new connection.
- Storage: atomic write, modes, start with a stored valid pair (no program call needed), a stored pair for another name ignored, turning off deletes the files.
- Setting: off by default; turning on needs elevation; the card states.
- Device link: `trusted` flags and the default entry.
