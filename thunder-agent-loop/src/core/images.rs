//! Image attachment validation and codec helpers shared by every ingress
//! (daemon `run_task`, TUI `/image`, future plugins).
//!
//! The model-facing representation lives in
//! [`ContentPart`](crate::types::message::ContentPart); this module deals only
//! with raw bytes so an ingress can validate *before* it constructs that part:
//!
//! - **Magic bytes** are the authority on the real format, so a payload that
//!   claims `image/png` but contains an executable is rejected.
//! - **Limits** bound how many/large images a single turn may carry.
//! - **Base64** is decoded for validation and re-encoded canonically, so a host
//!   cannot smuggle a non-base64/whitespace-padded payload to the provider.

/// Maximum number of image attachments on a single user turn.
pub const MAX_IMAGES_PER_MESSAGE: usize = 10;
/// Maximum decoded size of a single image attachment (5 MiB).
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;

/// Check if a MIME type is an allowed image format.
pub fn is_allowed_image_mime(mime: &str) -> bool {
    matches!(
        mime.to_ascii_lowercase().as_str(),
        "image/png" | "image/jpeg" | "image/jpg" | "image/webp" | "image/gif"
    )
}

/// Detect an image MIME type from magic bytes.
///
/// Returns `None` when the bytes are not a recognized image, so the caller can
/// reject a payload whose declared MIME was a lie.
pub fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 8 && &bytes[0..8] == b"\x89PNG\r\n\x1a\n" {
        return Some("image/png");
    }
    if bytes.len() >= 3 && &bytes[0..3] == b"\xFF\xD8\xFF" {
        return Some("image/jpeg");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.len() >= 6 && (&bytes[0..6] == b"GIF87a" || &bytes[0..6] == b"GIF89a") {
        return Some("image/gif");
    }
    None
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encode bytes as standard base64 with `=` padding.
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;

        out.push(BASE64_ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(BASE64_ALPHABET[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(BASE64_ALPHABET[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(BASE64_ALPHABET[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Decode standard base64 (padding optional; ASCII whitespace ignored).
pub fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;

    for byte in input.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => continue,
            b'\n' | b'\r' | b' ' | b'\t' => continue,
            _ => return Err("Attachment payload is not valid base64".to_string()),
        } as u32;

        buf = (buf << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }

    Ok(out)
}

/// Strip an optional `data:<mime>;base64,` prefix and decode the payload.
pub fn decode_image_data_url(data: &str) -> Result<Vec<u8>, String> {
    let payload = data
        .split_once("base64,")
        .map(|(_, rest)| rest)
        .unwrap_or(data);
    base64_decode(payload)
}

/// Validate raw image bytes and return `(mime, canonical_base64)`.
///
/// Rejects non-images (magic-byte mismatch) and oversized payloads.
pub fn validate_image_bytes(bytes: &[u8], label: &str) -> Result<(&'static str, String), String> {
    if bytes.is_empty() {
        return Err("Empty image attachment".to_string());
    }
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "Image '{label}' is {:.1} MB, exceeding the {:.0} MB limit",
            bytes.len() as f64 / (1024.0 * 1024.0),
            MAX_IMAGE_BYTES as f64 / (1024.0 * 1024.0),
        ));
    }
    let mime = sniff_image_mime(bytes).ok_or_else(|| {
        format!("Attachment '{label}' is not a recognized image (png/jpeg/webp/gif)")
    })?;
    Ok((mime, base64_encode(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes() -> Vec<u8> {
        let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
        v.extend_from_slice(&[0u8; 16]);
        v
    }

    #[test]
    fn base64_round_trips() {
        for sample in [
            b"".as_slice(),
            b"f",
            b"fo",
            b"foo",
            b"foob",
            b"fooba",
            b"foobar",
        ] {
            let encoded = base64_encode(sample);
            assert_eq!(
                base64_decode(&encoded).unwrap(),
                sample,
                "sample {sample:?}"
            );
        }
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_decode("Zm9vYmFy").unwrap(), b"foobar");
    }

    #[test]
    fn base64_rejects_invalid_chars() {
        assert!(base64_decode("not*base64").is_err());
    }

    #[test]
    fn sniffs_known_formats() {
        assert_eq!(sniff_image_mime(&png_bytes()), Some("image/png"));
        assert_eq!(sniff_image_mime(b"GIF89a..."), Some("image/gif"));
        assert_eq!(sniff_image_mime(b"MZ\x90\x00 executable"), None);
    }

    #[test]
    fn validates_and_canonicalizes() {
        let (mime, b64) = validate_image_bytes(&png_bytes(), "x.png").unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(base64_decode(&b64).unwrap(), png_bytes());
    }
}
