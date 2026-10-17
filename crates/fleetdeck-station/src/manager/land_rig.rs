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

use super::{Shared, decide};
use crate::coordinator::Bindings;
use crate::manager::Policy;
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
        };
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
    /// without a committer identity.
    pub(super) fn script(&self, identity: bool) {
        let ident = if identity {
            "export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@example.com \
             GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@example.com"
        } else {
            "unset GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL GIT_COMMITTER_NAME GIT_COMMITTER_EMAIL EMAIL"
        };
        fs::write(
            &self.program,
            format!(
                "#!/bin/sh\nexport GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1\n{ident}\nexec git \"$@\"\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&self.program, fs::Permissions::from_mode(0o755)).unwrap();
        let _ = &self.identity;
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

    pub(super) fn shared(&self) -> (Shared, watch::Sender<bool>) {
        let options = self.options();
        let (tx, rx) = watch::channel(false);
        let shared = Shared {
            store: self.store.clone(),
            policy: Arc::clone(&self.hub),
            options: Arc::new(move || options.clone()),
            shutdown: rx,
            cleanup: Arc::new(Notify::new()),
        };
        (shared, tx)
    }

    /// Runs steps 1 to 4 of an admitted land by hand.
    pub(super) async fn run(&self, land_id: &str) -> LandRunRow {
        let (shared, _tx) = self.shared();
        let row = self.row(land_id).await;
        land_run::run(&shared, row).await;
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
