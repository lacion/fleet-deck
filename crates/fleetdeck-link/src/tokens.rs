//! Single-use stream tokens (ADR-004 §9.2, G2 and G6).
//!
//! A control response that authorizes a stream (for example `artifact.open`)
//! returns a token: 128 random bits, bound to the `attempt_id` of the link
//! that carried the request, valid for [`TOKEN_TTL`], usable once. The peer
//! sends the token in the `open` frame of the data channel; the data channel
//! redeems it here. An unknown, used, expired or foreign token resets the
//! stream with [`BAD_STREAM_TOKEN`]. A foreign token is used up too, so a
//! token that leaked to another link is worth nothing afterwards.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::time::Instant;

/// How long an issued token stays valid.
pub const TOKEN_TTL: Duration = Duration::from_secs(30);
/// The stream reset reason for a token that cannot open a stream.
pub const BAD_STREAM_TOKEN: &str = "bad_stream_token";
/// Valid tokens that one node holds at most; expired ones do not count.
pub const MAX_TOKENS: usize = 4096;

/// A stream token. Its text is a capability: `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct StreamToken(String);

impl StreamToken {
    /// The text for the control response and the `open` frame.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for StreamToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StreamToken(<redacted>)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    #[error("{MAX_TOKENS} stream tokens are already open")]
    TooMany,
    #[error("the system random generator failed")]
    Random,
}

/// Why a token did not open a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Unknown,
    Expired,
    Foreign,
}

#[derive(Debug)]
struct Issued {
    attempt_id: String,
    purpose: String,
    expires: Instant,
}

/// The tokens that one node issued, for all of its links. Cheap to clone.
#[derive(Clone, Default)]
pub struct StreamTokens {
    issued: Arc<Mutex<HashMap<String, Issued>>>,
}

impl fmt::Debug for StreamTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamTokens")
            .field("open", &self.lock().len())
            .finish()
    }
}

impl StreamTokens {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Issued>> {
        self.issued.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Issues a token for one stream on the link attempt `attempt_id`.
    /// `purpose` is what the stream serves; the data channel gives it to the
    /// application when the peer opens the stream.
    pub fn issue(&self, attempt_id: &str, purpose: &str) -> Result<StreamToken, TokenError> {
        let mut bytes = [0u8; 16];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
            .map_err(|_| TokenError::Random)?;
        let text: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let now = Instant::now();
        let mut issued = self.lock();
        issued.retain(|_, i| i.expires > now);
        if issued.len() >= MAX_TOKENS {
            return Err(TokenError::TooMany);
        }
        issued.insert(
            text.clone(),
            Issued {
                attempt_id: attempt_id.to_owned(),
                purpose: purpose.to_owned(),
                expires: now + TOKEN_TTL,
            },
        );
        Ok(StreamToken(text))
    }

    /// Uses up `token` on the link attempt `attempt_id`; returns its purpose.
    pub fn redeem(&self, attempt_id: &str, token: &str) -> Result<String, Refusal> {
        let issued = self.lock().remove(token).ok_or(Refusal::Unknown)?;
        if issued.expires <= Instant::now() {
            return Err(Refusal::Expired);
        }
        if issued.attempt_id != attempt_id {
            return Err(Refusal::Foreign);
        }
        Ok(issued.purpose)
    }

    /// Tokens that were issued and not used (expired ones included until the
    /// next `issue`).
    #[must_use]
    pub fn open(&self) -> usize {
        self.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_token_opens_one_stream_on_its_own_attempt_within_30_seconds() {
        let tokens = StreamTokens::default();
        let t = tokens.issue("att-a", "artifact:1").unwrap();
        assert_eq!(t.as_str().len(), 32);
        assert_eq!(format!("{t:?}"), "StreamToken(<redacted>)");
        assert_eq!(
            tokens.redeem("att-a", t.as_str()),
            Ok("artifact:1".to_owned())
        );
        assert_eq!(
            tokens.redeem("att-a", t.as_str()),
            Err(Refusal::Unknown),
            "used"
        );
        assert_eq!(tokens.redeem("att-a", "00"), Err(Refusal::Unknown));

        let t = tokens.issue("att-a", "p").unwrap();
        assert_eq!(tokens.redeem("att-b", t.as_str()), Err(Refusal::Foreign));
        assert_eq!(
            tokens.redeem("att-a", t.as_str()),
            Err(Refusal::Unknown),
            "burned"
        );

        let t = tokens.issue("att-a", "p").unwrap();
        tokio::time::advance(TOKEN_TTL).await;
        assert_eq!(tokens.redeem("att-a", t.as_str()), Err(Refusal::Expired));
        let _ = tokens.issue("att-a", "p").unwrap();
        tokio::time::advance(TOKEN_TTL).await;
        let _ = tokens.issue("att-a", "p").unwrap();
        assert_eq!(
            tokens.open(),
            1,
            "expired tokens are dropped at the next issue"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn at_most_4096_tokens_are_open() {
        let tokens = StreamTokens::default();
        for _ in 0..MAX_TOKENS {
            tokens.issue("a", "p").unwrap();
        }
        assert_eq!(tokens.issue("a", "p"), Err(TokenError::TooMany));
        tokio::time::advance(TOKEN_TTL).await;
        assert!(tokens.issue("a", "p").is_ok());
    }
}
