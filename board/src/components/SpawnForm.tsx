import React, { useEffect, useMemo, useRef, useState } from 'react';
import { spawnSession, saveSettings, reasonOf, armUnsupervised, type ApiResult } from '../api.ts';
import { batchTotal, expandBatchTasks, parseBatchTasks } from '../util.ts';
import { useModal } from '../useModal.ts';
import DirPicker from './DirPicker.tsx';
import type { SessionEntry } from '../../../contracts/index.ts';

// Local structural mirrors of the durable settings store: every field the form
// reads defensively (`settings?.…`) is OPTIONAL here so each `?.`/`??` stays
// honest while the real contract type flows in structurally (util.ts doctrine).
interface GatewayLike {
  ready?: boolean;
  base_url?: string | null;
  auth_style?: 'bearer' | 'api-key';
  model_discovery?: boolean;
  default?: boolean;
  token_set?: boolean;
}
interface SettingsLike {
  gateway?: GatewayLike;
  repo_transport?: { value?: 'ssh' | 'https' };
  repo_default_org?: { value?: string | null; source?: string };
  repos_dir?: { resolved?: string };
  browse_root?: { resolved?: string | null };
  fav_dirs?: string[];
  repo_setup?: Record<string, string>;
}
// The catalog rows the form completes against — repo_name is optional here so
// the defensive `r.repo_name?.…` below is not flagged (the contract marks it
// required; the real RepoCatalogEntry[] flows in structurally).
interface RepoCatalogLike {
  repo_name?: string;
  origin_url: string | null;
  root: string;
}
// {ok} | {err} save-result notes (gateway, default org, repos dir).
interface SaveNote {
  ok?: string;
  err?: string;
}
// The POST body shared by every agent in a submit; each field is set
// conditionally, so all are optional.
interface SpawnBody {
  kind?: 'shell';
  cwd?: string;
  repo?: string;
  branch?: string;
  branch_mode?: 'worktree' | 'in-place';
  repo_host?: 'github' | 'gitlab';
  repo_org?: string;
  repo_transport?: 'ssh' | 'https';
  model?: string;
  permission_mode?: string;
  worktree?: boolean;
  remote_control?: boolean;
  dangerously_skip_permissions?: boolean;
  arm_token?: string;
  gateway?: boolean;
  setup_cmd?: string;
  prompt?: string;
  plan_id?: number;
}
// The spawn-response fields this form reads. api.ts's ApiJson declares only
// {ok, reason, err} and explicitly invites callers to reveal the extra fields
// they read — so this is a local cast target for res.json, not the wire type.
interface SpawnJson {
  ok?: boolean;
  provisioning?: boolean;
  callsign?: string;
  session_id?: string;
}
// Settings-save responses echo the resolved settings back.
interface SettingsSaveJson {
  ok?: boolean;
  settings?: SettingsLike;
}
// The arm endpoint mints a one-time token.
interface ArmJson {
  ok?: boolean;
  arm_token?: string;
}
// The gateway-profile save patch (write-only token).
interface GatewaySaveBody {
  gateway_base_url?: string | null;
  gateway_token?: string;
  gateway_auth_style?: string;
  gateway_model_discovery?: boolean;
  gateway_default?: boolean;
}
// The onSpawned (plan-mark) callback's result.
interface SpawnResult {
  ok?: boolean;
  text: string;
}
// One failed leg of a batch, and the running batch progress.
interface BatchFailure {
  prompt: string;
  reason: string;
}
interface BatchProgress {
  done: number;
  total: number;
  failed: BatchFailure[];
}

interface SpawnFormProps {
  sessions?: SessionEntry[];
  repoCatalog?: RepoCatalogLike[];
  settings?: SettingsLike;
  homeDir: string;
  prefillPrompt: string;
  prefillCwd: string;
  planMode: boolean;
  planId: number | null;
  onClose: () => void;
  onSpawned?: ((json: SpawnJson) => Promise<SpawnResult | null>) | undefined;
}

// v2.2 repo+branch mode — client-side mirrors of the daemon's input gates, for
// instant feedback only: the DAEMON is the authority (git check-ref-format gets
// the last word on a branch, parseRepoInput on a repo), same doctrine as
// validSuffix in util.js. Refusing the obvious junk here just saves a POST.
const branchProblem = (b: string): string | null => {
  if (b.length > 200) return 'too long for a branch name';
  if (b.startsWith('-')) return 'a branch cannot start with “-”';
  let hasControl = false;
  for (let i = 0; i < b.length; i++) {
    if (b.charCodeAt(i) <= 0x1f) {
      hasControl = true;
      break;
    }
  }
  if (/[\s~^:?*[\\]/.test(b) || hasControl)
    return 'no spaces or git-special characters (~ ^ : ? * [ \\)';
  if (b.includes('..') || b.includes('@{')) return 'no “..” or “@{”';
  if (b.endsWith('.lock') || b.endsWith('/') || b.startsWith('/')) return 'not a valid ref name';
  return null;
};

// The repo's basename, for the destination preview — works for https/ssh URLs,
// scp-like git@host:org/repo, org/repo shorthand (incl. gitlab subgroups —
// split-and-pop takes the LAST segment), absolute paths, bare names.
const repoNameOf = (input: string): string | null => {
  const s = (input || '').trim().replace(/\/+$/, '');
  if (!s) return null;
  const tail = s.split(/[/:]/).pop() ?? '';
  const name = tail.replace(/\.git$/, '');
  return name || null;
};

// v2.4 — is this repo field `org/repo` shorthand? A client mirror of the daemon's
// parseRepoInput shorthand branch (repos.mjs), for feedback only: the daemon
// decides. Shorthand-shaped iff trimmed & non-empty, contains a "/", no
// whitespace, no ":" (rules out URLs and scp-style git@host:path), no "@", does
// not start with "/", "~", ".", or "-" (rules out absolute/home/relative/argv),
// and every "/"-segment is non-empty and not "."/"..". Everything else (a URL, an
// absolute path, a bare name) is NOT shorthand, so it must never carry repo_host.
const isShorthandRepo = (input: string): boolean => {
  const s = (input || '').trim();
  if (!s.includes('/')) return false;
  if (/[\s:@]/.test(s)) return false; // whitespace, URL scheme/host, scp-style userinfo
  if (/^[/~.-]/.test(s)) return false; // absolute, home, relative, or argv-flag lead
  return s.split('/').every((seg) => seg && seg !== '.' && seg !== '..');
};

// A bare repo name can be promoted to <default-org>/<name> by the daemon when
// no known local checkout exists. Client mirror for the preview/form guard only.
const isBareRepo = (input: string): boolean => {
  const s = (input || '').trim();
  return !!s && !/[\s/:@]/.test(s) && !/^[-.~]/.test(s);
};

// The origin a shorthand resolves to, composed with the daemon's exact rule
// (repos.mjs parseRepoInput): the input minus any trailing `.git`, then either
// scp-style ssh (git@<github.com|gitlab.com>:<slug>.git) or https
// (https://<host>/<slug>.git). GitLab shorthand may carry subgroups
// (group/sub/repo) — the whole path rides through untouched. Only ever called on
// isShorthandRepo-true input; a feedback-only preview, the daemon composes the
// origin it actually clones.
const shorthandOrigin = (input: string, host: string, transport: string): string => {
  const slug = (input || '').trim().replace(/\.git$/i, '');
  const domain = host === 'gitlab' ? 'gitlab.com' : 'github.com';
  return transport === 'ssh' ? `git@${domain}:${slug}.git` : `https://${domain}/${slug}.git`;
};

// v1.2 spawn form — POST /api/spawn on an explicit human click, never any
// other path. Fail-loud: a {ok:false, reason} renders inline, no silent
// retry; success shows "spawning — <callsign>" then closes.
//
// v1.3 additions:
//   - "run unsupervised" is hazard-styled with a TWO-STEP confirm: checking
//     it only reveals the confirm row; a second explicit arm checkbox is what
//     puts dangerously_skip_permissions:true on the POST body. Un-arming (or
//     unchecking step one) drops the flag. While revealed-but-unarmed the
//     Spawn button stays disabled — the form never quietly downgrades.
//   - plan execution prefill (planMode): prompt arrives prefilled, the
//     textarea grows, and the caret parks right after "Custom instructions: "
//     so the human edits exactly where the contract expects; after a
//     successful POST the onSpawned callback (App) marks the plan executed
//     and any failure of THAT surfaces here instead of auto-closing over it.
//
// v1.6: "remote control" checkbox → remote_control:true on the POST body —
// the session comes up remote-controllable from claude.ai (web/phone) from
// birth; the claude.ai link lands on the card once the daemon harvests it.
//
// BATCH: ticking "batch" turns the prompt box into a task list — one agent per
// line, "3x <task>" to run a line several times — and fans the whole list out
// across the repo in one submit. Two rules make it safe:
//
//   • It is opt-in. A multi-line prompt is still ONE prompt otherwise, which
//     plan execution depends on absolutely (the plan IS the prompt).
//   • Every agent in a batch gets its OWN git worktree, forced, not offered.
//     N agents sharing one working tree overwrite each other's edits, and the
//     only thing that would tell you is a conflict warning after the fact.
//
// Since the daemon no longer caps how many agents may be live, the preview
// below — the exact list, counted, before you click — IS the guardrail.
export default function SpawnForm({
  sessions,
  repoCatalog,
  settings,
  homeDir,
  prefillPrompt,
  prefillCwd,
  planMode,
  planId,
  onClose,
  onSpawned,
}: SpawnFormProps) {
  const [cwd, setCwd] = useState(prefillCwd || '');
  const [prompt, setPrompt] = useState(prefillPrompt || '');
  const [model, setModel] = useState('');
  const [permissionMode, setPermissionMode] = useState('default');
  const [worktree, setWorktree] = useState(false);
  const [batch, setBatch] = useState(false); // prompt box becomes a task list
  const [remote, setRemote] = useState(false); // v1.6: remote control from birth
  const [shellOnly, setShellOnly] = useState(false);
  const [setupCmd, setSetupCmd] = useState('');
  const [saveSetupDefault, setSaveSetupDefault] = useState(false);
  const setupRepoSeed = useRef<string | null | undefined>(null);
  // v0.15 — LLM gateway. Seeded ONCE from the daemon's gateway_default, and
  // deliberately NOT live-adopted from later frames, for the same reason as the
  // transport pill above: a control that decides WHO IS BILLED must never flip
  // under the human while they are looking at it.
  const [gateway, setGateway] = useState(() => !!settings?.gateway?.default);
  const [gwEdit, setGwEdit] = useState(false); // the inline config drawer
  const [gwUrl, setGwUrl] = useState(settings?.gateway?.base_url ?? '');
  const [gwToken, setGwToken] = useState(''); // write-only; never seeded
  const [gwStyle, setGwStyle] = useState<string>(() => settings?.gateway?.auth_style ?? 'bearer');
  const [gwDiscovery, setGwDiscovery] = useState(
    () => settings?.gateway?.model_discovery !== false,
  );
  const [gwAlways, setGwAlways] = useState(() => !!settings?.gateway?.default);
  const [gwNote, setGwNote] = useState<SaveNote | null>(null); // {ok} | {err} after a save
  const [gwBusy, setGwBusy] = useState(false);
  const [unsup, setUnsup] = useState(false); // step 1: reveal the confirm
  const [armed, setArmed] = useState(false); // step 2: actually send the flag
  // 0.16.0 — the server requires a one-time arm token for any unsupervised
  // spawn (the API half of this two-step). Fetched when the human arms, echoed
  // in the spawn body; single-use, 60s TTL, re-fetched on every arm.
  const [armToken, setArmToken] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null); // "spawning — <callsign>"
  const [progress, setProgress] = useState<BatchProgress | null>(null); // batch: {done, total, failed[]}
  // v2.2 — repo+branch as the alternative to cwd. Plan execution stays on the
  // directory path (the plan already knows its repo's checkout), so the toggle
  // hides in planMode and 'dir' is always the starting mode.
  const [targetMode, setTargetMode] = useState<'dir' | 'repo'>('dir'); // 'dir' | 'repo'
  const [repo, setRepo] = useState('');
  const [branch, setBranch] = useState('');
  const [branchMode, setBranchMode] = useState<'worktree' | 'in-place'>('worktree'); // 'worktree' | 'in-place'
  // v2.4 — host for `org/repo` shorthand: parseRepoInput hardcodes github.com,
  // but this user is on github AND gitlab, so shorthand needs an explicit pick.
  const [repoHost, setRepoHost] = useState<'github' | 'gitlab'>('github'); // 'github' | 'gitlab'
  // v2.5 — clone transport for `org/repo` shorthand: ssh (scp-style git@…) or
  // https. Seeded ONCE, at mount, from the daemon's RESOLVED setting (ssh is the
  // durable default — this fleet's forge auth is SSH-only).
  // DOCTRINE: this is deliberately NOT live-adopted from later snapshot frames
  // the way reposDir is. A transport pill the user is looking at must never flip
  // under them — not when an unrelated frame arrives, and least of all when
  // their own accepted spawn just PERSISTED the very setting this seeds from
  // (D2). The daemon owns the durable default; this box owns the current pick
  // until the form closes. baseBody() sends it only for shorthand; the daemon
  // remembers an EXPLICIT pick on the accepted spawn, so there is no save here.
  const [repoTransport, setRepoTransport] = useState<'ssh' | 'https'>(
    () => settings?.repo_transport?.value ?? 'ssh',
  );
  // Bare repo names resolve through this namespace when they are not already in
  // the local catalog. Seeded ONCE like transport (never flips under the human).
  // On Coder the daemon resolves `textemma` unless an env/persisted choice wins.
  const [defaultOrg, setDefaultOrg] = useState(() => settings?.repo_default_org?.value ?? '');
  const [orgNote, setOrgNote] = useState<SaveNote | null>(null);
  const savedOrg = useRef(settings?.repo_default_org?.value ?? '');
  // the repos root: seeded from the daemon's resolved setting, editable here,
  // PERSISTED on commit (blur/Enter) — that is what survives reboots
  const [reposDir, setReposDir] = useState(settings?.repos_dir?.resolved ?? '');
  const [dirNote, setDirNote] = useState<SaveNote | null>(null); // {ok} | {err} after a save
  const savedDir = useRef(settings?.repos_dir?.resolved ?? '');
  // v2.3 — the folder picker; which field a pick fills: null | 'cwd' | 'repo' | 'reposDir'
  const [pickerFor, setPickerFor] = useState<'cwd' | 'repo' | 'reposDir' | null>(null);
  const cwdRef = useRef<HTMLInputElement | null>(null);
  const promptRef = useRef<HTMLTextAreaElement | null>(null);
  const closeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const dialogRef = useRef<HTMLDivElement | null>(null);
  // 2.3 — the 202 watch: the accepted provisioning spawn's session_id, watched
  // in the snapshot stream (sessions is threaded from App) until the card goes
  // live (close as it does) or tombstones (stay open, say why).
  const [watchSid, setWatchSid] = useState<string | null | undefined>(null);
  const watchTimer = useRef<ReturnType<typeof setTimeout> | null>(null); // overall cap — a missed frame must never wedge the modal
  const watchDone = useRef(false); // exactly one of success/failure/timeout resolves the watch
  // M-A2 — trap Tab + restore focus on close; the form parks initial focus
  // itself (cwd, or the plan caret) in the effect below.
  useModal(dialogRef, { initialFocus: false });

  useEffect(() => {
    if (planMode && promptRef.current) {
      // park the caret after "Custom instructions: " — the one editable spot
      const el = promptRef.current;
      el.focus();
      const marker = 'Custom instructions: ';
      const idx = (prefillPrompt || '').indexOf(marker);
      const pos = idx >= 0 ? idx + marker.length : 0;
      try {
        el.setSelectionRange(pos, pos);
      } catch {
        /* older engines */
      }
      el.scrollTop = 0;
    } else {
      cwdRef.current?.focus();
    }
  }, []);
  useEffect(
    () => () => {
      clearTimeout(closeTimer.current ?? undefined);
      clearTimeout(watchTimer.current ?? undefined);
    },
    [],
  );

  // 2.3 option 1 — watch the provisioning card in the live snapshot stream.
  // The old flow closed 1400 ms after the 202 no matter what, so a failed
  // clone surfaced only as a tombstoned card the human had to go find and
  // expand. While the card provisions, its OWN note is mirrored into the form
  // (cloning → preparing… — live narration instead of the frozen "(the card
  // narrates from here)" line that used to excuse closing early). Success
  // (col is neither queued nor offline — the first hook's idle flip, or any
  // lane) closes the form with the card. Failure (ANY offline transition —
  // "spawn failed:" tombstones carry the note + fail_detail inline, an
  // unprefixed one resolves with the bare note rather than narrating a dead
  // card for ten minutes). A missed frame can never wedge the modal: an
  // overall cap (11 min — the daemon's clone cap is 600 s, tunable upward,
  // so this must not race it) resolves to a plain note, and the header ✕
  // stays live the whole time (onClose is untouched — the watch state dies
  // with the form).
  useEffect(() => {
    if (!watchSid) return undefined;
    const sess = (sessions ?? []).find((s) => s.session_id === watchSid);
    if (!sess) return undefined; // not in a frame yet — the cap below bounds the wait
    if (watchDone.current) return undefined;
    if (sess.col === 'offline') {
      // ANY offline transition ends the watch as a failure — a tombstone
      // without the "spawn failed:" prefix (daemon crash mid-clone, watchdog
      // sweep) is still a dead card; narrating it further would pin the modal
      // on a corpse until the cap. The prefixed tombstone carries the
      // distilled git remedy inline.
      watchDone.current = true;
      clearTimeout(watchTimer.current ?? undefined);
      setBusy(false);
      setNote(null);
      const note =
        typeof sess.note === 'string' && sess.note
          ? sess.note
          : 'spawn failed — the card went offline';
      setErr(sess.spawn?.fail_detail ? `${note}\n${sess.spawn.fail_detail}` : note);
      return undefined;
    }
    if (sess.col && sess.col !== 'queued') {
      watchDone.current = true;
      clearTimeout(watchTimer.current ?? undefined);
      setNote(`live — ${sess.callsign || 'new session'}`);
      closeTimer.current = setTimeout(onClose, 1400);
      return undefined;
    }
    // still queued: keep narrating the card's own note
    if (sess.note && sess.note !== 'spawning…') setNote(sess.note);
    return undefined;
  }, [watchSid, sessions]);

  // v2.2 — snapshots keep flowing while the form is open; adopt a repos-root
  // that arrives (or changes under us) ONLY while the box is untouched — a
  // half-typed path must never be replaced by a websocket frame.
  useEffect(() => {
    const v = settings?.repos_dir?.resolved ?? '';
    if (reposDir === savedDir.current && v !== savedDir.current) {
      savedDir.current = v;
      setReposDir(v);
    }
  }, [settings?.repos_dir?.resolved]);

  // distinct places the fleet has been seen working → cwd suggestions
  const suggestions = [
    ...new Set(
      (sessions ?? []).flatMap((s) => [s.worktree, s.cwd]).filter((v): v is string => Boolean(v)),
    ),
  ];

  const repoMode = targetMode === 'repo' && !planMode;
  // recently-used repos (the daemon's durable catalog) → repo suggestions;
  // names and origin URLs both complete, because both are valid input
  const repoSuggestions = [
    ...new Set(
      (repoCatalog ?? [])
        .flatMap((r) => [r.repo_name, r.origin_url])
        .filter((v): v is string => Boolean(v)),
    ),
  ];
  const repoName = repoMode ? repoNameOf(repo) : null;
  // a catalog hit by name or URL means no clone: the daemon will use that root
  const knownRepo =
    repoMode && repo.trim()
      ? (repoCatalog ?? []).find(
          (r) =>
            r.repo_name?.toLowerCase() === repo.trim().toLowerCase() ||
            r.origin_url === repo.trim() ||
            r.root === repo.trim(),
        )
      : null;
  const bareRepo = repoMode && isBareRepo(repo);
  const bareUsingDefault = bareRepo && !knownRepo && !!defaultOrg.trim();
  const shorthandInput = bareUsingDefault ? `${defaultOrg.trim()}/${repo.trim()}` : repo.trim();
  // v2.4 — shorthand-shaped input needs an explicit host toggle (github default).
  // A bare name + default org is the same effective shorthand; a catalog hit is
  // already local, so neither the org nor the host/transport controls apply.
  const shorthand = repoMode && (isShorthandRepo(repo) || bareUsingDefault);
  const showHostToggle = shorthand && !knownRepo;
  // 3+ segments (group/sub/repo) is a gitlab-only shape: github shorthand is
  // exactly org/repo, and the daemon 400s a subgrouped github resolve — so the
  // EFFECTIVE host overrides the pill for that shape.
  const subgrouped = shorthand && shorthandInput.split('/').length > 2;
  const effectiveHost = subgrouped ? 'gitlab' : repoHost;
  const repoOrgErr =
    bareRepo && !knownRepo && !defaultOrg.trim()
      ? 'bare repo names need a default org (or enter owner/repo)'
      : null;
  const branchErr = repoMode && branch.trim() ? branchProblem(branch.trim()) : null;
  const cwdRepo = !repoMode
    ? ((sessions ?? []).find((s) => ((s.worktree ?? '') || s.cwd) === cwd.trim())?.repo_name ??
        '') ||
      (repoCatalog ?? []).find((r) => r.root === cwd.trim())?.repo_name
    : null;
  const selectedRepoName = repoMode ? (knownRepo?.repo_name ?? '') || repoName : cwdRepo;

  useEffect(() => {
    if (setupRepoSeed.current === selectedRepoName) return;
    setupRepoSeed.current = selectedRepoName;
    setSetupCmd(selectedRepoName ? (settings?.repo_setup?.[selectedRepoName] ?? '') : '');
    setSaveSetupDefault(false);
  }, [selectedRepoName, settings?.repo_setup]);

  // the repos-root override persists on commit, not per keystroke — an
  // unfinished path half-typed into the box must never become a setting
  const commitReposDir = async (value = reposDir) => {
    const v = value.trim();
    if (v === savedDir.current) return;
    const res = await saveSettings({ repos_dir: v || null });
    if (res.ok && res.json?.ok) {
      const j = res.json as SettingsSaveJson;
      savedDir.current = j.settings?.repos_dir?.resolved ?? v;
      setReposDir(savedDir.current);
      setDirNote({ ok: v ? 'saved — future clones land here' : 'cleared — back to the default' });
    } else {
      setDirNote({ err: reasonOf(res, `save failed (${res.status})`) });
    }
  };

  const commitDefaultOrg = async (value = defaultOrg) => {
    const v = value.trim();
    if (v === savedOrg.current) return;
    const res = await saveSettings({ repo_default_org: v || null });
    if (res.ok && res.json?.ok) {
      const j = res.json as SettingsSaveJson;
      savedOrg.current = j.settings?.repo_default_org?.value ?? v;
      setDefaultOrg(savedOrg.current || '');
      setOrgNote({ ok: v ? 'saved — bare repo names use this org' : 'cleared' });
    } else {
      setOrgNote({ err: reasonOf(res, `save failed (${res.status})`) });
    }
  };

  // v0.15 — the gateway profile lives in the same durable settings store as the
  // repos root, and (like it) is edited HERE, at the moment it matters, because
  // this board has no settings panel to put it in. The credential is write-only:
  // the daemon serves `token_set`, never the token, so an empty box means
  // "leave whatever is stored alone" and never "clear it".
  const gwProfile: GatewayLike = settings?.gateway ?? {};
  const gwReady = !!gwProfile.ready;
  const saveGateway = async () => {
    const body: GatewaySaveBody = {};
    const url = gwUrl.trim();
    if (url !== (gwProfile.base_url ?? '')) body.gateway_base_url = url || null;
    if (gwToken) body.gateway_token = gwToken;
    if (gwStyle !== (gwProfile.auth_style ?? 'bearer')) body.gateway_auth_style = gwStyle;
    if (gwDiscovery !== (gwProfile.model_discovery !== false))
      body.gateway_model_discovery = gwDiscovery;
    if (gwAlways !== !!gwProfile.default) body.gateway_default = gwAlways;
    if (!Object.keys(body).length) {
      setGwEdit(false);
      return;
    }
    setGwBusy(true);
    const res = await saveSettings(body);
    setGwBusy(false);
    // Cleared on EVERY outcome, not just success: a rejected save (a bad URL
    // beside a good token) would otherwise leave the credential sitting in
    // component state and in the input's value for the life of the form.
    setGwToken('');
    if (res.ok && res.json?.ok) {
      setGwNote({ ok: 'saved' });
      setGwEdit(false);
    } else {
      setGwNote({ err: reasonOf(res, `save failed (${res.status})`) });
    }
  };

  // Remote control and the gateway are mutually exclusive, and not by our
  // choice: Claude Code disables Remote Control whenever ANTHROPIC_BASE_URL
  // points at a non-Anthropic host. The daemon refuses the pair with a 400; the
  // UI disables the losing checkbox so the human never composes a spawn that
  // cannot be accepted. Turning one ON turns the other OFF rather than silently
  // blocking the submit button.
  const gatewayOn = gateway && gwReady;

  // Batch is meaningless for plan execution: there, the prompt IS the plan, and
  // splitting it on newlines would fan a single brief out into dozens of agents.
  // In repo mode it's deferred (one human-chosen branch and N forced per-agent
  // worktrees contradict each other) — the note under the checkbox says so.
  const canBatch = !planMode && !repoMode && !shellOnly;
  const batching = batch && canBatch;
  const tasks = useMemo(() => (batching ? parseBatchTasks(prompt) : []), [batching, prompt]);
  const total = batching ? batchTotal(tasks) : 1;

  // 0.16.0 — one unsupervised concept, two pickers: the "run unsupervised"
  // checkbox AND permission-mode=bypassPermissions now route through the same
  // two-step (the dropdown used to bypass it entirely, and the daemon refuses
  // either without an arm token).
  const bypassPicked = permissionMode === 'bypassPermissions';
  const unsupEffective = unsup || bypassPicked;
  // hazard picked but not armed → refuse to spawn either way
  const blocked = unsupEffective && !armed;

  // Arming mints the server-side capability the spawn body must echo. A batch
  // consumes one token per agent, so a batch fetch mints up front and each
  // leg re-mints as it goes (see submitBatch).
  const armUnsupervisedNow = async (): Promise<string | null> => {
    try {
      const res = await armUnsupervised();
      const aj: ArmJson | null = res.json;
      if (res.ok && aj?.arm_token) {
        setArmToken(aj.arm_token);
        return aj.arm_token;
      }
      setErr(`could not arm unsupervised spawning (${res.status})`);
    } catch {
      setErr('could not arm unsupervised spawning — daemon unreachable');
    }
    setArmToken(null);
    return null;
  };
  // a batch needs at least one task, and each agent needs its own worktree
  const emptyBatch = batching && total === 0;

  /** The POST body shared by every agent in this submit; `prompt` varies. */
  const baseBody = (): SpawnBody => {
    if (shellOnly && !repoMode && !planMode) {
      return { kind: 'shell', cwd: cwd.trim() };
    }
    // BUG-040 (board half): plan_id rides the spawn body so the daemon claims
    // the plan's execution atomically BEFORE launching — spawn-first-mark-
    // after let two boards both launch off one stale snapshot. A claim refusal
    // (409 — already executed/claimed) arrives as an ordinary spawn failure.
    const planBody = planId ? { plan_id: planId } : null;
    // repo mode replaces cwd wholesale: the daemon refuses both together, and
    // branch_mode subsumes the worktree flag (it IS the worktree decision)
    const body: SpawnBody = repoMode
      ? { repo: repo.trim(), branch: branch.trim(), branch_mode: branchMode }
      : { cwd: cwd.trim() };
    // v2.4 — repo_host rides ONLY for `org/repo` shorthand (github|gitlab). Never
    // for a URL, scp-style, absolute path, or bare name: the daemon reads it only
    // there, and absent means github, so back-compat holds for every other input.
    // The EFFECTIVE host, not the pill: subgroups force gitlab (see above).
    if (shorthand) body.repo_host = effectiveHost;
    // Explicit on THIS spawn so clicking Spawn immediately after editing the org
    // cannot race the input's async onBlur settings save. The daemon persists an
    // accepted repo_org as the next default, mirroring repo_transport.
    if (bareUsingDefault) body.repo_org = defaultOrg.trim();
    // v2.5 — repo_transport rides ONLY for shorthand, exactly like repo_host: the
    // daemon reads it only there, absent resolves to the persisted setting, and
    // the accepted spawn remembers an explicit pick (D2) — so there is no
    // separate persistence call from the board.
    if (shorthand) body.repo_transport = repoTransport;
    if (model.trim()) body.model = model.trim();
    if (permissionMode !== 'default') body.permission_mode = permissionMode;
    if (!repoMode && (worktree || batching)) body.worktree = true; // forced for a batch
    // `gatewayOn` wins outright, and this guard is NOT redundant with the
    // disabled checkbox. `remote` is state, not a render: tick remote while the
    // gateway box is disabled (a profile with a URL and no token yet), then save
    // a token in the drawer — `gwReady` flips, the remote box goes disabled and
    // renders unchecked, but `remote` is still true. Without this the form would
    // submit a pair the daemon is guaranteed to 400.
    if (remote && !gatewayOn) body.remote_control = true;
    if (unsupEffective && armed) {
      body.dangerously_skip_permissions = true;
      if (armToken) body.arm_token = armToken;
    }
    // Always explicit, never omitted: silence would defer to gateway_default,
    // and the checkbox the human is looking at is the answer — even when it
    // agrees with the default it was seeded from.
    body.gateway = gatewayOn;
    if (setupCmd !== '') body.setup_cmd = setupCmd;
    if (planBody) Object.assign(body, planBody);
    return body;
  };

  const persistSetupDefault = async (): Promise<boolean> => {
    if (!saveSetupDefault || !selectedRepoName) return true;
    // A per-repository PATCH, not a whole-object replace: the snapshot this
    // board holds can be stale, and spreading it would silently delete another
    // board's concurrent save (BUG-147). The daemon merges server-side.
    const res = await saveSettings({ repo_setup_patch: { [selectedRepoName]: setupCmd } });
    if (res.ok && res.json?.ok) return true;
    setErr(reasonOf(res, `setup default save failed (${res.status})`));
    return false;
  };

  // One agent — the original path, byte for byte, including the plan-mark hook.
  const submitOne = async () => {
    const body = baseBody();
    if (prompt.trim()) body.prompt = prompt.trim();
    const res = await spawnSession(body);
    if (!(res.ok && res.json?.ok)) {
      setErr(reasonOf(res, `spawn failed (${res.status})`));
      setBusy(false);
      return;
    }
    const sj = res.json as SpawnJson;
    // v2.2/2.3 — a 202 means the clone is running DETACHED (up to the daemon's
    // 600 s cap). The old "the card narrates from here" close-at-1400ms flow
    // dropped the human into silence and made a failed clone a card they had
    // to go find. Instead the form now WATCHES the returned session_id in the
    // snapshot stream (the effect above) and stays open until the card goes
    // live or tombstones. A non-202 spawn keeps the original timed close.
    if (sj.provisioning) {
      setNote(`cloning — ${(sj.callsign ?? '') || (sj.session_id ?? '') || 'new session'}`);
      // Escape hatch: a missed snapshot frame must never wedge the modal. The
      // daemon's clone cap is 600 s and user-tunable upward — 660 s here so a
      // legitimately long clone doesn't lose the race to its own timeout.
      // Timing out is NOT a failure — the board-level spawn-failure banner
      // (2.3 option 2) still catches the tombstone after the form closes.
      watchDone.current = false;
      clearTimeout(watchTimer.current ?? undefined);
      watchTimer.current = setTimeout(() => {
        if (watchDone.current) return;
        watchDone.current = true;
        setBusy(false);
        setNote(
          (n) =>
            `${(n ?? '') || 'provisioning'} — still going after 11 min; the card narrates from here`,
        );
        closeTimer.current = setTimeout(onClose, 4000);
      }, 660_000);
      setWatchSid(sj.session_id);
      // busy stays true while the watch owns the form — no re-submit mid-clone
      return;
    }
    setNote(`spawning — ${(sj.callsign ?? '') || (sj.session_id ?? '') || 'new session'}`);
    // plan execution (BUG-040): with plan_id on the body the daemon claimed
    // and recorded the execution server-side — the board marks NOTHING after
    // the fact. onSpawned stays as a legacy-notification hook (a daemon that
    // predates the claim endpoint and claims nothing still leaves the plan
    // executable for a retry; the mark endpoint keeps its transition matrix).
    let extra: SpawnResult | null = null;
    if (onSpawned) {
      try {
        extra = await onSpawned(res.json);
      } catch {
        extra = { ok: false, text: 'plan mark failed — daemon unreachable' };
      }
    }
    if (extra && !extra.ok) {
      setErr(extra.text);
    } else {
      if (extra?.text) {
        const extraText = extra.text;
        setNote((n) => `${n ?? ''} · ${extraText}`);
      }
      closeTimer.current = setTimeout(onClose, 1400);
    }
    // busy stays true: the form is closing (or the error owns it), no re-submit
  };

  // N agents, one at a time. Sequential on purpose: each spawn shells out to
  // `git worktree add`, and a failure part-way through must leave the agents
  // that DID launch alone and say plainly which ones didn't.
  const submitBatch = async () => {
    const prompts = expandBatchTasks(tasks);
    const launched: string[] = [];
    const failed: BatchFailure[] = [];
    setProgress({ done: 0, total: prompts.length, failed });
    for (const [i, p] of prompts.entries()) {
      let res: ApiResult | null;
      try {
        res = await spawnSession({ ...baseBody(), prompt: p });
      } catch {
        res = null;
      }
      // Arm tokens are single-use: after the first leg of an armed batch, mint
      // the next one so every agent carries a fresh capability.
      if (unsupEffective && armed && i + 1 < prompts.length) {
        const next = await armUnsupervisedNow();
        if (!next) {
          failed.push(
            ...prompts.slice(i + 1).map((pr) => ({ prompt: pr, reason: 'arm token refused' })),
          );
          break;
        }
      }
      if (res?.ok && res.json?.ok) {
        const sb = res.json as SpawnJson;
        launched.push((sb.callsign ?? '') || (sb.session_id ?? ''));
      } else {
        failed.push({
          prompt: p,
          reason: res ? reasonOf(res, `spawn failed (${res.status})`) : 'daemon unreachable',
        });
      }
      setProgress({ done: i + 1, total: prompts.length, failed: [...failed] });
    }
    if (failed.length) {
      // Partial success, handled honestly. The agents that came up are up and
      // are NOT re-spawned by a retry: the task list is rewritten to hold only
      // the ones that failed, so the button now means "try these again" and the
      // preview above shows exactly that. Never close over a failure.
      setPrompt(failed.map((f) => f.prompt).join('\n'));
      setProgress(null);
      setErr(
        launched.length
          ? `spawned ${launched.length} of ${prompts.length} (${launched.join(', ')}) — the ${failed.length} left above failed: ${failed[0]?.reason ?? ''}`
          : `none of the ${prompts.length} spawned: ${failed[0]?.reason ?? ''}`,
      );
      setBusy(false);
      return;
    }
    setNote(`spawning ${launched.length} — ${launched.join(', ')}`);
    closeTimer.current = setTimeout(onClose, 1800);
  };

  // repo mode swaps the required fields: repo + a well-formed branch
  const targetReady = repoMode
    ? !!(repo.trim() && branch.trim() && !branchErr && !repoOrgErr)
    : !!cwd.trim();

  // Why the button is inert, said next to the button: the same gates
  // targetReady (and the other disables) use, one source of truth — the field
  // errors already render inline at their inputs; this repeats whichever one
  // is actually blocking, or names the empty required field.
  const blockReason = busy
    ? null // the busy label owns the footer instead
    : note
      ? null // the success note owns it
      : blocked
        ? null // the arm-the-confirm note owns it
        : emptyBatch
          ? null // the batch note owns it
          : repoMode
            ? !repo.trim()
              ? 'enter a repo'
              : !branch.trim()
                ? 'enter a branch'
                : (repoOrgErr ?? (branchErr ? `branch: ${branchErr}` : null))
            : !cwd.trim()
              ? 'enter a working directory'
              : null;

  const submit = async () => {
    if (!targetReady || busy || note || blocked || emptyBatch) return;
    setBusy(true);
    setErr(null);
    try {
      if (!(await persistSetupDefault())) {
        setBusy(false);
        return;
      }
      if (batching) await submitBatch();
      else await submitOne();
    } catch {
      setErr('daemon unreachable');
      setBusy(false);
    }
  };

  const onCtrlEnter = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) void submit();
  };

  // folder picker → the field that opened it
  const handlePick = (absPath: string) => {
    if (pickerFor === 'cwd') {
      setCwd(absPath);
      if (err) setErr(null);
    } else if (pickerFor === 'repo') {
      setRepo(absPath);
      setDirNote(null);
      if (err) setErr(null);
    } else if (pickerFor === 'reposDir') {
      setReposDir(absPath);
      void commitReposDir(absPath);
    }
    setPickerFor(null);
  };

  return (
    <div className="fd-composewrap" onClick={onClose}>
      <div
        className="fd-compose fd-spawn"
        role="dialog"
        aria-modal="true"
        aria-label="Spawn a session"
        ref={dialogRef}
        onClick={(e) => {
          e.stopPropagation();
        }}
      >
        <div style={{ display: 'flex', alignItems: 'center' }}>
          <span className="lbl">{planMode ? 'SPAWN SESSION — EXECUTE PLAN' : 'SPAWN SESSION'}</span>
          <span className="fd-spacer" />
          <button type="button" className="fd-x" aria-label="Close" onClick={onClose}>
            ✕
          </button>
        </div>
        <div className="fd-form">
          {/* v2.2 — where the agent works: an existing directory, or a repo the
              daemon materializes (clone if missing, branch created if new) */}
          {!planMode && (
            <div className="frow">
              <span className="fl">target</span>
              <div className="fd-fsmodes" role="radiogroup" aria-label="Spawn target">
                <button
                  type="button"
                  className={`fd-target${!repoMode ? ' on' : ''}`}
                  onClick={() => {
                    setTargetMode('dir');
                    if (err) setErr(null);
                  }}
                >
                  directory
                </button>
                <button
                  type="button"
                  className={`fd-target${repoMode ? ' on' : ''}`}
                  onClick={() => {
                    setTargetMode('repo');
                    setBatch(false);
                    setShellOnly(false);
                    if (err) setErr(null);
                  }}
                >
                  repo + branch
                </button>
              </div>
            </div>
          )}
          {!repoMode && (
            <div className="frow">
              <span className="fl">cwd *</span>
              <input
                ref={cwdRef}
                className="fd-input"
                list="fd-cwd-suggest"
                placeholder="/path/to/repo"
                value={cwd}
                onChange={(e) => {
                  setCwd(e.target.value);
                  if (err) setErr(null);
                }}
                onKeyDown={onCtrlEnter}
              />
              <button
                type="button"
                className="fd-browsebtn"
                title="browse folders"
                onClick={() => {
                  setPickerFor('cwd');
                }}
              >
                🗀
              </button>
              <datalist id="fd-cwd-suggest">
                {suggestions.map((p) => (
                  <option key={p} value={p} />
                ))}
              </datalist>
            </div>
          )}
          {!repoMode && !planMode && (
            <div className="frow">
              <span className="fl">session</span>
              <label className="fd-check">
                <input
                  type="checkbox"
                  checked={shellOnly}
                  onChange={(e) => {
                    const on = e.target.checked;
                    setShellOnly(on);
                    if (on) {
                      setBatch(false);
                      setWorktree(false);
                      setRemote(false);
                      setGateway(false);
                      setUnsup(false);
                      setArmed(false);
                      setArmToken(null);
                      setPermissionMode('default');
                      setGwEdit(false);
                    }
                    if (err) setErr(null);
                  }}
                />
                shell only (no claude)
              </label>
            </div>
          )}
          {repoMode && (
            <>
              <div className="frow">
                <span className="fl">repo *</span>
                <input
                  className="fd-input"
                  list="fd-repo-suggest"
                  placeholder="repo (uses default org) · org/repo · https://… · git@…"
                  value={repo}
                  onChange={(e) => {
                    setRepo(e.target.value);
                    setDirNote(null);
                    if (err) setErr(null);
                  }}
                  onKeyDown={onCtrlEnter}
                />
                <button
                  type="button"
                  className="fd-browsebtn"
                  title="browse for a local repo folder"
                  onClick={() => {
                    setPickerFor('repo');
                  }}
                >
                  🗀
                </button>
                <datalist id="fd-repo-suggest">
                  {repoSuggestions.map((p) => (
                    <option key={p} value={p} />
                  ))}
                </datalist>
              </div>
              {(!repo.trim() || bareRepo) && !knownRepo && (
                <div className="frow top">
                  <span className="fl">default org</span>
                  <div className="fd-setupbox">
                    <input
                      className="fd-input"
                      placeholder="owner or group/subgroup"
                      value={defaultOrg}
                      onChange={(e) => {
                        setDefaultOrg(e.target.value);
                        setOrgNote(null);
                        if (err) setErr(null);
                      }}
                      onBlur={() => void commitDefaultOrg()}
                      onKeyDown={(e) => {
                        if (e.key === 'Enter') {
                          e.preventDefault();
                          void commitDefaultOrg();
                        }
                      }}
                    />
                    <span className="hint">
                      {orgNote?.ok
                        ? `✓ ${orgNote.ok}`
                        : orgNote?.err
                          ? `✗ ${orgNote.err}`
                          : bareUsingDefault
                            ? `${repo.trim()} resolves to ${defaultOrg.trim()}/${repo.trim()} (${(settings?.repo_default_org?.source ?? '') || 'default'})`
                            : settings?.repo_default_org?.source === 'coder'
                              ? 'Coder default: textemma — edit to override'
                              : 'used only for a bare repo name; owner/repo stays explicit'}
                    </span>
                  </div>
                </div>
              )}
              {repoOrgErr && (
                <div className="frow">
                  <span className="fl" />
                  <span className="fd-spawnerr">✗ {repoOrgErr}</span>
                </div>
              )}
              {/* v2.4 — shorthand is host-ambiguous; make the human pick. Reuses
                  the target-toggle look (fd-fsmodes / fd-target). github default.
                  Selection renders from the EFFECTIVE host: with 3+ segments the
                  github pill is disabled (subgroups are gitlab-only) and gitlab
                  shows selected, whatever the pill state underneath says. */}
              {showHostToggle && (
                <div className="frow">
                  <span className="fl">host</span>
                  <div className="fd-fsmodes" role="radiogroup" aria-label="Shorthand host">
                    <button
                      type="button"
                      className={`fd-target${effectiveHost === 'github' ? ' on' : ''}`}
                      disabled={subgrouped}
                      title={
                        subgrouped
                          ? 'github shorthand is org/repo — subgroups need gitlab or a full URL'
                          : undefined
                      }
                      onClick={() => {
                        setRepoHost('github');
                        if (err) setErr(null);
                      }}
                    >
                      github
                    </button>
                    <button
                      type="button"
                      className={`fd-target${effectiveHost === 'gitlab' ? ' on' : ''}`}
                      onClick={() => {
                        setRepoHost('gitlab');
                        if (err) setErr(null);
                      }}
                    >
                      gitlab
                    </button>
                  </div>
                </div>
              )}
              {/* v2.5 — clone transport for the shorthand: ssh default (this
                  fleet's forge auth is SSH), https selectable. SAME visibility as
                  the host toggle above (showHostToggle) and the same pill look
                  (fd-fsmodes / fd-target). No save on click — the accepted spawn
                  persists the pick daemon-side (D2); this is a live preview knob. */}
              {showHostToggle && (
                <div className="frow">
                  <span className="fl">transport</span>
                  <div className="fd-fsmodes" role="radiogroup" aria-label="Clone transport">
                    <button
                      type="button"
                      className={`fd-target${repoTransport === 'ssh' ? ' on' : ''}`}
                      onClick={() => {
                        setRepoTransport('ssh');
                        if (err) setErr(null);
                      }}
                    >
                      ssh · git@…
                    </button>
                    <button
                      type="button"
                      className={`fd-target${repoTransport === 'https' ? ' on' : ''}`}
                      onClick={() => {
                        setRepoTransport('https');
                        if (err) setErr(null);
                      }}
                    >
                      https
                    </button>
                  </div>
                </div>
              )}
              <div className="frow">
                <span className="fl">branch *</span>
                <input
                  className="fd-input"
                  placeholder="existing branch, or a new one to create"
                  value={branch}
                  onChange={(e) => {
                    setBranch(e.target.value);
                    if (err) setErr(null);
                  }}
                  onKeyDown={onCtrlEnter}
                />
              </div>
              {branchErr && (
                <div className="frow">
                  <span className="fl" />
                  <span className="fd-spawnerr">✗ {branchErr}</span>
                </div>
              )}
              <div className="frow top">
                <span className="fl">branch mode</span>
                <div className="fd-branchmode">
                  <label className="fd-check">
                    <input
                      type="radio"
                      name="fd-branch-mode"
                      checked={branchMode === 'worktree'}
                      onChange={() => {
                        setBranchMode('worktree');
                      }}
                    />
                    own worktree — the repo&apos;s main checkout is never touched
                  </label>
                  <label className="fd-check">
                    <input
                      type="radio"
                      name="fd-branch-mode"
                      checked={branchMode === 'in-place'}
                      onChange={() => {
                        setBranchMode('in-place');
                      }}
                    />
                    in place — <code>git switch</code> in the checkout itself (refused if dirty)
                  </label>
                </div>
              </div>
              {repoName && (
                <div className="frow top">
                  <span className="fl">destination</span>
                  <div className="fd-destbox">
                    {knownRepo ? (
                      <span className="known">
                        already local · <span className="mono">{knownRepo.root}</span> — no clone
                        needed
                      </span>
                    ) : (
                      <>
                        <div className="row">
                          <input
                            className="fd-input"
                            placeholder={
                              (settings?.repos_dir?.resolved ?? '') ||
                              'repos root (e.g. ~/projects)'
                            }
                            value={reposDir}
                            onChange={(e) => {
                              setReposDir(e.target.value);
                              setDirNote(null);
                            }}
                            onBlur={() => void commitReposDir()}
                            onKeyDown={(e) => {
                              if (e.key === 'Enter') void commitReposDir();
                            }}
                          />
                          <button
                            type="button"
                            className="fd-browsebtn"
                            title="browse folders"
                            onClick={() => {
                              setPickerFor('reposDir');
                            }}
                          >
                            🗀
                          </button>
                          <span className="sep">/</span>
                          <span className="mono nm">{repoName}</span>
                        </div>
                        <span className="hint">
                          {dirNote?.ok
                            ? `✓ ${dirNote.ok}`
                            : dirNote?.err
                              ? `✗ ${dirNote.err}`
                              : shorthand
                                ? // v2.4 — spell out the exact origin the host pick resolves to, so
                                  // the human sees precisely what gets cloned before they click.
                                  // The EFFECTIVE host: subgroups preview gitlab, never a github
                                  // origin the daemon would refuse. The transport pick steers the
                                  // spelling (ssh git@… vs https://…), same as the daemon will.
                                  `not on this machine yet — cloned from ${shorthandOrigin(shorthandInput, effectiveHost, repoTransport)} on spawn; the root is remembered across restarts`
                                : 'not on this machine yet — cloned here on spawn; the root is remembered across restarts'}
                        </span>
                      </>
                    )}
                  </div>
                </div>
              )}
            </>
          )}
          <div className="frow top">
            <span className="fl">{batching ? 'tasks' : 'prompt'}</span>
            <textarea
              ref={promptRef}
              className="fd-input"
              rows={planMode ? 12 : batching ? 6 : 3}
              placeholder={
                batching
                  ? 'One agent per line:\n  fix the flaky worktree test\n  3x audit the spawn path'
                  : 'Initial prompt (optional)'
              }
              value={prompt}
              disabled={shellOnly}
              onChange={(e) => {
                setPrompt(e.target.value);
                if (err) setErr(null);
              }}
              onKeyDown={onCtrlEnter}
            />
          </div>
          {canBatch && (
            <div className="frow">
              <span className="fl">batch</span>
              <label className="fd-check">
                <input
                  type="checkbox"
                  checked={batch}
                  onChange={(e) => {
                    setBatch(e.target.checked);
                    if (err) setErr(null);
                  }}
                />
                one agent per line — prefix <code>3x</code> to repeat a line
              </label>
            </div>
          )}
          {batching && total > 0 && (
            <div className="frow top">
              <span className="fl" />
              <div className="fd-batchpreview">
                <div className="hd">
                  {total} agent{total === 1 ? '' : 's'}, each in its own worktree of{' '}
                  <b>{cwd.trim() || 'cwd'}</b>
                </div>
                <ol>
                  {tasks.map((t, i) => (
                    <li key={i}>
                      {t.count > 1 && <span className="mult">{t.count}×</span>}
                      {t.prompt}
                    </li>
                  ))}
                </ol>
              </div>
            </div>
          )}
          <div className="frow">
            <span className="fl">model</span>
            <input
              className="fd-input"
              placeholder="default"
              value={model}
              disabled={shellOnly}
              onChange={(e) => {
                setModel(e.target.value);
                if (err) setErr(null);
              }}
              onKeyDown={onCtrlEnter}
            />
          </div>
          <div className="frow">
            <span className="fl">permission-mode</span>
            <select
              className="fd-input"
              value={permissionMode}
              disabled={shellOnly}
              onChange={(e) => {
                setPermissionMode(e.target.value);
                // bypassPermissions is the same hazard as "run unsupervised" —
                // picking it reveals the same arm row below (0.16.0: the daemon
                // refuses it without the arm token either way).
                if (e.target.value !== 'bypassPermissions') setArmed(false);
                if (err) setErr(null);
              }}
            >
              <option value="default">default</option>
              <option value="acceptEdits">acceptEdits</option>
              <option value="plan">plan</option>
              <option value="bypassPermissions">bypassPermissions ⚠</option>
            </select>
          </div>
          {permissionMode === 'plan' && (
            <div className="frow">
              <span className="fl" />
              <span className="fd-spawnhint">
                plan mode: the agent proposes a plan first — approve it from its NEEDS YOU card and
                it lands in the PLANS library
              </span>
            </div>
          )}
          {!repoMode && (
            <div className="frow">
              <span className="fl">worktree</span>
              <label className="fd-check">
                <input
                  type="checkbox"
                  checked={worktree || batching}
                  disabled={batching || shellOnly}
                  onChange={(e) => {
                    setWorktree(e.target.checked);
                  }}
                />
                {batching
                  ? 'each agent gets its own worktree — required for a batch'
                  : 'work in a fresh git worktree'}
              </label>
            </div>
          )}
          <div className="frow top">
            <span className="fl">setup command</span>
            <div className="fd-setupbox">
              <textarea
                className="fd-input"
                rows={3}
                placeholder="e.g. super code · python -m venv .venv"
                value={setupCmd}
                disabled={shellOnly}
                onChange={(e) => {
                  setSetupCmd(e.target.value);
                  if (err) setErr(null);
                }}
                onKeyDown={onCtrlEnter}
              />
              <span className="hint">
                runs visibly through POSIX sh; bashisms need <code>bash -c &apos;…&apos;</code>
              </span>
              {setupCmd !== '' && !shellOnly && (
                <span className="fd-setupcallout">
                  will run: <code>{setupCmd}</code>
                  {batching ? ' · every batch spawn inherits this command' : ''}
                </span>
              )}
              {setupCmd !== '' && !shellOnly && selectedRepoName && (
                <label className="fd-check">
                  <input
                    type="checkbox"
                    checked={saveSetupDefault}
                    onChange={(e) => {
                      setSaveSetupDefault(e.target.checked);
                    }}
                  />
                  save as default for {selectedRepoName}
                </label>
              )}
            </div>
          </div>
          {/* v0.15 — LLM gateway: route this session's API traffic through a
              proxy (CLIProxyAPI, a corporate gateway) instead of Anthropic. */}
          <div className="frow">
            <span className="fl">gateway</span>
            <div className="fd-gwbox">
              <label className={`fd-check${gwReady ? '' : ' disabled'}`}>
                <input
                  type="checkbox"
                  checked={gatewayOn}
                  disabled={!gwReady || shellOnly}
                  onChange={(e) => {
                    setGateway(e.target.checked);
                    // Claude Code turns Remote Control off on a non-Anthropic
                    // base URL, so choosing the gateway un-chooses the 📱 link
                    // rather than letting the daemon 400 the pair.
                    if (e.target.checked) setRemote(false);
                  }}
                />
                🛰 route through {(gwProfile.base_url ?? '') || 'a gateway'}
              </label>
              <button
                type="button"
                className="fd-gwedit"
                disabled={shellOnly}
                onClick={() => {
                  setGwEdit((v) => !v);
                  setGwNote(null);
                }}
              >
                {gwReady ? 'edit' : 'set up'}
              </button>
            </div>
          </div>
          {gwEdit && (
            <div className="frow top">
              <span className="fl" />
              <div className="fd-gwconfig">
                <input
                  className="fd-input"
                  placeholder="http://127.0.0.1:8317"
                  value={gwUrl}
                  onChange={(e) => {
                    setGwUrl(e.target.value);
                    setGwNote(null);
                  }}
                />
                <input
                  className="fd-input"
                  type="password"
                  autoComplete="off"
                  placeholder={
                    gwProfile.token_set ? 'token stored — type to replace' : 'gateway token'
                  }
                  value={gwToken}
                  onChange={(e) => {
                    setGwToken(e.target.value);
                    setGwNote(null);
                  }}
                />
                <div className="fd-fsmodes">
                  {['bearer', 'api-key'].map((style) => (
                    <button
                      key={style}
                      type="button"
                      className={`fd-target${gwStyle === style ? ' on' : ''}`}
                      onClick={() => {
                        setGwStyle(style);
                        setGwNote(null);
                      }}
                    >
                      {style === 'bearer' ? 'bearer · Authorization' : 'api-key · x-api-key'}
                    </button>
                  ))}
                </div>
                <label className="fd-check">
                  <input
                    type="checkbox"
                    checked={gwDiscovery}
                    onChange={(e) => {
                      setGwDiscovery(e.target.checked);
                      setGwNote(null);
                    }}
                  />
                  ask the gateway for its model list
                </label>
                <label className="fd-check">
                  <input
                    type="checkbox"
                    checked={gwAlways}
                    onChange={(e) => {
                      setGwAlways(e.target.checked);
                      setGwNote(null);
                    }}
                  />
                  route every new session through it by default
                </label>
                <div className="row">
                  <button
                    type="button"
                    className="fd-gwsave"
                    disabled={gwBusy}
                    onClick={() => void saveGateway()}
                  >
                    {gwBusy ? 'saving…' : 'save'}
                  </button>
                  <span className="hint">
                    {gwNote?.ok
                      ? `✓ ${gwNote.ok}`
                      : gwNote?.err
                        ? `✗ ${gwNote.err}`
                        : 'stored on this machine; the token is never sent back to the board'}
                  </span>
                </div>
              </div>
            </div>
          )}
          {/* v1.6 — remote control from birth (/rc) */}
          <div className="frow">
            <span className="fl">remote control</span>
            <label className={`fd-check${gatewayOn ? ' disabled' : ''}`}>
              <input
                type="checkbox"
                checked={remote && !gatewayOn}
                disabled={gatewayOn || shellOnly}
                onChange={(e) => {
                  setRemote(e.target.checked);
                }}
              />
              📱 drive it from claude.ai (web / phone)
            </label>
          </div>
          {gatewayOn && (
            <div className="frow">
              <span className="fl" />
              <span className="hint">
                Claude Code disables remote control when the base URL isn’t Anthropic’s
              </span>
            </div>
          )}
          {/* v1.3 — unsupervised (two-step: reveal, then arm). 0.16.0: the
              bypassPermissions dropdown lands here too, and arming mints the
              one-time server token the spawn body must echo. */}
          <div className="frow">
            <span className="fl">unsupervised</span>
            <label className="fd-check hazard">
              <input
                type="checkbox"
                checked={unsupEffective}
                disabled={shellOnly}
                onChange={(e) => {
                  setUnsup(e.target.checked);
                  if (!e.target.checked) {
                    setArmed(false);
                    setArmToken(null);
                    if (bypassPicked) setPermissionMode('default');
                  }
                  if (err) setErr(null);
                }}
              />
              run unsupervised
            </label>
          </div>
          {unsupEffective && (
            <div className="frow top">
              <span className="fl" />
              <div className="fd-hazardconfirm">
                <div className="warn">⚠ this session will never ask permission for anything</div>
                <div className="sub">no permission cards will ever reach this board for it</div>
                <label className="fd-check hazard">
                  <input
                    type="checkbox"
                    checked={armed}
                    onChange={(e) => {
                      const on = e.target.checked;
                      void (async () => {
                        if (on) {
                          const token = await armUnsupervisedNow();
                          setArmed(!!token);
                        } else {
                          setArmed(false);
                          setArmToken(null);
                        }
                        if (err) setErr(null);
                      })();
                    }}
                  />
                  I understand — arm it
                </label>
              </div>
            </div>
          )}
        </div>
        {err && <div className="fd-spawnerr">✗ {err}</div>}
        <div className="foot">
          {busy && progress && !note ? (
            <span className="note">
              spawning {progress.done} of {progress.total}…
            </span>
          ) : busy && !note ? (
            <span className="note">
              spawning — the request can take a while (a fresh clone holds it open up to 2 min)…
            </span>
          ) : note ? (
            <span className="note" style={{ color: 'var(--ok)' }}>
              {note}
            </span>
          ) : blocked ? (
            <span className="note" style={{ color: 'var(--hazard)' }}>
              arm the unsupervised confirm — or uncheck it
            </span>
          ) : emptyBatch ? (
            <span className="note">write at least one task, one per line</span>
          ) : blockReason ? (
            <span className="note" style={{ color: 'var(--hazard)' }}>
              ✗ {blockReason}
            </span>
          ) : (
            <span className="note">
              {shellOnly
                ? 'opens a shell-only terminal'
                : `starts ${total > 1 ? `${total} new billed Claude sessions` : 'a new billed Claude session'}`}
              {repoMode && !knownRepo && repoName ? ' — clones the repo first' : ''}
            </span>
          )}
          <span className="fd-spacer" />
          <button
            type="button"
            className="send"
            onClick={() => void submit()}
            disabled={!targetReady || busy || blocked || emptyBatch}
          >
            {busy
              ? 'Spawning…'
              : shellOnly
                ? 'Open shell ⏎'
                : total > 1
                  ? `Spawn ${total} ⏎`
                  : 'Spawn ⏎'}
          </button>
        </div>
      </div>
      {pickerFor && (
        <DirPicker
          root={(settings?.browse_root?.resolved ?? '') || homeDir || '~'}
          favs={settings?.fav_dirs ?? []}
          onPick={handlePick}
          onClose={() => {
            setPickerFor(null);
          }}
        />
      )}
    </div>
  );
}
