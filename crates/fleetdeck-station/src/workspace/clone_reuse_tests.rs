use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use super::*;

const URL: &str = "https://github.com/acme/tools.git";

/// The roots of the tests: a canonical directory `root`.
struct Roots {
    dir: tempfile::TempDir,
    root: PathBuf,
}

impl Roots {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap().join("projects");
        fs::create_dir(&root).unwrap();
        Self { dir, root }
    }

    fn inside(&self) -> impl Fn(&Path) -> Option<PathBuf> + '_ {
        |p| {
            let c = p.canonicalize().ok()?;
            c.starts_with(&self.root).then_some(c)
        }
    }

    /// A directory with a `.git` directory whose config is `config`.
    fn checkout(&self, name: &str, config: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::create_dir_all(path.join(".git")).unwrap();
        fs::write(path.join(".git").join("config"), config).unwrap();
        path
    }

    fn run(&self, url: &str, target: &Path) -> Result<CloneTarget, Refused> {
        clone_or_reuse(url, &target.to_string_lossy(), self.inside())
    }
}

fn origin(url: &str) -> String {
    format!(
        "[core]\n\trepositoryformatversion = 0\n[remote \"origin\"]\n\turl = {url}\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n"
    )
}

#[test]
fn a_new_path_is_cloned() {
    let r = Roots::new();
    let target = r.root.join("tools");
    match r.run(URL, &target).unwrap() {
        CloneTarget::Clone(WorkspacePlan::Clone { url, path }, planned) => {
            assert_eq!(url, URL);
            assert_eq!(planned, target);
            assert_eq!(path, target.to_string_lossy());
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_bad_url_is_refused_before_the_target_is_read() {
    let r = Roots::new();
    let target = r.checkout("tools", &origin(URL));
    let err = r
        .run("https://u:t@github.com/acme/tools.git", &target)
        .unwrap_err();
    assert!(err.forbidden, "{err:?}");
}

#[test]
fn a_clone_of_the_same_repository_is_reused_also_in_another_form() {
    let r = Roots::new();
    let target = r.checkout("tools", &origin(URL));
    let want = CloneTarget::Reuse(target.canonicalize().unwrap());
    for same in [
        URL,
        "https://github.com/acme/tools",
        "https://GitHub.com/acme/tools.git/",
        "git@github.com:acme/tools.git",
        "ssh://git@github.com/acme/tools.git",
        "ssh://git@github.com:22/acme/tools.git",
    ] {
        assert_eq!(r.run(same, &target).unwrap(), want, "{same}");
    }
    // The origin may be in the ssh form and the request in the https form.
    let target = r.checkout("ssh-tools", &origin("git@github.com:acme/tools.git"));
    assert_eq!(
        r.run(URL, &target).unwrap(),
        CloneTarget::Reuse(target.canonicalize().unwrap())
    );
    // A quoted value and a key in upper case are read.
    let target = r.checkout(
        "quoted",
        "[remote \"origin\"]\n\tURL = \"https://github.com/acme/tools.git\"\n",
    );
    assert_eq!(
        r.run(URL, &target).unwrap(),
        CloneTarget::Reuse(target.canonicalize().unwrap())
    );
}

#[test]
fn a_clone_of_another_repository_is_refused() {
    let r = Roots::new();
    let target = r.checkout("tools", &origin("https://github.com/acme/other.git"));
    let err = r.run(URL, &target).unwrap_err();
    assert!(!err.forbidden);
    assert!(
        err.message.contains("clone of another repository"),
        "{err:?}"
    );
    // The path is compared as it is: another case is another repository.
    let target = r.checkout("case", &origin("https://github.com/Acme/Tools.git"));
    assert!(r.run(URL, &target).is_err());
    // Another host.
    let target = r.checkout("host", &origin("https://gitlab.com/acme/tools.git"));
    assert!(r.run(URL, &target).is_err());
}

#[test]
fn a_git_file_is_not_a_plain_clone() {
    let r = Roots::new();
    let target = r.root.join("linked");
    fs::create_dir(&target).unwrap();
    fs::write(target.join(".git"), "gitdir: /elsewhere\n").unwrap();
    let err = r.run(URL, &target).unwrap_err();
    assert!(!err.forbidden);
    assert!(err.message.contains("is not a plain clone"), "{err:?}");
}

#[test]
fn a_plain_directory_is_not_a_git_repository() {
    let r = Roots::new();
    let target = r.root.join("plain");
    fs::create_dir(&target).unwrap();
    let err = r.run(URL, &target).unwrap_err();
    assert!(!err.forbidden);
    assert!(err.message.contains("is not a Git repository"), "{err:?}");
}

#[test]
fn a_file_at_the_target_is_not_a_git_repository() {
    let r = Roots::new();
    let target = r.root.join("file");
    fs::write(&target, "x").unwrap();
    assert!(r.run(URL, &target).is_err());
}

#[test]
fn a_symlinked_target_is_refused() {
    let r = Roots::new();
    let real = r.checkout("real", &origin(URL));
    let link = r.root.join("link");
    symlink(&real, &link).unwrap();
    let err = r.run(URL, &link).unwrap_err();
    assert!(err.forbidden, "{err:?}");
}

#[test]
fn a_target_outside_the_roots_is_forbidden() {
    let r = Roots::new();
    let outside = r.dir.path().canonicalize().unwrap().join("elsewhere");
    fs::create_dir_all(outside.join(".git")).unwrap();
    fs::write(outside.join(".git").join("config"), origin(URL)).unwrap();
    let err = r.run(URL, &outside).unwrap_err();
    assert!(err.forbidden, "{err:?}");
    // A new target outside the roots is forbidden by the clone plan.
    let err = r.run(URL, &outside.join("new")).unwrap_err();
    assert!(err.forbidden, "{err:?}");
}

#[test]
fn a_config_without_an_origin_url_is_refused() {
    let r = Roots::new();
    let none = r.checkout("none", "[core]\n\tbare = false\n");
    let err = r.run(URL, &none).unwrap_err();
    assert!(err.message.contains("has no origin remote"), "{err:?}");
    // Another remote is not the origin; a `url` after the next section is
    // not the origin's.
    let other = r.checkout(
        "other",
        &format!("[remote \"up\"]\n\turl = {URL}\n[remote \"origin\"]\n\tfetch = x\n[branch \"main\"]\n\turl = {URL}\n"),
    );
    let err = r.run(URL, &other).unwrap_err();
    assert!(err.message.contains("has no origin remote"), "{err:?}");
}

#[test]
fn an_unreadable_config_is_refused() {
    let r = Roots::new();
    let big = r.checkout("big", &"#".repeat(64 * 1024 + 1));
    let err = r.run(URL, &big).unwrap_err();
    assert!(err.message.contains("unreadable Git config"), "{err:?}");
    let utf8 = r.checkout("utf8", "");
    fs::write(utf8.join(".git").join("config"), [0xff, 0xfe, 0x00]).unwrap();
    let err = r.run(URL, &utf8).unwrap_err();
    assert!(err.message.contains("unreadable Git config"), "{err:?}");
    let missing = r.root.join("missing");
    fs::create_dir_all(missing.join(".git")).unwrap();
    let err = r.run(URL, &missing).unwrap_err();
    assert!(err.message.contains("unreadable Git config"), "{err:?}");
}

#[test]
fn a_config_of_exactly_64_kib_is_read() {
    let r = Roots::new();
    let head = origin(URL);
    let pad = "#".repeat(64 * 1024 - head.len());
    let target = r.checkout("edge", &format!("{head}{pad}"));
    assert_eq!(
        fs::metadata(target.join(".git/config")).unwrap().len(),
        64 * 1024
    );
    assert!(matches!(r.run(URL, &target), Ok(CloneTarget::Reuse(_))));
}

#[test]
fn normalize_makes_one_form_of_a_url() {
    let n = normalize;
    assert_eq!(
        n("https://GitHub.com/acme/tools.git"),
        "github.com/acme/tools"
    );
    assert_eq!(
        n("https://user@github.com:8443/acme/tools/"),
        "github.com/acme/tools"
    );
    assert_eq!(
        n("ssh://git@github.com:22/acme/tools.git"),
        "github.com/acme/tools"
    );
    assert_eq!(n("git@GitHub.com:acme/tools.git"), "github.com/acme/tools");
    assert_eq!(n("github.com:acme/tools"), "github.com/acme/tools");
    assert_eq!(n("ssh://git@[::1]:22/acme/tools.git"), "::1/acme/tools");
    assert_eq!(n("file:///srv/tools"), "/srv/tools");
    assert_eq!(n("/srv/tools.git"), "/srv/tools");
}
