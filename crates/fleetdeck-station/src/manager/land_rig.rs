//! The rig of the land tests (FG-8): a bare remote, a home, the project
//! `p1`, sessions with their own branches, and the station store and policy.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use fleetdeck_core::ChildEnv;
use fleetdeck_core::config::StationConfig;
use fleetdeck_core::paths::Env;
use fleetdeck_proto::command::{
    CommandEnvelope, CommandOrigin, RETRY_WINDOW_MS, ReceiptState, SessionCommand,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::fleet_git::{
    CleanupMode, GitSnapshot, HomeCheck, HomeCheckState, LandMode, LandPhase, LandPlan, LandState,
    LocalRemotes, PushTarget, RemoteIdentity, StationHome, remote_identity,
};
use fleetdeck_proto::mutation::CommandAuthority;
use fleetdeck_proto::{CommandId, Event, EventBody, RejectCode};
use fleetdeck_testkit::Det;
use tokio::sync::{Notify, watch};

use super::{Deps, LandEngine, Shared, decide};
use crate::coordinator::Bindings;
use crate::integrations::Integrations;
use crate::manager::Policy;
use crate::manager::fleet_git_writer::{FleetGitWriter, LandLocks, LandTimeouts};
use crate::manager::land_run;
use crate::manager::policy_hub::PolicyHub;
use crate::shell::{ShellConfig, Shells};
use crate::store::git_state::tests::{new_git, store, view};
use crate::store::land_runs::LandRunRow;
use crate::store::{Caller, Decided, NewEvent, ReadResult, Store};
use crate::workspace::GitOptions;
use crate::workspace::branches::parse_git_version;

pub(super) struct Rig {
    pub(super) _dir: tempfile::TempDir,
    pub(super) root: PathBuf,
    pub(super) program: PathBuf,
    pub(super) bare: PathBuf,
    pub(super) home: PathBuf,
    pub(super) det: Det,
    pub(super) store: Store,
    pub(super) hub: Arc<PolicyHub>,
    pub(super) identity: PathBuf,
    pub(super) url: String,
    pub(super) remote: RemoteIdentity,
    /// The per-land locks that the writer and the abandonment share.
    pub(super) locks: Arc<LandLocks>,
    /// The integration service of the rig (a fake `gh` goes here).
    pub(super) integrations: std::sync::Mutex<Arc<Integrations>>,
    pub(super) timeouts: std::sync::Mutex<LandTimeouts>,
}

pub(super) fn config(text: &str) -> StationConfig {
    let env = Env {
        home: Some("/home/t".into()),
        ..Env::default()
    };
    StationConfig::parse(text, &env).unwrap()
}

pub(super) fn policy(root: &Path, check_cmd: bool, fleet_control: bool) -> Policy {
    let cfg = config(&format!(
        "allowed_roots = [\"{}\"]\nallow_check_cmd = {check_cmd}\nallow_local_remotes = true\n\
         allow_fleet_control = {fleet_control}\n",
        root.display()
    ));
    Policy::from_config(&cfg).with_setup_envs(BTreeMap::from([(
        "claude".into(),
        ChildEnv::from_process(),
    )]))
}

pub(super) fn ended() -> NewEvent {
    Event::SessionEnded {
        reason: "process_exit".to_owned(),
        exit_code: Some(0),
    }
    .into()
}

impl Rig {
    /// A bare remote with `main`, a home with `origin`, the project `p1`, and
    /// a policy. `None` when Git is older than 2.45.
    pub(super) async fn new(check_cmd: bool) -> Option<Self> {
        let out = Command::new("git").arg("--version").output().unwrap();
        if parse_git_version(&String::from_utf8_lossy(&out.stdout)).is_none_or(|v| v < (2, 45)) {
            eprintln!("skipped: Fleet Git needs Git 2.45 or newer");
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (bare, home) = (root.join("remote.git"), root.join("work"));
        fs::create_dir(&bare).unwrap();
        fs::create_dir(&home).unwrap();
        let det = Det::new();
        let url = format!("file://{}", bare.display());
        let remote = remote_identity(&url, LocalRemotes::Allow).unwrap();
        let hub =
            PolicyHub::new(policy(&root, check_cmd, true)).with_fleet_control(Bindings::default());
        let rig = Self {
            program: root.join("git-wrapper"),
            identity: root.join("identity-on"),
            _dir: dir,
            root,
            bare,
            home,
            store: store(&det).await,
            det,
            hub: Arc::new(hub),
            url,
            remote,
            locks: LandLocks::new(),
            integrations: std::sync::Mutex::new(Arc::new(Integrations::default())),
            timeouts: std::sync::Mutex::new(LandTimeouts::default()),
        };
        fs::create_dir(rig.root.join("fx")).unwrap();
        rig.script(true);
        rig.git(&rig.bare, &["init", "--bare", "-b", "main"]);
        rig.git(&rig.home, &["init", "-b", "main"]);
        rig.commit(&rig.home, "a.txt", "one");
        rig.git(&rig.home, &["remote", "add", "origin", &rig.url]);
        rig.git(&rig.home, &["push", "origin", "main"]);
        let mut project = view("p1");
        project.settings.sync = true;
        project.settings.cleanup = CleanupMode::Merged;
        project.clone_url = rig.url.clone();
        project.remote = rig.remote.clone();
        project.home_path = rig.home.display().to_string();
        let check = HomeCheck {
            project_id: "p1".to_owned(),
            generation: 1,
            state: HomeCheckState::Verified,
            canonical_path: Some(project.home_path.clone()),
            seen: None,
            code: None,
        };
        let snapshot = GitSnapshot {
            revision: 1,
            projects: vec![project.clone()],
            homes: vec![StationHome {
                project_id: "p1".to_owned(),
                generation: 1,
                path: project.home_path.clone(),
                sync: true,
            }],
        };
        rig.store
            .git_apply_snapshot(snapshot, "d".to_owned(), vec![check])
            .await
            .unwrap();
        Some(rig)
    }

    /// The Git program of the station: no user configuration; with or
    /// without a committer identity. It acts on the files of `fx/`: a file
    /// `block_<subcommand>` holds that subcommand until the file is removed
    /// (`entered_<subcommand>` tells that it waits); `pushes` logs each push;
    /// `reject_push` answers a push with a `!` status line and pushes nothing;
    /// `hang_after_push` pushes and then does not end; `fail_push_once`, `fail_push` and `fail_push_main` end a push with exit 128 and
    /// no status line; `fail_ls_remote` ends `ls-remote` the same way;
    /// `bump` holds a count:
    /// that many pushes to `main` first make another commit on `main`
    /// (in the clone `bumper`, a file named in `bump_name`).
    pub(super) fn script(&self, identity: bool) {
        let ident = if identity {
            "export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@example.com \
             GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@example.com"
        } else {
            "unset GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL GIT_COMMITTER_NAME GIT_COMMITTER_EMAIL EMAIL"
        };
        let fx = self.root.join("fx");
        fs::write(
            &self.program,
            format!(
                r#"#!/bin/sh
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
{ident}
FX={fx}
sub=""; skip=0
for a in "$@"; do
  if [ "$skip" = 1 ]; then skip=0; continue; fi
  case "$a" in -c) skip=1;; -*) ;; *) sub="$a"; break;; esac
done
if [ -e "$FX/block_$sub" ]; then
  touch "$FX/entered_$sub"
  while [ -e "$FX/block_$sub" ]; do sleep 0.05; done
fi
if [ "$sub" = ls-remote ] && [ -e "$FX/fail_ls_remote" ]; then
  echo "fatal: unable to access the remote" >&2
  exit 128
fi
if [ "$sub" = push ]; then
  echo "$*" >> "$FX/pushes"
  for last; do :; done
  case "$last" in *:refs/heads/main)
    if [ -e "$FX/fail_push_main" ]; then
      echo "fatal: Could not read from remote repository." >&2
      exit 128
    fi;;
  esac
  if [ -e "$FX/fail_push_once" ]; then
    rm -f "$FX/fail_push_once"
    echo "fatal: Could not read from remote repository." >&2
    exit 128
  fi
  if [ -e "$FX/fail_push" ]; then
    echo "fatal: Could not read from remote repository." >&2
    exit 128
  fi
  if [ -e "$FX/reject_push" ]; then
    printf '!	%s:%s	[rejected] (stale info)
' "${{last%%:*}}" "${{last#*:}}"
    echo "rejected" >&2
    exit 1
  fi
  case "$last" in *:refs/heads/main)
    if [ -s "$FX/bump" ]; then
      n=$(cat "$FX/bump")
      if [ "$n" -gt 0 ]; then
        echo $((n-1)) > "$FX/bump"
        ( cd "$FX/../bumper" && git pull -q --ff-only origin main           && echo "$n" >> "$(cat "$FX/bump_name")" && git add . && git commit -qm bump           && git push -q origin main ) >/dev/null 2>&1
      fi
    fi;;
  esac
  if [ -e "$FX/hang_after_push" ]; then git "$@"; sleep 30; exit 0; fi
fi
exec git "$@"
"#,
                fx = fx.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&self.program, fs::Permissions::from_mode(0o755)).unwrap();
        let _ = &self.identity;
    }

    pub(super) fn fx(&self, name: &str) -> PathBuf {
        self.root.join("fx").join(name)
    }

    /// Sets or removes a flag file of `fx/`.
    pub(super) fn flag(&self, name: &str, on: bool) {
        if on {
            fs::write(self.fx(name), "1").unwrap();
        } else {
            let _ = fs::remove_file(self.fx(name));
        }
    }

    /// The lines that the fake Git or forge wrote to `fx/<name>`.
    pub(super) fn lines(&self, name: &str) -> Vec<String> {
        fs::read_to_string(self.fx(name))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// Makes the next `count` pushes to `main` first add a commit to `main`,
    /// in the file `file`.
    pub(super) fn bump(&self, count: u32, file: &str) {
        let bumper = self.root.join("bumper");
        if !bumper.exists() {
            self.git(&self.root, &["clone", "-q", &self.url, "bumper"]);
        }
        fs::write(self.fx("bump_name"), file).unwrap();
        fs::write(self.fx("bump"), count.to_string()).unwrap();
    }

    /// Waits until the fake Git holds `subcommand` (it is blocked).
    pub(super) async fn entered(&self, subcommand: &str) {
        let name = format!("entered_{subcommand}");
        for _ in 0..600 {
            if self.fx(&name).exists() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("git {subcommand} was not reached");
    }

    /// A fake `gh` for the integration service. `fx/prs` is the answer of the
    /// lookup; `fx/create_mode` is `ok`, `drop`, `bad502`, `r422` or `down` (nothing
    /// is created and the lookup fails from then on); the
    /// calls go to `fx/forge_calls` and each create to `fx/creates`;
    /// `fx/find_fail` makes the lookup fail; `fx/block_forge_get` holds it.
    pub(super) fn fake_forge(&self) {
        let fx = self.root.join("fx");
        let script = fx.join("forge-cli");
        fs::write(
            &script,
            format!(
                r#"#!/bin/sh
FX={fx}
echo "$@" >> "$FX/forge_calls"
case "$*" in
  *"-X GET"*)
    if [ -e "$FX/block_forge_get" ]; then
      touch "$FX/entered_forge_get"
      while [ -e "$FX/block_forge_get" ]; do sleep 0.05; done
    fi
    if [ -e "$FX/find_fail" ]; then echo "gh: unreachable" >&2; exit 1; fi
    printf 'HTTP/2.0 200 OK

'
    if [ -s "$FX/prs" ]; then cat "$FX/prs"; else echo '[]'; fi;;
  *"-X POST"*)
    cat > /dev/null
    echo post >> "$FX/creates"
    mode=$(cat "$FX/create_mode")
    made='[{{"number":5,"html_url":"https://forge.example/pull/5"}}]'
    case "$mode" in
      ok) echo "$made" > "$FX/prs"
          printf 'HTTP/2.0 201 Created

{{"number":5,"html_url":"https://forge.example/pull/5"}}';;
      drop) echo "$made" > "$FX/prs"; echo "gh: connection reset" >&2; exit 1;;
      bad502) echo "$made" > "$FX/prs"
          printf 'HTTP/2.0 502 Bad Gateway

bad gateway'; exit 1;;
      down) touch "$FX/find_fail"; echo "gh: connection refused" >&2; exit 1;;
      r422) printf 'HTTP/2.0 422 Unprocessable

{{"message":"Validation Failed"}}'; exit 1;;
    esac;;
esac
"#,
                fx = fx.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(self.fx("create_mode"), "ok").unwrap();
        self.set_integrations(Integrations::with_programs(
            Vec::new(),
            crate::integrations::exec::Programs {
                gh: script.clone(),
                glab: script,
                curl: "curl".into(),
            },
        ));
    }

    /// The default plan in mode `pr`, with a forge that matches the remote.
    pub(super) fn pr_plan(&self, id: &str) -> LandPlan {
        let mut plan = self.plan(id);
        plan.mode = LandMode::Pr;
        plan.forge = Some(fleetdeck_proto::integrations::ForgeBinding {
            kind: fleetdeck_proto::integrations::ForgeKind::Github,
            host: "localhost".to_owned(),
            repo: self.remote.repo["localhost/".len()..].to_owned(),
        });
        plan
    }

    pub(super) fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    pub(super) fn commit(&self, dir: &Path, file: &str, text: &str) -> String {
        fs::write(dir.join(file), format!("{text}\n")).unwrap();
        self.git(dir, &["add", "."]);
        self.git(dir, &["commit", "-m", text]);
        self.git(dir, &["rev-parse", "HEAD"])
    }

    pub(super) fn options(&self) -> GitOptions {
        GitOptions {
            program: self.program.clone(),
            ..GitOptions::isolated()
        }
    }

    pub(super) fn wt(&self, id: &str) -> PathBuf {
        self.root.join(format!("work--{id}"))
    }

    /// The session `id` in its own worktree on `fd/alpha/<id>`, with one
    /// commit of its own, ended unless `live`.
    pub(super) async fn session(&self, id: &str, live: bool) -> PathBuf {
        self.session_with(id, live, "work.txt").await
    }

    pub(super) async fn session_with(&self, id: &str, live: bool, file: &str) -> PathBuf {
        let wt = self.wt(id);
        let branch = format!("fd/alpha/{id}");
        self.git(
            &self.home,
            &["worktree", "add", "-q", "-b", &branch, wt.to_str().unwrap()],
        );
        self.commit(&wt, file, "session work");
        self.record(id, live, Some(&wt)).await;
        wt
    }

    pub(super) async fn record(&self, id: &str, live: bool, wt: Option<&Path>) {
        let cwd = wt.map_or_else(|| self.home.clone(), Path::to_path_buf);
        let cwd = cwd.display().to_string();
        self.store
            .create_session(crate::store::NewSession {
                session_id: id.into(),
                provider: ProviderKind::Claude,
                provider_instance: "claude".into(),
                cwd: cwd.clone(),
                worktree: None,
                branch: None,
                mode: None,
                title: None,
                offline_approval_policy: crate::store::OfflineApprovalPolicy::Wait,
            })
            .await
            .unwrap();
        let named: NewEvent = Event::SessionNamed {
            callsign: id.to_owned(),
            worktree: Some(cwd),
        }
        .into();
        self.store.append(id.into(), vec![named]).await.unwrap();
        if wt.is_some() {
            let mut git = new_git(id, "p1");
            git.repo_root = self.home.display().to_string();
            git.remote_dest = self.remote.dest.clone();
            let push = Some(("origin".to_owned(), self.remote.dest.clone()));
            self.store.git_record_session(git, push).await.unwrap();
        }
        if !live {
            self.store.append(id.into(), vec![ended()]).await.unwrap();
        }
    }

    pub(super) fn plan(&self, id: &str) -> LandPlan {
        LandPlan {
            project_id: "p1".to_owned(),
            settings_revision: 1,
            home_generation: 1,
            mode: LandMode::Merge,
            fetch_remote: "origin".to_owned(),
            remote: self.remote.clone(),
            default_branch: None,
            branch_ref: format!("fd/alpha/{id}"),
            publish_ref: format!("fd/alpha/{id}"),
            branch_target: PushTarget {
                target: "origin".to_owned(),
                dest: self.remote.dest.clone(),
            },
            forge: None,
            check_cmd: None,
            check_env: BTreeMap::new(),
            pr_title: "title".to_owned(),
            pr_body: "body".to_owned(),
        }
    }

    pub(super) async fn submit(
        &self,
        command_id: &str,
        origin: CommandOrigin,
        authority: Option<CommandAuthority>,
        command: SessionCommand,
    ) -> Decided {
        let envelope = CommandEnvelope {
            authority,
            command_id: CommandId::from(command_id.to_owned()),
            target_incarnation: None,
            retry_until: self.store.now_ms() + RETRY_WINDOW_MS,
            origin,
            command: command.clone(),
        };
        let policy = self.hub.current();
        let hub = Arc::clone(&self.hub);
        let shells = Arc::new(Shells::new(ShellConfig::default()));
        self.store
            .decide(envelope, Caller::local(), move |d| {
                let bindings = hub.fleet_bindings().cloned().unwrap_or_default();
                decide(d, &policy, &bindings, origin, Some(&*shells), command)
            })
            .await
            .unwrap()
    }

    pub(super) fn land_command(session: &str, land_id: &str, plan: LandPlan) -> SessionCommand {
        SessionCommand::Land {
            session_id: session.into(),
            land_id: land_id.to_owned(),
            plan: Box::new(plan),
        }
    }

    /// Admits a land of `session` with its default plan.
    pub(super) async fn admit(&self, session: &str, land_id: &str) -> Decided {
        let plan = self.plan(session);
        self.admit_plan(session, land_id, plan).await
    }

    pub(super) async fn admit_plan(&self, session: &str, land_id: &str, plan: LandPlan) -> Decided {
        let command = Self::land_command(session, land_id, plan);
        let decided = self
            .submit(
                &format!("cmd-{land_id}"),
                CommandOrigin::Board,
                None,
                command,
            )
            .await;
        assert_eq!(
            decided.receipt.state,
            ReceiptState::Accepted,
            "{:?}",
            decided.receipt.reject
        );
        decided
    }

    /// The time limits of the next `shared()` and `deps()`.
    pub(super) fn set_timeouts(&self, timeouts: LandTimeouts) {
        *self.timeouts.lock().unwrap() = timeouts;
    }

    /// The integration service of the next `shared()` and `deps()`.
    pub(super) fn set_integrations(&self, integrations: Integrations) {
        *self.integrations.lock().unwrap() = Arc::new(integrations);
    }

    /// The writer and the shared state of the land tasks.
    pub(super) fn deps(&self) -> Deps {
        let options = self.options();
        let timeouts = *self.timeouts.lock().unwrap();
        let (writer, _task) = FleetGitWriter::start_with(
            self.store.clone(),
            Arc::clone(&self.hub),
            Arc::new(move || options.clone()),
            Arc::clone(&self.locks),
            timeouts,
        );
        let service = Arc::clone(&self.integrations.lock().unwrap());
        Deps {
            writer,
            locks: Arc::clone(&self.locks),
            integrations: Arc::new(move || Arc::clone(&service)),
            timeouts,
        }
    }

    pub(super) fn shared(&self) -> (Shared, watch::Sender<bool>) {
        let options = self.options();
        let (tx, rx) = watch::channel(false);
        let deps = self.deps();
        let shared = Shared {
            store: self.store.clone(),
            policy: Arc::clone(&self.hub),
            options: Arc::new(move || options.clone()),
            shutdown: rx,
            cleanup: Arc::new(Notify::new()),
            writer: deps.writer,
            locks: deps.locks,
            integrations: deps.integrations,
            timeouts: deps.timeouts,
        };
        (shared, tx)
    }

    /// An engine handle for the abandonment and the status.
    pub(super) fn engine(&self) -> LandEngine {
        LandEngine {
            wake: Arc::new(Notify::new()),
            store: self.store.clone(),
            locks: Arc::clone(&self.locks),
            timeouts: *self.timeouts.lock().unwrap(),
        }
    }

    /// Runs steps 1 to 4 of an admitted land by hand.
    pub(super) async fn run(&self, land_id: &str) -> LandRunRow {
        let (shared, _tx) = self.shared();
        let row = self.row(land_id).await;
        let _ = land_run::steps(&shared, row).await;
        self.row(land_id).await
    }

    /// Runs an admitted land by hand: steps 1 to 4, then the publication.
    pub(super) async fn run_all(&self, land_id: &str) -> LandRunRow {
        let (shared, _tx) = self.shared();
        let row = self.row(land_id).await;
        land_run::run(&shared, row).await;
        self.row(land_id).await
    }

    /// Drives a run from its stored state, as a land task or a recovery does.
    pub(super) async fn drive(&self, land_id: &str) -> LandRunRow {
        let (shared, _tx) = self.shared();
        let row = self.row(land_id).await;
        land_run::drive_loop(&shared, row).await;
        self.row(land_id).await
    }

    /// Recovers a run as the engine does at a station start.
    pub(super) async fn reconcile(&self, land_id: &str) -> LandRunRow {
        let (shared, _tx) = self.shared();
        let row = self.row(land_id).await;
        crate::manager::land_reconcile::run(&shared, row).await;
        self.row(land_id).await
    }

    pub(super) async fn row(&self, land_id: &str) -> LandRunRow {
        self.store
            .land_run(land_id.to_owned())
            .await
            .unwrap()
            .unwrap()
    }

    pub(super) async fn phases(&self) -> Vec<(LandState, LandPhase)> {
        let ReadResult::Events { events, .. } = self.store.read_after(0, 1000).await.unwrap()
        else {
            panic!("snapshot required");
        };
        events
            .into_iter()
            .filter_map(|e| match e.event {
                EventBody::Known(Event::SessionLand(info)) => Some((info.state, info.phase)),
                _ => None,
            })
            .collect()
    }

    /// The attempt of each `session.land` event, in order.
    pub(super) async fn attempts(&self) -> Vec<u32> {
        let ReadResult::Events { events, .. } = self.store.read_after(0, 1000).await.unwrap()
        else {
            panic!("snapshot required");
        };
        events
            .into_iter()
            .filter_map(|e| match e.event {
                EventBody::Known(Event::SessionLand(info)) => Some(info.attempt),
                _ => None,
            })
            .collect()
    }

    pub(super) fn refs(&self) -> String {
        self.git(&self.bare, &["for-each-ref"])
    }
}

pub(super) fn rejected(decided: &Decided) -> (RejectCode, String) {
    assert_eq!(decided.receipt.state, ReceiptState::Rejected);
    let reject = decided.receipt.reject.clone().unwrap();
    (reject.code, reject.message)
}

pub(super) async fn refused(
    rig: &Rig,
    session: &str,
    land_id: &str,
    plan: LandPlan,
) -> (RejectCode, String) {
    let command = Rig::land_command(session, land_id, plan);
    let decided = rig
        .submit(
            &format!("refuse-{session}-{land_id}"),
            CommandOrigin::Board,
            None,
            command,
        )
        .await;
    rejected(&decided)
}

/// Records the fence of `land_id` (T3.2c has the real writer).
pub(super) async fn fence(rig: &Rig, land_id: &str) {
    let envelope = CommandEnvelope {
        authority: None,
        command_id: CommandId::from(format!("fence-{land_id}")),
        target_incarnation: None,
        retry_until: rig.store.now_ms() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Local,
        command: SessionCommand::Rename {
            session_id: "s1".into(),
            title: None,
        },
    };
    let (id, now) = (land_id.to_owned(), rig.store.now_ms());
    rig.store
        .decide(envelope, Caller::local(), move |d| {
            crate::store::land_fences::insert(d.connection(), &id, now)?;
            Ok(fleetdeck_proto::command::CommandResult {
                session_id: None,
                input_id: None,
            })
        })
        .await
        .unwrap();
}
