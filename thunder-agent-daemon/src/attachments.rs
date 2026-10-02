//! Image attachment ingestion for `run_task`.
//!
//! A host (Electron panel, TUI) sends attachments either as filesystem `path`s
//! or inline base64 `data`. Every byte is validated here before it can reach the
//! model. The codec/validation primitives live in
//! [`thunder_agent_loop::core::images`] so the TUI and daemon share one rule set.
//!
//! - **Path jail**: a `path` must live inside the workspace, one of the run's
//!   extra roots, or the system temp dir (where clipboard pastes land).
//! - **Magic bytes**: the real format is sniffed from the header, so a payload
//!   declared `image/png` but containing an executable is rejected.
//! - **Limits**: at most [`MAX_IMAGES_PER_MESSAGE`] images, each size-capped.

use crate::protocol::Attachment;
use std::path::{Path, PathBuf};
use thunder_agent_loop::core::images::{
    base64_decode, is_allowed_image_mime, validate_image_bytes, MAX_IMAGES_PER_MESSAGE,
};
use thunder_agent_loop::types::message::ContentPart;

/// Resolve wire-level [`Attachment`]s into multimodal [`ContentPart`]s.
///
/// Returns an error (surfaced to the host, not the model) when any attachment
/// fails validation, so a bad upload never silently degrades to text.
pub async fn resolve_attachments(
    attachments: &[Attachment],
    workspace: &Path,
    extra_roots: &[String],
) -> Result<Vec<ContentPart>, String> {
    if attachments.is_empty() {
        return Ok(Vec::new());
    }
    if attachments.len() > MAX_IMAGES_PER_MESSAGE {
        return Err(format!(
            "Too many image attachments: {} (maximum {})",
            attachments.len(),
            MAX_IMAGES_PER_MESSAGE
        ));
    }

    let mut out = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        let (bytes, declared_mime) =
            load_attachment_bytes(attachment, workspace, extra_roots).await?;

        if let Some(ref declared) = declared_mime {
            if !is_allowed_image_mime(declared) {
                return Err(format!("Unsupported image type '{declared}'"));
            }
        }

        // The sniffed type is authoritative: it is what the provider will see.
        let label = attachment.name.as_deref().unwrap_or("attachment");
        let (mime, data) = validate_image_bytes(&bytes, label)?;

        out.push(ContentPart::Image {
            mime_type: mime.to_string(),
            data,
            name: attachment.name.clone(),
            path: None,
            sha256: None,
        });
    }

    Ok(out)
}

/// Does the selected model accept image input?
pub fn model_supports_images(input: &[String]) -> bool {
    input.iter().any(|m| m.eq_ignore_ascii_case("image"))
}

async fn load_attachment_bytes(
    attachment: &Attachment,
    workspace: &Path,
    extra_roots: &[String],
) -> Result<(Vec<u8>, Option<String>), String> {
    if let Some(ref path_str) = attachment.path {
        let raw = Path::new(path_str);
        let path: PathBuf = if raw.is_relative() {
            workspace.join(raw)
        } else {
            raw.to_path_buf()
        };

        if !path_is_allowed(&path, workspace, extra_roots).await {
            return Err(format!(
                "Attachment path '{}' is outside the workspace, shared roots, and temp dir",
                path.display()
            ));
        }

        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| format!("Failed to read attachment '{}': {e}", path.display()))?;
        Ok((bytes, attachment.mime_type.clone()))
    } else if let Some(ref data) = attachment.data {
        let bytes = base64_decode(
            data.split_once("base64,")
                .map(|(_, rest)| rest)
                .unwrap_or(data.as_str()),
        )?;
        Ok((bytes, attachment.mime_type.clone()))
    } else {
        Err("Attachment requires either 'path' or 'data'".to_string())
    }
}

async fn path_is_allowed(path: &Path, workspace: &Path, extra_roots: &[String]) -> bool {
    if within(path, workspace).await {
        return true;
    }
    for root in extra_roots {
        if within(path, Path::new(root)).await {
            return true;
        }
    }
    within(path, &std::env::temp_dir()).await
}

async fn within(path: &Path, root: &Path) -> bool {
    let (Ok(root), Ok(path)) = (
        tokio::fs::canonicalize(root).await,
        tokio::fs::canonicalize(path).await,
    ) else {
        return false;
    };
    path.starts_with(&root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use thunder_agent_loop::core::images::{base64_encode, MAX_IMAGE_BYTES};

    fn png_bytes() -> Vec<u8> {
        let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
        v.extend_from_slice(&[0u8; 16]);
        v
    }

    #[tokio::test]
    async fn resolves_inline_png() {
        let data = base64_encode(&png_bytes());
        let attachment = Attachment {
            path: None,
            data: Some(data),
            mime_type: Some("image/png".to_string()),
            name: Some("pixel.png".to_string()),
        };
        let parts = resolve_attachments(&[attachment], Path::new("."), &[])
            .await
            .unwrap();
        assert_eq!(parts.len(), 1);
        assert!(parts[0].is_image());
        assert_eq!(parts[0].as_image().unwrap().0, "image/png");
    }

    #[tokio::test]
    async fn rejects_non_image_payload() {
        let data = base64_encode(b"MZ\x90\x00 executable");
        let attachment = Attachment {
            path: None,
            data: Some(data),
            mime_type: Some("image/png".to_string()),
            name: None,
        };
        let err = resolve_attachments(&[attachment], Path::new("."), &[])
            .await
            .unwrap_err();
        assert!(err.contains("not a recognized image"), "got: {err}");
    }

    #[tokio::test]
    async fn rejects_too_many_images() {
        let data = base64_encode(&png_bytes());
        let one = || Attachment {
            path: None,
            data: Some(data.clone()),
            mime_type: Some("image/png".to_string()),
            name: None,
        };
        let many: Vec<Attachment> = (0..=MAX_IMAGES_PER_MESSAGE).map(|_| one()).collect();
        let err = resolve_attachments(&many, Path::new("."), &[])
            .await
            .unwrap_err();
        assert!(err.contains("Too many image attachments"), "got: {err}");
    }

    #[tokio::test]
    async fn rejects_oversized_image() {
        // A PNG header plus a payload over the cap; the header is valid so the
        // size check is what must trip.
        let mut bytes = png_bytes();
        bytes.resize(MAX_IMAGE_BYTES + 1, 0);
        let attachment = Attachment {
            path: None,
            data: Some(base64_encode(&bytes)),
            mime_type: Some("image/png".to_string()),
            name: Some("huge.png".to_string()),
        };
        let err = resolve_attachments(&[attachment], Path::new("."), &[])
            .await
            .unwrap_err();
        assert!(err.contains("exceeding"), "got: {err}");
    }
}
