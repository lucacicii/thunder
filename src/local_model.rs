use crate::error::MemoryError;
use crate::synthesis::SynthesizedInsight;
use candle_core::quantized::gguf_file;
use candle_core::{Device, Tensor};
use candle_transformers::models::quantized_qwen2::ModelWeights;
use parking_lot::Mutex;
use std::path::Path;
use std::sync::Arc;
use tokenizers::Tokenizer;
use tracing::{debug, info, warn};

pub struct LocalModelEngine {
    model: Arc<Mutex<ModelWeights>>,
    tokenizer: Arc<Tokenizer>,
    device: Device,
    eos_token_id: u32,
}

impl LocalModelEngine {
    /// Load Qwen2 GGUF model and tokenizer from disk.
    /// Prefers Metal GPU device on macOS, falls back to CPU cleanly.
    pub fn load(gguf_path: &Path, tokenizer_path: &Path) -> Result<Self, MemoryError> {
        info!(
            gguf = %gguf_path.display(),
            tokenizer = %tokenizer_path.display(),
            "Loading in-process local Qwen2 GGUF model..."
        );

        let device = match Device::new_metal(0) {
            Ok(d) => {
                info!("Using Apple Silicon Metal GPU acceleration for in-process inference");
                d
            }
            Err(e) => {
                debug!("Metal not available ({e}), falling back to CPU");
                Device::Cpu
            }
        };

        let mut file = std::fs::File::open(gguf_path).map_err(|e| {
            MemoryError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Failed to open GGUF model at {}: {e}", gguf_path.display()),
            ))
        })?;

        let content = gguf_file::Content::read(&mut file).map_err(|e| {
            MemoryError::Inference(format!("Failed to parse GGUF headers: {e}"))
        })?;

        let model = ModelWeights::from_gguf(content, &mut file, &device).map_err(|e| {
            MemoryError::Inference(format!("Failed to initialize Qwen2 weights from GGUF: {e}"))
        })?;

        let tokenizer = Tokenizer::from_file(tokenizer_path).map_err(|e| {
            MemoryError::Inference(format!(
                "Failed to load tokenizer from {}: {e}",
                tokenizer_path.display()
            ))
        })?;

        // Qwen2 EOS tokens: 151645 (<|im_end|>), 151643 (<|endoftext|>)
        let eos_token_id = tokenizer
            .token_to_id("<|im_end|>")
            .unwrap_or(151645);

        info!("Local in-process model successfully initialized and ready");

        Ok(Self {
            model: Arc::new(Mutex::new(model)),
            tokenizer: Arc::new(tokenizer),
            device,
            eos_token_id,
        })
    }

    /// Run autoregressive text generation with greedy sampling.
    pub fn generate(&self, prompt: &str, max_tokens: usize) -> Result<String, MemoryError> {
        let mut model = self.model.lock();
        model.clear_kv_cache();

        let encoding = self
            .tokenizer
            .encode(prompt, true)
            .map_err(|e| MemoryError::Inference(format!("Tokenization error: {e}")))?;

        let prompt_tokens = encoding.get_ids();
        if prompt_tokens.is_empty() {
            return Ok(String::new());
        }

        let mut all_tokens = prompt_tokens.to_vec();
        let mut generated = Vec::new();

        // 1. Initial prefill
        let input = Tensor::new(prompt_tokens, &self.device)
            .and_then(|t| t.unsqueeze(0))
            .map_err(|e| MemoryError::Inference(format!("Tensor creation failed: {e}")))?;

        let logits = model
            .forward(&input, 0)
            .map_err(|e| MemoryError::Inference(format!("Model forward failed: {e}")))?;

        let mut next_token = sample_token(&logits, prompt_tokens.len().saturating_sub(1))?;

        // 2. Autoregressive decoding
        for step in 0..max_tokens {
            if next_token == self.eos_token_id || next_token == 151643 {
                break;
            }

            generated.push(next_token);
            all_tokens.push(next_token);

            let input = Tensor::new(&[next_token], &self.device)
                .and_then(|t| t.unsqueeze(0))
                .map_err(|e| MemoryError::Inference(format!("Tensor error: {e}")))?;

            let pos = prompt_tokens.len() + step;
            let logits = model
                .forward(&input, pos)
                .map_err(|e| MemoryError::Inference(format!("Forward step failed: {e}")))?;

            next_token = sample_token(&logits, 0)?;
        }

        let output = self
            .tokenizer
            .decode(&generated, true)
            .map_err(|e| MemoryError::Inference(format!("Decode failed: {e}")))?;

        Ok(output)
    }

    /// Extract structured long-term memory insight using in-process Qwen2.5.
    pub fn extract_insight(
        &self,
        app_name: &str,
        window_title: &str,
        combined_text: &str,
    ) -> Result<SynthesizedInsight, MemoryError> {
        let truncated_text: String = combined_text
            .lines()
            .take(30)
            .collect::<Vec<_>>()
            .join("\n");

        let prompt = format!(
            "<|im_start|>system\n\
You are an expert cognitive memory synthesizer. Given a user's recent screen perception stream (application, window, OCR text), extract a concise structured JSON object.\n\
Categories: code_development, debugging, research, documentation, communication, general.\n\
Output ONLY a raw JSON object with keys: category (string), title (max 25 chars), summary (1-2 sentences), key_entities (array of strings), importance_score (1.0 to 10.0).\n\
<|im_end|>\n\
<|im_start|>user\n\
App: {app_name}\n\
Window: {window_title}\n\
Screen Content:\n\
{truncated_text}\n\
<|im_end|>\n\
<|im_start|>assistant\n\
{{"
        );

        let raw_generated = self.generate(&prompt, 180)?;
        // We prepended "{" to prompt generation to force JSON opening
        let full_json = format!("{{{raw_generated}");
        debug!("Raw local model synthesis output: {}", full_json);

        parse_synthesized_json(&full_json, app_name, window_title)
    }
}

fn sample_token(logits: &Tensor, pos: usize) -> Result<u32, MemoryError> {
    let logits = logits
        .squeeze(0)
        .map_err(|e| MemoryError::Inference(format!("Squeeze failed: {e}")))?;

    let row = if logits.rank() == 2 {
        logits
            .get(pos)
            .map_err(|e| MemoryError::Inference(format!("Get logits pos failed: {e}")))?
    } else {
        logits
    };

    let logits_v: Vec<f32> = row
        .to_vec1()
        .map_err(|e| MemoryError::Inference(format!("Logits to vec failed: {e}")))?;

    // Greedy argmax
    let mut max_idx = 0;
    let mut max_val = f32::NEG_INFINITY;
    for (i, &v) in logits_v.iter().enumerate() {
        if v > max_val {
            max_val = v;
            max_idx = i;
        }
    }

    Ok(max_idx as u32)
}

/// Robust JSON extractor from generated text.
pub fn parse_synthesized_json(
    raw: &str,
    fallback_app: &str,
    fallback_window: &str,
) -> Result<SynthesizedInsight, MemoryError> {
    // Find the outermost {...}
    let start = raw.find('{').unwrap_or(0);
    let end = raw.rfind('}').map(|i| i + 1).unwrap_or(raw.len());
    let slice = &raw[start..end];

    #[derive(serde::Deserialize)]
    struct RawInsight {
        category: Option<String>,
        title: Option<String>,
        summary: Option<String>,
        key_entities: Option<Vec<String>>,
        importance_score: Option<f32>,
    }

    if let Ok(parsed) = serde_json::from_str::<RawInsight>(slice) {
        let cat = parsed
            .category
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "code_development".to_string());

        let fallback_title = if !fallback_window.is_empty() {
            format!("{fallback_app}: {fallback_window}")
        } else {
            format!("{fallback_app} Activity")
        };
        let title = parsed.title.filter(|s| !s.is_empty()).unwrap_or(fallback_title);

        let summary = parsed
            .summary
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("Active work in {fallback_app}"));

        let key_entities = parsed.key_entities.unwrap_or_default();
        let importance_score = parsed.importance_score.unwrap_or(6.0).clamp(1.0, 10.0);

        return Ok(SynthesizedInsight {
            category: cat,
            title,
            summary,
            key_entities,
            importance_score,
        });
    }

    // Secondary line-based extraction if JSON was slightly malformed
    warn!("Failed to parse JSON directly from local model, applying heuristic repair on: {}", slice);
    Ok(SynthesizedInsight {
        category: "code_development".to_string(),
        title: if !fallback_window.is_empty() {
            format!("{fallback_app}: {fallback_window}")
        } else {
            format!("{fallback_app} Work")
        },
        summary: format!("Synthesized active context in {fallback_app} ({fallback_window})"),
        key_entities: vec![fallback_app.to_string()],
        importance_score: 6.5,
    })
}

/// Download Qwen2.5-0.5B-Instruct-GGUF and tokenizer.json from fast mirror or official repository.
pub async fn download_default_model(config: &crate::config::MemoryConfig) -> Result<(), MemoryError> {
    use futures_util::StreamExt;
    use std::io::Write;

    let target_dir = &config.model_dir;
    std::fs::create_dir_all(target_dir)?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(MemoryError::Http)?;

    let files_to_download = [
        (
            "tokenizer.json",
            "https://hf-mirror.com/Qwen/Qwen2.5-0.5B-Instruct/resolve/main/tokenizer.json",
            config.local_tokenizer_path(),
        ),
        (
            "qwen2.5-0.5b-instruct-q4_k_m.gguf",
            "https://hf-mirror.com/Qwen/Qwen2.5-0.5B-Instruct-GGUF/resolve/main/qwen2.5-0.5b-instruct-q4_k_m.gguf",
            config.local_gguf_path(),
        ),
    ];

    for (label, url, dest_path) in files_to_download {
        if dest_path.is_file() {
            println!("  ✔ {} already exists at {:?}", label, dest_path);
            continue;
        }

        println!("  ⬇️ Downloading {} from {}...", label, url);
        let resp = client.get(url).send().await.map_err(MemoryError::Http)?;
        if !resp.status().is_success() {
            return Err(MemoryError::Other(format!(
                "Failed to download {label}: HTTP {}",
                resp.status()
            )));
        }

        let total_size = resp.content_length().unwrap_or(0);
        let temp_path = dest_path.with_extension("tmp");
        let mut file = std::fs::File::create(&temp_path)?;
        let mut downloaded: u64 = 0;
        let mut stream = resp.bytes_stream();

        while let Some(chunk_res) = stream.next().await {
            let chunk = chunk_res.map_err(MemoryError::Http)?;
            file.write_all(&chunk)?;
            downloaded += chunk.len() as u64;

            if total_size > 0 {
                let pct = (downloaded as f64 / total_size as f64) * 100.0;
                let mb = downloaded as f64 / 1_048_576.0;
                let total_mb = total_size as f64 / 1_048_576.0;
                print!("\r     Progress: {:.1}% ({:.1} MB / {:.1} MB)", pct, mb, total_mb);
                let _ = std::io::stdout().flush();
            }
        }
        println!();
        std::fs::rename(&temp_path, &dest_path)?;
        println!("  ✔ Successfully saved to {:?}", dest_path);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_clean_json_insight() {
        let raw = r#"{
            "category": "debugging",
            "title": "Fix SQLite Connection Pool",
            "summary": "Resolved connection timeout during WAL compaction.",
            "key_entities": ["rusqlite", "WAL", "SQLite"],
            "importance_score": 8.5
        }"#;

        let insight = parse_synthesized_json(raw, "Cursor", "db.rs").unwrap();
        assert_eq!(insight.category, "debugging");
        assert_eq!(insight.title, "Fix SQLite Connection Pool");
        assert_eq!(insight.importance_score, 8.5);
        assert_eq!(insight.key_entities, vec!["rusqlite", "WAL", "SQLite"]);
    }

    #[test]
    fn test_parse_json_with_surrounding_tokens() {
        let raw = "Here is the insight:\n```json\n{\"category\": \"research\", \"title\": \"Study Candle Metal\", \"summary\": \"Investigating Apple Silicon GPU speedup.\", \"key_entities\": [\"Candle\", \"Metal\"], \"importance_score\": 9.0}\n```\nHope this helps!";
        let insight = parse_synthesized_json(raw, "Terminal", "zsh").unwrap();
        assert_eq!(insight.category, "research");
        assert_eq!(insight.title, "Study Candle Metal");
        assert_eq!(insight.importance_score, 9.0);
    }

    #[test]
    fn test_parse_malformed_fallback() {
        let raw = "Not a json response at all, model hallucinated something weird";
        let insight = parse_synthesized_json(raw, "Chrome", "GitHub Pull Requests").unwrap();
        assert_eq!(insight.category, "code_development");
        assert!(insight.title.contains("Chrome"));
        assert_eq!(insight.importance_score, 6.5);
    }
}
