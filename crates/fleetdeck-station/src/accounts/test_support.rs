//! Fake `gh`, `glab` and `curl` programs and a service on a temporary
//! directory, for the tests of this module.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::Accounts;
use super::store::AccountStore;
use crate::integrations::exec::Programs;

/// An executable shell script `name` in `dir`.
pub fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A fake `curl`: it appends its arguments to `argv.log` and its stdin (the
/// config) to `stdin.log`, and answers with the body of the first case
/// whose pattern the arguments and the config hold, then the HTTP status.
/// A call whose config asks for the status (`write-out`) gets the status
/// after the body; any other call exits 22 for a status of 400 or more, as
/// `--fail-with-body` does. A call that no case matches answers 404 `{}`.
pub fn curl(dir: &Path, cases: &[(&str, &str, u16)]) -> PathBuf {
    let mut body = format!(
        "D='{}'\nprintf '%s\\n' \"$*\" >> \"$D/argv.log\"\ncat > \"$D/in.tmp\"\ncat \"$D/in.tmp\" >> \"$D/stdin.log\"\nprintf '\\n---\\n' >> \"$D/stdin.log\"\nALL=\"$* $(cat \"$D/in.tmp\")\"\nfinish() {{\n  if grep -q 'FD-STATUS' \"$D/in.tmp\"; then printf '\\nFD-STATUS:%s' \"$1\"; exit 0; fi\n  if [ \"$1\" -ge 400 ]; then exit 22; fi\n  exit 0\n}}\ncase \"$ALL\" in\n",
        dir.display()
    );
    for (i, (pattern, answer, status)) in cases.iter().enumerate() {
        assert!(
            !pattern.contains('\'') && !answer.contains('\''),
            "{pattern}"
        );
        body.push_str(&format!(
            "*'{pattern}'*) cat <<'FD_EOF_{i}'\n{answer}\nFD_EOF_{i}\nfinish {status};;\n"
        ));
    }
    body.push_str("*) echo '{}'; finish 404;;\nesac");
    script(dir, "curl", &body)
}

/// The text of a log file of a fake program.
pub fn log(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap_or_default()
}

/// A service with a store in `tmp/data/accounts`, no environment token, the
/// built-in Linear app off and short limits.
pub fn service(tmp: &Path, programs: Programs) -> Accounts {
    let mut accounts = Accounts::new(programs);
    accounts.env_probe = |_| false;
    accounts.builtin_probe = || false;
    accounts.limits.start_wait = Duration::from_secs(5);
    accounts.attach(AccountStore::open(&tmp.join("data").join("accounts")).unwrap());
    accounts
}

/// Programs that do nothing useful: every test replaces the ones it uses.
pub fn programs(tmp: &Path) -> Programs {
    let none = script(tmp, "none", "echo 'no canned answer' >&2; exit 1");
    Programs {
        gh: none.clone(),
        glab: none.clone(),
        curl: none,
    }
}
