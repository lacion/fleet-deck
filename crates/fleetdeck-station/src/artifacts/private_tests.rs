//! The private paths of the station (DL-173 section 2.1) on the artifact
//! publish: the station's credential and config locations are never
//! published, also when an HTML page names them, and a normal file next to
//! them still is.

use fleetdeck_proto::artifacts::{ArtifactPublish, code};
use fleetdeck_proto::event::ProviderKind;

use super::service_tests::{Rig, publish, rig_with, write};
use crate::store::{NewSession, OfflineApprovalPolicy};
use crate::workspace::private_paths::private_paths_tests::{
    NEEDLE, NORMAL, PRIVATE_FILES, layout, private_of,
};

async fn private_rig() -> Rig {
    rig_with(private_of_layout).await
}

/// The layout below the allowed root, and the set of a station that runs
/// in it.
fn private_of_layout(base: &std::path::Path) -> crate::workspace::PrivatePaths {
    layout(base);
    private_of(base)
}

fn absolute(r: &Rig, rel: &str) -> String {
    r.base.join(rel).display().to_string()
}

#[tokio::test]
async fn a_private_file_is_not_found_and_a_normal_file_next_to_it_is_published() {
    let r = private_rig().await;
    for file in PRIVATE_FILES {
        let err = r
            .service
            .publish(publish(&absolute(&r, file)))
            .await
            .unwrap_err();
        assert_eq!(err.code, code::NOT_FOUND, "{file}: {err}");
        // Also without a session, by the absolute path.
        let err = r
            .service
            .publish(ArtifactPublish {
                session_id: None,
                ..publish(&absolute(&r, file))
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, code::NOT_FOUND, "{file}: {err}");
    }
    // A path through a symlink to a private directory resolves to it.
    std::os::unix::fs::symlink(
        r.base.join(".local/share/fleetdeck/station"),
        r.work.join("shortcut"),
    )
    .unwrap();
    let err = r
        .service
        .publish(publish("shortcut/secrets/jira_token"))
        .await
        .unwrap_err();
    assert_eq!(err.code, code::NOT_FOUND);
    // The refusals left no version behind.
    assert!(
        r.store
            .summary("s1".into())
            .await
            .unwrap()
            .unwrap()
            .artifacts_count
            == 0
    );
    for file in NORMAL {
        let got = r
            .service
            .publish(publish(&absolute(&r, file)))
            .await
            .unwrap();
        assert!(got.created, "{file}");
    }
}

#[tokio::test]
async fn a_hard_link_to_a_private_file_is_not_published() {
    let r = private_rig().await;
    let link = r.work.join("innocent.txt");
    std::fs::hard_link(r.base.join(".config/fleetdeck/station.toml"), &link).unwrap();
    // The link was made after the set was built, but the file existed.
    let err = r
        .service
        .publish(publish("innocent.txt"))
        .await
        .unwrap_err();
    assert_eq!(err.code, code::NOT_FOUND);
}

#[tokio::test]
async fn a_hard_link_to_a_credential_file_is_not_published() {
    let r = private_rig().await;
    let station = r.base.join(".local/share/fleetdeck/station");
    write(&station.join("accounts/acct-x.json"), "account\n");
    let config = r.base.join(".config/fleetdeck/station.toml");
    let temp = r.base.join(".config/fleetdeck/station.toml.tmp-save");
    write(&temp, "saved\n");
    std::fs::rename(&temp, &config).unwrap();
    for (source, name) in [
        (station.join("accounts/acct-x.json"), "link-account.txt"),
        (station.join("secrets/jira_token"), "link-secret.txt"),
        (config, "link-config.txt"),
    ] {
        std::fs::hard_link(source, r.work.join(name)).unwrap();
        let err = r.service.publish(publish(name)).await.unwrap_err();
        assert_eq!(err.code, code::NOT_FOUND, "{name}");
    }
}

#[tokio::test]
async fn a_link_to_a_native_credential_is_not_published_after_the_original_is_gone() {
    let r = private_rig().await;
    let key = r.base.join(".ssh/id_ed25519");
    let hosts = r.base.join(".config/gh/hosts.yml");
    std::fs::hard_link(&key, r.work.join("link-ssh.txt")).unwrap();
    std::fs::remove_file(&key).unwrap();
    // A login replaces the hosts file after the start; the next rescan
    // registers the new identity.
    let temp = r.base.join(".config/gh/hosts.yml.tmp-login");
    write(&temp, "rotated once\n");
    std::fs::rename(&temp, &hosts).unwrap();
    r.private.rescan();
    std::fs::hard_link(&hosts, r.work.join("link-gh.txt")).unwrap();
    write(&temp, "rotated twice\n");
    std::fs::rename(&temp, &hosts).unwrap();
    for name in ["link-ssh.txt", "link-gh.txt"] {
        let err = r.service.publish(publish(name)).await.unwrap_err();
        assert_eq!(err.code, code::NOT_FOUND, "{name}");
    }
}

#[tokio::test]
async fn an_html_page_does_not_bundle_a_private_file_that_it_names() {
    let r = private_rig().await;
    // The page lies in the config directory: its sibling `station.toml`
    // and the temp file are private; `readme.txt` is not.
    let page = r.base.join(".config/fleetdeck/page.html");
    write(
        &page,
        r#"<a href="station.toml">c</a><a href="station.toml.tmp-x">t</a><a href="readme.txt">r</a>"#,
    );
    let got = r
        .service
        .publish(publish(&page.display().to_string()))
        .await
        .unwrap();
    assert_eq!(got.version.files, ["page.html", "readme.txt"]);
    // A page in the station directory is private itself.
    let inside = r.base.join(".local/share/fleetdeck/station/page.html");
    write(&inside, r#"<img src="station.db">"#);
    let err = r
        .service
        .publish(publish(&inside.display().to_string()))
        .await
        .unwrap_err();
    assert_eq!(err.code, code::NOT_FOUND);
    // A page that names a hard link to a private file leaves it out.
    let page = r.work.join("page.html");
    write(
        &page,
        r#"<a href="linked.txt">l</a><a href="plain.txt">p</a>"#,
    );
    write(&r.work.join("plain.txt"), NEEDLE);
    std::fs::hard_link(
        r.base.join(".config/fleetdeck/station.toml"),
        r.work.join("linked.txt"),
    )
    .unwrap();
    let got = r.service.publish(publish("page.html")).await.unwrap();
    assert_eq!(got.version.files, ["page.html", "plain.txt"]);
}

#[tokio::test]
async fn a_session_folder_inside_a_private_directory_publishes_nothing() {
    let r = private_rig().await;
    let station = r.base.join(".local/share/fleetdeck/station");
    r.store
        .create_session(NewSession {
            session_id: "s2".into(),
            provider: ProviderKind::Claude,
            provider_instance: "claude".into(),
            cwd: station.display().to_string(),
            worktree: None,
            branch: None,
            mode: None,
            title: None,
            offline_approval_policy: OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    let err = r
        .service
        .publish(ArtifactPublish {
            session_id: Some("s2".into()),
            ..publish("station.db")
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, code::OUTSIDE_ROOTS);
}

#[tokio::test]
async fn a_service_without_private_paths_refuses_every_publish() {
    let r = rig_with(|_| crate::workspace::PrivatePaths::deny_all()).await;
    write(&r.work.join("a.html"), "<p>a</p>");
    let err = r.service.publish(publish("a.html")).await.unwrap_err();
    assert_eq!(err.code, code::OUTSIDE_ROOTS);
}

#[tokio::test]
async fn a_link_to_a_deleted_or_replaced_credential_is_not_published() {
    use std::os::unix::fs::MetadataExt;
    let mut kept = None;
    let r = rig_with(|base| {
        let private = private_of_layout(base);
        kept = Some(private.clone());
        private
    })
    .await;
    let private = kept.unwrap();
    let dirs = layout(&r.base);
    let station = dirs.station();
    let store = crate::accounts::store::AccountStore::open(&station.join("accounts"))
        .unwrap()
        .with_private(std::sync::Arc::new(private.clone()));
    let account = |token: &str| crate::accounts::store::StoredAccount {
        kind: fleetdeck_proto::accounts::AccountKind::Jira,
        method: fleetdeck_proto::accounts::AccountMethod::Oauth,
        site: None,
        cloud_id: None,
        user: "Ana".into(),
        access_token: fleetdeck_core::Secret::new(token.into()),
        refresh_token: None,
        expires_at: None,
        email: None,
        generation: 0,
    };
    let swap = |path: &std::path::Path, text: &str| {
        let tmp = path.with_extension("tmp-swap");
        write(&tmp, text);
        std::fs::rename(&tmp, path).unwrap();
    };
    let mut names = Vec::new();
    let mut link = |source: &std::path::Path, name: &str| {
        std::fs::hard_link(source, r.work.join(name)).unwrap();
        names.push(name.to_owned());
    };
    // A logout.
    store.put_account("jira-0a1b2c3d", &account("t1")).unwrap();
    link(
        &station.join("accounts/acct-jira-0a1b2c3d.json"),
        "gone-logout.txt",
    );
    store.delete_account("jira-0a1b2c3d").unwrap();
    // An atomic replacement of an account.
    store.put_account("jira-0a1b2c3e", &account("t2")).unwrap();
    link(
        &station.join("accounts/acct-jira-0a1b2c3e.json"),
        "gone-replaced.txt",
    );
    store.put_account("jira-0a1b2c3e", &account("t3")).unwrap();
    // A secret that a `set_secret` replaced.
    let secret = station.join("secrets/provider-claude-1");
    swap(&secret, "secret-1\n");
    private.register_dir(&station.join("secrets"));
    link(&secret, "gone-secret.txt");
    swap(&secret, "secret-2\n");
    private.register_dir(&station.join("secrets"));
    // The station.toml after a save.
    let config = dirs.station_config();
    swap(&config, "config-1\n");
    private.register(&config);
    link(&config, "gone-config.txt");
    swap(&config, "config-2\n");
    private.register(&config);
    for name in &names {
        assert_eq!(std::fs::metadata(r.work.join(name)).unwrap().nlink(), 1);
        let err = r.service.publish(publish(name)).await.unwrap_err();
        assert_eq!(err.code, code::NOT_FOUND, "{name}");
    }
}
