//! `asset::fetch` follows redirects itself (FDV2-004): every target is
//! checked, the auth header goes only to the auth host, and a host outside
//! the binding must be public and is pinned.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use fleetdeck_proto::integrations::code;

use super::super::exec::Programs;
use super::*;

const AUTH: &str = "Authorization: Bearer SECRET-TOKEN";
const PNG: &str = "\\211PNG\\r\\n\\032\\nDATA";

/// A `printf` format for an answer with a redirect.
fn redirect(to: &str) -> String {
    format!("\\ntext/html\\n302\\n{to}")
}

/// A `printf` format for an image answer.
fn image() -> String {
    format!("{PNG}\\nimage/png\\n200\\n")
}

/// The script of the fake `curl`; the routes follow it.
const SCRIPT: &str = r#"#!/bin/sh
D='@DIR@'
cat > "$D/in.tmp"
cat "$D/in.tmp" >> "$D/stdin.log"
printf -- '-----\n' >> "$D/stdin.log"
URL=$(sed -n 's/^url = "\(.*\)"$/\1/p' "$D/in.tmp")
case "$URL" in
"#;

/// A fake `curl`: it logs the config on stdin and answers by the `url` line.
fn fake_curl(dir: &Path, routes: &[(&str, String)]) -> PathBuf {
    let mut script = SCRIPT.replace("@DIR@", &dir.display().to_string());
    for (url, answer) in routes {
        assert!(!url.contains('\'') && !answer.contains('\''));
        script.push_str(&format!("'{url}') printf '{answer}';;\n"));
    }
    script.push_str("*) echo \"no route: $URL\" >&2; exit 1;;\nesac\n");
    let path = dir.join("curl");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn programs(dir: &Path, routes: &[(&str, String)]) -> Programs {
    Programs {
        curl: fake_curl(dir, routes),
        ..Programs::default()
    }
}

/// The configs that curl got, one per call.
fn configs(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("stdin.log"))
        .unwrap_or_default()
        .split("-----\n")
        .filter(|c| !c.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

fn hosts() -> Vec<String> {
    vec!["a.test".to_owned(), "b.test".to_owned()]
}

async fn run(
    dir: &Path,
    routes: &[(&str, String)],
    start: &str,
) -> Result<fleetdeck_proto::integrations::Asset, Fail> {
    fetch(
        &programs(dir, routes),
        start,
        &hosts(),
        Some((AUTH, "a.test")),
        &["SECRET-TOKEN"],
    )
    .await
}

fn not_allowed_fail() -> Fail {
    Fail::new(
        code::INVALID_PARAMS,
        "the image redirects to a host that is not allowed",
    )
}

#[tokio::test]
async fn a_redirect_inside_the_binding_keeps_the_auth_header_for_the_auth_host_only() {
    let tmp = tempfile::tempdir().unwrap();
    let routes = [
        ("https://a.test/x", redirect("https://b.test/y")),
        ("https://b.test/y", redirect("https://a.test/z")),
        ("https://a.test/z", image()),
    ];
    let got = run(tmp.path(), &routes, "https://a.test/x").await.unwrap();
    assert_eq!(got.content_type, "image/png");
    let cfgs = configs(tmp.path());
    assert_eq!(cfgs.len(), 3);
    assert!(cfgs[0].contains("header = \"Authorization: Bearer SECRET-TOKEN\""));
    assert!(!cfgs[1].contains("header ="), "{}", cfgs[1]);
    assert!(cfgs[2].contains("header = \"Authorization"));
    for cfg in &cfgs {
        assert!(!cfg.contains("resolve"), "{cfg}");
        assert!(cfg.lines().any(|l| l == "no-location"), "{cfg}");
    }
}

#[tokio::test]
async fn a_redirect_to_a_private_or_local_address_is_refused() {
    for target in [
        "https://127.0.0.1/a.png",
        "https://10.0.0.1/a.png",
        "https://100.100.1.1/a.png",
        "https://169.254.169.254/a.png",
        "https://192.168.1.1/a.png",
        "https://0.0.0.0/a.png",
        "https://[fe80::1]/a.png",
        "https://[::1]/a.png",
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let routes = [("https://a.test/x", redirect(target)), (target, image())];
        let err = run(tmp.path(), &routes, "https://a.test/x")
            .await
            .unwrap_err();
        assert_eq!(err, not_allowed_fail(), "{target}");
        assert_eq!(configs(tmp.path()).len(), 1, "{target}: curl got one call");
    }
}

#[tokio::test]
async fn a_redirect_to_a_public_host_has_no_auth_header_and_is_pinned() {
    let tmp = tempfile::tempdir().unwrap();
    let routes = [
        ("https://a.test/x", redirect("https://8.8.8.8/p.png")),
        ("https://8.8.8.8/p.png", image()),
    ];
    let got = run(tmp.path(), &routes, "https://a.test/x").await.unwrap();
    assert_eq!(got.content_type, "image/png");
    let cfgs = configs(tmp.path());
    assert_eq!(cfgs.len(), 2);
    assert!(!cfgs[0].contains("resolve"));
    assert!(!cfgs[1].contains("header ="), "{}", cfgs[1]);
    assert!(
        cfgs[1].contains("resolve = \"8.8.8.8:443:8.8.8.8\"\n"),
        "{}",
        cfgs[1]
    );
}

#[tokio::test]
async fn an_explicit_port_443_is_allowed_and_another_port_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let routes = [
        ("https://a.test/x", redirect("https://8.8.8.8:443/p.png")),
        ("https://8.8.8.8:443/p.png", image()),
    ];
    run(tmp.path(), &routes, "https://a.test/x").await.unwrap();
    assert!(configs(tmp.path())[1].contains("resolve = \"8.8.8.8:443:8.8.8.8\"\n"));

    let tmp = tempfile::tempdir().unwrap();
    let routes = [
        ("https://a.test/x", redirect("https://8.8.8.8:8443/p.png")),
        ("https://8.8.8.8:8443/p.png", image()),
    ];
    let err = run(tmp.path(), &routes, "https://a.test/x")
        .await
        .unwrap_err();
    assert_eq!(err, not_allowed_fail());
    assert_eq!(configs(tmp.path()).len(), 1);
}

#[tokio::test]
async fn a_bad_target_is_refused() {
    for target in [
        "http://b.test/a.png",
        "https://user:pw@b.test/a.png",
        "https://user@8.8.8.8/a.png",
        "ftp://b.test/a.png",
        "//b.test/a.png",
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let routes = [("https://a.test/x", redirect(target)), (target, image())];
        let err = run(tmp.path(), &routes, "https://a.test/x")
            .await
            .unwrap_err();
        assert_eq!(err, not_allowed_fail(), "{target}");
        assert_eq!(configs(tmp.path()).len(), 1, "{target}");
    }
}

#[tokio::test]
async fn three_redirects_are_followed_and_a_fourth_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let routes = [
        ("https://a.test/1", redirect("https://a.test/2")),
        ("https://a.test/2", redirect("https://a.test/3")),
        ("https://a.test/3", redirect("https://a.test/4")),
        ("https://a.test/4", image()),
    ];
    run(tmp.path(), &routes, "https://a.test/1").await.unwrap();
    assert_eq!(configs(tmp.path()).len(), 4);

    let tmp = tempfile::tempdir().unwrap();
    let routes = [
        ("https://a.test/1", redirect("https://a.test/2")),
        ("https://a.test/2", redirect("https://a.test/3")),
        ("https://a.test/3", redirect("https://a.test/4")),
        ("https://a.test/4", redirect("https://a.test/5")),
        ("https://a.test/5", image()),
    ];
    let err = run(tmp.path(), &routes, "https://a.test/1")
        .await
        .unwrap_err();
    assert_eq!(
        err,
        Fail::new(code::INVALID_PARAMS, "the image has too many redirects")
    );
    assert_eq!(configs(tmp.path()).len(), 4, "the fifth URL is not fetched");
}

#[tokio::test]
async fn a_redirect_status_without_a_target_is_the_answer() {
    let tmp = tempfile::tempdir().unwrap();
    let routes = [(
        "https://a.test/x",
        "<html/>\\ntext/html\\n302\\n".to_owned(),
    )];
    let err = run(tmp.path(), &routes, "https://a.test/x")
        .await
        .unwrap_err();
    assert_eq!(err.code, code::BAD_ANSWER);
    assert_eq!(configs(tmp.path()).len(), 1);
}

#[tokio::test]
async fn curl_gets_q_first_and_a_config_that_turns_redirects_off() {
    let tmp = tempfile::tempdir().unwrap();
    let curl = tmp.path().join("curl");
    let script = format!(
        "#!/bin/sh\necho \"$@\" >> '{d}/argv.log'\ncat >> '{d}/stdin.log'\nprintf '\\ntext/html\\n200\\n'\n",
        d = tmp.path().display()
    );
    std::fs::write(&curl, script).unwrap();
    std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o755)).unwrap();
    let programs = Programs {
        curl,
        ..Programs::default()
    };
    let req = http::Request {
        url: "https://a.test/x".into(),
        method: "GET",
        content_type: true,
        ..http::Request::default()
    };
    http::send(&programs, &req, &[], 1024).await.unwrap();
    let argv = std::fs::read_to_string(tmp.path().join("argv.log")).unwrap();
    assert_eq!(argv.trim(), "-q -sS --fail-with-body -K -");
    let cfg = std::fs::read_to_string(tmp.path().join("stdin.log")).unwrap();
    assert!(cfg.lines().any(|l| l == "no-location"), "{cfg}");
}

#[tokio::test]
async fn the_auth_header_goes_only_to_the_origin_of_the_auth_host() {
    let tmp = tempfile::tempdir().unwrap();
    let hosts = vec!["t.example".to_owned()];
    // NV-14: another port of the same host is not on a host of the binding.
    let routes = [("https://t.example:8443/p.png", image())];
    let err = fetch(
        &programs(tmp.path(), &routes),
        "https://t.example:8443/p.png",
        &hosts,
        Some((AUTH, "t.example")),
        &["SECRET-TOKEN"],
    )
    .await
    .unwrap_err();
    assert_eq!(
        err,
        Fail::new(
            code::INVALID_PARAMS,
            "the asset URL is not on a host of this binding"
        )
    );
    assert!(configs(tmp.path()).is_empty(), "curl got no call");

    let tmp = tempfile::tempdir().unwrap();
    let routes = [("https://t.example/p.png", image())];
    fetch(
        &programs(tmp.path(), &routes),
        "https://t.example/p.png",
        &hosts,
        Some((AUTH, "t.example")),
        &["SECRET-TOKEN"],
    )
    .await
    .unwrap();
    let cfgs = configs(tmp.path());
    assert_eq!(cfgs.len(), 1);
    assert!(cfgs[0].contains("header = \"Authorization: Bearer SECRET-TOKEN\""));
}

#[tokio::test]
async fn a_host_entry_with_a_port_allows_that_port_only() {
    let hosts = vec!["git.example:8443".to_owned()];
    let tmp = tempfile::tempdir().unwrap();
    let routes = [("https://git.example:8443/a.png", image())];
    fetch(
        &programs(tmp.path(), &routes),
        "https://git.example:8443/a.png",
        &hosts,
        Some((AUTH, "git.example:8443")),
        &["SECRET-TOKEN"],
    )
    .await
    .unwrap();
    assert!(configs(tmp.path())[0].contains("header = \"Authorization"));

    let tmp = tempfile::tempdir().unwrap();
    let routes = [("https://git.example/a.png", image())];
    let err = fetch(
        &programs(tmp.path(), &routes),
        "https://git.example/a.png",
        &hosts,
        None,
        &[],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::INVALID_PARAMS);
    assert!(configs(tmp.path()).is_empty());
}

#[tokio::test]
async fn a_redirect_to_another_port_of_the_auth_host_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let routes = [
        ("https://a.test/x", redirect("https://a.test:8443/y")),
        ("https://a.test:8443/y", image()),
    ];
    let err = run(tmp.path(), &routes, "https://a.test/x")
        .await
        .unwrap_err();
    assert_eq!(err, not_allowed_fail());
    assert_eq!(configs(tmp.path()).len(), 1, "curl got one call");
}

#[test]
fn an_entry_without_a_port_means_port_443() {
    assert_eq!(origin_of_entry("T.Example"), "t.example:443");
    assert_eq!(origin_of_entry("t.example:8443"), "t.example:8443");
    let hosts = vec!["t.example".to_owned()];
    assert!(in_hosts(&hosts, "t.example:443"));
    assert!(!in_hosts(&hosts, "t.example:8443"));
    assert!(auth_headers(Some((AUTH, "t.example")), "t.example:8443").is_empty());
    assert_eq!(
        auth_headers(Some((AUTH, "T.example")), "t.example:443"),
        [AUTH]
    );
}

/// True when a real `curl` is on the path.
fn real_curl() -> bool {
    std::process::Command::new("curl")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[tokio::test]
async fn a_user_curlrc_with_location_does_not_make_the_real_curl_follow() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    if !real_curl() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(home.join(".curlrc"), "location\n").unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let mut requests = 0_u32;
        while let Ok(Ok((mut tcp, _))) =
            tokio::time::timeout(std::time::Duration::from_secs(3), listener.accept()).await
        {
            requests += 1;
            let mut buf = [0_u8; 2048];
            let _ = tcp.read(&mut buf).await;
            let answer = format!(
                "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{port}/next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = tcp.write_all(answer.as_bytes()).await;
        }
        requests
    });
    // The config of a request, with the URL on the local server and without
    // the `https` pin (the test server has no TLS).
    let req = http::Request {
        url: format!("http://127.0.0.1:{port}/first"),
        method: "GET",
        content_type: true,
        ..http::Request::default()
    };
    let cfg: String = http::config(&req)
        .lines()
        .filter(|l| !l.starts_with("proto"))
        .map(|l| format!("{l}\n"))
        .collect();
    let mut child = tokio::process::Command::new("curl")
        .args(http::ARGS)
        .env("HOME", &home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(cfg.as_bytes()).await.unwrap();
    drop(stdin);
    let out = child.wait_with_output().await.unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let (_, trailer) = http::split_trailer(&out.stdout);
    assert_eq!(trailer.status, 302, "{text}");
    assert!(
        trailer.redirect_url.ends_with("/next"),
        "{}",
        trailer.redirect_url
    );
    assert_eq!(server.await.unwrap(), 1, "curl made one request only");
}
