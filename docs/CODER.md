# Fleet Deck on Coder

Run the board as an always-on service inside a [Coder](https://coder.com) workspace, and drive your
whole fleet from a browser tab — spawn agents, watch them work in the terminal grid, answer their
permission prompts. No Claude Code session has to exist first, and you never need a shell on the box.

This is *standalone mode*. The plugin still works exactly as it always has; standalone is a second way
to run the same daemon. If you also install the plugin (and you should — see
[The plugin is not optional](#the-plugin-is-not-optional)), the two cooperate.

---

## The short version

```hcl
resource "coder_script" "fleetdeck" {
  agent_id     = coder_agent.main.id
  display_name = "Fleet Deck"
  icon         = "/icon/terminal.svg"
  run_on_start = true
  script       = <<-EOT
    set -e
    npm install -g fleetdeck

    export FLEETDECK_TRUSTED_ORIGINS="https://fleetdeck--${data.coder_workspace_owner.me.name}--${data.coder_workspace.me.name}--main.${var.coder_wildcard_domain}"
    export FLEETDECK_PROXY_AUTH="trust"

    fleetdeck doctor || true          # warnings must not block boot
    fleetdeck service install
    fleetdeck service start           # backgrounds, then returns
  EOT
}

resource "coder_app" "fleetdeck" {
  agent_id     = coder_agent.main.id
  slug         = "fleetdeck"
  display_name = "Fleet Deck"
  url          = "http://localhost:4711"
  subdomain    = true                 # strongly recommended — see below
  share        = "owner"
  open_in      = "tab"

  healthcheck {
    url       = "http://localhost:4711/health"
    interval  = 10
    threshold = 6
  }
}
```

Two things in there are load-bearing and everything else is taste. Read on.

---

## 1. `FLEETDECK_TRUSTED_ORIGINS` — without this, nothing works

Fleet Deck's daemon has always assumed it is talking to a browser on the same machine. It enforces
that with a same-origin wall on every state-changing POST, both WebSocket upgrades, and the mutating
GETs, plus a `Host` allowlist that defeats DNS rebinding. Loopback needs no token, so that wall is
the *only* thing standing between the fleet and any website you happen to visit — it is not
decoration, and it does not get to be switched off.

Coder's app proxy does not rewrite `Host`. So the daemon sees the *browser-facing* host —
`fleetdeck--luis--dev--main.coder.example.com`, not `localhost:4711` — and refuses the request. The
board's static shell would load and then every single thing in it would fail.

`FLEETDECK_TRUSTED_ORIGINS` is how you tell the daemon "this other origin is also me":

```sh
FLEETDECK_TRUSTED_ORIGINS="https://fleetdeck--luis--dev--main.coder.example.com"
```

- A **scheme is required**. `https://x.example.com` does not also trust `http://x.example.com`.
- Comma-separate several.
- One **leading wildcard label** is allowed: `https://*.coder.example.com` matches
  `fleetdeck--luis--dev--main.coder.example.com` but *not* `coder.example.com` itself and *not*
  `a.b.coder.example.com`. It is deliberately one label deep — a shared apex must not hand your fleet
  to every subdomain on it.
- A malformed entry is a **startup refusal**, not a silent fallback. If you typo it, you find out
  immediately instead of debugging a board that mysteriously 403s.

Everything else stays exactly as tight as it was. An origin you did not name is still refused.

## 2. `FLEETDECK_PROXY_AUTH` — who is the authenticator?

Coder proxies to `http://localhost:4711`, so the daemon sees a **loopback peer** — and loopback is
auto-authorized. Left alone, that would hand the whole fleet, `/api/spawn` included, to anyone who can
reach the proxy. Whether that is fine depends on something the daemon cannot possibly know, so you say
it out loud:

| Value | Meaning |
| --- | --- |
| `token` *(default)* | A browser arriving through a trusted external origin must **still** present the bearer token. The board shows its token gate; `fleetdeck token` prints it. |
| `trust` | **The proxy is the authenticator.** A trusted origin is sufficient and the board needs no token. |

On Coder, `trust` is the right answer *provided you keep `share = "owner"`* (the default): Coder
authenticates the user with its own session before it ever forwards a byte, and only the workspace
owner gets through. You are delegating auth to Coder, deliberately.

If you set `share = "authenticated"` or `"public"`, **do not use `trust`.** `public` in particular
would put an unauthenticated remote-code-execution endpoint on the internet. Use `token`, or better,
don't.

`FLEETDECK_PROXY_AUTH=trust` with no trusted origins is a startup refusal — there would be nothing to
trust.

**One deliberate exception under `trust`: `gateway_*` settings writes still require the bearer.** They
are the single write that reroutes every future session's LLM traffic and can exfiltrate the gateway
credential, and the trusted-origin signal is derived from `Host`/`Origin` headers a local process can
forge — too much authority to hang on a spoofable header. So under `trust`, save gateway settings once
with the key from `fleetdeck token`; everything else stays tokenless. (On a single-user workspace you
can waive even this with `FLEETDECK_TRUST_LOOPBACK=on`, which keys off the real loopback peer rather
than a header — see §"Without a wildcard domain".)

## 3. Prefer `subdomain = true`

Coder can serve an app two ways, and they are not equally good here.

**Subdomain apps** (`subdomain = true`) give the board its own hostname, so it sees `/` as its root
and everything is simple. This needs `CODER_WILDCARD_ACCESS_URL`, a wildcard DNS record, and a
matching TLS cert on the Coder deployment. Ask your Coder admin whether you have it.

**Path-based apps** (`subdomain = false`) serve at
`https://coder.example.com/@user/workspace.main/apps/fleetdeck/`. Fleet Deck **does** work under one —
the board resolves its assets, API calls and WebSockets relative to wherever it was loaded from, so
the stripped prefix never reaches the daemon. But you should still avoid it:

- Every path-based app in your deployment **shares one origin** with the Coder API itself. Coder's own
  security guidance says a malicious workspace could reuse Coder cookies to call the API, tells
  production deployments to set `CODER_DISABLE_PATH_APPS=true`, and gates path-app sharing behind a
  flag literally named `--dangerous-allow-path-app-sharing`.
- Same-origin therefore stops meaning "same app", which is exactly the ground Fleet Deck's CSRF wall
  stands on. If you must use a path-based app, set `FLEETDECK_PROXY_AUTH=token` and let the token do
  the work the origin no longer can.
- It may simply be turned off in your deployment.

## 4. There is no systemd in a Coder workspace

A Coder workspace container makes PID 1 the agent's init script. There is no init system, so
`fleetdeck service install` detects that and writes a small supervised wrapper instead of a systemd
unit — same commands either way:

```sh
fleetdeck service install     # systemd user unit if systemd is there; a supervised wrapper if not
fleetdeck service start       # backgrounds, waits until the board actually answers, then returns
fleetdeck service stop
fleetdeck status
```

Both supervisors restart the daemon if it dies, and both decline to restart it after a clean shutdown
or after exit code 3 (*"another daemon already owns the port"* — respawning that is a hot loop).

`service start` **returns immediately**. This matters: a `coder_script` that never exits leaves the
workspace stuck "starting" forever.

`service install` **freezes the current `FLEETDECK_*` environment** into `$FLEETDECK_HOME/service.env`,
because a systemd user unit does not inherit your shell. **Re-run `fleetdeck service install` after
changing any `FLEETDECK_*` variable**, or the service will keep using the old value.

## The plugin is not optional

The board can launch `claude` into a tmux pane all by itself. What it cannot do by itself is *know
what that agent is doing* — the status, the model, the file edits, the permission prompts all arrive
through the plugin's hooks. Without the plugin installed for the `claude` CLI, spawning appears to
work and then every card sits at its initial state forever, which is a genuinely baffling way for this
to fail.

So bake it into the image:

```sh
claude plugin marketplace add lacion/fleet-deck
claude plugin install fleetdeck@fleetdeck
```

`fleetdeck doctor` warns loudly if it is missing. It warns rather than fails, because you need the
board up to read the warning.

## Your image needs

- **Node ≥ 22.5** — the daemon stores state in `node:sqlite`, which landed in 22.5. There is no
  polyfill and no fallback.
- **tmux** — every agent runs in a pane.
- **The `claude` CLI**, and the Fleet Deck plugin installed for it.

`fleetdeck doctor` checks all four and exits non-zero if a hard one is missing.

## `/workspace` is detected automatically

A Coder workspace's home directory is usually ephemeral; the disk that survives
rebuilds is `/workspace`. When the daemon sees a Coder agent's environment
(`CODER`, `CODER_WORKSPACE_NAME` or `CODER_AGENT_URL`) **and** `/workspace`
exists, it roots two defaults there instead of home:

- **repo-mode spawns clone into `/workspace`** (elsewhere: `~/projects`) —
  override with `FLEETDECK_REPOS_DIR` or the spawn dialog's destination field;
- **the ⌸ Files explorer and 🗀 folder picker open at `/workspace`**
  (elsewhere: home) — override with `FLEETDECK_BROWSE_ROOT`, or pick any folder
  and hit **set as default root**.

Both are just seeded defaults: an explicit setting or env var always wins, and
nothing changes for non-Coder machines.

## Without a wildcard domain: just port-forward

If your Coder deployment has no wildcard DNS and path-based apps are disabled, you do not need any of
the above. Forward the port and open the board on your own machine, where it is loopback and every
default already applies:

```sh
coder port-forward my-workspace --tcp 4711:4711
# → http://localhost:4711
```

No trusted origins, no proxy auth. Since 0.16.0 the four *power* routes (typing into terminals, mail,
gateway settings, arming unsupervised spawns) still ask the browser for the board key even over
loopback — everything else works without it. On a single-user workspace that gate protects nothing
(there are no other OS users), so opt out of it in the workspace env:

```sh
FLEETDECK_TRUST_LOOPBACK=on
```

and the port-forwarded board needs no key at all, exactly like pre-0.16.0. Keep it **off** on any
machine other people can log into: the loopback gates are what stop another local user from typing
into your agents' terminals. Hooks stay authenticated either way (their shims read the key file
themselves), and this knob refuses to combine with LAN mode or `FLEETDECK_REQUIRE_TOKEN`.

## Troubleshooting

| Symptom | Cause |
| --- | --- |
| Board shell loads, then everything 403s | `FLEETDECK_TRUSTED_ORIGINS` is unset or does not match the URL in your address bar. Compare them character by character — the scheme counts. |
| Board asks for a token you did not expect | `FLEETDECK_PROXY_AUTH` is `token` (the default). Either run `fleetdeck token` and paste it, or set `trust` if Coder is authenticating. |
| Terminals/mail ask for a key on a port-forwarded board | The 0.16.0 loopback power gates. Set `FLEETDECK_TRUST_LOOPBACK=on` on a single-user workspace, or paste the key once (`fleetdeck token` on the workspace). |
| Daemon refuses to start | It prints the reason on stderr — a malformed trusted origin, or `trust` with nothing to trust. Check `$FLEETDECK_HOME/fleetd.log`. |
| Cards appear and never change | The plugin is not installed for the `claude` CLI. Run `fleetdeck doctor`. |
| Config changes did nothing | You changed the environment but did not re-run `fleetdeck service install`. |
| Workspace stuck "starting" | Something in your `coder_script` is not exiting. `fleetdeck service start` returns on its own; don't wrap it in a foreground loop. |
