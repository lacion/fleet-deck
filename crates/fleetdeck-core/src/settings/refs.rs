//! Secret references: `{ secret = "<name>" }` in a config file points to a
//! file of the secret store (`<station data>/secrets/<name>`, mode 0600).
//! No message of this module holds a secret value.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// The longest secret name.
pub const MAX_SECRET_NAME: usize = 96;

/// `[a-z0-9-]{1,96}`. It cannot name a path, so a reference cannot leave
/// the store.
#[must_use]
pub fn valid_secret_name(name: &str) -> bool {
    (1..=MAX_SECRET_NAME).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Reads the value of a reference.
///
/// # Errors
/// A fixed description (it holds the secret name, never a value): no store,
/// a bad name, a missing file, a link, a file that is not a regular file,
/// a file that is not mode 0600, or a file that is not UTF-8 text.
pub fn read_reference(store: Option<&Path>, name: &str) -> Result<String, String> {
    let store = store.ok_or_else(|| {
        "a secret reference needs the secret store; start Fleet Deck normally".to_owned()
    })?;
    if !valid_secret_name(name) {
        return Err("the secret name must be 1-96 of a-z, 0-9, `-`".to_owned());
    }
    let path = store.join(name);
    let link = fs::symlink_metadata(&path)
        .map_err(|e| format!("the secret `{name}` cannot be read ({})", e.kind()))?;
    if link.file_type().is_symlink() || !link.file_type().is_file() {
        return Err(format!("the secret `{name}` is not a regular file"));
    }
    let mode = link.permissions().mode() & 0o777;
    if mode != 0o600 {
        return Err(format!(
            "the secret `{name}` has mode {mode:o}; use mode 600"
        ));
    }
    fs::read_to_string(&path)
        .map_err(|e| format!("the secret `{name}` cannot be read ({})", e.kind()))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn names() {
        assert!(valid_secret_name("provider-claude-api-key-0a1b2c3d"));
        assert!(!valid_secret_name(""));
        assert!(!valid_secret_name("Upper"));
        assert!(!valid_secret_name("a_b"));
        assert!(!valid_secret_name("../x"));
        assert!(!valid_secret_name(&"a".repeat(97)));
        assert!(valid_secret_name(&"a".repeat(96)));
    }

    #[test]
    fn reads_only_private_regular_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        assert!(read_reference(None, "x").is_err());
        assert!(read_reference(Some(p), "missing").is_err());
        fs::write(p.join("open"), "v").unwrap();
        fs::set_permissions(p.join("open"), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_reference(Some(p), "open").unwrap_err().contains("600"));
        fs::write(p.join("ok"), "v-1").unwrap();
        fs::set_permissions(p.join("ok"), fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_reference(Some(p), "ok").unwrap(), "v-1");
        symlink(p.join("ok"), p.join("link")).unwrap();
        assert!(read_reference(Some(p), "link").is_err());
        assert!(read_reference(Some(p), "../ok").is_err());
    }
}
