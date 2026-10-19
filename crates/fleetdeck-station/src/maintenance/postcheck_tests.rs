use fleetdeck_proto::desktop_tools::PostCheck;

use super::postcheck::check;
use super::test_support::{package, postcheck_script, private_dir};

async fn answer(body: &str, transaction: &[(&str, &str)]) -> PostCheck {
    let dir = private_dir();
    let program = postcheck_script(dir.path(), body);
    let transaction: Vec<_> = transaction.iter().map(|(n, v)| package(n, v)).collect();
    check(&program, &transaction).await
}

#[tokio::test]
async fn maintenance_postcheck_with_equal_versions_is_ok() {
    let body = "printf 'wtype 1.0-1\\ngrim 2.3-1\\nextra 9-9\\n'";
    let got = answer(body, &[("wtype", "1.0-1"), ("grim", "2.3-1")]).await;
    assert_eq!(got, PostCheck::Ok);
}

#[tokio::test]
async fn maintenance_postcheck_with_another_version_is_a_change() {
    let got = answer("echo 'wtype 1.1-1'", &[("wtype", "1.0-1")]).await;
    assert_eq!(got, PostCheck::TransactionChanged);
}

#[tokio::test]
async fn maintenance_postcheck_with_a_missing_package_is_a_change() {
    let got = answer(
        "echo 'wtype 1.0-1'",
        &[("wtype", "1.0-1"), ("grim", "2.3-1")],
    )
    .await;
    assert_eq!(got, PostCheck::TransactionChanged);
}

#[tokio::test]
async fn maintenance_postcheck_that_fails_is_a_change() {
    let got = answer("echo 'wtype 1.0-1'; exit 1", &[("wtype", "1.0-1")]).await;
    assert_eq!(got, PostCheck::TransactionChanged);
}

#[tokio::test]
async fn maintenance_postcheck_gets_the_query_with_the_names_only() {
    // The script prints the package only when it got exactly `-Q -- wtype`.
    let body = r#"[ "$1 $2 $3 $#" = "-Q -- wtype 3" ] && echo 'wtype 1.0-1'"#;
    let got = answer(body, &[("wtype", "1.0-1")]).await;
    assert_eq!(got, PostCheck::Ok);
}

#[tokio::test]
async fn maintenance_postcheck_runs_with_the_fixed_environment() {
    let body = r#"[ "$PATH $LANG" = "/usr/bin:/bin C" ] && echo 'wtype 1.0-1'"#;
    let got = answer(body, &[("wtype", "1.0-1")]).await;
    assert_eq!(got, PostCheck::Ok);
}
