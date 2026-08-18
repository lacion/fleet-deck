# Fleet Deck on Coder

Run the board as an always-on service inside a [Coder](https://coder.com) workspace, and drive your
whole fleet from a browser tab — spawn agents, watch them work in the terminal grid, answer their
permission prompts. No Claude Code session has to exist first, and you never need a shell on the box.

The non-negotiable DX rule is that Fleet Deck may fail without making the workspace or an ordinary
terminal fail with it. The startup script is bounded and exits zero after recording a Fleet Deck
failure; automatic hook failures return Claude's neutral result with no stderr, warning, or injected
context. Interactive hooks default to observation-only outside Fleet Deck-owned Claude panes.

This is *standalone mode*. The plugin still works exactly as it always has; standalone is a second way
to run the same daemon. If you also install the plugin (and you should — see
[The plugin is required when Fleet Deck is active](#the-plugin-is-required-when-fleet-deck-is-active)), the two cooperate.

---

## The short version

```hcl
resource "coder_script" "fleetdeck" {
  agent_id     = coder_agent.main.id
  display_name = "Fleet Deck"
  icon         = "/icon/terminal.svg"
  run_on_start = true
  script       = <<-EOT
    set +e
    (
      set -e
      # Pin this deliberately. For a fleet rollout, use the immutable runtime
      # pattern below instead of updating a live global install in place.
      npm install -g fleetdeck@0.23.5

      # A named coder_app's hostname is <slug>--<workspace>--<owner> — the agent
      # name appears only in raw-PORT app hostnames (see "The exact hostname" below).
      export FLEETDECK_TRUSTED_ORIGINS="https://fleetdeck--${data.coder_workspace.me.name}--${data.coder_workspace_owner.me.name}.${var.coder_wildcard_domain}"
      export FLEETDECK_PROXY_AUTH="trust"

      fleetdeck doctor || true          # warnings must not block boot
      fleetdeck service install
      fleetdeck service start           # backgrounds, verifies readiness, then returns
    )
    rc=$?
    if [ "$rc" -ne 0 ]; then
      echo "WARN: Fleet Deck setup failed ($rc); continuing workspace startup."
    fi
    exit 0
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

The origin/auth settings are load-bearing. The install command is intentionally simple enough to show
the service shape; production fleets should also make package installation atomic and fail-open.

## Production rollout pattern: immutable and version-locked

Treat the daemon and Claude plugin as one release. Pin an exact version, install it into a new staging
directory, validate both the CLI version and bundled daemon, atomically rename that directory into a
versioned runtime slot, then point `service install` at that absolute CLI. Never run a floating global
update over the files a live daemon is executing.

A reliable workspace-start script has these properties:

1. The image contains one root-owned, fully validated copy of its default Fleet Deck runtime,
   including `compatibility.json`. A fresh persistent volume is restored atomically from that local
   copy; default-version startup never depends on npm being reachable.
2. Before starting Fleet Deck or repairing its plugin, the script reads that policy and checks the
   exact installed `claude --version`. Unsupported, prerelease, and unknown versions stay completely
   inactive even when Bun, npm, or the persistent runtime is missing.
3. A supported CLI then unlocks the runtime path: Bun and the complete baked image runtime must
   validate before any persistent runtime is trusted.
4. A warm runtime is accepted only when its full package tree and CLI link match the image copy, not
   merely when `fleetdeck --version` agrees. An enabled unverified plugin is quarantined before a
   same-version partial/stale runtime is restored locally. An explicit non-default
   `FLEETDECK_VERSION` may instead use bounded npm staging; its package manifest, policy, CLI, daemon
   bundle, and board entry point must all validate before rename.
5. The selected runtime's policy is checked again, the old daemon is stopped, and startup waits for
   restored Claude state plus a valid local plugin inventory before configuration changes begin.
6. The Claude marketplace source is pinned to the matching immutable Git tag
   `lacion/fleet-deck@vX.Y.Z`. A source/version stamp makes warm starts network-free; a missing or
   mismatched stamp forces one bounded remove/re-add and plugin reinstall, including the
   same-version-cache case that `plugin update` cannot repair. The exact source is verified before
   the daemon starts. A manually disabled plugin stays disabled, including across a failed destructive
   repair and the next boot; separate owner-only markers retain compatibility and crash-recovery intent.
7. `fleetdeck service install` and `service start` run through the absolute validated runtime only
   after the pinned plugin source is ready. Fleet Deck re-enables only a disable it owns, and only
   after service health succeeds; a manual disable remains untouched.
8. `compatibility.json` is the sole Claude range definition. The range is never copied into the Coder
   template, and every hook independently rechecks the exact running Claude process in case it changed
   after workspace startup.
9. Claude Code remains under the engineer's control. The image installs Claude's normal current
   release and does not set `DISABLE_AUTOUPDATER`; engineers may update or downgrade independently.
10. Every network call and lock acquisition has a timeout. An EXIT trap removes only the exact staging
   directory, releases Coder startup coordination, reports the failure, and exits zero so the developer
   still gets a workspace.

This keeps the previous runtime available during a failed download, makes rollback a version change,
and prevents the CLI/bundle split that produces source-module errors such as a missing
`src/daemon/takeover.ts`.

---

## 1. `FLEETDECK_TRUSTED_ORIGINS` — without this, nothing works

Fleet Deck's daemon has always assumed it is talking to a browser on the same machine. It enforces
that with a same-origin wall on every state-changing POST, both WebSocket upgrades, and the mutating
GETs, plus a `Host` allowlist that defeats DNS rebinding. Loopback needs no token, so that wall is
the *only* thing standing between the fleet and any website you happen to visit — it is not
decoration, and it does not get to be switched off.

Coder's app proxy does not rewrite `Host`. So the daemon sees the *browser-facing* host —
`fleetdeck--dev--luis.coder.example.com`, not `localhost:4711` — and refuses the request. The
board's static shell would load and then every single thing in it would fail.

`FLEETDECK_TRUSTED_ORIGINS` is how you tell the daemon "this other origin is also me":

```sh
FLEETDECK_TRUSTED_ORIGINS="https://fleetdeck--dev--luis.coder.example.com"
```

**The exact hostname.** For a *named* app like the `coder_app` above (slug `fleetdeck`), Coder
generates the subdomain as `<slug>--<workspace>--<owner>` — for owner `luis` and workspace `dev`,
that is `fleetdeck--dev--luis.coder.example.com`. The **agent name is not in it**: Coder only
includes the agent in *raw-port* app hostnames, which are `<port>--<agent>--<workspace>--<owner>`
(e.g. `4711--main--dev--luis.coder.example.com`). Trusting `fleetdeck--luis--dev--main…` (agent
included, owner first) looks plausible and matches nothing — the shell loads, then `/state`, both
WebSockets, and every control request 403. If the board 403s anyway, copy the hostname from your
address bar character for character.

- A **scheme is required**. `https://x.example.com` does not also trust `http://x.example.com`.
- Comma-separate several.
- One **leading wildcard label** is allowed: `https://*.coder.example.com` matches
  `fleetdeck--dev--luis.coder.example.com` but *not* `coder.example.com` itself and *not*
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
fleetdeck service start       # backgrounds, waits for this exact version to answer, then returns
fleetdeck service stop
fleetdeck status
```

Both supervisors restart the daemon if it dies, and both decline to restart it after a clean shutdown
or after exit code 3 (*"another daemon already owns the port"* — respawning that is a hot loop).

Startup uses a progressive 30-second readiness deadline. It verifies the installed version, home,
port, PID, process shape and managed identity before trusting or replacing an existing daemon. A
systemd upgrade uses `systemctl --user restart` so `Restart=always` cannot race a raw signal; the
no-systemd path retires only the matching supervisor/daemon pair. A foreign port owner is diagnosed
and never killed. If a genuinely slow image needs longer, set
`FLEETDECK_SERVICE_START_TIMEOUT_MS` (250–300000 ms), re-run `service install`, and retry.

Interactive Claude hooks default to `FLEETDECK_HOLD_SCOPE=spawned`: Fleet Deck may hold a prompt only
for a live agent it launched into its own tmux pane. The daemon injects that pane's exact session id,
and the hook verifies the incoming payload matches it before taking the long wait. Shell panes get no
marker; a nested or manually launched Claude process cannot inherit permission to stall. A Claude
session started in an ordinary Coder terminal falls through promptly to Claude's native prompt even
when nobody has the board open. `off` provides telemetry without board-side prompt control. The
legacy `all` setting can broaden daemon-side admission, but the command shim still requires the exact
Fleet Deck-owned session marker for a long wait, so an ordinary terminal cannot silently acquire a
minutes-long hold. Even an eligible pane is held only while an authorized board tab is connected.
Closing the last tab releases all live hooks immediately without re-arming them; a terminal-viewer
socket never counts as a board answer consumer.

`service start` **returns after bounded readiness; it does not remain in the foreground**. This
matters: a `coder_script` that starts its own foreground supervisor leaves the workspace stuck
"starting" forever.

`service install` **freezes the current `FLEETDECK_*` environment** into `$FLEETDECK_HOME/service.env`,
because a systemd user unit does not inherit your shell. **Re-run `fleetdeck service install` after
changing any `FLEETDECK_*` variable**, or the service will keep using the old value.

## The plugin is required when Fleet Deck is active

The board can launch `claude` into a tmux pane all by itself. What it cannot do by itself is *know
what that agent is doing* — the status, the model, the file edits, the permission prompts all arrive
through the plugin's hooks. Without the plugin installed for the `claude` CLI, spawning appears to
work and then every card sits at its initial state forever, which is a genuinely baffling way for this
to fail.

Install it during workspace startup, after restoring the persistent Claude state and only after the
runtime compatibility check passes. Do not rely on an image-baked copy when the workspace mounts
`~/.claude`, because that mount hides the image layer:

```sh
FLEETDECK_VERSION=0.23.5
claude plugin marketplace add "lacion/fleet-deck@v$FLEETDECK_VERSION" --scope user
claude plugin install fleetdeck@fleetdeck
```

Publish the Fleet Deck tag and npm package before deploying a Coder template that selects that
version: bootstrap intentionally requires both `fleetdeck@X.Y.Z` and `vX.Y.Z`. Claude Code runs the
plugin from its versioned cache, not from the marketplace checkout, so a marketplace fetch without a
manifest version change does not update running hooks. A same-version cache previously sourced from
mutable `main` is also indistinguishable through `plugin list`; migrate it once by removing/re-adding
the marketplace at the exact tag and uninstalling/reinstalling `fleetdeck@fleetdeck`. Persist an
owner-only source stamp only after `claude plugin list --json` reports the exact version and the pinned
repair succeeds. A valid exact plugin may remain disabled by the engineer; source identity and enabled
state are separate facts. Serialize this with every other writer of Claude's configuration and bound
both the lock acquisition and network repair so a GitHub outage never blocks workspace login.

If Claude is outside the range in `compatibility.json`, do not repair the marketplace or start Fleet
Deck. Stop the service and quarantine an enabled user-scope plugin quietly. Publish the owner-only
`plugin-disabled-by-compat` marker before attempting that disable so a crash cannot lose ownership of
the transition; on a later supported start, re-enable only when that marker proves Fleet Deck owns the
disable. A separate crash-recovery marker preserves a manual disable across uninstall/reinstall
failures. Project, local, and managed settings can override user scope, which is why every shipped hook
independently checks the exact running Claude process before it touches the daemon, token, timers, or
network.

`fleetdeck doctor` warns loudly if it is missing. It warns rather than fails, because you need the
board up to read the warning.

## Your image needs

- **Bun 1.3.14+** — the CLI, hook shims, and daemon runtime use Bun; SQLite is provided by
  `bun:sqlite`.
- **Node/npm** — used while building the image and only for an explicit non-default Fleet Deck version
  override at workspace startup. They are not the daemon runtime.
- **tmux 3.4+** — every managed Claude agent runs in a pane, and Fleet Deck uses the 3.4 no-start
  probe to avoid attaching to an unintended replacement tmux server.
- **The `claude` CLI**, on whichever update channel or exact version the engineer chooses. Fleet Deck
  activates the plugin only when that version is inside `compatibility.json`.

`fleetdeck doctor` checks the runtime, tmux, Claude CLI and plugin, and exits non-zero if a hard
requirement is missing.

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

## Git authentication for repo spawns

Fleet Deck does not own GitHub or GitLab credentials. Before a remote repo
spawn creates a card, it resolves the exact origin and runs a bounded,
non-interactive `git ls-remote`. This is the authority: `gh auth status` or
`glab auth status` can be green while Git itself is still using another
credential path.

Coder configures Git-over-HTTPS through `GIT_ASKPASS` and its external-auth
providers. If that access is missing, the spawn form explains the failure and
links to the workspace owner's Coder **External authentication** page. Complete
the GitHub/GitLab OAuth flow there, return to the still-open spawn form, and
click **Check access**. For SSH, register the key printed by `coder publickey`
with the forge and establish host trust, or choose HTTPS in the form.

The board never asks for, receives, or persists an OAuth token. Outside Coder
it shows the relevant `gh auth login` or `glab auth login` command instead.
See Coder's [external Git authentication](https://coder.com/docs/admin/external-auth),
the [GitHub CLI auth manual](https://cli.github.com/manual/gh_auth_login), and
the [GitLab CLI documentation](https://docs.gitlab.com/cli/).

Provisioning is cancellable. Kill on a card that still says `cloning…` stops
the Git process group (including credential/SSH helpers), removes only the
Fleet Deck temporary checkout, prevents a late tmux launch, and retires the
card as `spawn cancelled`.

## Test the complete Coder path locally

A local Coder server is the most useful acceptance environment because it exercises the parts a
plain `fleetdeck serve` cannot: workspace startup scripts, persistent home state, the app proxy,
plugin cache, no-systemd supervision, tmux, and a real Claude CLI in one container.

The validated shape is:

- Coder server on `http://127.0.0.1:3000` with a local PostgreSQL database;
- a Docker-backed template whose image includes Node/npm, Bun 1.3.14+, tmux 3.4+, and Claude Code;
- `coder_app.url = "http://localhost:4711"`, `subdomain = false` for a no-DNS local test, and
  `share = "owner"`;
- the Fleet Deck repository mounted read-only only when testing a working-tree plugin; the packaged
  acceptance path installs a freshly packed tarball into a new immutable runtime slot;
- a **fresh workspace and persistent volume for the final run**, so an old runtime or plugin cache
  cannot make a broken bootstrap look healthy.

Open the board through Coder's path app, not directly, for example:

```text
http://127.0.0.1:3000/@OWNER/WORKSPACE.AGENT/apps/fleetdeck/
```

Then verify the story, not just the landing page:

1. The workspace reaches Running even if Fleet Deck installation is deliberately made to fail.
2. With a supported Claude version, `fleetdeck status` and `/health` report `managed: true` and the
   exact pinned **Fleet Deck** version; `claude plugin list --json` reports that same Fleet Deck
   version.
3. With versions immediately below and above the declared Claude range, workspace startup still
   succeeds, the Fleet Deck service/plugin stay inactive, and a real Claude session receives no hook
   stderr, warning, or model context. Return to a supported version and prove only a
   compatibility-disabled plugin is re-enabled; a manually disabled plugin stays disabled.
4. A normal Coder terminal Claude question renders natively without waiting on the board.
5. A board-spawned Claude question appears in Needs you, accepts every required answer, and resumes.
6. Terminal input/output, terminal grid, files, worktrees, compose, rename, share, theme, and density
   work through the path prefix.
7. Restart the service while the board is open; the board reconnects and the owned pane/card survives.
8. Recheck the board at 390 px width so no header action, rail, modal, or terminal control is off-screen.

The current release was exercised this way with Coder 2.33.7, Claude Code 2.1.234, Bun 1.3.14, and
tmux 3.5a. A Debian Bookworm image with tmux 3.3a correctly failed the spawn requirement; use a newer
tmux package rather than weakening the guard.

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
| `Cannot find module .../src/daemon/takeover.ts` | The CLI and package payload are stale or incomplete. Install the current exact release into a clean runtime slot; validate the bundled daemon before switching, then re-run `service install` and `service start`. |
| Startup says no managed daemon answered within 5 s | Upgrade the CLI. Current startup uses a progressive 30 s readiness deadline. If it still fails, `fleetdeck status` distinguishes an owned slow boot from another process on the port; inspect `fleetd.log`. |
| Fleet Deck is inactive after a Claude update | Compare `claude --version` with the installed runtime's `compatibility.json`. Claude is intentionally unaffected; install a Fleet Deck release that supports it rather than pinning or silently downgrading the engineer's CLI. |
| A manually disabled Fleet Deck plugin comes back | This is a bootstrap bug. Re-enabling is allowed only when the owner-only compatibility marker proves Fleet Deck disabled it; without that marker, preserve the engineer's choice. |
| `AskUserQuestion` stalls an ordinary terminal | The plugin or daemon is stale. Update both Fleet Deck channels to the same version, re-run `service install`, and start a new Claude session. Current shims require an exact Fleet Deck-owned session marker before any long wait. |
| Cards appear and never change | The plugin is not installed for the `claude` CLI. Run `fleetdeck doctor`. |
| Cards move but prompt behavior is inconsistent | The cached plugin version differs from the daemon. Compare `claude plugin list --json` with `fleetdeck --version`; update the marketplace/plugin and start a new session. |
| Config changes did nothing | You changed the environment but did not re-run `fleetdeck service install`. |
| Workspace stuck "starting" | Something in your `coder_script` is not exiting. `fleetdeck service start` returns on its own; don't wrap it in a foreground loop. |
