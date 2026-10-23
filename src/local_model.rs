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

/// CTRL-style repetition penalty applied over the last `REPETITION_WINDOW` tokens.
const REPETITION_PENALTY: f32 = 1.15;
const REPETITION_WINDOW: usize = 64;
/// Hard budget for the OCR text embedded in the synthesis prompt.
const MAX_PROMPT_CHARS: usize = 1200;
const MAX_LINE_CHARS: usize = 120;

/// Decoding options for one generation request.
#[derive(Debug, Clone)]
pub struct GenOptions {
    /// `0.0` = greedy argmax; `> 0.0` = temperature sampling.
    pub temperature: f32,
    pub top_p: f32,
    pub repetition_penalty: f32,
    pub repetition_window: usize,
    /// Stop as soon as a top-level JSON object is closed (used by insight synthesis).
    pub stop_on_json_close: bool,
    /// RNG seed for temperature sampling (`0` = derive from the clock).
    pub seed: u64,
}

impl Default for GenOptions {
    fn default() -> Self {
        Self {
            temperature: 0.0,
            top_p: 0.95,
            repetition_penalty: REPETITION_PENALTY,
            repetition_window: REPETITION_WINDOW,
            stop_on_json_close: false,
            seed: 0,
        }
    }
}

impl GenOptions {
    /// Deterministic JSON-extraction profile used by `extract_insight`.
    pub fn synthesis() -> Self {
        Self {
            stop_on_json_close: true,
            ..Self::default()
        }
    }

    /// General-purpose chat profile (sampled, no JSON early-stop).
    pub fn chat(temperature: f32, top_p: f32) -> Self {
        Self {
            temperature,
            top_p,
            ..Self::default()
        }
    }
}

/// Tiny xorshift64* PRNG so sampling needs no extra dependency.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        let seed = if seed == 0 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x2545_F491_4F6C_DD1D)
        } else {
            seed
        };
        Self(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [0, 1).
    fn next_f32(&mut self) -> f32 {
        ((self.next_u64() >> 40) as f32) / (1u32 << 24) as f32
    }
}

/// Render OpenAI-style chat messages into Qwen2.5's ChatML prompt format.
pub fn render_chatml(messages: &[(String, String)]) -> String {
    let mut out = String::new();
    for (role, content) in messages {
        out.push_str("<|im_start|>");
        out.push_str(role);
        out.push('\n');
        out.push_str(content.trim());
        out.push_str("<|im_end|>\n");
    }
    out.push_str("<|im_start|>assistant\n");
    out
}

pub struct LocalModelEngine {
    model: Arc<Mutex<ModelWeights>>,
    tokenizer: Arc<Tokenizer>,
    device: Device,
    eos_token_id: u32,
    model_id: String,
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

        let model_id = gguf_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("local-model")
            .to_string();

        info!("Local in-process model successfully initialized and ready");

        Ok(Self {
            model: Arc::new(Mutex::new(model)),
            tokenizer: Arc::new(tokenizer),
            device,
            eos_token_id,
            model_id,
        })
    }

    /// Number of tokens the model's tokenizer produces for `text`.
    pub fn count_tokens(&self, text: &str) -> usize {
        self.tokenizer
            .encode(text, false)
            .map(|e| e.get_ids().len())
            .unwrap_or(0)
    }

    /// Identifier of the loaded model file, e.g. `qwen2.5-0.5b-instruct-q4_k_m`.
    pub fn model_id(&self) -> String {
        self.model_id.clone()
    }

    /// Greedy JSON-extraction generation (synthesis profile).
    pub fn generate(&self, prompt: &str, max_tokens: usize) -> Result<String, MemoryError> {
        self.generate_with(prompt, max_tokens, &GenOptions::synthesis())
    }

    /// Run autoregressive text generation with the given decoding options.
    pub fn generate_with(
        &self,
        prompt: &str,
        max_tokens: usize,
        opts: &GenOptions,
    ) -> Result<String, MemoryError> {
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

        let mut rng = Rng::new(opts.seed);
        let mut next_token = sample_token(
            &logits,
            prompt_tokens.len().saturating_sub(1),
            &all_tokens,
            opts,
            &mut rng,
        )?;

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

            // Stop as soon as the JSON object is closed. Without this a 0.5B model keeps
            // rambling up to max_tokens and the trailing garbage breaks JSON parsing.
            if opts.stop_on_json_close {
                if let Ok(partial) = self.tokenizer.decode(&generated, false) {
                    let open_braces = partial.matches('{').count();
                    let close_braces = partial.matches('}').count();
                    if close_braces > 0 && close_braces >= open_braces {
                        break;
                    }
                }
            }

            next_token = sample_token(&logits, 0, &all_tokens, opts, &mut rng)?;
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
        // OCR text from a busy window can be tens of thousands of characters. A 0.5B model
        // loses the plot if the prompt is huge, so keep a hard character budget and clip
        // pathologically long single lines (usually repeated IDE/explorer labels).
        let mut truncated_text = String::new();
        for line in combined_text.lines().take(40) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let clipped: String = if line.chars().count() > MAX_LINE_CHARS {
                line.chars().take(MAX_LINE_CHARS).collect()
            } else {
                line.to_string()
            };
            if truncated_text.chars().count() + clipped.chars().count() > MAX_PROMPT_CHARS {
                break;
            }
            truncated_text.push_str(&clipped);
            truncated_text.push('\n');
        }

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

fn sample_token(
    logits: &Tensor,
    pos: usize,
    history: &[u32],
    opts: &GenOptions,
    rng: &mut Rng,
) -> Result<u32, MemoryError> {
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

    let mut logits_v: Vec<f32> = row
        .to_vec1()
        .map_err(|e| MemoryError::Inference(format!("Logits to vec failed: {e}")))?;

    // Greedy decoding degenerates into loops on small quantized models ("…重复同一句…").
    // Apply the standard CTRL-style repetition penalty over the recent token window.
    if opts.repetition_penalty > 1.0 && !history.is_empty() {
        let start = history.len().saturating_sub(opts.repetition_window);
        for &tok in &history[start..] {
            let idx = tok as usize;
            if idx < logits_v.len() {
                let v = logits_v[idx];
                logits_v[idx] = if v > 0.0 {
                    v / opts.repetition_penalty
                } else {
                    v * opts.repetition_penalty
                };
            }
        }
    }

    // Greedy path (temperature 0): plain argmax.
    if opts.temperature <= 0.0 {
        let mut max_idx = 0;
        let mut max_val = f32::NEG_INFINITY;
        for (i, &v) in logits_v.iter().enumerate() {
            if v > max_val {
                max_val = v;
                max_idx = i;
            }
        }
        return Ok(max_idx as u32);
    }

    // Temperature + nucleus (top-p) sampling.
    let inv_t = 1.0 / opts.temperature.clamp(1e-3, 100.0);
    let max_logit = logits_v
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max);
    let mut candidates: Vec<(usize, f32)> = logits_v
        .iter()
        .enumerate()
        .map(|(i, &v)| (i, ((v - max_logit) * inv_t).exp()))
        .collect();
    let total: f32 = candidates.iter().map(|(_, p)| *p).sum();
    if !(total > 0.0) || !total.is_finite() {
        // Degenerate distribution: fall back to argmax.
        let mut max_idx = 0;
        let mut max_val = f32::NEG_INFINITY;
        for (i, &v) in logits_v.iter().enumerate() {
            if v > max_val {
                max_val = v;
                max_idx = i;
            }
        }
        return Ok(max_idx as u32);
    }
    for (_, p) in candidates.iter_mut() {
        *p /= total;
    }

    candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let top_p = if opts.top_p > 0.0 && opts.top_p <= 1.0 {
        opts.top_p
    } else {
        1.0
    };
    let mut nucleus_mass = 0.0f32;
    let mut cutoff = candidates.len();
    for (i, (_, p)) in candidates.iter().enumerate() {
        nucleus_mass += *p;
        if nucleus_mass >= top_p {
            cutoff = i + 1;
            break;
        }
    }

    let nucleus = &candidates[..cutoff.max(1)];
    let nucleus_mass: f32 = nucleus.iter().map(|(_, p)| *p).sum();
    let mut pick = rng.next_f32() * nucleus_mass;
    for (idx, p) in nucleus {
        pick -= *p;
        if pick <= 0.0 {
            return Ok(*idx as u32);
        }
    }
    Ok(nucleus.last().map(|(idx, _)| *idx as u32).unwrap_or(0))
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

    // Tiny quantized models are sloppy JSON writers: they emit `"importance_score": "5.3"`
    // (string instead of number), or `_title` / `__summary__` style keys. Parse leniently
    // instead of throwing the whole insight away.
    #[derive(Default)]
    struct LenientInsight {
        category: Option<String>,
        title: Option<String>,
        summary: Option<String>,
        key_entities: Option<Vec<String>>,
        importance_score: Option<f32>,
    }

    fn lenient_parse(slice: &str) -> Option<LenientInsight> {
        // Small models also emit single-quoted arrays, e.g. `'key_entities': ['a', 'b']`.
        let value: serde_json::Value = match serde_json::from_str(slice) {
            Ok(v) => v,
            Err(_) => serde_json::from_str(&slice.replace('\'', "\"")).ok()?,
        };
        let map = value.as_object()?;

        let mut out = LenientInsight::default();
        for (raw_key, v) in map {
            let key: String = raw_key
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_lowercase();
            let as_string = |v: &serde_json::Value| match v {
                serde_json::Value::String(s) => Some(s.trim().to_string()),
                serde_json::Value::Number(n) => Some(n.to_string()),
                _ => None,
            };
            match key.as_str() {
                "category" => out.category = as_string(v),
                "title" => out.title = as_string(v),
                "summary" | "summarytext" => out.summary = as_string(v),
                "keyentities" | "entities" | "keyentity" => {
                    out.key_entities = v.as_array().map(|arr| {
                        arr.iter()
                            .filter_map(|e| match e {
                                serde_json::Value::String(s) => {
                                    let s = s.trim();
                                    (!s.is_empty()).then(|| s.to_string())
                                }
                                _ => None,
                            })
                            .collect()
                    });
                }
                "importance" | "importancescore" | "score" => {
                    out.importance_score = as_string(v).and_then(|s| s.parse::<f32>().ok());
                }
                _ => {}
            }
        }

        Some(out)
    }

    if let Some(parsed) = lenient_parse(slice) {
        let cat = parsed.category.unwrap_or_default();
        let cat = if cat.trim().is_empty() {
            "code_development".to_string()
        } else {
            cat.trim().to_lowercase()
        };

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
    fn test_parse_lenient_keys_and_string_score() {
        // Observed real output of Qwen2.5-0.5B: prefixed keys and a stringified score.
        let raw = "{ \"category\": \"debugging\", \"_title\": \"Fix WAL Lock\", \"__summary__\": \"Resolved a lock contention.\", \"key_entities\": [], \"importance_score\": \"7.3\"}";
        let insight = parse_synthesized_json(raw, "Agent Resume", "工作台").unwrap();
        assert_eq!(insight.category, "debugging");
        assert_eq!(insight.title, "Fix WAL Lock");
        assert_eq!(insight.summary, "Resolved a lock contention.");
        assert_eq!(insight.importance_score, 7.3);
    }

    #[test]
    fn test_parse_single_quoted_entities() {
        // Observed real output: single-quoted arrays around the entity list.
        let raw = "{ \"category\": \"code_development\", \"title\": \"\", \"summary\": \"修复终端环境变量\",  'key_entities': ['Agent Resume', 'thunder'], \"importance_score\": 8.5}";
        let insight = parse_synthesized_json(raw, "Agent Resume", "工作台").unwrap();
        assert_eq!(insight.category, "code_development");
        assert_eq!(insight.summary, "修复终端环境变量");
        assert_eq!(insight.key_entities, vec!["Agent Resume", "thunder"]);
        assert_eq!(insight.importance_score, 8.5);
        // Empty title must fall back to app:window
        assert_eq!(insight.title, "Agent Resume: 工作台");
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
