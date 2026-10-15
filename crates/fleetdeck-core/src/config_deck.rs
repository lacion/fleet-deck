//! The deck setting types of `deck.toml`.

/// How a clone reaches the Git host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloneTransport {
    /// `https://host/path.git`, with the Git credential helper.
    Https,
    /// `git@host:path.git`, with the SSH keys.
    Ssh,
}

impl CloneTransport {
    /// The text form, as in `deck.toml`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Https => "https",
            Self::Ssh => "ssh",
        }
    }
}
