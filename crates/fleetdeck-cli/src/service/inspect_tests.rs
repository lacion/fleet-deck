//! Tests of the service inspection of the bootstrap (P8 T3b, D13). The
//! machine is the recording rig of the service tests: no program runs.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::service::fake::ok;
use crate::service::program;
use crate::service::tests::Rig;
use crate::service::{Context, Platform};

/// A station service as `fleetdeck service install station` leaves it: the
/// unit with our data directory and the program link at the running binary.
fn installed(rig: &Rig) {
    rig.put_installed(Kind::Station);
    let root = Kind::Station.root(&rig.ctx.dirs);
    program::ensure(&root, &rig.ctx.exe, "1.2.3", "01aaa").unwrap();
    if rig.ctx.platform == Platform::Systemd {
        exec_start(rig, &expected_argv(&rig.ctx));
        environment(rig, &rendered_environment(&rig.ctx), "", "");
    } else {
        plist(rig, &expected_argv(&rig.ctx));
    }
}

/// The prefix of the `ExecStart` query of the inspection.
const EXEC_START: &[&str] = &[
    "--user",
    "show",
    "fleetdeck-station.service",
    "-p",
    "ExecStart",
];

/// The prefix of the environment query of the inspection.
const ENVIRONMENT: &[&str] = &[
    "--user",
    "show",
    "fleetdeck-station.service",
    "-p",
    "Environment",
];

/// The answer of `systemctl --user show <unit> -p ExecStart --value` for one
/// entry that runs `argv`.
fn exec_line(argv: &[String]) -> String {
    format!(
        "{{ path={} ; argv[]={} ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }}\n",
        argv[0],
        argv.join(" ")
    )
}

fn exec_start(rig: &Rig, argv: &[String]) {
    rig.fake
        .always("systemctl", EXEC_START, ok(&exec_line(argv)));
}

/// The assignments that the install renders, as `KEY=value`.
fn rendered_environment(ctx: &Context) -> Vec<String> {
    let program = Kind::Station.root(&ctx.dirs).join("current");
    crate::service::validate(ctx, &program)
        .unwrap()
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect()
}

/// One assignment as `systemctl show` prints it: in double quotes, with a
/// backslash before `"` and `\`, when it holds white space or such a
/// character.
fn shown(assignment: &str) -> String {
    if assignment.contains(|c: char| c.is_whitespace() || c == '"' || c == '\\') {
        let body = assignment.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{body}\"")
    } else {
        assignment.to_owned()
    }
}

/// The answer of the environment query: the assignments and the two file
/// properties (`files` and `unset` are the text after the `=`).
fn environment(rig: &Rig, assignments: &[String], files: &str, unset: &str) {
    let shown: Vec<String> = assignments.iter().map(|a| shown(a)).collect();
    rig.fake.always(
        "systemctl",
        ENVIRONMENT,
        ok(&format!(
            "Environment={}\nEnvironmentFiles={files}\nUnsetEnvironment={unset}\n",
            shown.join(" ")
        )),
    );
}

/// Writes the plist of an install whose program arguments are `argv`.
fn plist(rig: &Rig, argv: &[String]) {
    let path = rig.file(Kind::Station);
    let text =
        crate::service::launchd::render(&rig.ctx, Kind::Station, Path::new(&argv[0])).unwrap();
    // Replace the whole array by the given arguments.
    let (head, tail) = text.split_once("<array>\n").unwrap();
    let (_, tail) = tail.split_once("    </array>").unwrap();
    let items: String = argv
        .iter()
        .map(|a| format!("        <string>{}</string>\n", escape(a)))
        .collect();
    std::fs::write(path, format!("{head}<array>\n{items}    </array>{tail}")).unwrap();
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn active(rig: &Rig, yes: bool) {
    let answer = if yes {
        ok("active\n")
    } else {
        crate::service::fake::fail(3, "")
    };
    rig.fake
        .always("systemctl", &["--user", "is-active"], answer);
}

fn inspected(rig: &Rig) -> ServiceInspect {
    inspect(&rig.ctx, &rig.fake)
}

#[test]
fn no_unit_is_absent() {
    let rig = Rig::systemd();
    assert_eq!(inspected(&rig), ServiceInspect::Absent);
    // A deck service alone is not a station service.
    rig.put_installed(Kind::Deck);
    assert_eq!(inspected(&rig), ServiceInspect::Absent);
}

#[test]
fn a_matching_unit_reports_whether_it_is_active() {
    let rig = Rig::systemd();
    installed(&rig);
    active(&rig, false);
    assert_eq!(inspected(&rig), ServiceInspect::Matching { active: false });
    active(&rig, true);
    assert_eq!(inspected(&rig), ServiceInspect::Matching { active: true });
}

#[test]
fn a_both_service_is_a_conflict() {
    let rig = Rig::systemd();
    installed(&rig);
    rig.put_installed(Kind::Both);
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
    // Also without a station unit.
    let rig = Rig::systemd();
    rig.put_installed(Kind::Both);
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
}

#[test]
fn another_data_directory_or_no_marker_is_a_conflict() {
    let rig = Rig::systemd();
    installed(&rig);
    rig.put_file(Kind::Station, Some("/somewhere/else"));
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
    rig.put_file(Kind::Station, None);
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
}

#[test]
fn a_missing_program_link_is_a_conflict() {
    let rig = Rig::systemd();
    rig.put_installed(Kind::Station);
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
    // A link whose target is gone.
    let root = Kind::Station.root(&rig.ctx.dirs);
    std::fs::create_dir_all(&root).unwrap();
    std::os::unix::fs::symlink("releases/none/fleetdeck", root.join("current")).unwrap();
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
}

#[test]
fn another_program_than_the_running_binary_is_a_conflict() {
    let rig = Rig::systemd();
    installed(&rig);
    active(&rig, true);
    assert_eq!(inspected(&rig), ServiceInspect::Matching { active: true });
    std::fs::write(&rig.ctx.exe, b"another build").unwrap();
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
}

#[test]
fn an_unreadable_unit_is_a_conflict() {
    let rig = Rig::systemd();
    // A directory where the unit file should be: reading it fails.
    std::fs::create_dir_all(rig.file(Kind::Station)).unwrap();
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
}

#[test]
fn sha256_file_is_the_lowercase_digest() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f");
    std::fs::write(&path, b"abc").unwrap();
    assert_eq!(
        sha256_file(&path).unwrap(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    // More than one 64 KiB read.
    std::fs::write(&path, vec![b'a'; 200_000]).unwrap();
    assert_eq!(
        sha256_file(&path).unwrap(),
        crate::provision::sha256_hex(&vec![b'a'; 200_000])
    );
    assert!(sha256_file(&dir.path().join("missing")).is_err());
}

#[test]
fn a_changed_exec_start_with_the_right_marker_is_a_conflict() {
    let rig = Rig::systemd();
    installed(&rig);
    active(&rig, true);
    assert_eq!(inspected(&rig), ServiceInspect::Matching { active: true });
    // Another program, same marker and same arguments.
    let mut argv = expected_argv(&rig.ctx);
    argv[0] = "/tmp/other/fleetdeck".to_owned();
    exec_start(&rig, &argv);
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
    // Another subcommand.
    let mut argv = expected_argv(&rig.ctx);
    *argv.last_mut().unwrap() = "deck".to_owned();
    exec_start(&rig, &argv);
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
    // `path=` differs from `argv[0]`.
    let argv = expected_argv(&rig.ctx);
    let line = exec_line(&argv).replacen(&format!("path={}", argv[0]), "path=/bin/sh", 1);
    rig.fake.always("systemctl", EXEC_START, ok(&line));
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
}

#[test]
fn a_different_data_dir_argument_is_a_conflict() {
    let mut rig = Rig::systemd();
    rig.ctx.data_dir_flag = true;
    installed(&rig);
    active(&rig, true);
    assert_eq!(inspected(&rig), ServiceInspect::Matching { active: true });
    let mut argv = expected_argv(&rig.ctx);
    let at = argv.iter().position(|a| a == "--data-dir").unwrap();
    argv[at + 1] = "/somewhere/else".to_owned();
    exec_start(&rig, &argv);
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
    // No `--data-dir` although we use one.
    let argv: Vec<String> = expected_argv(&rig.ctx)
        .into_iter()
        .enumerate()
        .filter(|(i, _)| *i != at && *i != at + 1)
        .map(|(_, a)| a)
        .collect();
    exec_start(&rig, &argv);
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
    // A `--data-dir` although we use none.
    let rig = Rig::systemd();
    installed(&rig);
    active(&rig, true);
    let mut argv = expected_argv(&rig.ctx);
    argv.splice(1..1, ["--data-dir".to_owned(), rig.data()]);
    exec_start(&rig, &argv);
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
}

#[test]
fn a_missing_failed_or_multiple_exec_start_is_a_conflict() {
    let rig = Rig::systemd();
    installed(&rig);
    active(&rig, true);
    let line = exec_line(&expected_argv(&rig.ctx));
    for answer in [
        ok(""),
        ok("not a line\n"),
        ok(&format!("{line}{line}")),
        ok(&line.replace(" ; ignore_errors=no", "")),
        crate::service::fake::fail(1, "no unit"),
    ] {
        rig.fake.always("systemctl", EXEC_START, answer);
        assert_eq!(inspected(&rig), ServiceInspect::Conflict);
    }
    rig.fake.always(
        "systemctl",
        EXEC_START,
        Err(crate::service::runner::RunError::Start("none".to_owned())),
    );
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
}

#[test]
fn a_launchd_plist_is_compared_by_its_program_arguments() {
    let mut rig = Rig::launchd();
    rig.ctx.data_dir_flag = true;
    installed(&rig);
    rig.fake.always(
        "launchctl",
        &["print", "gui/1000/dev.fleetdeck.station"],
        ok("state = running\n"),
    );
    assert_eq!(inspected(&rig), ServiceInspect::Matching { active: true });
    // The plist that the install renders matches too.
    let root = Kind::Station.root(&rig.ctx.dirs);
    let text =
        crate::service::launchd::render(&rig.ctx, Kind::Station, &root.join("current")).unwrap();
    std::fs::write(rig.file(Kind::Station), text).unwrap();
    assert_eq!(inspected(&rig), ServiceInspect::Matching { active: true });
    // Another program, another data directory, no arguments array.
    let good = expected_argv(&rig.ctx);
    let mut other = good.clone();
    other[0] = "/tmp/other/fleetdeck".to_owned();
    plist(&rig, &other);
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
    let mut other = good.clone();
    let at = other.iter().position(|a| a == "--data-dir").unwrap();
    other[at + 1] = "/somewhere/else".to_owned();
    plist(&rig, &other);
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
    rig.put_installed(Kind::Station);
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
}

#[test]
fn the_plist_scanner_unescapes_the_five_entities() {
    let text = "<key>ProgramArguments</key>\n    <array>\n        <string>/a&amp;b</string>\n        <string>&lt;&gt;&quot;&apos;</string>\n    </array>\n";
    assert_eq!(
        plist_arguments(text).unwrap(),
        ["/a&b".to_owned(), "<>\"'".to_owned()]
    );
    assert!(
        plist_arguments("<key>ProgramArguments</key><array><integer>1</integer></array>").is_none()
    );
}

#[test]
fn a_redirected_or_extended_service_environment_is_a_conflict() {
    let rig = Rig::systemd();
    installed(&rig);
    active(&rig, true);
    assert_eq!(inspected(&rig), ServiceInspect::Matching { active: true });
    let good = rendered_environment(&rig.ctx);
    // A drop-in that redirects the data directory; `ExecStart` is unchanged.
    let mut other = good.clone();
    other.push("XDG_DATA_HOME=/other".to_owned());
    environment(&rig, &other, "", "");
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
    // The same name with another value.
    let mut other = good.clone();
    other[0] = "PATH=/tmp/evil".to_owned();
    environment(&rig, &other, "", "");
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
    // An extra harmless variable: the set must be exact.
    let mut other = good.clone();
    other.push("HARMLESS=1".to_owned());
    environment(&rig, &other, "", "");
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
    // A missing variable.
    environment(&rig, &good[1..], "", "");
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
    // An `EnvironmentFile=`.
    environment(&rig, &good, "/etc/evil.env (ignore_errors=no)", "");
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
    // An `UnsetEnvironment=`.
    environment(&rig, &good, "", "PATH");
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
    // The unit as rendered.
    environment(&rig, &good, "", "");
    assert_eq!(inspected(&rig), ServiceInspect::Matching { active: true });
}

#[test]
fn the_environment_answer_is_parsed_with_systemd_quoting() {
    let mut rig = Rig::systemd();
    rig.ctx.xdg_data_home = Some("/data \"home\"\\x".to_owned());
    installed(&rig);
    active(&rig, true);
    let good = rendered_environment(&rig.ctx);
    assert!(good.contains(&"XDG_DATA_HOME=/data \"home\"\\x".to_owned()));
    assert_eq!(inspected(&rig), ServiceInspect::Matching { active: true });
    let mut other = good.clone();
    let at = other
        .iter()
        .position(|a| a.starts_with("XDG_DATA_HOME="))
        .unwrap();
    other[at] = "XDG_DATA_HOME=/data".to_owned();
    environment(&rig, &other, "", "");
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
    assert_eq!(
        split_assignments("A=1  \"B=x y\" C=\"p q\"").unwrap(),
        ["A=1", "B=x y", "C=p q"]
    );
    assert_eq!(split_assignments("").unwrap(), Vec::<String>::new());
    assert!(split_assignments("\"open").is_none());
    assert!(split_assignments("end\\").is_none());
}

#[test]
fn a_missing_failed_or_repeated_environment_answer_is_a_conflict() {
    let rig = Rig::systemd();
    installed(&rig);
    active(&rig, true);
    let assignments = rendered_environment(&rig.ctx).join(" ");
    let all = format!("Environment={assignments}\nEnvironmentFiles=\nUnsetEnvironment=\n");
    for answer in [
        ok(""),
        ok("not a line\n"),
        // A property is missing.
        ok(&format!("Environment={assignments}\nEnvironmentFiles=\n")),
        ok("EnvironmentFiles=\nUnsetEnvironment=\n"),
        // A property is repeated.
        ok(&format!("{all}EnvironmentFiles=\n")),
        // A quote that stays open.
        ok("Environment=\"open\nEnvironmentFiles=\nUnsetEnvironment=\n"),
        crate::service::fake::fail(1, "no unit"),
        Err(crate::service::runner::RunError::Start("none".to_owned())),
    ] {
        rig.fake.always("systemctl", ENVIRONMENT, answer);
        assert_eq!(inspected(&rig), ServiceInspect::Conflict);
    }
    rig.fake.always("systemctl", ENVIRONMENT, ok(&all));
    assert_eq!(inspected(&rig), ServiceInspect::Matching { active: true });
}

/// Writes the plist as rendered; `edit` changes its text first.
fn plist_edited(rig: &Rig, edit: impl Fn(&str) -> String) {
    let root = Kind::Station.root(&rig.ctx.dirs);
    let text =
        crate::service::launchd::render(&rig.ctx, Kind::Station, &root.join("current")).unwrap();
    std::fs::write(rig.file(Kind::Station), edit(&text)).unwrap();
}

#[test]
fn a_launchd_plist_with_another_environment_is_a_conflict() {
    let mut rig = Rig::launchd();
    rig.ctx.xdg_data_home = Some("/data & <more>".to_owned());
    rig.ctx.data_dir_flag = true;
    installed(&rig);
    rig.fake.always(
        "launchctl",
        &["print", "gui/1000/dev.fleetdeck.station"],
        ok("state = running\n"),
    );
    // As rendered, with an escaped value.
    plist_edited(&rig, str::to_owned);
    assert_eq!(inspected(&rig), ServiceInspect::Matching { active: true });
    // A changed variable.
    plist_edited(&rig, |t| {
        t.replace("<string>json</string>", "<string>text</string>")
    });
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
    plist_edited(&rig, |t| t.replace("/data &amp; &lt;more&gt;", "/other"));
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
    // An extra variable.
    plist_edited(&rig, |t| {
        t.replace(
            "    </dict>\n    <key>StandardOutPath</key>",
            "        <key>HARMLESS</key>\n        <string>1</string>\n    </dict>\n    <key>StandardOutPath</key>",
        )
    });
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
    // A missing variable.
    plist_edited(&rig, |t| {
        t.replace(
            "        <key>FLEETDECK_LOG_FORMAT</key>\n        <string>json</string>\n",
            "",
        )
    });
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
    // An odd number of items, and no dict.
    plist_edited(&rig, |t| {
        t.replace("        <key>FLEETDECK_LOG_FORMAT</key>\n", "")
    });
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
    plist_edited(&rig, |t| t.replace("EnvironmentVariables", "Other"));
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
}
