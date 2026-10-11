//! `Secret<T>` (PLAN §12, §7 threat table): a wrapper for key material and
//! pairing secrets. It does not implement `Serialize`, `Display`, or `Clone`,
//! and its `Debug` output is redacted, so a secret cannot reach a log line, a
//! read model, or the wire by accident. Code that needs the value calls
//! [`Secret::expose`], which is easy to find in a review.

use std::fmt;

pub struct Secret<T>(T);

impl<T> Secret<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// The only way to read the value.
    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_is_redacted() {
        let s = Secret::new("hunter2".to_owned());
        assert_eq!(format!("{s:?}"), "Secret(<redacted>)");
        assert_eq!(s.expose(), "hunter2");
    }
}
