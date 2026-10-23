//! Turning wire [`Attachment`]s into model content parts.
//!
//! Thunder's multimodal pipeline was complete long before anything used it:
//! `ContentPart::Image` (`thunder-protocol`), `ChatMessage::user_multimodal`,
//! `Conversation::add_user_message_with_parts`, and the provider's
//! `part_to_content` -> pi-ai `Content::Image` mapping all existed. Only the
//! daemon was missing: `run_task` and `steer_task` destructured their
//! `attachments` field with `..`, so an app could attach an image, receive
//! `{"started": true}`, and the model would be handed plain text. Nothing was
//! logged, nothing failed — the request simply dropped half of itself.
//!
//! This module closes that hole. Its contract is: **an attachment either becomes
//! a content part, or the caller gets an error naming why it did not.** Two
//! deliberate exceptions, both of which are loud rather than silent:
//!
//! - a non-image mime type is skipped with a `warn!` (models consume images as
//!   content parts; a text file belongs inlined in the prompt by the app, which
//!   knows what it is for);
//! - an attachment that is over the size ceiling fails the whole request, because
//!   the user explicitly attached that image and silently sending the turn
//!   without it is the bug this module exists to prevent.

use crate::Attachment;
use std::path::Path;
use thunder_protocol::ContentPart;

/// One image. Providers bill and cap per image, so the ceiling is per attachment.
pub(crate) const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
/// Per message, so five 10 MiB images cannot become a 50 MiB request body.
pub(crate) const MAX_TOTAL_BYTES: usize = 20 * 1024 * 1024;

/// Convert wire attachments into content parts, in the order they were sent.
///
/// The returned vector contains images only; the caller is responsible for
/// putting the prompt text in front of them (see [`with_leading_text`]).
pub(crate) fn content_parts(attachments: &[Attachment]) -> Result<Vec<ContentPart>, String> {
    let mut parts = Vec::new();
    let mut total_bytes = 0usize;

    for attachment in attachments {
        let label = attachment_label(attachment);

        let mime_type = match resolve_mime_type(attachment) {
            Some(mime) => mime,
            None => {
                return Err(format!(
                    "attachment {label}: cannot tell its media type; send `mimeType` \
                     (or a name/path ending in .png/.jpg/.jpeg/.webp/.gif)"
                ))
            }
        };

        if !mime_type.starts_with("image/") {
            tracing::warn!(
                attachment = %label,
                mime_type = %mime_type,
                "skipping non-image attachment: models consume images as content parts, \
                 and a text payload has to be inlined into the prompt by the host"
            );
            continue;
        }

        let data = match payload(attachment, &label, &mime_type)? {
            Some(data) => data,
            None => continue,
        };

        let bytes = decoded_len(&data).ok_or_else(|| {
            format!(
                "attachment {label}: `data` is not valid base64 (length {} )",
                data.len()
            )
        })?;
        if bytes > MAX_IMAGE_BYTES {
            return Err(format!(
                "attachment {label}: {bytes} bytes exceeds the {MAX_IMAGE_BYTES}-byte per-image limit"
            ));
        }
        total_bytes += bytes;
        if total_bytes > MAX_TOTAL_BYTES {
            return Err(format!(
                "attachments: {total_bytes} bytes in total exceeds the {MAX_TOTAL_BYTES}-byte limit"
            ));
        }

        parts.push(ContentPart::Image {
            mime_type,
            data,
            name: attachment.name.clone(),
            path: attachment.path.clone(),
            sha256: None,
        });
    }

    Ok(parts)
}

/// Put the prompt text in front of the image parts.
///
/// Order matters: an image with no instruction is a question nobody asked.
/// Providers that read `parts` as the whole user turn (see
/// `thunder-providers::client::build_context`) need the text to be a part too,
/// which is why the text is duplicated here instead of relying on
/// `ChatMessage::User::content`.
///
/// No attachments means no parts at all, so a text-only run keeps the exact
/// message shape it had before multimodal support existed.
pub(crate) fn with_leading_text(
    text: impl Into<String>,
    parts: Vec<ContentPart>,
) -> Vec<ContentPart> {
    if parts.is_empty() {
        return parts;
    }
    let mut with_text = Vec::with_capacity(parts.len() + 1);
    with_text.push(ContentPart::text(text));
    with_text.extend(parts);
    with_text
}

/// A stable, human-readable way to point at one attachment in an error.
fn attachment_label(attachment: &Attachment) -> String {
    attachment
        .name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .map(|name| format!("'{name}'"))
        .or_else(|| {
            attachment
                .path
                .as_deref()
                .filter(|path| !path.trim().is_empty())
                .map(|path| format!("'{path}'"))
        })
        .unwrap_or_else(|| "(unnamed)".to_string())
}

/// Explicit `mimeType` wins; otherwise infer the image type from a file name.
fn resolve_mime_type(attachment: &Attachment) -> Option<String> {
    if let Some(mime) = attachment.mime_type.as_deref() {
        let mime = mime.trim().to_ascii_lowercase();
        // Some clients hand over a full data URL as the mime type; recover the type.
        if let Some(stripped) = mime.strip_prefix("data:") {
            let stripped = stripped.split(';').next().unwrap_or_default();
            if !stripped.is_empty() {
                return Some(stripped.to_string());
            }
        }
        if !mime.is_empty() && !mime.starts_with("data:") {
            return Some(mime);
        }
    }

    for candidate in [attachment.path.as_deref(), attachment.name.as_deref()]
        .into_iter()
        .flatten()
    {
        if let Some(mime) = image_mime_from_extension(candidate) {
            return Some(mime.to_string());
        }
    }

    None
}

fn image_mime_from_extension(file: &str) -> Option<&'static str> {
    let extension = Path::new(file)
        .extension()
        .and_then(|ext| ext.to_str())?
        .to_ascii_lowercase();
    match extension.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

/// The base64 body: sent inline when present, read off disk when only a path was given.
fn payload(
    attachment: &Attachment,
    label: &str,
    mime_type: &str,
) -> Result<Option<String>, String> {
    if let Some(data) = attachment.data.as_deref() {
        let data = data.trim();
        if !data.is_empty() {
            return Ok(Some(strip_data_url(data)));
        }
    }

    let Some(path) = attachment
        .path
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    else {
        return Err(format!(
            "attachment {label}: carried neither `data` nor `path`, so there is nothing to send"
        ));
    };

    let metadata = std::fs::metadata(path)
        .map_err(|err| format!("attachment {label}: cannot read '{path}': {err}"))?;
    if metadata.len() > MAX_IMAGE_BYTES as u64 {
        return Err(format!(
            "attachment {label}: {path} is {} bytes, over the {MAX_IMAGE_BYTES}-byte per-image limit",
            metadata.len()
        ));
    }
    let bytes = std::fs::read(path)
        .map_err(|err| format!("attachment {label}: cannot read '{path}': {err}"))?;
    if bytes.is_empty() {
        return Err(format!("attachment {label}: '{path}' is empty"));
    }
    if !file_magic_matches(&bytes, mime_type) {
        return Err(format!(
            "attachment {label}: '{path}' does not look like {mime_type}"
        ));
    }

    Ok(Some(base64_encode(&bytes)))
}

/// Accept a full data URL as well as bare base64 — both are in the wild, and
/// silently sending `data:image/png;base64,…` as if it were base64 would corrupt
/// the image rather than fail.
fn strip_data_url(data: &str) -> String {
    match data.split_once(',') {
        Some((header, body)) if header.starts_with("data:") => body.to_string(),
        _ => data.to_string(),
    }
}

/// The decoded length a base64 body *should* produce, or `None` when it is not
/// base64 at all.
///
/// Length arithmetic instead of a full decode: a 10 MiB image should not be
/// decoded into a second 10 MiB buffer just to measure it, and a charset check
/// catches the mistakes that matter (a data URL that slipped through, a truncated
/// body) with no allocation at all.
fn decoded_len(encoded: &str) -> Option<usize> {
    if encoded.is_empty() || !encoded.bytes().all(is_base64_byte) {
        return None;
    }
    let padding = encoded
        .bytes()
        .rev()
        .take_while(|byte| *byte == b'=')
        .count();
    if padding > 2 {
        return None;
    }
    let length = encoded.len();
    if length % 4 != 0 {
        // Unpadded base64 is legal in the wire format but not what browsers emit;
        // round down rather than reject, so the size ceiling still applies.
        return Some(length / 4 * 3);
    }
    Some(length / 4 * 3 - padding)
}

fn is_base64_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=')
}

/// Sniff the leading bytes so a path pointing at, say, a JPEG named `.png` is
/// caught here rather than by the provider.
fn file_magic_matches(bytes: &[u8], mime_type: &str) -> bool {
    match mime_type {
        "image/png" => bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]),
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.len() > 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP",
        // An image type we do not sniff is still trusted: the ceiling and the
        // explicit mime type are the checks that matter.
        _ => true,
    }
}

/// Standard base64, the encoding every provider's image block expects.
fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attachment() -> Attachment {
        Attachment {
            path: None,
            data: None,
            mime_type: None,
            name: None,
        }
    }

    #[test]
    fn inline_base64_becomes_one_image_part() {
        let image = attachment();
        let image = Attachment {
            mime_type: Some("image/png".into()),
            data: Some("aGVsbG8=".into()),
            name: Some("shot.png".into()),
            ..image
        };

        let parts = content_parts(&[image]).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].as_image(), Some(("image/png", "aGVsbG8=")));
        match &parts[0] {
            ContentPart::Image { name, .. } => assert_eq!(name.as_deref(), Some("shot.png")),
            other => panic!("expected an image part, got {other:?}"),
        }
    }

    #[test]
    fn a_data_url_body_is_unwrapped() {
        let image = Attachment {
            mime_type: Some("image/png".into()),
            data: Some("data:image/png;base64,aGVsbG8=".into()),
            ..attachment()
        };
        assert_eq!(
            content_parts(&[image]).unwrap()[0].as_image(),
            Some(("image/png", "aGVsbG8="))
        );
    }

    #[test]
    fn a_data_url_in_the_mime_field_is_understood() {
        let image = Attachment {
            mime_type: Some("data:image/jpeg;base64".into()),
            data: Some("aGVsbG8=".into()),
            ..attachment()
        };
        assert_eq!(
            content_parts(&[image]).unwrap()[0].as_image(),
            Some(("image/jpeg", "aGVsbG8="))
        );
    }

    #[test]
    fn the_media_type_is_inferred_from_the_name() {
        let image = Attachment {
            name: Some("diagram.PNG".into()),
            data: Some("aGVsbG8=".into()),
            ..attachment()
        };
        assert_eq!(
            content_parts(&[image]).unwrap()[0].as_image(),
            Some(("image/png", "aGVsbG8="))
        );
    }

    #[test]
    fn a_png_on_disk_is_read_and_encoded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        let bytes = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x01, 0x02];
        std::fs::write(&path, bytes).unwrap();

        let image = Attachment {
            path: Some(path.display().to_string()),
            ..attachment()
        };
        let parts = content_parts(&[image]).unwrap();
        assert_eq!(parts[0].as_image().unwrap().0, "image/png");
        assert_eq!(parts[0].as_image().unwrap().1, base64_encode(&bytes));
    }

    #[test]
    fn a_file_that_is_not_the_declared_type_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake.png");
        std::fs::write(&path, b"not an image").unwrap();

        let image = Attachment {
            path: Some(path.display().to_string()),
            mime_type: Some("image/png".into()),
            ..attachment()
        };
        let err = content_parts(&[image]).unwrap_err();
        assert!(err.contains("does not look like image/png"), "{err}");
    }

    #[test]
    fn an_attachment_with_no_payload_is_an_error_not_a_silent_drop() {
        let image = Attachment {
            mime_type: Some("image/png".into()),
            name: Some("ghost.png".into()),
            ..attachment()
        };
        let err = content_parts(&[image]).unwrap_err();
        assert!(err.contains("'ghost.png'"), "{err}");
        assert!(err.contains("neither `data` nor `path`"), "{err}");
    }

    #[test]
    fn an_unknown_media_type_is_an_error() {
        let image = Attachment {
            data: Some("aGVsbG8=".into()),
            name: Some("blob.bin".into()),
            ..attachment()
        };
        let err = content_parts(&[image]).unwrap_err();
        assert!(err.contains("cannot tell its media type"), "{err}");
    }

    #[test]
    fn non_image_attachments_are_skipped_not_fatal() {
        let text = Attachment {
            mime_type: Some("text/plain".into()),
            data: Some("aGVsbG8=".into()),
            name: Some("notes.txt".into()),
            ..attachment()
        };
        assert!(content_parts(&[text]).unwrap().is_empty());
    }

    #[test]
    fn an_oversized_image_fails_the_request() {
        let image = Attachment {
            mime_type: Some("image/png".into()),
            // 4 bytes of base64 encode 3 bytes; claim just over the ceiling.
            data: Some("A".repeat((MAX_IMAGE_BYTES / 3 + 2) * 4)),
            ..attachment()
        };
        let err = content_parts(&[image]).unwrap_err();
        assert!(err.contains("per-image limit"), "{err}");
    }

    #[test]
    fn a_body_that_is_not_base64_is_an_error() {
        let image = Attachment {
            mime_type: Some("image/png".into()),
            data: Some("not base64 at all!".into()),
            ..attachment()
        };
        let err = content_parts(&[image]).unwrap_err();
        assert!(err.contains("not valid base64"), "{err}");
    }

    #[test]
    fn the_text_part_leads_and_nothing_changes_without_images() {
        assert!(with_leading_text("hi", Vec::new()).is_empty());

        let parts = with_leading_text(
            "what colour?",
            vec![ContentPart::image("image/png", "aGVsbG8=")],
        );
        assert_eq!(parts.len(), 2);
        match &parts[0] {
            ContentPart::Text { text } => assert_eq!(text, "what colour?"),
            other => panic!("expected the prompt text first, got {other:?}"),
        }
        assert!(parts[1].is_image());
    }

    #[test]
    fn decoded_len_matches_a_real_decode() {
        use base64::Engine;
        for encoded in ["aGVsbG8=", "aGVsbG8h", "YQ==", "YWJjZA=="] {
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap();
            assert_eq!(decoded_len(encoded), Some(decoded.len()), "{encoded}");
        }
        assert_eq!(decoded_len("a==="), None);
        assert_eq!(decoded_len(""), None);
    }
}
