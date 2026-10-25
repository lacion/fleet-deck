//! The provisioning file of a new station (P8, design `omarchy-p8.md` 2.3):
//! the deck address and link key fingerprint and the station name, as TOML.
//!
//! The file is written by `fleetdeck deck provision-file` and read by later
//! P8 commands. It has exactly four keys. An unknown key, a file over
//! [`MAX_BYTES`], a version other than 1 and a fingerprint that is not 64
//! lowercase hex digits are errors.

use serde::{Deserialize, Serialize};

/// The largest provisioning file that [`parse`] reads.
pub const MAX_BYTES: usize = 16 * 1024;

/// The provisioning file (P8 §2.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvisionFile {
    pub version: u32,
    pub deck: String,
    pub deck_fingerprint: String,
    pub name: String,
}

/// Why [`parse`] refused a provisioning file.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProvisionFileError {
    #[error("the provisioning file is larger than {MAX_BYTES} bytes")]
    TooLarge,
    #[error("the provisioning file is not UTF-8")]
    NotUtf8,
    #[error("the provisioning file is not valid: {0}")]
    Toml(String),
    #[error("the provisioning file version {0} is not supported")]
    Version(u32),
    #[error("the deck fingerprint is not 64 lowercase hex digits")]
    Fingerprint,
}

/// Reads a provisioning file.
///
/// # Errors
/// [`ProvisionFileError`]: over [`MAX_BYTES`], not UTF-8, a TOML error or an
/// unknown or missing key, a version other than 1, or a `deck_fingerprint`
/// that is not 64 lowercase hex digits.
pub fn parse(bytes: &[u8]) -> Result<ProvisionFile, ProvisionFileError> {
    if bytes.len() > MAX_BYTES {
        return Err(ProvisionFileError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ProvisionFileError::NotUtf8)?;
    let file: ProvisionFile =
        toml::from_str(text).map_err(|e| ProvisionFileError::Toml(e.message().to_owned()))?;
    if file.version != 1 {
        return Err(ProvisionFileError::Version(file.version));
    }
    if !is_fingerprint(&file.deck_fingerprint) {
        return Err(ProvisionFileError::Fingerprint);
    }
    Ok(file)
}

fn is_fingerprint(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl ProvisionFile {
    /// The TOML text: the keys `version`, `deck`, `deck_fingerprint`, `name`
    /// in this order, and a final newline.
    ///
    /// A serializer failure gives the empty text, which [`parse`] refuses:
    /// the caller reads the text back before it writes it.
    #[must_use]
    pub fn to_toml(&self) -> String {
        let Ok(mut text) = toml::to_string(self) else {
            return String::new();
        };
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn file() -> ProvisionFile {
        ProvisionFile {
            version: 1,
            deck: "deck.example.ts.net:7421".to_owned(),
            deck_fingerprint: FP.to_owned(),
            name: "laptop".to_owned(),
        }
    }

    #[test]
    fn to_toml_is_exactly_the_four_keys_in_order() {
        assert_eq!(
            file().to_toml(),
            format!(
                "version = 1\ndeck = \"deck.example.ts.net:7421\"\ndeck_fingerprint = \"{FP}\"\nname = \"laptop\"\n"
            )
        );
    }

    #[test]
    fn the_file_reads_back_equal() {
        assert_eq!(parse(file().to_toml().as_bytes()), Ok(file()));
    }

    #[test]
    fn a_name_with_quote_and_backslash_round_trips() {
        let mut f = file();
        f.name = "a\"b\\c".to_owned();
        assert_eq!(parse(f.to_toml().as_bytes()), Ok(f));
    }

    #[test]
    fn an_unknown_key_is_an_error() {
        let text = format!("{}extra = 1\n", file().to_toml());
        assert!(matches!(
            parse(text.as_bytes()),
            Err(ProvisionFileError::Toml(_))
        ));
    }

    #[test]
    fn a_missing_key_is_an_error() {
        assert!(matches!(
            parse(b"version = 1\n"),
            Err(ProvisionFileError::Toml(_))
        ));
    }

    #[test]
    fn a_file_over_16_kib_is_an_error() {
        let mut text = file().to_toml();
        text.push_str(&" ".repeat(MAX_BYTES + 1 - text.len()));
        assert_eq!(text.len(), MAX_BYTES + 1);
        assert_eq!(parse(text.as_bytes()), Err(ProvisionFileError::TooLarge));
        // The limit itself is accepted.
        let ok = &text[..MAX_BYTES];
        assert_eq!(parse(ok.as_bytes()), Ok(file()));
    }

    #[test]
    fn a_file_that_is_not_utf8_is_an_error() {
        assert_eq!(parse(&[0xff, 0xfe]), Err(ProvisionFileError::NotUtf8));
    }

    #[test]
    fn version_two_is_an_error() {
        let mut f = file();
        f.version = 2;
        assert_eq!(
            parse(f.to_toml().as_bytes()),
            Err(ProvisionFileError::Version(2))
        );
    }

    #[test]
    fn a_bad_fingerprint_is_an_error() {
        for bad in [FP.to_uppercase(), FP[..63].to_owned(), format!("{FP}0")] {
            let mut f = file();
            f.deck_fingerprint = bad;
            assert_eq!(
                parse(f.to_toml().as_bytes()),
                Err(ProvisionFileError::Fingerprint)
            );
        }
    }
}
