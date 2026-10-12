//! User input for a prompt, a steer or a follow-up: text and pasted images
//! (PLAN Phase 6 "Screenshot/image paste into prompts", DL-304).
//!
//! An image travels inline as base64 inside the command. The whole command
//! goes to the station in one control frame (`MAX_CONTROL_FRAME_BYTES`), so
//! the images of one input stay below [`MAX_IMAGE_BYTES`] together.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::command::SessionCommand;

/// The most images in one input.
pub const MAX_IMAGES: usize = 4;
/// The most base64 characters of all images of one input together. This
/// keeps a command with images below the 1 MiB control frame.
pub const MAX_IMAGE_BYTES: usize = 768 * 1024;
/// The image types that both providers accept.
pub const IMAGE_MEDIA_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// One pasted image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "command/")]
pub struct ImageInput {
    /// One of [`IMAGE_MEDIA_TYPES`].
    pub media_type: String,
    /// Standard base64 without a `data:` prefix.
    pub data: String,
}

impl ImageInput {
    /// A `data:` URL of the image.
    #[must_use]
    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.media_type, self.data)
    }
}

/// User text for a prompt, a steer, or a follow-up, with optional images.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "command/")]
pub struct UserInput {
    pub text: String,
    /// Pasted images, in paste order. Old peers send no field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<ImageInput>>")]
    pub images: Vec<ImageInput>,
}

impl UserInput {
    /// Text without images.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            images: Vec::new(),
        }
    }

    /// Checks the image limits, the media types and the base64 alphabet.
    ///
    /// # Errors
    /// A message that names the first broken limit.
    pub fn check(&self) -> Result<(), String> {
        if self.images.len() > MAX_IMAGES {
            return Err(format!(
                "an input has at most {MAX_IMAGES} images, not {}",
                self.images.len()
            ));
        }
        let total: usize = self.images.iter().map(|i| i.data.len()).sum();
        if total > MAX_IMAGE_BYTES {
            return Err(format!(
                "the images of an input have at most {MAX_IMAGE_BYTES} base64 bytes, not {total}"
            ));
        }
        for image in &self.images {
            if !IMAGE_MEDIA_TYPES.contains(&image.media_type.as_str()) {
                return Err(format!(
                    "the image type `{}` is not supported",
                    image.media_type
                ));
            }
            if image.data.is_empty() || !is_base64(&image.data) {
                return Err("an image is not standard base64".to_owned());
            }
        }
        Ok(())
    }
}

fn is_base64(s: &str) -> bool {
    s.len().is_multiple_of(4)
        && s.trim_end_matches('=').len() + 2 >= s.len()
        && s.trim_end_matches('=')
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
}

impl SessionCommand {
    /// The user input that the command carries, if any.
    #[must_use]
    pub fn user_input(&self) -> Option<&UserInput> {
        match self {
            Self::Start { prompt, .. } => prompt.as_ref(),
            Self::Prompt { input, .. }
            | Self::Steer { input, .. }
            | Self::FollowUp { input, .. } => Some(input),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(data: &str) -> ImageInput {
        ImageInput {
            media_type: "image/png".to_owned(),
            data: data.to_owned(),
        }
    }

    #[test]
    fn an_input_without_images_keeps_the_old_wire_form() {
        let json = serde_json::to_string(&UserInput::new("hi")).unwrap();
        assert_eq!(json, r#"{"text":"hi"}"#);
        let back: UserInput = serde_json::from_str(r#"{"text":"hi"}"#).unwrap();
        assert_eq!(back, UserInput::new("hi"));
    }

    #[test]
    fn image_limits_refuse_bad_types_bad_base64_and_too_many_bytes() {
        let mut input = UserInput::new("look");
        input.images.push(png("iVBORw0KGgo="));
        assert_eq!(input.check(), Ok(()));
        assert_eq!(
            input.images[0].data_url(),
            "data:image/png;base64,iVBORw0KGgo="
        );

        let mut bad_type = input.clone();
        bad_type.images[0].media_type = "image/svg+xml".to_owned();
        assert!(bad_type.check().unwrap_err().contains("image/svg+xml"));

        let mut bad_data = input.clone();
        bad_data.images[0].data = "not base64!".to_owned();
        assert!(bad_data.check().is_err());

        let mut too_many = input.clone();
        too_many.images = vec![png("AAAA"); MAX_IMAGES + 1];
        assert!(too_many.check().unwrap_err().contains("at most"));

        let mut too_big = input;
        too_big.images = vec![png(&"A".repeat(MAX_IMAGE_BYTES + 4))];
        assert!(too_big.check().unwrap_err().contains("base64 bytes"));
    }
}
