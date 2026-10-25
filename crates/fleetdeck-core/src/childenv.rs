//! The environment of child processes (PLAN §3 rule 6, §6.7, lesson L10).
//!
//! A child (provider CLI or owner shell) never inherits the station's
//! environment as is. [`ChildEnv::scrubbed`] keeps the owner's normal
//! environment (`PATH`, `HOME`, locale, git and SSH settings) and removes:
//!
//! - every ambient provider credential and routing variable
//!   (`ANTHROPIC_*`, `CLAUDE_*`, `OPENAI_*`, `CODEX_*`): a provider instance
//!   gets only what its own `[[provider_instance]]` entry configures, so two
//!   instances never share an account by accident (L10);
//! - the session markers of an agent that started the station
//!   (`CLAUDECODE`, `AI_AGENT`), so a child does not think it is nested;
//! - the station's own settings (`FLEETDECK_*`);
//! - names that are not valid UTF-8 (they cannot be checked).
//!
//! [`ChildEnv::for_instance`] then adds the instance's `config_home`
//! (`CLAUDE_CONFIG_DIR` / `CODEX_HOME`) and its `env` table. The `Debug`
//! output lists names only: values can hold credentials.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;

use crate::config::ProviderInstance;

/// Exact names removed from every child environment.
pub const SCRUBBED_NAMES: &[&str] = &["CLAUDECODE", "AI_AGENT"];

/// Name prefixes removed from every child environment.
pub const SCRUBBED_PREFIXES: &[&str] =
    &["ANTHROPIC_", "CLAUDE_", "OPENAI_", "CODEX_", "FLEETDECK_"];

/// True if the scrub removes `name` from an inherited environment.
#[must_use]
pub fn is_scrubbed(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return true;
    };
    let upper = name.to_ascii_uppercase();
    SCRUBBED_NAMES.contains(&upper.as_str())
        || SCRUBBED_PREFIXES.iter().any(|p| upper.starts_with(p))
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChildEnvError {
    #[error("invalid environment variable name `{0}`")]
    BadName(String),
    #[error("the value of `{0}` contains a NUL byte")]
    BadValue(String),
}

/// The complete environment of one child process. A host clears the
/// inherited environment and sets exactly these variables.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ChildEnv {
    vars: BTreeMap<OsString, OsString>,
}

impl ChildEnv {
    /// An empty environment.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// `vars` without the scrubbed names (module docs).
    pub fn scrubbed(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Self {
        Self {
            vars: vars.into_iter().filter(|(k, _)| !is_scrubbed(k)).collect(),
        }
    }

    /// The station's own environment, scrubbed.
    #[must_use]
    pub fn from_process() -> Self {
        Self::scrubbed(std::env::vars_os())
    }

    /// `base` plus what `instance` configures: its `config_home` variable
    /// and its `env` table (these may set provider variables again).
    pub fn for_instance(
        mut base: Self,
        instance: &ProviderInstance,
    ) -> Result<Self, ChildEnvError> {
        if let Some(home) = &instance.config_home {
            base.set(instance.driver.config_home_var(), home.as_os_str())?;
        }
        for (name, value) in &instance.env {
            base.set(name, value.expose())?;
        }
        Ok(base)
    }

    /// Sets one variable. The name must be non-empty and contain no `=` or
    /// NUL; the value must contain no NUL.
    pub fn set(
        &mut self,
        name: impl AsRef<OsStr>,
        value: impl AsRef<OsStr>,
    ) -> Result<(), ChildEnvError> {
        let (name, value) = (name.as_ref(), value.as_ref());
        let shown = || name.to_string_lossy().into_owned();
        let bytes = name.as_encoded_bytes();
        if bytes.is_empty() || bytes.contains(&b'=') || bytes.contains(&0) {
            return Err(ChildEnvError::BadName(shown()));
        }
        if value.as_encoded_bytes().contains(&0) {
            return Err(ChildEnvError::BadValue(shown()));
        }
        self.vars.insert(name.to_owned(), value.to_owned());
        Ok(())
    }

    /// Removes one variable.
    pub fn remove(&mut self, name: impl AsRef<OsStr>) {
        self.vars.remove(name.as_ref());
    }

    #[must_use]
    pub fn get(&self, name: impl AsRef<OsStr>) -> Option<&OsStr> {
        self.vars.get(name.as_ref()).map(OsString::as_os_str)
    }

    /// The names, sorted.
    pub fn names(&self) -> impl Iterator<Item = &OsStr> {
        self.vars.keys().map(OsString::as_os_str)
    }

    /// The variables, sorted by name.
    pub fn iter(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> {
        self.vars
            .iter()
            .map(|(k, v)| (k.as_os_str(), v.as_os_str()))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.vars.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.vars.is_empty()
    }
}

impl fmt::Debug for ChildEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChildEnv")
            .field("names", &self.vars.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::Driver;
    use crate::secret::Secret;

    fn vars(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect()
    }

    #[test]
    fn ambient_provider_credentials_and_markers_are_removed() {
        let env = ChildEnv::scrubbed(vars(&[
            ("PATH", "/usr/bin"),
            ("HOME", "/home/o"),
            ("LANG", "C.UTF-8"),
            ("SSH_AUTH_SOCK", "/tmp/agent"),
            ("GIT_AUTHOR_NAME", "Owner"),
            ("ANTHROPIC_API_KEY", "sk-ant-1"),
            ("ANTHROPIC_BASE_URL", "http://proxy"),
            ("ANTHROPIC_AUTH_TOKEN", "t"),
            ("CLAUDE_CODE_OAUTH_TOKEN", "o"),
            ("CLAUDE_CONFIG_DIR", "/other/account"),
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_SESSION_ID", "s"),
            ("AI_AGENT", "claude"),
            ("OPENAI_API_KEY", "sk-1"),
            ("CODEX_HOME", "/other/codex"),
            ("FLEETDECK_LOG", "debug"),
            ("fleetdeck_lower", "x"),
        ]));
        let names: Vec<_> = env
            .names()
            .map(|n| n.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            ["GIT_AUTHOR_NAME", "HOME", "LANG", "PATH", "SSH_AUTH_SOCK"]
        );
    }

    #[test]
    fn non_utf8_names_are_removed() {
        use std::os::unix::ffi::OsStringExt;
        let bad = OsString::from_vec(vec![b'X', 0xff]);
        let env = ChildEnv::scrubbed([(bad, OsString::from("v")), ("OK".into(), "1".into())]);
        assert_eq!(env.len(), 1);
        assert_eq!(env.get("OK"), Some(OsStr::new("1")));
    }

    #[test]
    fn the_instance_adds_back_only_its_own_settings() {
        let base = ChildEnv::scrubbed(vars(&[("PATH", "/bin"), ("ANTHROPIC_API_KEY", "ambient")]));
        let mut work = ProviderInstance::default_for(Driver::Claude);
        work.config_home = Some(PathBuf::from("/home/o/.claude-work"));
        work.env
            .insert("ANTHROPIC_API_KEY".into(), Secret::new("work-key".into()));
        let env = ChildEnv::for_instance(base.clone(), &work).unwrap();
        assert_eq!(env.get("ANTHROPIC_API_KEY"), Some(OsStr::new("work-key")));
        assert_eq!(
            env.get("CLAUDE_CONFIG_DIR"),
            Some(OsStr::new("/home/o/.claude-work"))
        );

        let codex = ProviderInstance::default_for(Driver::Codex);
        let env = ChildEnv::for_instance(base, &codex).unwrap();
        assert_eq!(env.get("ANTHROPIC_API_KEY"), None);
        assert_eq!(env.get("CODEX_HOME"), None);
        assert_eq!(env.get("PATH"), Some(OsStr::new("/bin")));
    }

    #[test]
    fn bad_names_and_values_are_refused() {
        let mut env = ChildEnv::empty();
        assert!(matches!(env.set("", "v"), Err(ChildEnvError::BadName(_))));
        assert!(matches!(
            env.set("A=B", "v"),
            Err(ChildEnvError::BadName(_))
        ));
        assert!(matches!(
            env.set("A\0", "v"),
            Err(ChildEnvError::BadName(_))
        ));
        assert!(matches!(
            env.set("A", "v\0w"),
            Err(ChildEnvError::BadValue(_))
        ));
        assert!(env.is_empty());
    }

    #[test]
    fn debug_output_shows_names_but_not_values() {
        let mut env = ChildEnv::empty();
        env.set("API_KEY", "very-secret").unwrap();
        let shown = format!("{env:?}");
        assert!(
            shown.contains("API_KEY") && !shown.contains("very-secret"),
            "{shown}"
        );
    }
}
