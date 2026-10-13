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
