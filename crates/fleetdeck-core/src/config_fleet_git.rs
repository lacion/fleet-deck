//! The Fleet Git keys of `station.toml` (design §3.4, FG-5, FG-6, FG-12):
//! `clone_root`, `traits`, `allow_check_cmd`, `allow_local_remotes` and
//! `[taildrive_mounts]`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{ConfigError, invalid};
use crate::paths::{PathError, expand_path};

/// The most `traits` labels and the most `taildrive_mounts` entries.
const MAX_ENTRIES: usize = 16;

/// The parsed keys (FG-5, FG-6, FG-12).
#[derive(Debug, Default)]
pub(super) struct FleetGitKeys {
    pub(super) clone_root: Option<PathBuf>,
    pub(super) traits: Vec<String>,
    pub(super) allow_check_cmd: bool,
    pub(super) allow_local_remotes: bool,
    pub(super) taildrive_mounts: BTreeMap<String, PathBuf>,
}

/// The raw values of the keys.
pub(super) struct RawFleetGit {
    pub(super) clone_root: Option<String>,
    pub(super) traits: Option<Vec<String>>,
    pub(super) allow_check_cmd: Option<bool>,
    pub(super) allow_local_remotes: Option<bool>,
    pub(super) taildrive_mounts: Option<BTreeMap<String, String>>,
}

fn path_error(field: &str, source: PathError) -> ConfigError {
    ConfigError::Path {
        field: field.to_owned(),
        source,
    }
}

/// Parses the keys. `allowed_roots` are the expanded roots of the config.
pub(super) fn parse(
    raw: RawFleetGit,
    home: Option<&Path>,
    allowed_roots: &[PathBuf],
) -> Result<FleetGitKeys, ConfigError> {
    let clone_root = match raw.clone_root.filter(|c| !c.is_empty()) {
        None => None,
        Some(text) => {
            let path =
                expand_path(&text, home).map_err(|source| path_error("clone_root", source))?;
            if !allowed_roots.iter().any(|root| path.starts_with(root)) {
                return Err(invalid(
                    "clone_root",
                    "the clone folder must be inside an allowed root",
                ));
            }
            Some(path)
        }
    };
    let traits = raw.traits.unwrap_or_default();
    if traits.len() > MAX_ENTRIES {
        return Err(invalid("traits", "give at most 16 labels"));
    }
    for (i, label) in traits.iter().enumerate() {
        if !valid_label(label) {
            return Err(invalid(
                format!("traits[{i}]"),
                "use 1-32 of a-z, 0-9, `_`, `-`",
            ));
        }
        if traits[..i].contains(label) {
            return Err(invalid(
                format!("traits[{i}]"),
                "Each station label at most once.",
            ));
        }
    }
    let mut taildrive_mounts = BTreeMap::new();
    let mounts = raw.taildrive_mounts.unwrap_or_default();
    if mounts.len() > MAX_ENTRIES {
        return Err(invalid("taildrive_mounts", "give at most 16 entries"));
    }
    for (key, value) in mounts {
        if !valid_share_key(&key) {
            return Err(invalid(
                "taildrive_mounts",
                "use <machine>/<share> as the key, for example nas/artifacts",
            ));
        }
        if value.contains('\0') {
            return Err(invalid("taildrive_mounts", "a mount path has a NUL"));
        }
        let path =
            expand_path(&value, home).map_err(|source| path_error("taildrive_mounts", source))?;
        taildrive_mounts.insert(key, path);
    }
    Ok(FleetGitKeys {
        clone_root,
        traits,
        allow_check_cmd: raw.allow_check_cmd.unwrap_or(false),
        allow_local_remotes: raw.allow_local_remotes.unwrap_or(false),
        taildrive_mounts,
    })
}

/// A station label: 1 to 32 of `[a-z0-9_-]` (design §9.1, O19).
fn valid_label(label: &str) -> bool {
    (1..=32).contains(&label.len())
        && label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

/// `<machine>/<share>`. The same rule as `share_ok` in
/// `fleetdeck_proto::fleet_git` (a private function there): machine
/// `[a-z0-9-]{1,63}`; share 1 to 64 of `[a-z0-9_() ]`, no leading or
/// trailing space.
fn valid_share_key(key: &str) -> bool {
    let Some((machine, share)) = key.split_once('/') else {
        return false;
    };
    (1..=63).contains(&machine.len())
        && machine
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && (1..=64).contains(&share.len())
        && !share.starts_with(' ')
        && !share.ends_with(' ')
        && share.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'(' | b')' | b' ')
        })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::super::{ConfigError, StationConfig};
    use crate::paths::Env;

    fn env() -> Env {
        Env {
            home: Some("/home/ana".into()),
            ..Env::default()
        }
    }

    fn parse(text: &str) -> Result<StationConfig, ConfigError> {
        StationConfig::parse(text, &env())
    }

    fn msg(text: &str) -> String {
        parse(text).unwrap_err().to_string()
    }

    #[test]
    fn the_defaults_are_off_and_empty() {
        let c = parse("").unwrap();
        assert_eq!(c.clone_root, None);
        assert!(c.traits.is_empty() && c.taildrive_mounts.is_empty());
        assert!(!c.allow_check_cmd && !c.allow_local_remotes);
    }

    #[test]
    fn clone_root_is_inside_an_allowed_root_and_empty_is_unset() {
        let ok = "allowed_roots = [\"/work\"]\nclone_root = \"/work/repos\"\n";
        assert_eq!(parse(ok).unwrap().clone_root, Some("/work/repos".into()));
        let equal = "allowed_roots = [\"/work\"]\nclone_root = \"/work\"\n";
        assert!(parse(equal).is_ok());
        let home = "clone_root = \"~/projects\"\n";
        assert_eq!(
            parse(home).unwrap().clone_root,
            Some("/home/ana/projects".into())
        );
        assert_eq!(parse("clone_root = \"\"\n").unwrap().clone_root, None);
        for bad in [
            "allowed_roots = [\"/work\"]\nclone_root = \"/other\"\n",
            "allowed_roots = [\"/work\"]\nclone_root = \"/work/../etc\"\n",
            "allowed_roots = [\"/work\"]\nclone_root = \"rel\"\n",
        ] {
            assert!(msg(bad).contains("clone_root"), "{bad}");
        }
    }

    #[test]
    fn traits_are_labels_at_most_16_and_unique() {
        let c = parse("traits = [\"gpu\", \"always_on\", \"a-1\"]\n").unwrap();
        assert_eq!(c.traits, ["gpu", "always_on", "a-1"]);
        for bad in [
            "traits = [\"GPU\"]\n",
            "traits = [\"\"]\n",
            "traits = [\"a b\"]\n",
        ] {
            assert!(msg(bad).contains("traits[0]"), "{bad}");
        }
        let long = format!("traits = [\"{}\"]\n", "a".repeat(33));
        assert!(parse(&long).is_err());
        assert!(msg("traits = [\"gpu\", \"gpu\"]\n").contains("at most once"));
        let many: Vec<String> = (0..17).map(|i| format!("\"l{i}\"")).collect();
        assert!(msg(&format!("traits = [{}]\n", many.join(","))).contains("at most 16"));
    }

    #[test]
    fn taildrive_mounts_have_a_share_key_and_an_absolute_path() {
        let c =
            parse("taildrive_mounts = { \"nas/out\" = \"/mnt/out\", \"nb/a (b)\" = \"~/m\" }\n")
                .unwrap();
        assert_eq!(c.taildrive_mounts["nas/out"], Path::new("/mnt/out"));
        assert_eq!(c.taildrive_mounts["nb/a (b)"], Path::new("/home/ana/m"));
        for bad in [
            "taildrive_mounts = { \"nas\" = \"/m\" }\n",
            "taildrive_mounts = { \"NAS/out\" = \"/m\" }\n",
            "taildrive_mounts = { \"nas/ out\" = \"/m\" }\n",
            "taildrive_mounts = { \"nas/out\" = \"rel\" }\n",
            "taildrive_mounts = { \"nas/out\" = \"/m/../x\" }\n",
        ] {
            assert!(msg(bad).contains("taildrive_mounts"), "{bad}");
        }
        let many: Vec<String> = (0..17).map(|i| format!("\"m/s{i}\" = \"/m{i}\"")).collect();
        let text = format!("taildrive_mounts = {{ {} }}\n", many.join(","));
        assert!(msg(&text).contains("at most 16"));
    }

    #[test]
    fn the_two_flags_parse() {
        let c = parse("allow_check_cmd = true\nallow_local_remotes = true\n").unwrap();
        assert!(c.allow_check_cmd && c.allow_local_remotes);
        assert!(parse("allow_check_cmd = \"yes\"\n").is_err());
    }
}
