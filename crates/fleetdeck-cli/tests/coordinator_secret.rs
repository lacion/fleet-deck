//! ADR-006 §8.2 secrecy tests of the coordinator credential handoff
//! (M3 6-5, DL-215).
//!
//! Test 10 starts a real child process with a prepared handoff and checks
//! that the raw credential is in the child's environment only: not in its
//! argv, not in a debug format of the handoff or the environment. Test 11
//! checks that each provider's arguments name the variable. Tests 12 and
//! 13 are station decision tests:
//! `manager::coordinator_spec_tests::a_coordinator_spec_for_an_unverified_handoff_is_refused_unsupported`
//! and `manager::tests::a_station_local_start_with_a_coordinator_spec_is_rejected`.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::{Command, Stdio};

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_station::coordinator::{Handoff, digest, handoff::TOKEN_ENV};

const URL: &str = "http://127.0.0.1:41234/mcp";

fn token_of(env: &ChildEnv) -> String {
    env.get(TOKEN_ENV).unwrap().to_str().unwrap().to_owned()
}

/// §8.2 test 10: the credential reaches the child only through its
/// environment, and nothing that the station prints shows it.
#[test]
fn the_coordinator_credential_reaches_the_child_only_through_its_environment() {
    for provider in [ProviderKind::Claude, ProviderKind::Codex] {
        let handoff = Handoff::prepare(provider, URL).unwrap();
        let mut env = ChildEnv::empty();
        env.set("PATH", "/usr/bin:/bin").unwrap();
        handoff.apply(&mut env).unwrap();
        let token = token_of(&env);
        assert!(token.starts_with("fdc1_"), "a minted credential");
        assert_eq!(
            digest(&token),
            handoff.digest,
            "the binding stores the digest"
        );

        // Nothing that the station prints shows the value.
        for printed in [format!("{handoff:?}"), format!("{env:?}")] {
            assert!(!printed.contains(&token), "{printed}");
        }

        // A real child: argv carries the provider arguments, the
        // environment carries the credential.
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 5")
            .arg("provider")
            .args(&handoff.args)
            .env_clear()
            .envs(env.iter())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let proc = format!("/proc/{}", child.id());
        // Read after the exec: until then the child shows the test's argv.
        let mut cmdline = Vec::new();
        for _ in 0..200 {
            cmdline = std::fs::read(format!("{proc}/cmdline")).unwrap();
            if cmdline.starts_with(b"/bin/sh\0") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let environ = std::fs::read(format!("{proc}/environ")).unwrap();
        child.kill().unwrap();
        child.wait().unwrap();

        let cmdline = String::from_utf8_lossy(&cmdline);
        assert!(
            cmdline.contains(TOKEN_ENV),
            "argv names the variable: {cmdline}"
        );
        assert!(!cmdline.contains(&token), "argv never holds the credential");
        let environ = String::from_utf8_lossy(&environ);
        assert!(
            environ
                .split('\0')
                .any(|kv| kv == format!("{TOKEN_ENV}={token}")),
            "the child environment holds the credential"
        );
    }
}

/// §8.2 test 11: each provider's arguments name the variable and hold no
/// credential, and a non-loopback endpoint gets no handoff.
#[test]
fn each_provider_handoff_names_the_variable_and_needs_a_loopback_endpoint() {
    let claude = Handoff::prepare(ProviderKind::Claude, URL).unwrap();
    assert_eq!(claude.args[0], "--mcp-config");
    assert!(claude.args[1].contains("Bearer ${FLEETDECK_COORDINATOR_TOKEN}"));
    let codex = Handoff::prepare(ProviderKind::Codex, URL).unwrap();
    assert!(
        codex
            .args
            .iter()
            .any(|a| a
                == "mcp_servers.fleetdeck.bearer_token_env_var=\"FLEETDECK_COORDINATOR_TOKEN\"")
    );
    for handoff in [&claude, &codex] {
        assert!(handoff.args.iter().all(|a| !a.contains("fdc1_")));
    }
    assert!(Handoff::prepare(ProviderKind::Claude, "http://example.com:1/mcp").is_err());
}
