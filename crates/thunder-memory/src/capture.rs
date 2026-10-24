use crate::error::MemoryError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{debug, info};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrItem {
    pub text: String,
    pub confidence: f32,
    pub box_rect: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawCaptureJson {
    pub timestamp_ms: i64,
    pub app_name: String,
    pub bundle_id: String,
    pub window_title: String,
    pub full_text: String,
    pub avg_confidence: f32,
    pub items: Vec<OcrItemRaw>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrItemRaw {
    pub text: String,
    pub confidence: f32,
    #[serde(rename = "box")]
    pub box_rect: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenCaptureRecord {
    pub timestamp_ms: i64,
    pub app_name: String,
    pub bundle_id: String,
    pub window_title: String,
    pub full_text: String,
    pub avg_confidence: f32,
    pub content_hash: String,
    pub metadata_json: String,
}

pub struct OcrEngine {
    helper_path: PathBuf,
}

impl OcrEngine {
    pub fn new() -> Result<Self, MemoryError> {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let bin_dir = PathBuf::from(home).join(".thunder").join("bin");
        std::fs::create_dir_all(&bin_dir)?;
        let helper_path = bin_dir.join("thunder-vision-ocr");

        let engine = Self { helper_path };
        engine.ensure_helper()?;
        Ok(engine)
    }

    /// Check if native Swift OCR binary is compiled; compile it if missing
    pub fn ensure_helper(&self) -> Result<(), MemoryError> {
        if self.helper_path.exists() {
            return Ok(());
        }

        info!(
            "Native Vision OCR helper not found at {:?}, checking Swift source...",
            self.helper_path
        );

        // Find swift source file
        let swift_src = Self::find_swift_source()?;
        info!("Compiling Swift OCR helper from {:?}...", swift_src);

        let output = Command::new("swiftc")
            .arg("-O")
            .arg(&swift_src)
            .arg("-o")
            .arg(&self.helper_path)
            .output()
            .map_err(|e| {
                MemoryError::Ocr(format!(
                    "Failed to invoke swiftc to compile {}: {}",
                    swift_src.display(),
                    e
                ))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(MemoryError::Ocr(format!(
                "swiftc compilation failed: {}",
                stderr
            )));
        }

        info!(
            "Successfully compiled native OCR helper to {:?}",
            self.helper_path
        );
        Ok(())
    }

    fn find_swift_source() -> Result<PathBuf, MemoryError> {
        // 1. Check relative to CARGO_MANIFEST_DIR or current directory
        let candidates = [
            PathBuf::from("crates/thunder-memory/swift/thunder_vision_ocr.swift"),
            PathBuf::from("swift/thunder_vision_ocr.swift"),
            PathBuf::from("../thunder-memory/swift/thunder_vision_ocr.swift"),
        ];

        for path in &candidates {
            if path.exists() {
                return Ok(path.clone());
            }
        }

        // 2. Embedded fallback content if file moved
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let fallback_path = PathBuf::from(home)
            .join(".thunder")
            .join("bin")
            .join("thunder_vision_ocr.swift");
        let embedded_swift = include_str!("../swift/thunder_vision_ocr.swift");
        std::fs::write(&fallback_path, embedded_swift)?;
        Ok(fallback_path)
    }

    /// Capture current screen and run Apple Vision OCR
    pub fn capture_screen(&self) -> Result<ScreenCaptureRecord, MemoryError> {
        let output = Command::new(&self.helper_path)
            .arg("--capture")
            .output()
            .map_err(|e| {
                MemoryError::Ocr(format!(
                    "Failed to execute OCR helper {:?}: {}",
                    self.helper_path, e
                ))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(MemoryError::Ocr(format!(
                "OCR capture helper failed: {}",
                stderr
            )));
        }

        self.parse_output(&output.stdout)
    }

    /// Run OCR on a specific image file
    pub fn ocr_image(&self, image_path: &Path) -> Result<ScreenCaptureRecord, MemoryError> {
        let output = Command::new(&self.helper_path)
            .arg(image_path)
            .output()
            .map_err(|e| {
                MemoryError::Ocr(format!(
                    "Failed to execute OCR helper on image {:?}: {}",
                    image_path, e
                ))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(MemoryError::Ocr(format!("OCR helper failed: {}", stderr)));
        }

        self.parse_output(&output.stdout)
    }

    fn parse_output(&self, stdout: &[u8]) -> Result<ScreenCaptureRecord, MemoryError> {
        let raw: RawCaptureJson = serde_json::from_slice(stdout).map_err(|e| {
            let text = String::from_utf8_lossy(stdout);
            MemoryError::Ocr(format!(
                "Failed to parse OCR helper JSON: {} (raw: {})",
                e, text
            ))
        })?;

        let content_hash = Self::compute_hash(&raw.app_name, &raw.window_title, &raw.full_text);

        let items: Vec<OcrItem> = raw
            .items
            .into_iter()
            .map(|i| OcrItem {
                text: i.text,
                confidence: i.confidence,
                box_rect: i.box_rect,
            })
            .collect();

        let metadata_json = serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string());

        debug!(
            "OCR complete: app='{}', win='{}', chars={}, hash={}",
            raw.app_name,
            raw.window_title,
            raw.full_text.len(),
            &content_hash[..8]
        );

        Ok(ScreenCaptureRecord {
            timestamp_ms: raw.timestamp_ms,
            app_name: raw.app_name,
            bundle_id: raw.bundle_id,
            window_title: raw.window_title,
            full_text: raw.full_text,
            avg_confidence: raw.avg_confidence,
            content_hash,
            metadata_json,
        })
    }

    pub fn compute_hash(app: &str, title: &str, text: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(app.as_bytes());
        hasher.update(b"|");
        hasher.update(title.as_bytes());
        hasher.update(b"|");
        // Normalize whitespace in text for perceptual stability
        let normalized: String = text.split_whitespace().collect::<Vec<&str>>().join(" ");
        hasher.update(normalized.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Check if content has changed compared to last hash
    pub fn is_diff(last_hash: Option<&str>, current_hash: &str) -> bool {
        match last_hash {
            Some(prev) => prev != current_hash,
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_hash_stability() {
        let h1 =
            OcrEngine::compute_hash("Cursor", "main.rs", "fn main() {   println!(\"hello\"); }");
        let h2 = OcrEngine::compute_hash(
            "Cursor",
            "main.rs",
            "fn main() {\n  println!(\"hello\");\n}",
        );
        assert_eq!(
            h1, h2,
            "Whitespace differences should normalize to same hash"
        );

        let h3 =
            OcrEngine::compute_hash("Cursor", "main.rs", "fn main() {   println!(\"world\"); }");
        assert_ne!(h1, h3, "Text changes must produce different hashes");
    }

    #[test]
    fn test_is_diff() {
        assert!(OcrEngine::is_diff(None, "hash1"));
        assert!(!OcrEngine::is_diff(Some("hash1"), "hash1"));
        assert!(OcrEngine::is_diff(Some("hash1"), "hash2"));
    }
}
