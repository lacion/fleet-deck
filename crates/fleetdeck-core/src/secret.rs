//! [`Secret<T>`]: a value that must never reach a log, a wire frame or the
//! board (doctrine; GATE-M1 G12).
//!
//! - `Debug` prints `Secret(<redacted>)`.
//! - It has no `Display`, `Serialize`, `Clone` or `PartialEq`. To use the
//!   value, call [`Secret::expose`]; the call is easy to find in review.
//! - Comparison is [`Secret::ct_eq`] (constant time) for byte-like values.

use std::fmt;

/// A secret value. See the module documentation.
pub struct Secret<T>(T);

impl<T> Secret<T> {
    pub const fn new(value: T) -> Self {
        Self(value)
    }

    /// The secret value. Do not log or serialize it.
    pub fn expose(&self) -> &T {
        &self.0
    }

    /// Consumes the wrapper and returns the value.
    pub fn into_exposed(self) -> T {
        self.0
    }
}

impl<T: AsRef<[u8]>> Secret<T> {
    /// Constant-time equality with another byte string. The length is not
    /// secret: values of different lengths are unequal at once.
    pub fn ct_eq(&self, other: &[u8]) -> bool {
        ct_eq(self.0.as_ref(), other)
    }
}

/// Constant-time comparison of two byte strings of the same length.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let diff = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(diff) == 0
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl From<String> for Secret<String> {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_is_redacted() {
        let s = Secret::new("hunter2".to_owned());
        assert_eq!(format!("{s:?}"), "Secret(<redacted>)");
        assert_eq!(format!("{s:#?}"), "Secret(<redacted>)");
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Holder {
            token: Secret<String>,
        }
        let text = format!(
            "{:?}",
            Holder {
                token: Secret::new("abc123".into())
            }
        );
        assert!(!text.contains("abc123"), "{text}");
    }

    /// Autoref probe: the inherent method exists only for `Serialize` types.
    struct Probe<T>(std::marker::PhantomData<T>);
    impl<T: serde::Serialize> Probe<T> {
        fn is_serialize(&self) -> bool {
            true
        }
    }
    trait Fallback {
        fn is_serialize(&self) -> bool {
            false
        }
    }
    impl<T> Fallback for &T {}

    #[test]
    #[allow(clippy::needless_borrow)] // both probes need the same receiver shape
    fn secrets_cannot_be_serialized() {
        assert!((&Probe::<String>(std::marker::PhantomData)).is_serialize());
        assert!(!(&Probe::<Secret<String>>(std::marker::PhantomData)).is_serialize());
    }

    #[test]
    fn expose_and_compare() {
        let s = Secret::new(b"token-1".to_vec());
        assert_eq!(s.expose(), b"token-1");
        assert!(s.ct_eq(b"token-1"));
        assert!(!s.ct_eq(b"token-2"));
        assert!(!s.ct_eq(b"token"));
        assert_eq!(s.into_exposed(), b"token-1".to_vec());
    }
}
