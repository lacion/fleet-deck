//! The start of a session in a synced project (FG-1, FG-2, FG-4, design
//! §4.5, §7.5): the decision, the fetch, the branch `fd/<slug>/<callsign>`,
//! the include files and the environment of the setup command.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{
    CommandOrigin, RETRY_WINDOW_MS, ReceiptState, SessionCommand, WorkspaceRequest,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::fleet_git::{
    FleetGitStart, GitSettings, GitSnapshot, HomeCheck, HomeCheckState, LocalRemotes, PushMode,
    PushState, StartKind, remote_identity,
};
use fleetdeck_proto::{CommandEnvelope, CommandId, Event, SessionId, SessionSummary};
use fleetdeck_testkit::FakeAgent;
use fleetdeck_testkit::fake::SCENARIO_ENV;

use crate::manager::Policy;
use crate::manager::policy::SetupEnvs;
use crate::manager::tests::{Station, WAIT, spec, text};
use crate::store::{Caller, Decided, OfflineApprovalPolicy};
use crate::workspace::branches::parse_git_version;

struct Env {
    st: Station,
    next: u32,
    bare: PathBuf,
    home: PathBuf,
    url: String,
    /// The commit that the remote has and the home does not.
    remote_tip: String,
    /// The commit that the home has.
    home_tip: String,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
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

fn commit(repo: &Path, file: &str, text: &str) -> String {
    fs::write(repo.join(file), format!("{text}\n")).unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-m", text]);
    git(repo, &["rev-parse", "HEAD"])
}

impl Env {
    /// A station named "Alpha" (slug `alpha`) with a home whose `origin` is
    /// one commit ahead of its local `main`. `None` when Git is older than
    /// 2.45.
    async fn new(setup_cmd: bool) -> Option<Self> {
        Self::with_program(setup_cmd, None).await
    }

    /// As [`Self::new`]; `program` is the provider program of the station.
    async fn with_program(setup_cmd: bool, program: Option<PathBuf>) -> Option<Self> {
        let out = Command::new("git").arg("--version").output().unwrap();
        if parse_git_version(&String::from_utf8_lossy(&out.stdout)).is_none_or(|v| v < (2, 45)) {
            eprintln!("skipped: Fleet Git needs Git 2.45 or newer");
            return None;
        }
        let st = match program {
            Some(program) => Station::with_program(8, program).await,
            None => Station::new(8).await,
        };
        let root = st.root.path().canonicalize().unwrap();
        let (projects, bare, home) = (
            root.join("projects"),
            root.join("remote.git"),
            root.join("projects/home"),
        );
        fs::create_dir(&bare).unwrap();
        git(&bare, &["init", "--bare", "-b", "main"]);
        let url = format!("file://{}", bare.display());
        fs::create_dir(&home).unwrap();
        git(&home, &["init", "-b", "main"]);
        git(&home, &["remote", "add", "origin", &url]);
        let home_tip = commit(&home, "a.txt", "one");
        git(&home, &["push", "origin", "main"]);
        // Another machine moves the remote.
        let other = root.join("other");
        fs::create_dir(&other).unwrap();
        git(&other, &["clone", &url, "."]);
        let remote_tip = commit(&other, "a.txt", "two");
        git(&other, &["push", "origin", "main"]);
        let mut policy = Policy::new(
            vec![projects],
            8,
            OfflineApprovalPolicy::Wait,
            BTreeMap::from([
                ("claude".into(), ProviderKind::Claude),
                ("codex".into(), ProviderKind::Codex),
            ]),
        )
        .with_name("Alpha")
        .with_allow_setup_cmd(setup_cmd);
        if setup_cmd {
            let mut base = ChildEnv::from_process();
            base.set("FG_CLASH", "instance").unwrap();
            let envs = SetupEnvs::from([("claude".into(), base)]);
            policy = policy.with_setup_envs(envs);
        }
        st.manager.policy_hub().swap(policy);
        Some(Self {
            st,
            next: 0,
            bare,
            home,
            url,
            remote_tip,
            home_tip,
        })
    }

    fn settings(&self) -> GitSettings {
        GitSettings {
            sync: true,
            push: PushMode::OnTurn,
            ..GitSettings::default()
        }
    }

    fn start_of(&self, settings: GitSettings) -> FleetGitStart {
        FleetGitStart {
            kind: StartKind::Worktree,
            project_id: "p1".to_owned(),
            project_name: "app".to_owned(),
            settings,
            settings_revision: 1,
            home_generation: 1,
            home_path: self.home.display().to_string(),
            remote: remote_identity(&self.url, LocalRemotes::Allow).unwrap(),
            slug: "alpha".to_owned(),
            forge: None,
            clone_url: self.url.clone(),
        }
    }

    /// The snapshot of the station: project `p1`, home check `state`.
    async fn snapshot(&self, settings: GitSettings, state: HomeCheckState) {
        let fg = self.start_of(settings.clone());
        let mut view = crate::store::git_state::tests::view("p1");
        view.settings = settings;
        view.remote = fg.remote.clone();
        view.clone_url = fg.clone_url.clone();
        view.home_path = fg.home_path.clone();
        let check = HomeCheck {
            project_id: "p1".to_owned(),
            generation: 1,
            state,
            canonical_path: Some(fg.home_path),
            seen: None,
            code: None,
        };
        let snapshot = GitSnapshot {
            revision: 1,
            projects: vec![view],
            homes: Vec::new(),
        };
        let store = self.st.manager.store();
        store
            .git_apply_snapshot(snapshot, "d".to_owned(), vec![check])
            .await
            .unwrap();
    }

    async fn submit(&mut self, command: SessionCommand) -> Decided {
        self.next += 1;
        let envelope = CommandEnvelope {
            authority: None,
            command_id: CommandId::from(format!("fg-{}", self.next)),
            target_incarnation: None,
            retry_until: self.st.manager.store().now_ms() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Local,
            command,
        };
        self.st
            .manager
            .submit(envelope, Caller::local())
            .await
            .unwrap()
    }

    fn start(
        &self,
        cwd: &Path,
        workspace: Option<WorkspaceRequest>,
        fleet: Option<FleetGitStart>,
        setup_cmd: Option<&str>,
    ) -> SessionCommand {
        SessionCommand::Start {
            spec: spec(&cwd.display().to_string(), "claude"),
            prompt: Some(text("hello")),
            workspace: workspace.map(Box::new),
            setup_cmd: setup_cmd.map(str::to_owned),
            batch: None,
            issue: None,
            origin_text: false,
            fleet_git: fleet.map(Box::new),
        }
    }

    async fn accepted(&mut self, command: SessionCommand) -> SessionId {
        let d = self.submit(command).await;
        assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
        d.receipt.result.unwrap().session_id.unwrap()
    }

    /// The message of a refused start; no session was made.
    async fn refused(&mut self, command: SessionCommand) -> String {
        let d = self.submit(command).await;
        assert_eq!(d.receipt.state, ReceiptState::Rejected, "{:?}", d.receipt);
        let sessions = self.st.manager.store().snapshot().await.unwrap().sessions;
        assert!(sessions.is_empty(), "{sessions:?}");
        d.receipt.reject.unwrap().message
    }

    async fn started(&self, session: &SessionId) -> SessionSummary {
        self.st
            .wait_for("session.started", |e| match e {
                Event::SessionStarted { .. } => Some(()),
                _ => None,
            })
            .await;
        self.st.settled(session).await;
        self.summary(session).await
    }

    async fn summary(&self, session: &SessionId) -> SessionSummary {
        let store = self.st.manager.store();
        store.summary(session.clone()).await.unwrap().unwrap()
    }

    /// Waits until the base of the session is recorded.
    async fn summary_with_base(&self, session: &SessionId) -> SessionSummary {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let summary = self.summary(session).await;
            if summary.base.is_some() {
                return summary;
            }
            assert!(tokio::time::Instant::now() < deadline, "no base");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    fn worktree() -> WorkspaceRequest {
        WorkspaceRequest::Worktree {
            branch: None,
            base: None,
        }
    }
}

#[tokio::test]
async fn a_synced_start_makes_the_slug_branch_from_the_fetched_remote() {
    let Some(mut env) = Env::new(false).await else {
        return;
    };
    env.snapshot(env.settings(), HomeCheckState::Verified).await;
    let fg = env.start_of(env.settings());
    let cmd = env.start(&env.home, Some(Env::worktree()), Some(fg), None);
    let session = env.accepted(cmd).await;
    let summary = env.started(&session).await;
    let branch = summary.branch.clone().unwrap();
    let callsign = summary.callsign.clone().unwrap();
    assert_eq!(branch, format!("fd/alpha/{callsign}"));
    // The session starts from the fetched remote commit, not from the old
    // local `main`.
    let base = summary_base(&env, &session).await;
    assert_eq!(base, env.remote_tip);
    assert_ne!(base, env.home_tip);
    let worktree = PathBuf::from(summary.worktree.unwrap());
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), env.remote_tip);
    assert!(!summary.base_stale);
    let store = env.st.manager.store();
    let row = store.git_session(session.clone()).await.unwrap().unwrap();
    assert_eq!(row.kind, "worktree");
    assert_eq!(row.slug.as_deref(), Some("alpha"));
    assert_eq!(row.branch.as_deref(), Some(branch.as_str()));
    assert_eq!(row.publish_ref.as_deref(), Some(branch.as_str()));
    assert!(row.created_branch && !row.base_stale);
    assert_eq!(row.repo_root, env.home.display().to_string());
    let push = store.git_push(session).await.unwrap().unwrap();
    assert_eq!(push.state, PushState::Idle);
    assert_eq!(push.target, "origin");
    assert_eq!(push.ref_name, branch);
    assert_eq!(push.target_dest, row.remote_dest);
}

async fn summary_base(env: &Env, session: &SessionId) -> String {
    env.summary_with_base(session).await.base.unwrap().oid
}

#[tokio::test]
async fn a_start_without_fleet_git_keeps_the_old_branch_and_makes_no_row() {
    let Some(mut env) = Env::new(false).await else {
        return;
    };
    env.snapshot(env.settings(), HomeCheckState::Verified).await;
    let cmd = env.start(&env.home, Some(Env::worktree()), None, None);
    let session = env.accepted(cmd).await;
    let summary = env.started(&session).await;
    let callsign = summary.callsign.clone().unwrap();
    assert_eq!(summary.branch, Some(format!("fd/{callsign}")));
    // No fetch: the base is the local `HEAD`.
    assert_eq!(summary_base(&env, &session).await, env.home_tip);
    let store = env.st.manager.store();
    assert!(store.git_session(session.clone()).await.unwrap().is_none());
    assert!(store.git_push(session).await.unwrap().is_none());
}

#[tokio::test]
async fn an_owner_branch_wins_and_is_never_pushed() {
    let Some(mut env) = Env::new(false).await else {
        return;
    };
    env.snapshot(env.settings(), HomeCheckState::Verified).await;
    let fg = env.start_of(env.settings());
    let workspace = WorkspaceRequest::Worktree {
        branch: Some("feature/x".to_owned()),
        base: None,
    };
    let cmd = env.start(&env.home, Some(workspace), Some(fg), None);
    let session = env.accepted(cmd).await;
    let summary = env.started(&session).await;
    assert_eq!(summary.branch.as_deref(), Some("feature/x"));
    let store = env.st.manager.store();
    let row = store.git_session(session.clone()).await.unwrap().unwrap();
    assert_eq!(row.kind, "worktree");
    assert_eq!(row.branch.as_deref(), Some("feature/x"));
    assert!((!row.created_branch) && row.publish_ref.is_none());
    assert!(store.git_push(session).await.unwrap().is_none());
}

#[tokio::test]
async fn an_in_place_start_gets_a_row_and_no_branch_or_push() {
    let Some(mut env) = Env::new(false).await else {
        return;
    };
    env.snapshot(env.settings(), HomeCheckState::Verified).await;
    let mut fg = env.start_of(env.settings());
    fg.kind = StartKind::InPlace;
    let cmd = env.start(&env.home, None, Some(fg), None);
    let session = env.accepted(cmd).await;
    env.started(&session).await;
    let store = env.st.manager.store();
    let row = store.git_session(session.clone()).await.unwrap().unwrap();
    assert_eq!(row.kind, "in_place");
    assert!(row.slug.is_none() && row.branch.is_none() && !row.created_branch);
    assert!(store.git_push(session).await.unwrap().is_none());
}

#[tokio::test]
async fn a_start_that_does_not_fit_the_station_is_refused() {
    let Some(mut env) = Env::new(false).await else {
        return;
    };
    env.snapshot(env.settings(), HomeCheckState::Verified).await;
    let outside = env.home.parent().unwrap().join("elsewhere");
    fs::create_dir(&outside).unwrap();
    type Change = Box<dyn Fn(&mut FleetGitStart)>;
    let cases: Vec<(&str, Change)> = vec![
        ("slug_mismatch", Box::new(|f| f.slug = "bravo".to_owned())),
        ("stale_generation", Box::new(|f| f.home_generation = 2)),
        (
            "unsupported_mode",
            Box::new(|f| f.kind = StartKind::Unknown),
        ),
        ("invalid_params", Box::new(|f| f.kind = StartKind::InPlace)),
    ];
    for (code, change) in cases {
        let mut fg = env.start_of(env.settings());
        change(&mut fg);
        let cmd = env.start(&env.home, Some(Env::worktree()), Some(fg), None);
        let message = env.refused(cmd).await;
        assert!(message.starts_with(&format!("{code}: ")), "{message}");
    }
    // A folder that is not the home or inside it.
    let fg = env.start_of(env.settings());
    let message = env
        .refused(env.start(&outside, None, Some(retarget(fg)), None))
        .await;
    assert_eq!(
        message,
        "not_a_home: This folder is not a home of the project."
    );
    // A snapshot with revision 3 of the settings: a start with revision 1 is old.
    let mut newer = env.settings();
    newer.push = PushMode::OnExit;
    let store = env.st.manager.store();
    let mut view = crate::store::git_state::tests::view("p1");
    view.settings = newer;
    view.settings_revision = 3;
    view.home_path = env.home.display().to_string();
    let check = HomeCheck {
        project_id: "p1".to_owned(),
        generation: 1,
        state: HomeCheckState::Verified,
        canonical_path: Some(view.home_path.clone()),
        seen: None,
        code: None,
    };
    let snapshot = GitSnapshot {
        revision: 2,
        projects: vec![view],
        homes: Vec::new(),
    };
    store
        .git_apply_snapshot(snapshot, "e".to_owned(), vec![check])
        .await
        .unwrap();
    let fg = env.start_of(env.settings());
    let message = env
        .refused(env.start(&env.home, Some(Env::worktree()), Some(fg), None))
        .await;
    assert_eq!(
        message,
        "stale_settings: The project settings changed while the start was on its way. Start again."
    );
}

fn retarget(mut fg: FleetGitStart) -> FleetGitStart {
    fg.kind = StartKind::InPlace;
    fg
}

#[tokio::test]
async fn a_home_that_is_not_verified_is_refused_and_so_is_an_unknown_project() {
    let Some(mut env) = Env::new(false).await else {
        return;
    };
    let unverified = "home_unverified: This folder is not a verified home of the project. Check it on the Projects page.";
    // No snapshot yet.
    let fg = env.start_of(env.settings());
    let message = env
        .refused(env.start(&env.home, Some(Env::worktree()), Some(fg), None))
        .await;
    assert_eq!(message, unverified);
    env.snapshot(env.settings(), HomeCheckState::Mismatch).await;
    let fg = env.start_of(env.settings());
    let message = env
        .refused(env.start(&env.home, Some(Env::worktree()), Some(fg), None))
        .await;
    assert_eq!(message, unverified);
}

#[tokio::test]
async fn a_gone_remote_starts_from_head_with_base_stale() {
    let Some(mut env) = Env::new(false).await else {
        return;
    };
    env.snapshot(env.settings(), HomeCheckState::Verified).await;
    fs::remove_dir_all(&env.bare).unwrap();
    let fg = env.start_of(env.settings());
    let cmd = env.start(&env.home, Some(Env::worktree()), Some(fg), None);
    let session = env.accepted(cmd).await;
    env.started(&session).await;
    let code = env
        .st
        .wait_for("workspace.base_stale", |e| match e {
            Event::WorkspaceBaseStale { code, text } => Some((code.clone(), text.clone())),
            _ => None,
        })
        .await;
    assert_eq!(code.0, "fetch_failed");
    assert!(
        code.1
            .starts_with("Fetch failed; this session started from the local HEAD:")
    );
    assert_eq!(summary_base(&env, &session).await, env.home_tip);
    assert!(env.summary(&session).await.base_stale);
    let store = env.st.manager.store();
    assert!(
        store
            .git_session(session)
            .await
            .unwrap()
            .unwrap()
            .base_stale
    );
}

#[tokio::test]
async fn a_blocking_fd_slug_branch_fails_the_start() {
    let Some(mut env) = Env::new(false).await else {
        return;
    };
    env.snapshot(env.settings(), HomeCheckState::Verified).await;
    git(&env.home, &["branch", "fd/alpha"]);
    let fg = env.start_of(env.settings());
    let cmd = env.start(&env.home, Some(Env::worktree()), Some(fg), None);
    let session = env.accepted(cmd).await;
    let message = env
        .st
        .wait_for("start_failed", |e| match e {
            Event::Error { code, message, .. } if code == "start_failed" => Some(message.clone()),
            _ => None,
        })
        .await;
    assert!(message.starts_with("The branch fd/alpha blocks the session branch fd/alpha/"));
    assert!(message.ends_with("git branch -m fd/alpha fd-alpha."));
    env.st.settled(&session).await;
    let worktree = env.summary(&session).await.worktree.unwrap();
    assert!(!Path::new(&worktree).exists());
}

#[tokio::test]
async fn ignored_files_are_copied_and_the_rest_is_listed_as_skipped() {
    let Some(mut env) = Env::new(false).await else {
        return;
    };
    fs::write(env.home.join(".gitignore"), ".env\n").unwrap();
    git(&env.home, &["add", ".gitignore"]);
    git(&env.home, &["commit", "-m", "ignore"]);
    fs::write(env.home.join(".env"), "TOKEN=1\n").unwrap();
    let mut settings = env.settings();
    settings.worktree_include = vec![".env".to_owned(), "a.txt".to_owned()];
    env.snapshot(settings.clone(), HomeCheckState::Verified)
        .await;
    let fg = env.start_of(settings);
    let cmd = env.start(&env.home, Some(Env::worktree()), Some(fg), None);
    let session = env.accepted(cmd).await;
    let summary = env.started(&session).await;
    let worktree = PathBuf::from(summary.worktree.unwrap());
    assert_eq!(
        fs::read_to_string(worktree.join(".env")).unwrap(),
        "TOKEN=1\n"
    );
    let files = env
        .st
        .wait_for("workspace.include_skipped", |e| match e {
            Event::WorkspaceIncludeSkipped { files } => Some(files.clone()),
            _ => None,
        })
        .await;
    assert_eq!(files.len(), 1);
    assert_eq!(
        (files[0].path.as_str(), files[0].reason.as_str()),
        ("a.txt", "not_ignored")
    );
}

#[tokio::test]
async fn the_setup_command_sees_the_project_environment() {
    let Some(mut env) = Env::new(true).await else {
        return;
    };
    let mut settings = env.settings();
    settings.worktree_env = BTreeMap::from([
        ("FG_TEST_FLAG".to_owned(), "sccache".to_owned()),
        // The environment of the provider instance wins on a clash.
        ("FG_CLASH".to_owned(), "project".to_owned()),
        // A refused name is dropped.
        ("HOME".to_owned(), "/nowhere".to_owned()),
    ]);
    env.snapshot(settings.clone(), HomeCheckState::Verified)
        .await;
    let fg = env.start_of(settings);
    let cmd = env.start(
        &env.home,
        Some(Env::worktree()),
        Some(fg),
        Some("printf '%s %s %s' \"$FG_TEST_FLAG\" \"$FG_CLASH\" \"$HOME\" > env-out.txt"),
    );
    let session = env.accepted(cmd).await;
    let summary = env.started(&session).await;
    let out = fs::read_to_string(PathBuf::from(summary.worktree.unwrap()).join("env-out.txt"));
    let home = std::env::var("HOME").unwrap_or_default();
    assert_eq!(out.unwrap(), format!("sccache instance {home}"));
}

/// A provider program that writes its environment to `provider-env.txt` next
/// to itself, then runs the fake Claude.
fn env_dump_program(dir: &Path) -> PathBuf {
    let fake = FakeAgent::Claude.locate().unwrap();
    let script = dir.join("provider.sh");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nenv > \"$(dirname \"$0\")/provider-env.txt\"\nexec '{}' \"$@\"\n",
            fake.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    script
}

#[tokio::test]
async fn the_provider_process_sees_the_project_environment() {
    let dir = tempfile::tempdir().unwrap();
    let program = env_dump_program(dir.path());
    let dump = dir.path().join("provider-env.txt");
    let Some(mut env) = Env::with_program(false, Some(program)).await else {
        return;
    };
    let mut settings = env.settings();
    settings.worktree_env = BTreeMap::from([
        ("FG_TEST_FLAG".to_owned(), "sccache".to_owned()),
        // The environment of the provider instance wins on a clash.
        (SCENARIO_ENV.to_owned(), "project".to_owned()),
        // A refused name is dropped.
        ("HOME".to_owned(), "/nowhere".to_owned()),
    ]);
    env.snapshot(settings.clone(), HomeCheckState::Verified)
        .await;
    // A worktree start, then an in-place start (design §7.3).
    let fg = env.start_of(settings.clone());
    let cmd = env.start(&env.home, Some(Env::worktree()), Some(fg), None);
    let session = env.accepted(cmd).await;
    env.started(&session).await;
    let seen = fs::read_to_string(&dump).unwrap();
    fs::remove_file(&dump).unwrap();
    let mut fg = env.start_of(settings);
    fg.kind = StartKind::InPlace;
    let cmd = env.start(&env.home, None, Some(fg), None);
    let session = env.accepted(cmd).await;
    env.started(&session).await;
    let seen_in_place = fs::read_to_string(&dump).unwrap();
    for seen in [seen, seen_in_place] {
        let lines: Vec<&str> = seen.lines().collect();
        assert!(lines.contains(&"FG_TEST_FLAG=sccache"), "{lines:?}");
        assert!(
            lines.contains(&format!("{SCENARIO_ENV}=sim").as_str()),
            "{lines:?}"
        );
        assert!(!lines.contains(&"HOME=/nowhere"), "{lines:?}");
    }
}
